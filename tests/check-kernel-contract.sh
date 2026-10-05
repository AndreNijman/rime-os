#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-kernel-contract.sh — does the kernel we built satisfy what
#  Containerfile.core assumes about it? Answered WITHOUT a 50-minute core build.
#
#  Rime builds its own kernel now (Containerfile.kernel). Every assumption core
#  makes about a kernel package was true of the COPR's by luck rather than by
#  agreement, and each one fails late and expensively: core's `rpm -q` gate is
#  ~45 minutes into a build, and its `ls -d /usr/lib/modules/*cachyos*` kver
#  glob fails even later, as an unset variable in a stage that looks unrelated.
#
#  So this installs the produced RPMs into a scratch fedora-bootc:45 and checks
#  each contract directly. Ten minutes instead of fifty, and it names which
#  contract broke rather than leaving a stage to fail obliquely.
#
#  It needs RPMs, which need a kernel build, so it is NOT a CI gate — it is the
#  thing to run after `./build-local.sh kernel` and before trusting a core
#  build. Point RPMS at the directory holding them:
#
#      RPMS=/path/to/rpms ./tests/check-kernel-contract.sh
#
#  or extract them from the kernel image first (`FROM scratch` has no shell, so
#  `podman create` + `podman cp`, not `podman run`):
#
#      cid=$(podman create localhost/rime-kernel:local /x)
#      podman cp "$cid:/rpms" /var/lab-scratch/kernel-rpms && podman rm "$cid"
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
RPMS="${RPMS:-/var/lab-scratch/kernel-build/out/rpms}"

