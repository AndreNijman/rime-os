#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-storage.sh — executable assertions for roadmap §48's Storage
#  Manager: the read-and-report half.
#
#  ── The four failures this exists to catch ──────────────────────────────────
#
#  1. A DYING DISK REPORTED AS A DISK NOBODY COULD MEASURE. smartctl's exit
#     status is a bitmask. Bits 0-2 mean the read did not happen; bits 3-7 mean
#     it did and the disk is failing. `if status != 0 { unavailable }` turns the
#     one row this whole feature exists for from red into grey.
#
#  2. A PERMANENT FALSE ALARM ABOUT A MACHINE WITH A TERABYTE FREE. On a bootc
#     machine / is a composefs image: 37.8M, 0 available, 100% used, on every
#     Rime machine, forever. Every disk-usage widget ever written points at /.
#
#  3. AN ENCRYPTED DISK CALLED UNENCRYPTED. `blkid` unprivileged exits 0 and
#     prints nothing, so a caller that trusts it reports "not encrypted" for a
#     device it never opened — the one sentence here a user would act on by
#     putting data at risk.
#
#  4. AN ERASE THE GUARD WAVED THROUGH. `rime storage erase` is the only thing
#     in this feature that destroys data, and "is the device mounted?" — the
#     check everybody writes — green-lights destroying THIS machine five
#     independent ways: / is composefs and not a block device at all; the whole
#     disk carrying the OS is a mount source zero times; the ESP is mounted
#     nowhere; an attached loop device is in no mount table anywhere; and one
#     sysfs directory with the wrong mode makes a disk's partitions vanish so
#     that nothing relates the disk to the OS on it. Every one of those was
#     measured on the development machine, and the last two were found by this
#     suite's own fixtures granting a permit.
#
#  Three modes:
#
#    (no argument)     Structural checks with no toolchain.
#    --with-binary     Drives `rime storage` and the notifier against a whole
#                      fixture machine: sysfs, mountinfo, udev records and
#                      captured smartctl output. It DIES if the binary is
#                      absent; a skipped assertion reports as a pass.
#    --destructive     Really erases a device. Opt-in, NOT in pr-validation.yml,
#                      needs `sudo -n`. See the banner on destructive_half.
#
#  NEITHER OF THE FIRST TWO TOUCHES A REAL DISK. Under $RIME_STORAGE_ROOT no
#  subprocess is spawned at all — smartctl's answer, the fstrim timer's state
#  and every filesystem's size come from files this script writes, and the
#  erase path returns before `wipefs` on a fixture root unconditionally.
# ─────────────────────────────────────────────────────────────────────────────
# shellcheck disable=SC2024  # `sudo cmd > "$out"` six times below. SC2024 is
# about a redirect that needs root to CREATE the file — the shell opens it, not
# sudo. Every target here is "$TMPD/out", and $TMPD is this suite's own
# `mktemp -d`, owned by whoever is running the tests. The non-root shell opening
# it is the intent: `sudo tee` would put the log under root ownership and the
# cleanup could then not remove it.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_BINARY=0
DESTRUCTIVE=0
case "${1:-}" in
    "")            ;;
    --with-binary) WITH_BINARY=1 ;;
    # A THIRD branch and never a flag folded into --with-binary: the two want
    # opposite environments. The binary half exports RIME_STORAGE_ROOT, and the
    # destructive half has one step that does NOT go through `sudo -n`'s
    # env_reset — the unprivileged one — so a leaked fixture root would make it
    # judge the fixture machine and report CouldNotVerify where the whole point
    # is NeedsRoot against the real one.
    --destructive) DESTRUCTIVE=1 ;;
    *) echo "usage: ${0##*/} [--with-binary|--destructive]" >&2; exit 2 ;;
esac

PASS=0 FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }
has() {
    if grep -qF -- "$1" "$2"; then ok "$3"; else
        bad "$3 — no '$1' in:"; sed 's/^/       /' "$2" >&2
    fi
}

# ── why no check below is `producer | grep -q` ───────────────────────────────
#
#  Under `set -o pipefail` that shape is a COIN FLIP, and in the direction
#  these checks are written it is a SILENT one. `grep -q` exits at its first
#  match and SIGPIPEs whatever feeds it, so the PIPELINE reports 141 rather
#  than grep's 0, and an `if` reads a match as "no match" — which for a check
#  whose match means `bad` turns a real regression into a pass.
#
#  Measured, and it is not a corner case. A mutation was spliced into
#  `rime/src/storage.rs` that made the CLI look up `ID_SERIAL_SHORT` — the
#  exact privacy regression the first check below exists to catch — 35 lines
#  into a 1,331-line file. The raw pipeline returned **141 in 40 of 40** bash
#  trials, and this suite **failed to report it in 24 of 25 runs**. The check
#  guarding the disk serial was 96% blind to the one change it is for.
#
#  A small producer hides it: `awk '/^fn x/,/^}/'` emits one function, finishes
#  writing before grep can exit, and caught the same class of mutation 20/20.
#  That is luck about a pipe buffer, not a property anyone maintains — a
#  function that grows, or a match near its start, flips it. So every check
#  captures the text first; command substitution reads its producer to EOF, so
#  there is nobody left to signal.
absent() {   # absent <text> <extended-regex> <message>
    if grep -qE -- "$2" <<<"$1"; then
        bad "$3 — matched /$2/ in:"; sed 's/^/       /' <<<"$1" >&2
    else ok "$3"; fi
}
present() {  # present <text> <extended-regex> <message>
    if grep -qE -- "$2" <<<"$1"; then ok "$3"
    else bad "$3 — no /$2/ in:"; sed 's/^/       /' <<<"$1" >&2; fi
}
hasnt() {
    if grep -qF -- "$1" "$2"; then
        bad "$3 — found '$1' in:"; sed 's/^/       /' "$2" >&2
    else ok "$3"; fi
}

CORE="$REPO/rimed/rimed-core/src/storage.rs"
CLI="$REPO/rimed/rime/src/storage.rs"
NOTICE="$REPO/files/system/libexec/rime-storage-notice"
TIMER="$REPO/files/system/units/rime-storage-notice.timer"
SVC="$REPO/files/system/units/rime-storage-notice.service"
for f in "$CORE" "$CLI" "$NOTICE" "$TIMER" "$SVC"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

sec "the smartctl bitmask is split, not compared against zero"
has 'const SMART_BITS' "$CORE" "every bit smartctl(8) defines is in one table"
has 'SMART says this disk is FAILING' "$CORE" "including bit 3"
# The mutation: somebody replacing the split with a comparison.
if grep -qE 'exit_code\s*!=\s*0|code\s*!=\s*0\s*\{' "$CORE"; then
    bad "a bare comparison against zero has appeared in the smart path"
else
    ok "no bare comparison against zero decides whether a read happened"
fi
has 'a_failing_disk_is_a_read_that_happened_and_not_an_unavailable_row' "$CORE" \
    "and a test names the case"

sec "a refused read carries the remedy, wherever the refusal came from"
has 'pub const REMEDY' "$CORE" "one wording, so the three surfaces cannot drift"
has 'fn remedy_for' "$CORE" "and smartctl's message gets it too, having no errno to give"
has 'a_refused_open_names_the_remedy_in_the_row_and_not_only_in_a_footer' "$CORE" \
    "with a test that names the case"

sec "free space is a property of a filesystem, not of a mount point"
has 'starts_with("/dev/")' "$CORE" "only block-device sources are considered"
has 'composefs' "$CORE" "the composefs root is named as the reason"
has 'the_composefs_root_of_a_bootc_machine_is_not_a_filesystem_to_warn_about' "$CORE" \
    "and a test holds it"
