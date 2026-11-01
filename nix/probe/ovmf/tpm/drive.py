#!/usr/bin/env python3
"""drive.py: the TPM lab's boot sequence in OVMF + swtpm, Secure Boot on (README.md here). Runs INSIDE rime-ovmf-tpm.

    drive.py SCENARIO LUKS_UUID       SCENARIO = main | mutant-bootc-reenrol

One varstore (Microsoft keys + the TEST MOK in MokList), one swtpm state directory, one LUKS2 data disk and one NixOS
disk (a qcow2 overlay over $LAB/disks/bls/disk.raw) carry over between boots, as they would on one machine.

main:
  P1  probe-fedora   warm-up (shim's first boot may write SbatLevel, which PCR 7 measures)
  P2  probe-fedora   BEFORE, Fedora chain: shim -> GRUB -> Fedora-signed kernel        (T1)
  P3  probe-mok      BEFORE, bootc-Rime chain: shim -> GRUB -> TEST-MOK-signed kernel   (T1)
  P4  probe-mok      the same again: PCR 7 is stable across boots
  N1  NixOS, init    AFTER: shim -> GRUB -> Rime boot hook's TEST-MOK-signed NixOS kernel (T2). No TPM slot yet: the
                     passphrase is typed; stage 2 writes a marker and seals a TPM slot to P3's PCR 7 (the bootc slot)
  N2  NixOS, rebind  the bootc slot must unseal with no prompt; then the slot is re-sealed to P2's PCR 7 (a slot this
                     chain does not satisfy, as after a Secure Boot key change)
  N3  NixOS, reenrol the stale slot is refused, the passphrase is typed; rime-luks-tpm-reenrol@ (rime-luks-enroll
                     --tpm-only) re-binds the TPM slot; the recovery and passphrase slots must be byte-identical
  N4  NixOS, clear   the new slot unseals with no prompt; then tpm2_clear
  N5  NixOS          TPM cleared: the PRE-EXISTING recovery key (made with the disk) is typed and unlocks   (T3)
  N6  NixOS, no TPM  no TPM device at all: the recovery key unlocks                                         (T3)
mutant-bootc-reenrol (the lab must catch bootc's re-enrolment, which replaced the recovery key; S3):
  M1  NixOS, init    passphrase; TPM slot sealed to a PCR 7 value nothing measures
  M2  NixOS, reenrol-bootc   passphrase; rime-luks-enroll's FULL path (--replace-recovery: what bootc did every run)
  M3  NixOS, clear   TPM unlock; tpm2_clear
  M4  NixOS          the pre-existing recovery key is typed: it must be REFUSED (the scenario's verdict is FAIL)

Every verdict needs a positive marker; a timeout is never a pass. Evidence: $OUT/SCENARIO/{NAME.serial.txt,
NAME.eventlog.bin, NAME.eventlog.yaml, NAME.pcr7-events.txt, NAME.swtpm.log}, $OUT/SCENARIO/results.tsv, pcr7.txt.
"""
import base64
import os
import re
import socket
import subprocess
import sys
import time

LAB = os.environ.get("LAB", "/lab")
OUT = os.environ.get("OUT", "/out")
CODE = f"{LAB}/fw/code-secboot.raw"
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b[()][A-Z0-9]|\r")
ZERO = "0" * 64


def stripped(path):
    try:
        with open(path, "rb") as f:
            return ANSI.sub(b"", f.read()).decode("utf-8", "replace")
    except FileNotFoundError:
        return ""


def secret(name):
    with open(f"{LAB}/keys/{name}") as f:
        return f.read().strip()


