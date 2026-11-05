#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-luks-enroll.sh — the retry rime-luks-enroll prints when the TPM
#  step declines (files/system/libexec/rime-luks-enroll).
#
#  The full path enrols a recovery key FIRST and only then decides whether a TPM
#  slot is worth having. When it declines (Secure Boot off, no TPM device, setup
#  mode) it tells the user what to run once the cause is fixed. The full path
#  also refuses a volume that already has a recovery key, so a retry that repeats
#  the full path always dies on the volume this same run just enrolled. These
#  cases run the full path to each decline, take the `sudo ...` line it printed,
#  fix the cause (a probe root with Secure Boot on and a TPM node) and execute
#  that line: it must enrol the TPM slot and keep the recovery key.
#
#  The shipped script runs unmodified except its two program constants, which
#  are absolute by design (a root process must not run a caller-named program):
#  the copy points them at stubs, and the test asserts nothing else differs.
#  The "device" is a JSON file the stubs treat as the LUKS2 header metadata.
#
#  Nothing is enrolled: no real volume, no TPM, no root.
#  Run from anywhere: ./tests/test-rime-luks-enroll.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2

SRC=../files/system/libexec/rime-luks-enroll
[ -f "$SRC" ] || { echo "cannot find $SRC"; exit 2; }
command -v python3 >/dev/null || { echo "python3 is required"; exit 2; }

T=$(mktemp -d /tmp/rime-luks-enroll-test.XXXXXX)
trap 'rm -rf "$T"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

# ── stubs ────────────────────────────────────────────────────────────────────
cat > "$T/cryptsetup" <<'STUB'
#!/usr/bin/env bash
case "$1" in
    isLuks) exit 0 ;;
    luksDump) [ "$2" = --dump-json-metadata ] || exit 9; cat "$3" ;;
    *) echo "cryptsetup stub: unexpected $*" >&2; exit 9 ;;
esac
STUB

cat > "$T/systemd-cryptenroll" <<'STUB'
#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
with open(sys.argv[0] + ".calls", "a") as log:
    log.write(" ".join(args) + "\n")
dev = args[-1]
meta = json.load(open(dev))
slots, tokens = meta["keyslots"], meta["tokens"]

def new_slot():
    n = 0
    while str(n) in slots:
        n += 1
    slots[str(n)] = {"type": "luks2", "kdf": {"salt": f"salt-{n}-{len(tokens)}"}}
    return n

def new_token(t):
    n = 0
    while str(n) in tokens:
        n += 1
    tokens[str(n)] = t

def wipe(kind):
    for tid, t in list(tokens.items()):
        if t["type"] == "systemd-" + kind:
            for k in t["keyslots"]:
                slots.pop(str(k), None)
            del tokens[tid]

out = ""
if "--recovery-key" in args:
    s = new_slot()
    new_token({"type": "systemd-recovery", "keyslots": [str(s)]})
    out = "-".join(["cbdefghi"] * 8) + "\n"
elif any(a.startswith("--wipe-slot=") for a in args):
    for a in args:
        if a.startswith("--wipe-slot="):
            wipe(a.split("=", 1)[1])
elif any(a.startswith("--tpm2-device=") for a in args):
    pcrs = [int(p) for a in args if a.startswith("--tpm2-pcrs=") for p in a.split("=", 1)[1].split("+")]
    s = new_slot()
    new_token({"type": "systemd-tpm2", "keyslots": [str(s)], "tpm2-pcrs": pcrs,
               "tpm2-pcr-bank": "sha256", "tpm2-policy-hash": "ab" * 32,
               "tpm2-pin": "--tpm2-with-pin=yes" in args})
else:
    sys.exit("systemd-cryptenroll stub: unexpected " + " ".join(args))
json.dump(meta, open(dev, "w"))
sys.stdout.write(out)
STUB
chmod +x "$T/cryptsetup" "$T/systemd-cryptenroll"

# ── the shipped script, with only its two program constants pointed at the stubs
ENROLL="$T/rime-luks-enroll"
sed -e "s|^readonly CRYPTSETUP=/usr/sbin/cryptsetup\$|readonly CRYPTSETUP=$T/cryptsetup|" \
    -e "s|^readonly CRYPTENROLL=/usr/bin/systemd-cryptenroll\$|readonly CRYPTENROLL=$T/systemd-cryptenroll|" \
    "$SRC" > "$ENROLL"
chmod +x "$ENROLL"
changed=$(diff "$SRC" "$ENROLL" | grep -c '^>')
if [ "$changed" = 2 ] && [ "$(wc -l < "$SRC")" = "$(wc -l < "$ENROLL")" ]; then
    ok "the copy differs from the shipped script in its two program constants only"
else
    bad "copy" "expected exactly 2 changed lines, got $changed: the constants moved, so this suite would test nothing"
    echo "passed: $pass  failed: $fail"; exit 1
fi

# ── probe roots: what the firmware and the TPM look like ─────────────────────
SB_GUID=8be4df61-93ca-11d2-aa0d-00e098032b8c
probe() {  # probe DIR SECUREBOOT SETUPMODE TPM(yes|no)
    local d="$1"
    mkdir -p "$d/sys/firmware/efi/efivars" "$d/dev"
    printf "\\x07\\x00\\x00\\x00\\x0$2" > "$d/sys/firmware/efi/efivars/SecureBoot-$SB_GUID"
    printf "\\x07\\x00\\x00\\x00\\x0$3" > "$d/sys/firmware/efi/efivars/SetupMode-$SB_GUID"
    # The script tests for a character device; /dev/null is one and a symlink is
    # followed, so no mknod (and no root) is needed.
    [ "$4" = no ] || ln -s /dev/null "$d/dev/tpmrm0"
}
probe "$T/sb-off"   0 0 yes
probe "$T/no-tpm"   1 0 no
probe "$T/setup"    1 1 yes
probe "$T/fixed"    1 0 yes