has 'SPACE_ATTENTION_BYTES' "$CORE" "a byte floor as well as a percentage"

sec "encryption is never inferred from a read that did not happen"
has 'Unknown(String)' "$CORE" "the unknown arm exists"
has 'run/udev/data' "$CLI" "the filesystem type comes from udev's own database"
hasnt 'Command::new("blkid")' "$CLI" "blkid is never spawned"
hasnt '"/usr/sbin/blkid"' "$CLI" "nor by absolute path"

sec "no report can print a disk serial"
# The udev record this module reads carries ID_SERIAL_SHORT, so the serial is
# right there and is never looked up. The binary half proves the output is
# clean; this is the structural half, and it looks for a LOOKUP rather than for
# the string, because the fixtures in the test module contain the string on
# purpose.
CLI_CODE="$(sed -E 's://.*$::' "$CLI")"
[[ -n "$CLI_CODE" ]] || bad "$CLI could not be read at all"
absent "$CLI_CODE" 'get\("ID_SERIAL|ID_SERIAL[A-Z_]*"\]' "the CLI never looks up ID_SERIAL"
absent "$CLI_CODE" 'Command::new.*nvme|"/usr/sbin/nvme"' "nvme-cli is never spawned"

sec "the notifier will not wake somebody over a read it could not perform"
has 'rc=$?' "$NOTICE" "the exit status is captured before anything else runs"
has 'grep ' "$NOTICE" "only attention rows reach the digest"
has 'rm -f "$SEEN"' "$NOTICE" "and a cleared warning forgets its digest"
has 'systemd/user' "$REPO/Containerfile.base" "the unit ships as a user unit"
hasnt '[Install]' "$SVC" "the service itself is not enabled; the timer is"
has 'Persistent=true' "$TIMER" "a machine that was off checks when it returns"

# ── the erase path, read as source ──────────────────────────────────────────
#
# Structural because these are claims about which code CANNOT run, and a test
# that drives the binary can only show that it did not run this time.

sec "a fixture can never erase anything"
# The gate is what makes the other half of this file safe to run at all. It has
# to be unconditional and above the subprocess: under a fixture root every path
# machine() reads came from files the suite wrote, so a fixture answering
# euid 0 holds a real Permit for a device that does not exist.
has 'if roots.is_fixture() {' "$CLI" "the fixture gate exists"
ERASE="$(awk '/^fn erase\(/,/^}/' "$CLI")"
[[ -n "$ERASE" ]] || bad "fn erase() was not found in $CLI"
present "$ERASE" 'is_fixture' "and it is inside erase() itself"
# Order matters more than presence: a gate below the spawn is not a gate.
gate="$(awk '/^fn erase\(/,/^}/' "$CLI" | grep -n 'is_fixture' | head -1 | cut -d: -f1)"
spawn="$(awk '/^fn erase\(/,/^}/' "$CLI" | grep -n 'Command::new(WIPEFS)' | head -1 | cut -d: -f1)"
if [[ -n "$gate" && -n "$spawn" && "$gate" -lt "$spawn" ]]; then
    ok "the gate is above the spawn, not inside a branch of it"
else
    bad "the fixture gate is at line $gate and wipefs is spawned at $spawn"
fi

sec "wipefs is spawned by absolute path and with an explicit backup directory"
# Every absolute path in the Rust comes from the rime_paths crate, chosen at
# build time; the bootc image sets no RIME_PATH_* and so gets the default there.
PATHS_RS="$REPO/rimed/rime-paths/src/lib.rs"
has 'const WIPEFS: &str = rime_paths::WIPEFS;' "$CLI" \
    "a PATH entry the caller controls would be a root shell that erases disks"
has 'WIPEFS = "/usr/sbin/wipefs";' "$PATHS_RS" \
    "and rime_paths' bootc value for it is /usr/sbin/wipefs"
hasnt 'RIME_PATH_WIPEFS' "$REPO/Containerfile.base" \
    "and the bootc image does not override it"
# Measured: `wipefs --backup` with no directory writes into $HOME, and $HOME
# under sudo is /root. The file a user is told to keep would land somewhere
# they will not look and may not be able to read.
has '--backup={}' "$CLI" "the backup directory is passed explicitly"
absent "$CLI_CODE" '"--backup"' \
    "and never as a bare --backup, which would write into root's home"

sec "a refused erase is a different exit status from a failed one"
# A caller — a settings page, a script, this suite — has to tell "the guard
# said no" from "the guard said yes and wipefs then failed". Both as 1 means a
# script cannot retry the second and must not retry the first.
has 'pub const EXIT_REFUSED' "$CLI" "the refused status has a name"
has 'Refusal::' "$CORE" "and the refusals are an enumeration, not strings"

sec "the enumerator sees every block device, and says so when it cannot"
# disks() skips devices/virtual/block, which is right for a health report and
# wrong here: it would make every loop device NotAKnownBlockDevice — the one
# class this command is ever tested against — and /dev/dm-0 unnameable while
# leaving whatever it maps perfectly erasable.
MACHINE="$(awk '/^pub fn machine\(/,/^}/' "$CLI")"
[[ -n "$MACHINE" ]] || bad "pub fn machine() was not found in $CLI"
absent "$MACHINE" 'disks\(' "machine() is its own enumerator and not disks()"
has 'pub fn machine(roots: &Roots) -> Result<Machine, String>' "$CLI" \
    "a partially-enumerated machine is not a machine to judge against"
# The defect this replaced: `let Ok(entries) = read_dir(&base) else { continue }`
# plus `.join("partition").exists()`. chmod 100 on one disk's sysfs directory
# dropped all five of its partitions and turned six refusals into a permit.
absent "$MACHINE" '\.exists\(\)' \
    "and never asks Path::exists(), which cannot tell EACCES from absent"
absent "$MACHINE" 'flatten\(\)' \
    "nor flattens away a device that exists and could not be named"

sec "an absence is only ever read from the one error that means absence"
# Two functions have an "absent" answer to give, and both must reach it from
# ErrorKind and never from a boolean.
for fn in 'fn loop_backing' 'pub fn machine'; do
    body="$(awk "/^${fn}/,/^}/" "$CLI")"
    [[ -n "$body" ]] || bad "${fn} was not found in $CLI"
    present "$body" 'ErrorKind::NotFound' \
        "${fn#*fn } reads absence from NotFound and not from a failed stat"
done
# dir_names has NO absent answer to give: a /sys/block that cannot be listed is
# not an absence of block devices, so every error is the Err arm and singling
# out NotFound would be the defect rather than the guard against it.
DIR_NAMES="$(awk '/^fn dir_names/,/^}/' "$CLI")"
[[ -n "$DIR_NAMES" ]] || bad "fn dir_names was not found in $CLI"
absent "$DIR_NAMES" 'ErrorKind' \
    "and a directory that will not list is never an empty directory"
has 'fn holders' "$CLI" "holders is read with read_dir"
HOLDERS="$(awk '/^fn holders/,/^}/' "$CLI")"
[[ -n "$HOLDERS" ]] || bad "fn holders was not found in $CLI"
present "$HOLDERS" 'Reading::Unavailable' \
    "and an unreadable holders directory is not an absence of holders"

# ── the destructive half ─────────────────────────────────────────────────────
#
#  THIS ONE REALLY ERASES A DEVICE. It is opt-in, it is not in
#  pr-validation.yml, and it needs `sudo -n`.
#
#  Every device it touches comes from `losetup --find --show` on a sparse file
#  it created seconds earlier, and it asserts
#  `/sys/block/<dev>/loop/backing_file` is that file before it does anything
#  else at all. That assertion is not belt-and-braces on the development
#  machine: /dev/loop0 there is attached to /lib/extensions/rime-user.raw, a
#  merged system extension carrying 219 packages, so a test that hardcoded a
#  loop device number or took one from argv would erase live machine state.
#
#  `backing_file` is a FILE CONTAINING A PATH, not a symlink. `realpath` on the
#  sysfs node compares a sysfs path against a tmp path and can therefore never
#  match — which fails safe, and means the test silently never runs. Hence the
#  `cat`.
SIG_BACKUPS="/var/lib/rime/storage/signature-backups"