class Boot:
    def __init__(self, sc, name, disk, *, tpm=True, data=False, action=None, secrets=(), timeout=420):
        self.sc, self.name, self.timeout, self.secrets = sc, name, timeout, list(secrets)
        self.base = f"{sc.dir}/{name}"
        self.serial = self.base + ".serial.log"
        self.ser_sock = f"/tmp/ser-{sc.name}-{name}.sock"
        self.tpm_sock = f"/tmp/swtpm-{sc.name}-{name}.sock"
        for p in (self.serial, self.ser_sock, self.tpm_sock):
            if os.path.exists(p):
                os.unlink(p)
        self.swtpm = None
        argv = [
            "qemu-system-x86_64",
            "-machine", "q35,smm=on,accel=kvm", "-cpu", "host", "-m", "2048", "-smp", "2",
            "-global", "driver=cfi.pflash01,property=secure,value=on",
            "-global", "ICH9-LPC.disable_s3=1",
            "-drive", f"if=pflash,unit=0,format=raw,readonly=on,file={CODE}",
            "-drive", f"if=pflash,unit=1,format=raw,file={sc.vars}",
            "-drive", f"if=none,id=boot,format={'qcow2' if disk.endswith('.qcow2') else 'raw'},file={disk}",
            "-device", "virtio-blk-pci,drive=boot,bootindex=0",
            "-chardev", f"socket,id=ser0,path={self.ser_sock},server=on,wait=off,logfile={self.serial}",
            "-serial", "chardev:ser0",
            "-display", "none", "-no-reboot", "-nic", "none",
        ]
        if data:
            argv += ["-drive", f"if=none,id=data,format=raw,file={sc.data}", "-device", "virtio-blk-pci,drive=data"]
        if tpm:
            self.swlog = open(self.base + ".swtpm.log", "wb")
            self.swtpm = subprocess.Popen(
                ["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={sc.tpmstate}",
                 "--ctrl", f"type=unixio,path={self.tpm_sock}", "--terminate",
                 "--log", f"file={self.base}.swtpm.log,level=2"],
                stdout=self.swlog, stderr=subprocess.STDOUT)
            for _ in range(100):
                if os.path.exists(self.tpm_sock):
                    break
                time.sleep(0.05)
            argv += ["-chardev", f"socket,id=chrtpm,path={self.tpm_sock}",
                     "-tpmdev", "emulator,id=tpm0,chardev=chrtpm", "-device", "tpm-crb,tpmdev=tpm0"]
        if action:
            argv += ["-fw_cfg", f"name=opt/rime/action,string={action}"]
        argv += ["-fw_cfg", f"name=opt/rime/pass,file={LAB}/keys/TEST-passphrase",
                 "-fw_cfg", f"name=opt/rime/marker,string={sc.marker}"]
        self.err = open(self.base + ".qemu.stderr", "wb")
        self.t0 = time.time()
        self.proc = subprocess.Popen(argv, stdout=self.err, stderr=subprocess.STDOUT)

    def text(self):
        return stripped(self.serial)

    def type_line(self, s):
        c = socket.socket(socket.AF_UNIX)
        c.connect(self.ser_sock)
        c.sendall(s.encode() + b"\r")
        time.sleep(0.5)
        c.close()

    def run(self):
        """Wait for the boot to end; answer every passphrase ask with the next planned secret."""
        asked = 0
        typed = []
        seen = None
        end = time.time() + self.timeout
        while time.time() < end and self.proc.poll() is None:
            t = self.text()
            n = len(re.findall(r"^RIME-ASK \d+", t, re.M))
            if n > asked:
                # type once the console agent's prompt is out (or 5 s after the ask, if its text came first)
                seen = seen or time.time()
                if "Please enter passphrase" in t.split(f"RIME-ASK {n}")[-1] or time.time() - seen > 5:
                    asked, seen = n, None
                    # an unplanned ask is answered with the passphrase so the sequence goes on; the verdict
                    # compares what was typed with the plan
                    what = self.secrets.pop(0) if self.secrets else "TEST-passphrase"
                    time.sleep(0.5)
                    self.type_line(secret(what))
                    typed.append(what)
            time.sleep(0.2)
        rc = self.proc.poll()
        if rc is None:
            self.proc.kill()
            self.proc.wait()
            rc = "timeout"
        if self.swtpm:
            try:
                self.swtpm.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.swtpm.kill()
                self.swtpm.wait()
            self.swlog.close()
        self.err.close()
        self.secs = int(time.time() - self.t0)
        t = self.text()
        with open(self.base + ".serial.txt", "w") as f:
            f.write(t)
        os.unlink(self.serial)
        self.typed = typed
        self.rc = rc
        return t


def field(t, key):
    m = re.findall(rf"^RIME-TPM: {re.escape(key)}[:=] ?(.*)$", t, re.M)
    return m[-1].strip() if m else None


def pcr7(t):
    v = field(t, "pcr7-sysfs")
    return v.lower() if v and re.fullmatch(r"[0-9A-Fa-f]{64}", v) else None


def luks_lines(t):
    return [l.split("luks| ", 1)[1] for l in t.splitlines() if l.startswith("RIME-TPM: luks| ")]