[ -d "$RPMS" ] || { echo "FATAL: $RPMS does not exist — set RPMS=<dir> (see the header)"; exit 1; }
ls "$RPMS"/*.rpm >/dev/null 2>&1 || { echo "FATAL: no rpms in $RPMS"; exit 1; }

# `-i` IS LOAD-BEARING AND WAS MISSING. Without it podman gives the container
# an empty stdin, so `bash -s` reads EOF, runs NOTHING, and exits 0 — the whole
# heredoc below is silently discarded and this script reports success having
# checked nothing at all. Measured on this machine:
#
#   podman run --rm    … bash -s <<'X' echo RAN; exit 7 X   ->  no output, rc=0
#   podman run --rm -i … bash -s <<'X' echo RAN; exit 7 X   ->  "RAN",    rc=7
#
# The sentinel below is the guard that stops this regressing quietly: if the
# body did not run, its first line is missing from the log and that is fatal.
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

podman run --rm -i -v "$RPMS":/krpms:z quay.io/fedora/fedora-bootc:45 /bin/bash -s 2>&1 <<'EOS' | tee "$LOG"
echo "CONTRACT-TEST-BODY-RAN"
set -uo pipefail
fail=0
chk() { if eval "$2" >/dev/null 2>&1; then echo "ok:   $1"; else echo "FAIL: $1"; fail=1; fi; }

echo "=== what core does: remove Fedora's kernel, install ours ==="
dnf5 -y remove --no-autoremove kernel kernel-core kernel-modules kernel-modules-core >/dev/null 2>&1 \
  || echo "(nothing to remove -- fedora-bootc may not carry them)"

dnf5 -y --nogpgcheck install /krpms/kernel-cachyos-*.rpm \
  || echo "WARN: install exited non-zero (expected: cosmetic %posttrans dracut)"

echo
echo "=== contract 1: the four packages core hard-gates on ==="
for p in kernel-cachyos kernel-cachyos-core kernel-cachyos-modules kernel-cachyos-devel-matched; do
    chk "rpm -q $p" "rpm -q $p"
done
rpm -q kernel-cachyos-core --qf 'kver from rpm: %{VERSION}-%{RELEASE}.%{ARCH}\n'

echo
echo "=== contract 2: core's kver glob finds exactly one directory ==="
n=$(ls -d /usr/lib/modules/*cachyos* 2>/dev/null | wc -l)
echo "matches for /usr/lib/modules/*cachyos*: $n"
[ "$n" -eq 1 ] || { echo "FAIL: core does KVER=\$(basename \$(ls -d /usr/lib/modules/*cachyos*)) and needs exactly one"; fail=1; }
KVER=$(basename "$(ls -d /usr/lib/modules/*cachyos* 2>/dev/null | head -1)")
echo "KVER=$KVER"

echo
echo "=== contract 3: the files bootc and the later stages need ==="
chk "vmlinuz present"            "test -s /usr/lib/modules/$KVER/vmlinuz"
chk "config present"             "test -s /usr/lib/modules/$KVER/config"
chk "System.map present"         "test -s /usr/lib/modules/$KVER/System.map"
chk "build/ symlink (akmods)"    "test -e /usr/lib/modules/$KVER/build"
chk "sign-file (module signing)" "test -x /usr/lib/modules/$KVER/build/scripts/sign-file"
chk "CONFIG_MODULE_SIG_HASH readable" \
    "test -n \"\$(sed -n 's/^CONFIG_MODULE_SIG_HASH=\"\\(.*\\)\"\$/\\1/p' /usr/lib/modules/$KVER/config)\""

echo
echo "=== contract 4: depmod works (core runs it) ==="
chk "depmod -a $KVER" "depmod -a $KVER"

echo
echo "=== contract 5: the kernel is a PE with an EFI stub (the UKI needs this) ==="
# NOT `head -c2 … | grep -q MZ`. This script sets `pipefail`, and `grep -q`
# exits the instant it matches — which can SIGPIPE the writer and make the
# whole pipeline return 141 ON A MATCH, reporting a perfectly good signable
# kernel as "not a PE image". Honest about the evidence: 300 iterations of the
# two-byte form here did NOT reproduce it, because `head` finishes writing two
# bytes before `grep` can exit. The pattern has mis-seeded suites in this
# repository before and reading the bytes directly costs nothing, so the race
# is removed rather than relied upon to stay lost.
if [ "$(head -c2 "/usr/lib/modules/$KVER/vmlinuz")" = "MZ" ]; then
    echo "ok:   vmlinuz starts with MZ -- a PE image, signable and UKI-stub-able"
else
    echo "FAIL: vmlinuz is not a PE image"; fail=1
fi
grep -q '^CONFIG_EFI_STUB=y' "/usr/lib/modules/$KVER/config" \
    && echo "ok:   CONFIG_EFI_STUB=y in the shipped config" \
    || { echo "FAIL: CONFIG_EFI_STUB is not y"; fail=1; }

echo
echo "=== contract 6: sched-ext and BTF really are in this kernel ==="
for o in CONFIG_SCHED_CLASS_EXT CONFIG_DEBUG_INFO_BTF CONFIG_DEBUG_INFO_BTF_MODULES CONFIG_SCHED_BORE; do
    grep -q "^${o}=y" "/usr/lib/modules/$KVER/config" \
        && echo "ok:   ${o}=y" \
        || { echo "FAIL: ${o} is not y"; fail=1; }
done
grep -E '^CONFIG_(PAHOLE_VERSION|HZ)=' "/usr/lib/modules/$KVER/config"
grep -q '^CONFIG_MODULE_ALLOW_BTF_MISMATCH=y' "/usr/lib/modules/$KVER/config" \
    && { echo "FAIL: CONFIG_MODULE_ALLOW_BTF_MISMATCH=y -- tolerating the defect we exist to fix"; fail=1; } \
    || echo "ok:   CONFIG_MODULE_ALLOW_BTF_MISMATCH is not set"

echo
echo "contract test fail=$fail"
exit "$fail"
EOS
# PIPESTATUS[0], not $?, because the container's output goes through `tee`.
rc="${PIPESTATUS[0]}"

# Did the body run at all? See the `-i` note above: the way this script failed
# was by checking nothing and saying so in no way whatsoever.
if ! grep -q 'CONTRACT-TEST-BODY-RAN' "$LOG"; then
    echo "FATAL: the container body never executed — not one contract was checked."
    echo "       This is what a missing \`-i\` on \`podman run\` looks like: bash -s"
    echo "       reads an empty stdin and exits 0. Do NOT read this as a pass."
    exit 1
fi

echo "contract-test exit=${rc}"
# `exit` explicitly. Without this the script ends on the `echo` and exits with
# ECHO's status, which is 0 — so every contract could FAIL, say so on stdout,
# and the script would still report success to anything that checks it. A
# checker whose exit code cannot express failure is one nobody can wire into a
# gate, which is most of the reason to have written it.
exit "${rc}"