# Every name the cleanup touches is a GLOBAL with a default, and that is not
# style. An EXIT trap runs after the function that installed it has returned,
# so a trap referring to one of its `local`s dies on `set -u` at the first
# line — and this file's first line is the umount, so NOTHING gets detached.
# Found by doing it: two loop devices left attached to deleted images, and the
# suite reported 59 passed. `${x:-}` everywhere below for the same reason: a
# cleanup that can abort is a cleanup that will.
DEVS=()
TMPD=""
MNT=""

cleanup_destructive() {
    local d
    [[ -n "${MNT:-}" ]] && sudo -n umount "${MNT}" 2>/dev/null
    for d in "${DEVS[@]:-}"; do
        [[ -n "$d" ]] || continue
        sudo -n losetup -d "$d" 2>/dev/null
        sudo -n rm -f "$SIG_BACKUPS/wipefs-${d#/dev/}"-*.bak 2>/dev/null
    done
    [[ -n "${TMPD:-}" ]] && rm -rf "${TMPD}"
    return 0
}

destructive_half() {
    sec "the destructive half — a real erase, on a loop device and nothing else"

    # This half must judge the REAL machine. A fixture root leaking in from the
    # environment would make every assertion below a statement about files this
    # script wrote, and the NeedsRoot step does not go through sudo's env_reset.
    unset RIME_STORAGE_ROOT

    local Rime="${Rime:-$REPO/rimed/target/debug/rime}"
    [[ -x "$Rime" ]] || { echo "FATAL: no rime binary at $Rime" >&2; exit 1; }
    sudo -n true 2>/dev/null \
        || { echo "FATAL: --destructive needs non-interactive sudo" >&2; exit 1; }

    # ONE trap doing everything, armed BEFORE anything exists to clean up. A
    # second `trap … EXIT` REPLACES the first rather than adding to it, so the
    # binary half's own trap and this one can never both be installed — which
    # is the other half of why --destructive is a separate branch.
    trap cleanup_destructive EXIT

    TMPD="$(mktemp -d)"
    MNT="$TMPD/mnt"
    local img="$TMPD/erase-me.img" out="$TMPD/out"
    local dev pimg pdev rc backing
    mkdir -p "$MNT"

    # ── the device, and the assertion that earns the right to touch it ──
    truncate -s 64M "$img" || { bad "could not create $img"; return; }
    dev="$(sudo -n losetup --find --show "$img")"
    [[ -n "$dev" ]] || { bad "losetup --find --show produced no device"; return; }
    DEVS+=("$dev")
    backing="$(cat "/sys/block/${dev#/dev/}/loop/backing_file" 2>/dev/null)"
    if [[ -z "$backing" || "$(realpath "$backing")" != "$(realpath "$img")" ]]; then
        bad "REFUSING TO CONTINUE: $dev backs '${backing:-nothing}', not $img"
        return
    fi
    ok "$dev is attached to the file this test made, asserted before anything else"

    # ── mounted: the one refusal everybody does write ──
    sudo -n /usr/sbin/mkfs.ext4 -q -F "$dev" 2>/dev/null || { bad "mkfs failed"; return; }
    sudo -n mount "$dev" "$MNT" || { bad "mount failed"; return; }
    sudo -n "$Rime" storage erase "$dev" --confirm "$dev" \
        --expect-backing-file "$img" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 3 ]] && ok "a mounted device is refused with exit 3" \
                      || bad "a mounted device exited $rc: $(cat "$out")"
    has 'is mounted at' "$out" "and the refusal names where"
    # The signature has to still be there. An erase that refused and wiped is
    # the failure this whole file exists to make impossible.
    if grep -qF ext4 <<<"$(sudo -n /usr/sbin/wipefs -n "$dev" 2>/dev/null)"; then
        ok "and the ext4 signature is still on the device"
    else
        bad "THE SIGNATURE IS GONE — a refused erase erased"
    fi
    sudo -n umount "$MNT"

    # ── privilege, first and alone ──
    "$Rime" storage erase "$dev" --confirm "$dev" \
        --expect-backing-file "$img" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 3 ]] && ok "an unprivileged erase is refused with exit 3" \
                      || bad "unprivileged erase exited $rc: $(cat "$out")"
    has 'root' "$out" "and says so"
    # Privilege is reported alone: a list of six refusals when the first is
    # "you are not root" is how somebody ends up reaching for --force.
    n="$(grep -c '^  - ' "$out")"
    [[ "$n" == "1" ]] && ok "and is the only thing it says" \
                      || bad "an unprivileged erase reported $n refusals"

    # ── the confirmation ──
    sudo -n "$Rime" storage erase "$dev" --confirm yes \
        --expect-backing-file "$img" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 3 ]] && ok "the wrong confirmation token is refused with exit 3" \
                      || bad "a wrong token exited $rc: $(cat "$out")"

    # ── the loop rule: this device is in no mount table at all ──
    sudo -n "$Rime" storage erase "$dev" --confirm "$dev" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 3 ]] && ok "an attached loop device with no asserted backing file is refused" \
                      || bad "an attached loop device exited $rc: $(cat "$out")"
    has 'loop device attached to' "$out" "naming the file it writes through to"
    has '--expect-backing-file' "$out" "and the way through"

    sudo -n "$Rime" storage erase "$dev" --confirm "$dev" \
        --expect-backing-file "$TMPD/not-this-one.img" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 3 ]] && ok "asserting the wrong backing file is refused with exit 3" \
                      || bad "a wrong backing file exited $rc: $(cat "$out")"
    has 'not-this-one.img' "$out" "and both paths are named"

    # ── a partition of an attached loop device is on that same file ──
    # Measured: /sys/block/loopN/loopNp1 has a `partition` file and no `loop/`
    # directory at all, so read on its own it answers "not a loop device" —
    # while wipefs on it writes through the parent into the backing file
    # exactly as it would on the whole device.
    pimg="$TMPD/parted.img"
    truncate -s 64M "$pimg"
    pdev="$(sudo -n losetup --find --show --partscan "$pimg")"
    if [[ -n "$pdev" ]]; then
        DEVS+=("$pdev")
        backing="$(cat "/sys/block/${pdev#/dev/}/loop/backing_file" 2>/dev/null)"
        if [[ "$(realpath "${backing:-/nonexistent}")" != "$(realpath "$pimg")" ]]; then
            bad "REFUSING TO CONTINUE: $pdev backs '${backing:-nothing}', not $pimg"
        else
            printf 'label: gpt\nsize=32MiB, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4\n' \
                | sudo -n /usr/sbin/sfdisk -q "$pdev" >/dev/null 2>&1
            sudo -n partprobe "$pdev" 2>/dev/null
            # Wait on the SYSFS directory, not on the /dev node. machine()
            # enumerates /sys/block and never looks at /dev, and the two
            # appear at slightly different moments — a wait on /dev/loopNp1
            # can be satisfied while /sys/block/loopN/loopNp1 is not there
            # yet, and the partition then reads as NotAKnownBlockDevice. That
            # is still exit 3, so the assertion on the refusal's WORDING is
            # the only thing that catches it. Seen once as a flake before
            # this loop watched the right path.
            local part="${pdev}p1" pn="${pdev#/dev/}" waited=0
            while [[ ! -d "/sys/block/$pn/${pn}p1" && "$waited" -lt 50 ]]; do
                sleep 0.1; waited=$((waited + 1))
            done
            if [[ -d "/sys/block/$pn/${pn}p1" ]]; then
                sudo -n "$Rime" storage erase "$part" --confirm "$part" > "$out" 2>&1; rc=$?
                [[ "$rc" -eq 3 ]] \
                    && ok "a partition of an attached loop device is refused too" \
                    || bad "$part exited $rc — the loop rule is downward only: $(cat "$out")"
                has 'loop device attached to' "$out" "inheriting its parent's backing file"
            else
                bad "/sys/block/$pn/${pn}p1 never appeared after sfdisk + partprobe"
            fi
        fi
    else
        bad "could not attach a second loop device for the partition case"
    fi

    # ── and finally the thing itself ──
    local before after
    before="$(sudo -n ls "$SIG_BACKUPS" 2>/dev/null | wc -l)"
    sudo -n "$Rime" storage erase "$dev" --confirm "$dev" \
        --expect-backing-file "$img" > "$out" 2>&1; rc=$?
    [[ "$rc" -eq 0 ]] && ok "an unmounted loop device with the file named erases, exit 0" \
                      || bad "the erase exited $rc: $(cat "$out")"
    if grep -qF ext4 <<<"$(sudo -n /usr/sbin/wipefs -n "$dev" 2>/dev/null)"; then
        bad "the erase reported success and the ext4 signature is still there"
    else
        ok "and the signature is really gone"
    fi
    after="$(sudo -n ls "$SIG_BACKUPS" 2>/dev/null | wc -l)"
    [[ "$after" -gt "$before" ]] && ok "a signature backup was written" \
                                 || bad "no backup appeared in $SIG_BACKUPS"
    # The path, not the word: guidance a user cannot follow is no guidance, and
    # the measured default for `wipefs --backup` is /root under sudo.
    has "$SIG_BACKUPS" "$out" "and the output names the directory it is in"
}