class Scenario:
    def __init__(self, name, uuid):
        self.name, self.uuid = name, uuid
        self.dir = f"{OUT}/{name}"
        os.makedirs(self.dir, exist_ok=True)
        w = f"/tmp/{name}"
        os.makedirs(w, exist_ok=True)
        self.vars, self.data, self.tpmstate = f"{w}/vars.fd", f"{w}/data.raw", f"{w}/tpmstate"
        self.nixos_disk = f"{w}/nixos.qcow2"
        subprocess.run(["cp", f"{LAB}/fw/vars-sb-mok.fd", self.vars], check=True)
        subprocess.run(["cp", "--sparse=always", f"{LAB}/disks/data.raw", self.data], check=True)
        os.makedirs(self.tpmstate, exist_ok=True)
        subprocess.run(["qemu-img", "create", "-q", "-f", "qcow2", "-F", "raw", "-b",
                        f"{LAB}/disks/bls/disk.raw", self.nixos_disk], check=True)
        self.marker = os.urandom(8).hex()
        self.rows = []
        self.pcr = {}

    def verdict(self, b, problems, what, detail=""):
        ok = not problems
        row = "\t".join(["PASS" if ok else "FAIL", self.name, b.name, what, f"{b.secs}s",
                         "; ".join(problems) or detail])
        print(row, flush=True)
        self.rows.append(row)
        with open(f"{self.dir}/results.tsv", "a") as f:
            f.write(row + "\n")

    def eventlog(self, b, t):
        # busybox base64 ignores -w 0 and wraps its output
        m = re.search(r"RIME-TPM: eventlog-begin\n(.+?)\nRIME-TPM: eventlog-end", t, re.S)
        if not m:
            return None
        # kernel console messages can land inside it; the replay check in common() proves the log decoded intact
        b64 = re.sub(r"\[\s*\d+\.\d+\][^\n]*\n?", "", m.group(1))
        raw = base64.b64decode(re.sub(r"\s", "", b64))
        with open(b.base + ".eventlog.bin", "wb") as f:
            f.write(raw)
        y = subprocess.run(["tpm2_eventlog", b.base + ".eventlog.bin"], capture_output=True, text=True)
        with open(b.base + ".eventlog.yaml", "w") as f:
            f.write(y.stdout + y.stderr)
        with open(b.base + ".pcr7-events.txt", "w") as f:
            f.write(pcr7_events(y.stdout))
        return replayed_pcr7(y.stdout) or ""

    def common(self, b, t, *, want_nixos, want_tpm=True):
        p = []
        if b.rc == "timeout":
            p.append("timed out")
        if want_nixos:
            if "RIME-TPM: stage-2 reached" not in t:
                p.append("no stage-2 marker")
            if "sb-state: SecureBoot enabled" not in t:
                p.append("SecureBoot not enabled")
            if "Loaded X.509 cert 'Rime TPM lab TEST MOK" not in t:
                p.append("TEST MOK not loaded by the NixOS kernel")
            if not re.search(r"RIME-TPM: cmdline: BOOT_IMAGE=\S*/rime/nixos/[0-9a-f]{32}-linux-\S+-signed\.efi ", t):
                p.append("kernel not the boot hook's signed copy")
        else:
            if "RIME-TPM: probe reached" not in t:
                p.append("no probe marker")
            if not re.search(r"secureboot-var=\d+ \d+ \d+ \d+ 1$", t, re.M):
                p.append("SecureBoot variable not 1")
        if "RIME-TPM: DONE, powering off" not in t:
            p.append("did not finish")
        if want_tpm:
            v = pcr7(t)
            if not v:
                p.append("no PCR 7 read")
            elif v == ZERO:
                p.append("PCR 7 is all zero")
            else:
                self.pcr[b.name] = v
                with open(f"{self.dir}/pcr7.txt", "a") as f:
                    f.write(f"{b.name}\t{v}\n")
            rp = self.eventlog(b, t)
            if rp is None:
                p.append("no event log")
            elif v and rp != v:
                p.append(f"the event log replays PCR 7 to {rp or '?'}, the TPM reads {v}")
            if want_nixos:
                pr = field(t, "pcrread") or ""
                if v and v.upper() not in pr.upper():
                    p.append(f"tpm2_pcrread does not match sysfs: {pr}")
        return p

    def unlock(self, b, t, *, asks, typed):
        p = []
        got = field(t, "asks-in-initrd")
        if got != str(asks):
            p.append(f"initrd asked {got} times, expected {asks}")
        if b.typed != typed:
            p.append(f"typed {b.typed}, planned {typed}")
        if field(t, "cryptdata") != "open":
            p.append("cryptdata not open")
        if field(t, "marker") != f"RIME-TPM-LAB-MARKER {self.marker}":
            p.append(f"marker read back as {field(t, 'marker')!r}")
        return p

    def nixos(self, name, action=None, secrets=(), tpm=True):
        b = Boot(self, name, self.nixos_disk, tpm=tpm, data=True, action=action, secrets=secrets)
        return b, b.run()

    def probe(self, name, kind):
        b = Boot(self, name, f"{LAB}/disks/probe-{kind}.raw", timeout=180)
        return b, b.run()

    def main(self):
        b, t = self.probe("P1", "fedora")
        self.verdict(b, self.common(b, t, want_nixos=False), "warm-up boot, Fedora chain",
                     f"pcr7 {self.pcr.get('P1')}")
        b, t = self.probe("P2", "fedora")
        self.verdict(b, self.common(b, t, want_nixos=False),
                     "T1 BEFORE: shim -> GRUB -> Fedora-signed kernel, PCR 7 recorded", f"pcr7 {self.pcr.get('P2')}")
        b, t = self.probe("P3", "mok")
        self.verdict(b, self.common(b, t, want_nixos=False),
                     "T1 BEFORE: shim -> GRUB -> TEST-MOK-signed kernel (bootc-Rime chain), PCR 7 recorded",
                     f"pcr7 {self.pcr.get('P3')}")
        b, t = self.probe("P4", "mok")
        p = self.common(b, t, want_nixos=False)
        if self.pcr.get("P4") != self.pcr.get("P3"):
            p.append(f"PCR 7 not stable: P3 {self.pcr.get('P3')} P4 {self.pcr.get('P4')}")
        self.verdict(b, p, "bootc-Rime chain again: PCR 7 stable", f"pcr7 {self.pcr.get('P4')}")
        bootc = self.pcr.get("P3") or ZERO
        fedora = self.pcr.get("P2") or ZERO

        b, t = self.nixos("N1", action=f"init:{bootc}", secrets=["TEST-passphrase"])
        p = self.common(b, t, want_nixos=True)
        p += self.unlock(b, t, asks=1, typed=["TEST-passphrase"])
        if field(t, "init rc") != "0":
            p.append(f"sealing the bootc-era slot failed (rc {field(t, 'init rc')})")
        same = self.pcr.get("N1") == bootc
        self.verdict(b, p, "T2 AFTER: NixOS on the same shim/GRUB/MOK chain, PCR 7 recorded; bootc-era slot sealed",
                     f"pcr7 {self.pcr.get('N1')} ({'==' if same else '!='} bootc-Rime P3)")

        n2_typed = [] if same else ["TEST-passphrase"]
        b, t = self.nixos("N2", action=f"rebind:{fedora}", secrets=n2_typed)
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=len(n2_typed), typed=n2_typed)
        if same and "RIME-TPM: cryptsetup| " not in t:
            p.append("no systemd-cryptsetup journal")
        if field(t, "rebind rc") != "0":
            p.append(f"re-sealing to the Fedora chain's PCR 7 failed (rc {field(t, 'rebind rc')})")
        self.verdict(b, p, "the bootc-era TPM slot unseals on NixOS with no prompt" if same else
                     "PCR 7 differs: the bootc-era slot is refused (passphrase typed)",
                     f"asks {field(t, 'asks-in-initrd')}")

        b, t = self.nixos("N3", action="reenrol", secrets=["TEST-passphrase"])
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=1, typed=["TEST-passphrase"])
        before = luks_lines(t)
        if not re.search(r"^RIME-TPM: reenrol answered=1 result=success$", t, re.M):
            p.append(f"re-enrolment unit: {field(t, 'reenrol answered')!r}")
        if not re.search(r"RIME-TPM: reenrol\| recovery: kept slots=", t):
            p.append("rime-luks-enroll did not report the kept slots")
        if not re.search(r"RIME-TPM: reenrol\| kept: unchanged [0-9a-f]{64}", t):
            p.append("rime-luks-enroll did not prove the kept slots unchanged")
        if not re.search(r"RIME-TPM: reenrol\| tpm2: enrolled binding=pcr7", t):
            p.append("no PCR 7 TPM slot enrolled")
        kept = [l for l in before if " recovery " in l or " passphrase " in l]
        n = len(kept)
        uniq = sorted(set(kept))
        if len(uniq) * 2 != n:   # luks| lines are printed before and after: each kept slot twice, identical
            p.append(f"recovery/passphrase slots changed: {kept}")
        self.verdict(b, p, "stale slot refused; rime-luks-tpm-reenrol@ (--tpm-only) re-binds PCR 7, kept slots unchanged",
                     " | ".join(uniq))

        b, t = self.nixos("N4", action="clear")
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=0, typed=[])
        if field(t, "clear rc") != "0":
            p.append(f"tpm2_clear rc {field(t, 'clear rc')}")
        self.verdict(b, p, "the re-enrolled slot unseals with no prompt; then tpm2_clear", "")

        b, t = self.nixos("N5", secrets=["TEST-recovery-key"])
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=1, typed=["TEST-recovery-key"])
        self.verdict(b, p, "T3: TPM cleared, the PRE-EXISTING recovery key unlocks", "")

        b, t = self.nixos("N6", secrets=["TEST-recovery-key"], tpm=False)
        p = self.common(b, t, want_nixos=True, want_tpm=False) + self.unlock(b, t, asks=1, typed=["TEST-recovery-key"])
        if field(t, "tpm") != "absent":
            p.append("a TPM was present")
        self.verdict(b, p, "T3: no TPM device, the PRE-EXISTING recovery key unlocks", "")

    def mutant_bootc_reenrol(self):
        b, t = self.nixos("M1", action=f"init:{'ab' * 32}", secrets=["TEST-passphrase"])
        p = self.common(b, t, want_nixos=True)
        self.verdict(b, p, "mutant: slot sealed to an unmeasured PCR 7 value", "")
        b, t = self.nixos("M2", action="reenrol-bootc", secrets=["TEST-passphrase"])
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=1, typed=["TEST-passphrase"])
        if field(t, "reenrol-bootc rc") != "0":
            p.append(f"bootc-style re-enrolment rc {field(t, 'reenrol-bootc rc')}")
        self.verdict(b, p, "mutant: bootc's full re-enrolment (recovery key replaced)", "")
        b, t = self.nixos("M3", action="clear")
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=0, typed=[])
        self.verdict(b, p, "mutant: TPM unlock; tpm2_clear", "")
        b, t = self.nixos("M4", secrets=["TEST-recovery-key"])
        p = self.common(b, t, want_nixos=True) + self.unlock(b, t, asks=1, typed=["TEST-recovery-key"])
        self.verdict(b, p, "T3 under the mutant: the pre-existing recovery key must unlock", "")