fresh_volume() {  # a LUKS2 header with one passphrase slot and no tokens
    printf '%s' '{"keyslots": {"0": {"type": "luks2", "kdf": {"salt": "passphrase"}}}, "tokens": {}}' > "$1"
}
types() { python3 -c 'import json,sys; print(" ".join(sorted(t["type"] for t in json.load(open(sys.argv[1]))["tokens"].values())))' "$1"; }
recovery_of() { python3 -c '
import json, sys
m = json.load(open(sys.argv[1]))
r = [t for t in m["tokens"].values() if t["type"] == "systemd-recovery"]
print(json.dumps([(t["keyslots"], [m["keyslots"][k] for k in t["keyslots"]]) for t in r], sort_keys=True))' "$1"; }

# decline_then_retry NAME PROBE REASON: the full path to the decline, then the
# printed retry against a probe root where the cause is fixed.
decline_then_retry() {
    local name="$1" probe_root="$2" reason="$3" dev="$T/$1.luks" key="$T/$1.key"
    fresh_volume "$dev"
    "$ENROLL" --device "$dev" --recovery-out "$key" --probe-root "$probe_root" \
        > "$T/$name.out" 2> "$T/$name.err"
    local rc=$?
    if [ "$rc" = 0 ] && grep -qx "tpm2: declined reason=$reason" "$T/$name.out" \
        && grep -q '^recovery: enrolled' "$T/$name.out"; then
        ok "$name: the full path enrols the recovery key, declines ($reason) and exits 0"
    else
        bad "$name" "rc=$rc, expected 0 with reason=$reason: $(cat "$T/$name.out" "$T/$name.err")"
        return
    fi
    local retry; retry="$(sed -n 's/^ *sudo //p' "$T/$name.err")"
    if [ "$(printf '%s\n' "$retry" | grep -c .)" != 1 ]; then
        bad "$name" "expected exactly one printed 'sudo ...' retry, got: [$retry]"
        return
    fi
    case "$retry" in
        "$ENROLL --tpm-only --device $dev")
            ok "$name: the printed retry re-binds the TPM slot only ($retry)" ;;
        *)  bad "$name" "the printed retry is not the TPM-only resumption: $retry" ;;
    esac
    local rec_before key_before; rec_before="$(recovery_of "$dev")"; key_before="$(cat "$key")"
    # The cause fixed (Secure Boot on, a TPM present): the probe root is the only
    # thing added to the line the user was told to run.
    eval "$retry --probe-root \"\$T/fixed\"" > "$T/$name.retry.out" 2> "$T/$name.retry.err"
    rc=$?
    if [ "$rc" = 0 ] && grep -q '^tpm2: enrolled binding=pcr7 ' "$T/$name.retry.out"; then
        ok "$name: the printed retry, run once the cause is fixed, enrols the TPM slot"
    else
        bad "$name" "the printed retry failed (rc=$rc): $(cat "$T/$name.retry.out" "$T/$name.retry.err")"
        return
    fi
    if [ "$(recovery_of "$dev")" = "$rec_before" ] && [ "$(cat "$key")" = "$key_before" ] \
        && grep -q '^kept: unchanged' "$T/$name.retry.out" \
        && [ "$(types "$dev")" = "systemd-recovery systemd-tpm2" ]; then
        ok "$name: the recovery key the first run filed still opens the volume"
    else
        bad "$name" "the recovery key changed or a token is missing: $(types "$dev")"
    fi
}

decline_then_retry secure-boot-off "$T/sb-off" secure-boot-off
decline_then_retry no-tpm          "$T/no-tpm" no-tpm2-device
decline_then_retry setup-mode      "$T/setup"  setup-mode

# The premise: repeating the full path on that volume is refused, which is why
# the retry must not be the full path.
dev="$T/secure-boot-off.luks"
"$ENROLL" --device "$dev" --recovery-out "$T/again.key" --probe-root "$T/fixed" > /dev/null 2> "$T/again.err"
rc=$?
if [ "$rc" = 1 ] && grep -q 'already has a recovery key' "$T/again.err" && [ ! -e "$T/again.key" ]; then
    ok "the full path refuses a volume that already has a recovery key"
else
    bad "premise" "rc=$rc: $(cat "$T/again.err")"
fi

# A choice the user made on the first run is not dropped by the retry.
dev="$T/pin.luks"; fresh_volume "$dev"
"$ENROLL" --device "$dev" --recovery-out "$T/pin.key" --probe-root "$T/no-tpm" --with-pin --policy pcr7 \
    > /dev/null 2> "$T/pin.err"
retry="$(sed -n 's/^ *sudo //p' "$T/pin.err")"
if [ "$retry" = "$ENROLL --tpm-only --device $dev --with-pin --policy pcr7" ]; then
    ok "the retry carries --with-pin and --policy"
else
    bad "pin" "printed retry: [$retry]"
fi

echo "passed: $pass  failed: $fail"
[ "$fail" = 0 ]