if [[ "$DESTRUCTIVE" -eq 1 ]]; then
    destructive_half
    printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
    [[ "$FAIL" -eq 0 ]] || exit 1
    exit 0
fi

if [[ "$WITH_BINARY" -eq 0 ]]; then
    printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
    [[ "$FAIL" -eq 0 ]] || exit 1
    exit 0
fi

# ── binary-driven half ──────────────────────────────────────────────────────

Rime="${Rime:-$REPO/rimed/target/debug/rime}"
[[ -x "$Rime" ]] || { echo "FATAL: no rime binary at $Rime" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export XDG_STATE_HOME="$TMP/state"
export XDG_CONFIG_HOME="$TMP/config"
mkdir -p "$XDG_STATE_HOME" "$XDG_CONFIG_HOME"

FIX="$TMP/machine"
export RIME_STORAGE_ROOT="$FIX"

# ── the two loop names the erase fixture is allowed to use ───────────────────
#
# Four erase requests in this suite are supposed to come back PERMITTED, so the
# only thing between them and `wipefs` is the fixture gate in erase(). They must
# therefore name devices that exist NOWHERE on the machine running the suite.
#
# They used to be hardcoded as loop7 and loop8, above a comment asserting those
# "exist on no machine this suite runs on". That was true of this laptop and
# false the first time CI ran it: a GitHub runner pre-creates /dev/loop0 through
# /dev/loop15, and the guard below fired exactly as designed —
#
#     FAIL /dev/loop7 EXISTS here and this suite expects a permit for it
#
# So the pair is chosen at runtime instead. The guard stays: it is what turns
# this from a promise into a measurement.
LA=""; LB=""
_n=7
while [ "$_n" -lt 512 ]; do
    _m=$((_n + 1))
    if [ ! -e "/dev/loop$_n" ] && [ ! -e "/dev/loop${_n}p1" ] \
    && [ ! -e "/dev/loop$_m" ] && [ ! -e "/dev/loop${_m}p1" ]; then
        LA="loop$_n"; LB="loop$_m"; break
    fi
    _n=$((_n + 1))
done
if [ -z "$LA" ]; then
    # Could-not-run, said out loud. Silently picking a name that exists is how
    # a suite ends up pointing wipefs at somebody's disk.
    echo "FATAL: every loop index from 7 to 512 exists on this machine, so this" >&2
    echo "       suite cannot name a device that is safe to be told to erase." >&2
    exit 1
fi
# Loop devices are major 7, minor = index; the partition keeps the old
# index*10+1 shape so an unchanged machine produces the unchanged 7:71.
LA_PART_MINOR=$(( ${LA#loop} * 10 + 1 ))

# A whole machine: one NVMe with five partitions, one loop device that must be
# skipped, a composefs root and a btrfs volume under five mount points.
build_machine() {
    rm -rf "$FIX"
    mkdir -p "$FIX/sys/block" "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1" \
             "$FIX/sys/devices/virtual/block/loop0" \
             "$FIX/proc/self" "$FIX/run/udev/data" "$FIX/.fixture/smartctl" \
             "$FIX/.fixture/space"
    # /sys/block entries are symlinks into /sys/devices; the loop device lives
    # under devices/virtual/block and is skipped by where it is, not by name.
    ln -sfn ../devices/pci0000:00/nvme/nvme0/nvme0n1 "$FIX/sys/block/nvme0n1"
    ln -sfn ../devices/virtual/block/loop0 "$FIX/sys/block/loop0"

    local d="$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1"
    mkdir -p "$d/queue" "$d/device/hwmon4" "$d/holders"
    printf '4000797360\n' > "$d/size"
    printf '0\n'          > "$d/queue/rotational"
    printf '512\n'        > "$d/queue/discard_granularity"
    printf '0\n'          > "$d/removable"
    printf '0\n'          > "$d/ro"
    printf 'SPCC M.2 PCIe SSD\n' > "$d/device/model"
    printf '37850\n'      > "$d/device/hwmon4/temp1_input"

    # The partition types are the real disk's, measured from
    # /run/udev/data/b259:*. p1 carries the ESP type GUID and is the ONLY
    # partition on that disk with an ID_PART_ENTRY_NAME at all; p2-p5 carry a
    # Linux-filesystem type GUID and no name. Keying the ESP rule on the name
    # would therefore miss the ESP of any installer that left it unnamed, and
    # treating a missing name as unverifiable refuses four of five ordinary
    # partitions for a reason that is not true.
    local ESP_GUID="c12a7328-f81f-11d2-ba4b-00a0c93ec93b"
    local LINUX_GUID="0fc63daf-8483-4772-8e79-3d69d8477de4"
    local i=1
    for spec in "p1:1228800:vfat:$ESP_GUID" "p2:4194304:ext4:$LINUX_GUID" \
                "p3:831107072:btrfs:$LINUX_GUID" "p4:4194304:ext4:$LINUX_GUID" \
                "p5:3160272896:btrfs:$LINUX_GUID"; do
        local part="${spec%%:*}" rest="${spec#*:}"
        local sectors="${rest%%:*}"; rest="${rest#*:}"
        local fs="${rest%%:*}" ptype="${rest#*:}"
        mkdir -p "$d/nvme0n1$part/holders"
        printf '%s\n' "$sectors" > "$d/nvme0n1$part/size"
        printf '1\n'             > "$d/nvme0n1$part/partition"
        printf '259:%s\n' "$i"   > "$d/nvme0n1$part/dev"
        {
            printf 'S:disk/by-label/rime-%s\n' "$part"
            printf 'I:11132713\n'
            printf 'E:ID_SERIAL_SHORT=NF12312150500322\n'
            printf 'E:ID_MODEL=SPCC M.2 PCIe SSD\n'
            printf 'E:ID_FS_TYPE=%s\n' "$fs"
            printf 'E:ID_PART_ENTRY_SCHEME=gpt\n'
            printf 'E:ID_PART_ENTRY_TYPE=%s\n' "$ptype"
            [[ "$part" == "p1" ]] && printf 'E:ID_PART_ENTRY_NAME=EFI System Partition\n'
        } > "$FIX/run/udev/data/b259:$i"
        i=$((i + 1))
    done

    # Two more loop devices, for the erase guard. **Neither is loop0.** The
    # development machine's real /dev/loop0 is attached to
    # /lib/extensions/rime-user.raw — a merged system extension — so a fixture
    # device sharing that name would, if a mutation ever dropped the fixture
    # gate in erase(), aim wipefs at live machine state. $LA and $LB are chosen
    # at the top of this file precisely because they exist on no machine.
    #
    # $LA has no `loop/` directory: not an attached loop device, and the one
    # device in this fixture a correct guard is supposed to permit.
    # $LB has one, so it is in use by whoever attached it — and it appears in
    # the mount table zero times, which is the whole point.
    local l
    for l in "$LA" "$LB"; do
        mkdir -p "$FIX/sys/devices/virtual/block/$l/holders" \
                 "$FIX/sys/devices/virtual/block/$l/queue"
        ln -sfn "../devices/virtual/block/$l" "$FIX/sys/block/$l"
        printf '131072\n' > "$FIX/sys/devices/virtual/block/$l/size"
        printf '0\n'      > "$FIX/sys/devices/virtual/block/$l/removable"
        printf '0\n'      > "$FIX/sys/devices/virtual/block/$l/ro"
        printf '0\n'      > "$FIX/sys/devices/virtual/block/$l/queue/rotational"
    done
    mkdir -p "$FIX/sys/devices/virtual/block/${LB}/loop"
    printf '/var/tmp/some-image.raw\n' \
        > "$FIX/sys/devices/virtual/block/${LB}/loop/backing_file"
    mkdir -p "$FIX/sys/devices/virtual/block/loop0/holders"

    # A partition with a legible, non-ESP type — the case that must come back
    # PERMITTED. It hangs off $LA rather than off nvme0n1 for one reason:
    # nvme0n1p1..p5 are the real names of real partitions on the machine that
    # runs this suite, one of which holds another operating system. A test
    # asserting "this device is erasable" must never name a device that
    # exists, because the only thing standing between that assertion and
    # `wipefs` is the fixture gate — and the whole point of a mutation battery
    # is to find out what happens when one `if` goes missing.
    mkdir -p "$FIX/sys/devices/virtual/block/${LA}/${LA}p1/holders"
    printf '65536\n' > "$FIX/sys/devices/virtual/block/${LA}/${LA}p1/size"
    printf '1\n'     > "$FIX/sys/devices/virtual/block/${LA}/${LA}p1/partition"
    printf '7:%s\n' "$LA_PART_MINOR" > "$FIX/sys/devices/virtual/block/$LA/${LA}p1/dev"
    {
        printf 'E:ID_FS_TYPE=ext4\n'
        printf 'E:ID_PART_ENTRY_SCHEME=gpt\n'
        printf 'E:ID_PART_ENTRY_TYPE=%s\n' "$LINUX_GUID"
    } > "$FIX/run/udev/data/b7:$LA_PART_MINOR"

    # Measured on the L16: / is composefs, and one btrfs volume is mounted at
    # five paths of which only /sysroot exposes the whole filesystem.
    cat > "$FIX/proc/self/mountinfo" <<'EOF'
49 1 0:39 / / ro,relatime shared:1 - overlay composefs ro,seclabel,lowerdir+=/x
46 49 0:36 /ostree/deploy/default/deploy/f3f5.0/etc /etc rw,relatime shared:2 - btrfs /dev/nvme0n1p5 rw,seclabel,ssd,discard=async,space_cache=v2
50 49 0:36 / /sysroot ro,relatime shared:3 - btrfs /dev/nvme0n1p5 rw,seclabel,ssd,discard=async,space_cache=v2
51 50 0:36 /ostree/deploy/default/var /var rw,relatime shared:4 - btrfs /dev/nvme0n1p5 rw,seclabel,ssd,discard=async,space_cache=v2
52 49 0:36 /boot /boot rw,relatime shared:5 - btrfs /dev/nvme0n1p5 rw,seclabel,ssd,discard=async,space_cache=v2
39 49 0:7 / /dev rw,nosuid shared:6 - devtmpfs devtmpfs rw,seclabel
41 39 0:27 / /dev/shm rw,nosuid,nodev shared:7 - tmpfs tmpfs rw,seclabel
53 49 0:40 / /boot/efi rw,relatime shared:8 - vfat /dev/nvme0n1p1 rw,fmask=0077
EOF
    # 1.5 TB, 27% used — the real figures from the L16.
    printf '1618000000000 1180000000000\n' > "$FIX/.fixture/space/sysroot"
    printf '629145600 500000000\n'         > "$FIX/.fixture/space/boot_efi"
    printf 'enabled\n' > "$FIX/.fixture/fstrim-enabled"
    printf 'Mon 2026-09-07 01:06:25 AWST\n' > "$FIX/.fixture/fstrim-last"
}

# The three smartctl answers this feature turns on.
smart_healthy() {
    printf '0\n' > "$FIX/.fixture/smartctl/nvme0n1.exit"
    cat > "$FIX/.fixture/smartctl/nvme0n1.json" <<'EOF'
{"smartctl": {"exit_status": 0},
 "model_name": "SPCC M.2 PCIe SSD", "firmware_version": "SN13683",
 "smart_status": {"passed": true},
 "temperature": {"current": 38},
 "nvme_smart_health_information_log": {
   "critical_warning": 0, "available_spare": 100, "available_spare_threshold": 10,
   "percentage_used": 1, "power_on_hours": 9222}}
EOF
}
smart_failing() {
    # Bit 3. The read HAPPENED and the disk is failing.
    printf '8\n' > "$FIX/.fixture/smartctl/nvme0n1.exit"
    cat > "$FIX/.fixture/smartctl/nvme0n1.json" <<'EOF'
{"smartctl": {"exit_status": 8},
 "model_name": "SPCC M.2 PCIe SSD",
 "smart_status": {"passed": false},
 "temperature": {"current": 62},
 "nvme_smart_health_information_log": {
   "critical_warning": 4, "available_spare": 3, "available_spare_threshold": 10,
   "percentage_used": 99, "power_on_hours": 51000}}
EOF
}
smart_refused() {
    # Exactly what an unprivileged run produces: exit 2, a full document, no
    # health log, and the reason inside `smartctl.messages`.
    printf '2\n' > "$FIX/.fixture/smartctl/nvme0n1.exit"
    cat > "$FIX/.fixture/smartctl/nvme0n1.json" <<'EOF'
{"json_format_version": [1, 0],
 "smartctl": {"version": [7, 5],
   "messages": [{"string": "Smartctl open device: /dev/nvme0n1 failed: Permission denied",
                 "severity": "error"}],
   "exit_status": 2},
 "local_time": {"time_t": 1788729982}}
EOF
}

build_machine
smart_healthy

sec "a healthy machine reads as a healthy machine"
"$Rime" storage status > "$TMP/out" 2>&1
has 'SPCC M.2 PCIe SSD' "$TMP/out" "the disk is named by model"
has 'solid state' "$TMP/out" "and by kind"
# 4000797360 sectors. /sys/block/<d>/size is ALWAYS in 512-byte units whatever
# the disk's own block size is, so this is 2048.4 GB. Multiplying by the
# logical block size instead is a factor-of-eight error on a 4K-sector disk, in
# the direction that makes a full disk look empty. Asserted here because a
# mutation to 4096 left the whole shell suite green.
has '2048.4 GB' "$TMP/out" "the size is sectors x 512, not sectors x the block size"
has '1% of the rated endurance is used' "$TMP/out" "wear comes from the SMART log"
has '9222 hours powered on' "$TMP/out" "with the hours beside it"
has '38 °C' "$TMP/out" "and the temperature"
has 'the filesystem discards as it frees blocks' "$TMP/out" \
    "btrfs's discard=async counts as trimming"
has '0 needing attention' "$TMP/out" "nothing to do"
hasnt 'loop0' "$TMP/out" "the loop device is not a disk"
hasnt 'NF12312150500322' "$TMP/out" "the disk serial, sitting in the udev record, is not printed"

sec "free space is one row per filesystem, and never the composefs root"
if grep -qE '^\[[a-z]+\] +[a-z0-9]+ on /( |$)' "$TMP/out"; then
    bad "the composefs root got a free-space row: $(grep -E ' on /( |$)' "$TMP/out")"
else
    ok "the composefs root has no row"
fi
hasnt 'composefs' "$TMP/out" "and composefs is not named as a filesystem to watch"
has 'btrfs on /sysroot' "$TMP/out" "the btrfs volume is named by the mount that IS the filesystem"
n="$(grep -c 'btrfs on ' "$TMP/out")"
[[ "$n" == "1" ]] && ok "five mount points produce one row" \
                  || bad "five mount points produced $n rows"
has 'vfat on /boot/efi' "$TMP/out" "a second filesystem still gets its own row"

sec "a disk that says it is failing is red, not grey"
smart_failing
"$Rime" storage status > "$TMP/out" 2>&1
has '[attention]' "$TMP/out" "the wear row needs attention"
has 'failing' "$TMP/out" "and says the disk is failing"
hasnt '[unavailable] /dev/nvme0n1 health' "$TMP/out" \
    "a non-zero exit did not turn the row into an unmeasured one"
has '1 needing attention' "$TMP/out" "counted once"
has '62 °C' "$TMP/out" "and the temperature still came through"

"$Rime" storage warnings > "$TMP/warn" 2>&1; rc=$?
[[ "$rc" -eq 1 ]] && ok "rime storage warnings exits 1 for a failing disk" \
                  || bad "warnings exited $rc for a failing disk"

sec "a refused read is grey, not red, and not silent"
smart_refused
"$Rime" storage status > "$TMP/out" 2>&1
has '[unavailable] /dev/nvme0n1 health' "$TMP/out" "the wear row is unavailable"
has 'Permission denied' "$TMP/out" "carrying what smartctl actually said"
has 'sudo rime storage status' "$TMP/out" "and the remedy"
hasnt '[attention] /dev/nvme0n1 health' "$TMP/out" "a refused read is not a failure"
has '38 °C' "$TMP/out" "and hwmon still gives a temperature with no privilege at all"

"$Rime" storage warnings > "$TMP/warn" 2>&1; rc=$?
[[ "$rc" -eq 0 ]] && ok "warnings exits 0 for a row nobody could measure" \
                  || bad "warnings exited $rc for an unmeasured row"
has 'unavailable' "$TMP/warn" "and still prints it"

# The remedy has to be on the ROW. `status` prints a footer telling the user to
# try sudo, and that footer reaches exactly one of the four surfaces: not
# --json, not `warnings`, not `rime doctor`. A caller parsing the JSON, or a
# notifier reading a doctor line, sees "Permission denied" and no next step.
"$Rime" storage warnings --json > "$TMP/warn.json" 2>&1
has 'run it with sudo' "$TMP/warn.json" "the JSON row carries the remedy, not only the footer"
"$Rime" storage status --json > "$TMP/status.json" 2>&1
has 'run it with sudo' "$TMP/status.json" "and so does status --json"
has 'Permission denied' "$TMP/status.json" "beside what smartctl actually said"
hasnt 'no SMART data' "$TMP/status.json" "and the reason is never 'no SMART data'"

sec "a filesystem with nothing left is an alert; a big one at the same percentage is not"
smart_healthy
printf '2000000000 100000000\n' > "$FIX/.fixture/space/boot_efi"
"$Rime" storage status > "$TMP/out" 2>&1
has '95% used' "$TMP/out" "the small filesystem is 95% used"
has '[attention] vfat on /boot/efi' "$TMP/out" "and that is an alert"
printf '4000000000000 400000000000\n' > "$FIX/.fixture/space/boot_efi"
"$Rime" storage status > "$TMP/out" 2>&1
has '90% used' "$TMP/out" "a 4 TB filesystem is 90% used"
hasnt '[attention] vfat' "$TMP/out" "and 400 GB free is not an alert"
printf '629145600 500000000\n' > "$FIX/.fixture/space/boot_efi"

sec "a partition nobody could look at is never called unencrypted"
mv "$FIX/run/udev/data/b259:3" "$TMP/udev-p3.saved"
"$Rime" storage status > "$TMP/out" 2>&1
has '[unavailable] /dev/nvme0n1p3' "$TMP/out" "the row is unavailable"
p3="$(grep '/dev/nvme0n1p3' "$TMP/out")"
case "$p3" in
    *"not encrypted"*) bad "a partition with no udev record was called unencrypted: $p3" ;;
    *) ok "and does not say it is unencrypted" ;;
esac
mv "$TMP/udev-p3.saved" "$FIX/run/udev/data/b259:3"

sec "a LUKS partition is named as encrypted"
sed -i 's/ID_FS_TYPE=btrfs/ID_FS_TYPE=crypto_LUKS/' "$FIX/run/udev/data/b259:3"
printf 'E:ID_FS_VERSION=2\n' >> "$FIX/run/udev/data/b259:3"
"$Rime" storage status > "$TMP/out" 2>&1
has 'encrypted (LUKS2)' "$TMP/out" "with its version"
sed -i 's/ID_FS_TYPE=crypto_LUKS/ID_FS_TYPE=btrfs/' "$FIX/run/udev/data/b259:3"

sec "a device the kernel forced read-only is an alert"
printf '1\n' > "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/ro"
"$Rime" storage status > "$TMP/out" 2>&1
has 'is read-only' "$TMP/out" "the row exists"
has 'nothing can be written to it' "$TMP/out" "and says what that means"
printf '0\n' > "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/ro"

sec "a device that supports discard with nothing trimming it is the only trim alert"
printf 'disabled\n' > "$FIX/.fixture/fstrim-enabled"
sed -i 's/,discard=async//g' "$FIX/proc/self/mountinfo"
"$Rime" storage status > "$TMP/out" 2>&1
has '[attention] /dev/nvme0n1 trim' "$TMP/out" "nothing is trimming a device that supports it"
printf '0\n' > "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/queue/discard_granularity"
"$Rime" storage status > "$TMP/out" 2>&1
has 'nothing to trim' "$TMP/out" "a device with no discard has nothing to trim"
hasnt '[attention] /dev/nvme0n1 trim' "$TMP/out" "and is not an alert"
build_machine; smart_healthy

# ── the erase guard, driven through the shipped binary ───────────────────────
#
# Seven of the ten refusals are only reachable from a shell with the fixture
# euid override. Without it every erase here would stop at NeedsRoot — the
# suite would report a pass for a guard whose other nine rules had never run
# once. Nothing below spawns wipefs: erase() returns on `roots.is_fixture()`
# above the subprocess, unconditionally.
ERC=0
erase_try() {   # <device> <token> [extra args…]
    local d="$1" t="$2"; shift 2
    "$Rime" storage erase "$d" --confirm "$t" "$@" > "$TMP/erase" 2>&1
    ERC=$?
}
refused_with() {   # <substring> <description>
    if [[ "$ERC" -ne 3 ]]; then
        bad "$2 — exit was $ERC, not 3:"; sed 's/^/       /' "$TMP/erase" >&2
    else
        has "$1" "$TMP/erase" "$2"
    fi
}

sec "nothing this suite expects a permit for exists on the machine running it"
# The fixture gate is one `if`. Everything below asks the shipped binary to
# erase things, and four of those requests are supposed to come back PERMITTED
# — so if the gate ever goes missing, those four are what wipefs gets pointed
# at. They are therefore named after devices that exist nowhere. This is the
# assertion that keeps that true as the fixture grows, and it is not
# theoretical: the development machine's /dev/loop0 is a live system extension
# and its nvme0n1p1..p5 include another installed operating system.
for d in "$LA" "${LA}p1" "$LB" "${LB}p1"; do
    if [[ -e "/dev/$d" ]]; then
        bad "/dev/$d EXISTS here and this suite expects a permit for it"
    else
        ok "/dev/$d is on no machine, so a lost fixture gate still erases nothing"
    fi
done

sec "an erase is refused by default, and every refusal is reachable"
printf '0\n' > "$FIX/.fixture/euid"

erase_try /dev/nvme0n1p5 /dev/nvme0n1p5
refused_with 'is mounted at' "a mounted partition is refused"

# The disk that carries this OS is a mount source ZERO times — measured. Only
# p5 appears in mountinfo, so `wipefs /dev/nvme0n1`, which takes the partition
# table and everything on it, passes an "is it mounted?" test cleanly.
erase_try /dev/nvme0n1 /dev/nvme0n1
refused_with 'carries the running operating system' \
    "the whole disk is refused although it is mounted nowhere itself"
has 'nvme0n1p5' "$TMP/erase" "and the evidence names the partition that is"

# / is composefs on overlay: not a block device, and never will be on a bootc
# machine. A critical-target list containing / protects nothing at all here.
hasnt 'mounted at /,' "$TMP/erase" "the composefs root is not offered as evidence"

erase_try /dev/nvme0n1p1 /dev/nvme0n1p1
refused_with 'is the EFI System Partition' "the ESP is refused"
has 'c12a7328' "$TMP/erase" "on its type GUID, not on its name"
has 'mounted nowhere' "$TMP/erase" "and the refusal says why nothing else caught it"

# nvme0n1p2..p5 have a type GUID and no ID_PART_ENTRY_NAME, exactly like the
# real disk. Refusing a partition for having no NAME would be four false
# refusals out of five, which is how a guard teaches a user it is noise — so
# the positive case has to be asserted, on a partition that exists nowhere.
erase_try /dev/${LA}p1 /dev/${LA}p1
if [[ "$ERC" -eq 0 ]]; then
    ok "an unmounted, unnamed, non-ESP partition is permitted"
else
    bad "a legible non-ESP partition was refused: $(cat "$TMP/erase")"
fi
# And the real disk's own unnamed partitions are judged on REASONS, never on
# having no name. **Deliberately with the WRONG token**, which is the technique
# the live sudo run against real machine state used: `Unconfirmed` guarantees
# no permit can be issued while every other rule still reports. /dev/nvme0n1p3
# exists on this machine and holds another installed operating system, so a
# correct token here would be a permit for it — safe only while the fixture
# gate holds, and a mutation battery exists to remove exactly that kind of
# only-thing-holding.
erase_try /dev/nvme0n1p3 not-the-token
has 'type it exactly' "$TMP/erase" "the wrong token alone stops it, so the rest can report freely"
hasnt 'could not be checked' "$TMP/erase" \
    "an unnamed partition with a legible type is not treated as unverifiable"
hasnt 'EFI System Partition' "$TMP/erase" "nor mistaken for the ESP"

erase_try /dev/sdz /dev/sdz
refused_with 'is not a block device on this machine' "a device that does not exist is refused"

erase_try /dev/${LA} yes
refused_with 'you have to type it exactly' "the wrong confirmation token is refused"

sec "a check that could not be performed refuses, and says which check"
printf '1000\n' > "$FIX/.fixture/euid"
erase_try /dev/${LA} /dev/${LA}
refused_with 'must run as root' "an unprivileged erase is refused"
n="$(grep -c '^  - ' "$TMP/erase")"
[[ "$n" == "1" ]] && ok "and privilege is reported alone, so nobody reaches for --force" \
                  || bad "an unprivileged erase reported $n refusals at once"

rm -f "$FIX/.fixture/euid"
erase_try /dev/${LA} /dev/${LA}
refused_with 'could not be checked' "an unreadable effective uid refuses"
has 'which user is running this' "$TMP/erase" "naming the check that did not happen"
hasnt 'must run as root' "$TMP/erase" \
    "and does not advise a sudo that would not have helped"
printf '0\n' > "$FIX/.fixture/euid"

mv "$FIX/proc/self/mountinfo" "$TMP/mountinfo.saved"
erase_try /dev/${LA} /dev/${LA}
refused_with 'could not be checked' "an unreadable mount table refuses"
has 'which filesystems are mounted' "$TMP/erase" \
    "because an unreadable mountinfo is not an empty mount table"
mv "$TMP/mountinfo.saved" "$FIX/proc/self/mountinfo"

# Every holders directory on the development machine is empty — there is no
# LUKS anywhere on it — so this rule cannot be exercised live even once. That
# is the argument for the guard being pure and fixture-driven.
mkdir -p "$FIX/sys/devices/virtual/block/${LA}/holders/dm-0"
erase_try /dev/${LA} /dev/${LA}
refused_with 'stacked on it' "a device with a holder is refused"
rmdir "$FIX/sys/devices/virtual/block/${LA}/holders/dm-0"

# Mode 100: traversable, unlistable — which is precisely what "the holders of
# this device cannot be listed" means, and the mode that reaches the holders
# reading itself.
chmod 100 "$FIX/sys/devices/virtual/block/${LA}/holders"
erase_try /dev/${LA} /dev/${LA}
refused_with 'could not be checked' "an unlistable holders directory refuses"
has "stacked on /dev/$LA" "$TMP/erase" "naming the check that did not happen"
hasnt 'has  stacked on it' "$TMP/erase" "and is not reported as an absence of holders"
chmod 755 "$FIX/sys/devices/virtual/block/${LA}/holders"

# Mode 000 is not traversable, so the enumerator's own probe for a `partition`
# file inside that directory fails first and the request is refused as an
# incomplete enumeration rather than as unlistable holders. Both refuse, which
# is the only thing that matters; asserted here so that the interaction is a
# recorded fact rather than a surprise for whoever changes either one.
chmod 000 "$FIX/sys/devices/virtual/block/${LA}/holders"
erase_try /dev/${LA} /dev/${LA}
[[ "$ERC" -eq 3 ]] && ok "an untraversable subdirectory of a device refuses as well" \
                   || bad "an untraversable subdirectory exited $ERC: $(cat "$TMP/erase")"
hasnt 'would erase' "$TMP/erase" "and is never permitted"
chmod 755 "$FIX/sys/devices/virtual/block/${LA}/holders"

# The defect: chmod 100 is readable to nobody and still traversable, so
# holders/ and dev keep answering and only the LISTING fails. Every partition
# of the disk then vanishes from the Machine, and the whole-disk erase went
# from six refusals to a granted permit. Nothing but the fixture gate stood
# between that and wipefs on the disk carrying the running system.
chmod 100 "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1"
erase_try /dev/nvme0n1 /dev/nvme0n1
refused_with 'could not all be listed' \
    "a disk whose partitions could not be listed is refused, not judged without them"
hasnt 'would erase' "$TMP/erase" "and certainly not permitted"
chmod 755 "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1"

# The other half of the same rule, and the one the battery proved was
# untested: the disk's directory LISTS perfectly, and one partition's own
# directory cannot be traversed, so the `partition` marker inside it cannot be
# stat'ed. Folding that into "not a partition" is one ErrorKind away and the
# whole-disk refusal keeps working — it just stops mentioning the ESP.
# Mutation M10 was completely green until this existed.
chmod 000 "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1"
erase_try /dev/nvme0n1 /dev/nvme0n1
refused_with 'could not all be listed' \
    "a partition whose own directory cannot be traversed refuses the whole request"
hasnt 'would erase' "$TMP/erase" "rather than quietly not being a partition"
chmod 755 "$FIX/sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1"

sec "an attached loop device is in no mount table at all"
# The finding this rule exists for: /dev/loop0 on the development machine is
# attached to /lib/extensions/rime-user.raw, a merged system extension carrying
# 219 packages, and it appears in /proc/self/mountinfo zero times, in
# /proc/1/mountinfo zero times, and its holders directory is empty. Every other
# rule in the guard passed it, and wipefs on a loop device writes straight
# through to the backing file.
erase_try /dev/${LB} /dev/${LB}
refused_with 'is a loop device attached to' "an attached loop device is refused"
has '/var/tmp/some-image.raw' "$TMP/erase" "naming the file it would write through to"
has 'losetup -d' "$TMP/erase" "with a way to detach it"
has '--expect-backing-file' "$TMP/erase" "and a way through for somebody who means it"

erase_try /dev/${LB} /dev/${LB} --expect-backing-file /var/tmp/other.raw
refused_with 'not to /var/tmp/other.raw' "asserting the wrong backing file is refused"
has 'some-image.raw' "$TMP/erase" "and both paths are named"

erase_try /dev/${LA} /dev/${LA} --expect-backing-file /var/tmp/some-image.raw
refused_with 'backs no file' "asserting a backing file for a device that has none is refused"

erase_try /dev/${LB} /dev/${LB} --expect-backing-file /var/tmp/some-image.raw
if [[ "$ERC" -eq 0 ]]; then
    ok "and naming the backing file correctly is permission"
else
    bad "the correct backing file was still refused: $(cat "$TMP/erase")"
fi

# `backing_file` exists and cannot be read. Reading that as "not an attached
# loop device" is one `Err` arm away, and it is the arm that erases the live
# system extension: /dev/loop0's node is there, and only its CONTENT says what
# it would destroy. Mutation M11 was completely green until this existed.
chmod 000 "$FIX/sys/devices/virtual/block/${LB}/loop/backing_file"
erase_try /dev/${LB} /dev/${LB}
refused_with 'could not be checked' "an unreadable backing_file refuses"
has 'attached loop device' "$TMP/erase" "naming the check that did not happen"
hasnt 'would erase' "$TMP/erase" "and is never read as 'not a loop device'"
chmod 644 "$FIX/sys/devices/virtual/block/${LB}/loop/backing_file"

# Measured: /sys/block/loopN/loopNp1 has a `partition` file and NO loop/
# directory, so read on its own a loop partition answers "not an attached loop
# device" — while wipefs on it writes through the parent into the backing file
# just the same. would_destroy walks downward only, so nothing else relates a
# partition back up to its disk.
mkdir -p "$FIX/sys/devices/virtual/block/${LB}/${LB}p1/holders"
printf '1\n' > "$FIX/sys/devices/virtual/block/${LB}/${LB}p1/partition"
erase_try /dev/${LB}p1 /dev/${LB}p1
refused_with 'is a loop device attached to' \
    "a partition of an attached loop device inherits its parent's backing file"
rm -rf "$FIX/sys/devices/virtual/block/${LB}/${LB}p1"

sec "and a device with nothing wrong with it is erasable"
erase_try /dev/${LA} /dev/${LA}
if [[ "$ERC" -eq 0 ]]; then
    ok "an unmounted device with no holders and no backing file is permitted"
    has 'would erase' "$TMP/erase" "and the fixture gate reports what it did not do"
else
    bad "the permittable device was refused: $(cat "$TMP/erase")"
fi
# The gate, again, from the outside: a fixture holds a real Permit and must
# still spawn nothing. There is no /dev/${LA} on this machine to erase, so the
# proof is that the run said "fixture root" rather than failing to find it.
has 'fixture root, nothing was touched' "$TMP/erase" "and says nothing was touched"

build_machine; smart_healthy

sec "the notifier"
NOTICE_BIN="$REPO/files/system/libexec/rime-storage-notice"
# A notify-send that records rather than notifies. Nothing reaches a desktop.
mkdir -p "$TMP/bin"
cat > "$TMP/bin/notify-send" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$TMP/notified"
EOF
chmod +x "$TMP/bin/notify-send"
export PATH="$TMP/bin:$PATH"
export RIME_BIN="$Rime"
rm -f "$TMP/notified"

"$NOTICE_BIN" > "$TMP/out" 2>&1
[[ ! -f "$TMP/notified" ]] && ok "a healthy machine notifies nobody" \
                           || bad "it notified: $(cat "$TMP/notified")"

smart_refused
"$NOTICE_BIN" > "$TMP/out" 2>&1
[[ ! -f "$TMP/notified" ]] && ok "a machine it could not measure notifies nobody" \
                           || bad "it notified about an unmeasured row: $(cat "$TMP/notified")"

smart_failing
"$NOTICE_BIN" > "$TMP/out" 2>&1
if [[ -f "$TMP/notified" ]]; then
    ok "a failing disk notifies"
    has 'failing' "$TMP/notified" "and says what is wrong"
    has 'critical' "$TMP/notified" "at critical urgency"
else
    bad "a failing disk notified nobody: $(cat "$TMP/out")"
fi
before="$(wc -l < "$TMP/notified")"
"$NOTICE_BIN" > "$TMP/out" 2>&1
after="$(wc -l < "$TMP/notified")"
[[ "$before" == "$after" ]] && ok "and does not say it again tomorrow" \
                            || bad "the same warning notified twice"

# The digest must be built from the ATTENTION rows alone. `rime storage
# warnings` prints the unavailable ones too, and their reasons move for
# reasons that are nothing to do with a disk — a run without privilege, a
# fixture path, a systemctl that answered differently. If those reach the
# digest, the same failing disk announces itself again every time one of them
# shifts, and the notifier becomes the thing users mute.
#
# So: hold the failing disk exactly where it is, and change only an
# unavailable row's reason. Nothing may be said.
rm -f "$FIX/.fixture/space/sysroot"          # unavailable: "no space fixture"
"$NOTICE_BIN" > "$TMP/out" 2>&1              # re-announce, digest now current
before="$(wc -l < "$TMP/notified")"
printf 'not-a-number\n' > "$FIX/.fixture/space/sysroot"   # unavailable: "malformed"
"$NOTICE_BIN" > "$TMP/out" 2>&1
after="$(wc -l < "$TMP/notified")"
[[ "$before" == "$after" ]] \
    && ok "an unavailable row changing its reason does not re-announce a standing warning" \
    || bad "the digest included an unavailable row, so a reason string re-notified"
printf '1618000000000 1180000000000\n' > "$FIX/.fixture/space/sysroot"

smart_healthy
"$NOTICE_BIN" > "$TMP/out" 2>&1
[[ ! -f "$XDG_STATE_HOME/rime/storage-warnings.seen" ]] \
    && ok "a cleared warning forgets its digest, so its return is announced" \
    || bad "the digest survived the warning clearing"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