def replayed_pcr7(yaml_text):
    """PCR 7 (sha256) as tpm2_eventlog computes it by replaying the log: its closing `pcrs:` block."""
    m = re.search(r"^pcrs:\s*\n(.*)", yaml_text, re.M | re.S)
    if not m:
        return None
    sha = re.search(r"^\s*sha256:\s*\n((?:[ \t]+\d+\s*:\s*0x[0-9A-Fa-f]+[ \t]*\n?)+)", m.group(1), re.M)
    if not sha:
        return None
    d = re.search(r"^\s+7\s*:\s*0x([0-9A-Fa-f]{64})", sha.group(1), re.M)
    return d.group(1).lower() if d else None


def pcr7_events(yaml_text):
    """The PCR 7 events of a tpm2_eventlog YAML dump: type, sha256 digest, variable name or a hint of the data."""
    out = []
    for blk in re.split(r"\n- EventNum: ", yaml_text):
        if not re.search(r"^\s*PCRIndex: 7\s*$", blk, re.M):
            continue
        typ = re.search(r"EventType: (\S+)", blk)
        dig = re.search(r"AlgorithmId: sha256\s*\n\s*Digest: \"?([0-9a-fA-F]+)", blk)
        var = re.search(r"UnicodeName: (.+)", blk)
        num = blk.split("\n", 1)[0].strip()
        out.append(f"{num:>4} {typ.group(1) if typ else '?':<32} {dig.group(1)[:16] if dig else '?':<16} "
                   f"{var.group(1).strip() if var else ''}")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("main", "mutant-bootc-reenrol"):
        sys.exit("usage: drive.py main|mutant-bootc-reenrol LUKS_UUID")
    sc = Scenario(sys.argv[1], sys.argv[2])
    getattr(sc, sys.argv[1].replace("-", "_"))()
    sys.exit(0 if all(r.startswith("PASS") for r in sc.rows) else 1)
