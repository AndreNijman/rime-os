#!/usr/bin/env python3
"""rime-luks-tpm-status: does each LUKS volume's TPM slot still unseal on THIS boot's PCR values?

    rime-luks-tpm-status [--write FILE]

Prints one JSON report (and, with --write, also replaces FILE with it atomically). Run as root: the LUKS2 header
is read with `cryptsetup luksDump --dump-json-metadata`. nix/modules/luks-tpm.nix runs it once per boot
(rime-luks-tpm-status.service -> /run/rime/luks-tpm.json, which `rime doctor` reads unprivileged) and after every
re-bind; `rime luks status|rebind` run it live.

How (no TPM command, nothing unsealed): systemd-cryptenroll seals the volume key to a TPM2 policy and stores that
policy's digest in the token (`tpm2-policy-hash`). For a PCR-value policy that digest is a pure function of the
selected PCRs' values, so it is recomputed here from the kernel's PCR readout
(/sys/class/tpm/tpm0/pcr-<bank>/<n>) the way systemd computes it (src/shared/tpm2-util.c,
tpm2_calculate_policy_pcr / tpm2_calculate_policy_auth_value):

    digest_0 = 32 zero bytes
    PolicyPCR:       digest = H(digest || CC_PolicyPCR 0x0000017F || TPML_PCR_SELECTION || H(PCR values, ascending))
    PolicyAuthValue: digest = H(digest || CC_PolicyAuthValue 0x0000016B)          (only for a token with a PIN)
    TPML_PCR_SELECTION = count 1 (u32 BE) || hash alg (u16 BE) || sizeofSelect 3 || 3-byte bitmap

Equal: the TPM will unseal it on this boot. Different: the PCR values changed since the enrolment (a new Secure
Boot key, a dbx update, firmware that measures differently), the volume asked for its passphrase at this boot and
will at every boot until it is re-bound (`rime luks rebind`). A signed (PolicyAuthorize, tpm2_pubkey_pcrs) or
pcrlock token cannot be judged this way and is reported "unchecked" with the reason, never "ok".

States per volume: ok | stale | unchecked | no-tpm-slot.
"""
import datetime
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile

SYSFS_TPM = os.environ.get("RIME_LUKS_TPM_SYSFS", "/sys/class/tpm/tpm0")
CC_POLICY_PCR = 0x0000017F
CC_POLICY_AUTH_VALUE = 0x0000016B
ALG = {"sha1": (0x0004, hashlib.sha1), "sha256": (0x000B, hashlib.sha256), "sha384": (0x000C, hashlib.sha384),
       "sha512": (0x000D, hashlib.sha512)}


def luks_volumes():
    """[(uuid, device)] of every LUKS block device the kernel knows."""
    out = subprocess.run(["lsblk", "-J", "-p", "-o", "PATH,FSTYPE,UUID"], capture_output=True, text=True, check=True)
    vols = []

    def walk(devs):
        for d in devs or []:
            if d.get("fstype") == "crypto_LUKS" and d.get("uuid"):
                vols.append((d["uuid"], d["path"]))
            walk(d.get("children"))

    walk(json.loads(out.stdout).get("blockdevices"))
    return sorted(set(vols))


def pcr_value(bank, n):
    p = os.path.join(SYSFS_TPM, f"pcr-{bank}", str(n))
    with open(p) as f:
        return bytes.fromhex(f.read().strip())


def policy_pcr_digest(bank, pcrs, values, pin):
    alg_id, h = ALG[bank]
    bitmap = bytearray(3)
    for n in pcrs:
        bitmap[n // 8] |= 1 << (n % 8)
    selection = (1).to_bytes(4, "big") + alg_id.to_bytes(2, "big") + bytes([3]) + bytes(bitmap)
    pcr_digest = h(b"".join(values[n] for n in sorted(pcrs))).digest()
    d = bytes(32)  # the policy session's own hash is SHA-256 whatever the PCR bank
    d = hashlib.sha256(d + CC_POLICY_PCR.to_bytes(4, "big") + selection + pcr_digest).digest()
    if pin:
        d = hashlib.sha256(d + CC_POLICY_AUTH_VALUE.to_bytes(4, "big")).digest()
    return d.hex()


def judge(uuid, device):
    v = {"uuid": uuid, "device": device}
    r = subprocess.run(["cryptsetup", "luksDump", "--dump-json-metadata", device], capture_output=True, text=True)
    if r.returncode != 0:
        return v | {"state": "unchecked", "why": f"cryptsetup luksDump failed: {r.stderr.strip() or r.returncode}"}
    tokens = [t for t in json.loads(r.stdout).get("tokens", {}).values() if t.get("type") == "systemd-tpm2"]
    if not tokens:
        return v | {"state": "no-tpm-slot", "why": "no systemd-tpm2 token: this volume asks for its passphrase"}
    if len(tokens) > 1:
        return v | {"state": "unchecked", "why": f"{len(tokens)} systemd-tpm2 tokens; Rime enrols exactly one"}
    t = tokens[0]
    pcrs = sorted(t.get("tpm2-pcrs") or [])
    bank = t.get("tpm2-pcr-bank") or "sha256"
    v |= {"pcrs": pcrs, "bank": bank, "pin": bool(t.get("tpm2-pin", False))}
    if t.get("tpm2_pubkey_pcrs") or t.get("tpm2_pubkey"):
        return v | {"state": "unchecked", "why": "a signed PCR policy (PolicyAuthorize): its digest names a key, not "
                                                 "PCR values, so this check cannot tell"}
    if t.get("tpm2_pcrlock"):
        return v | {"state": "unchecked", "why": "a pcrlock policy: judged by systemd-pcrlock, not here"}
    if not pcrs:
        return v | {"state": "stale", "why": "sealed to NO PCR: any boot chain unseals it (rime-luks-enroll refuses "
                                             "to write this; re-bind it)"}
    if bank not in ALG:
        return v | {"state": "unchecked", "why": f"PCR bank {bank!r} is not one this check knows"}
    try:
        values = {n: pcr_value(bank, n) for n in pcrs}
    except OSError as e:
        return v | {"state": "unchecked", "why": f"cannot read PCR values: {e}"}
    want = (t.get("tpm2-policy-hash") or "").lower()
    got = policy_pcr_digest(bank, pcrs, values, v["pin"])
    v |= {"policy_sealed": want, "policy_now": got}
    if got == want:
        return v | {"state": "ok", "why": f"its TPM slot matches this boot's PCR {','.join(map(str, pcrs))}"}
    return v | {"state": "stale", "why": f"its TPM slot was sealed to other PCR {','.join(map(str, pcrs))} values "
                                         "than this boot's (a new Secure Boot key, a dbx update or a firmware change "
                                         "since the enrolment), so it asks for the passphrase at every boot"}


def main(argv):
    write = None
    if argv[:1] == ["--write"] and len(argv) == 2:
        write = argv[1]
    elif argv:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    report = {"measured_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
              "tpm": os.path.isdir(SYSFS_TPM), "volumes": [judge(u, d) for u, d in luks_volumes()]}
    text = json.dumps(report, indent=2) + "\n"
    if write:
        os.makedirs(os.path.dirname(write), exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=os.path.dirname(write), prefix=".luks-tpm.")
        with os.fdopen(fd, "w") as f:
            f.write(text)
        os.chmod(tmp, 0o644)
        os.replace(tmp, write)
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
