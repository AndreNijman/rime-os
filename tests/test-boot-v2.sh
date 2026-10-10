#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-boot-v2.sh — executable assertions for roadmap §22's boot v2.
#
#  Two modes, because the checks split cleanly by what they need:
#
#    (no argument)     Everything that needs no toolchain: the shipped units,
#                      the health gate's actual exit codes, the build-time
#                      scripts' refusals, the tripwire against a script that
#                      touches a real boot path, and the schema parity between
#                      what rime-boot-health WRITES and what rime boot status
#                      READS. Wired into pr-validation.yml's `static` job,
#                      which has no path filter.
#
#    --with-binary     Adds the cases that drive the built `rime` binary
#                      against fixture roots. Wired into the `rust` job, the
#                      only one with a toolchain. It DIES if the binary is
#                      absent rather than skipping: a skipped check counts as
#                      success, which is the bug docs/p1-progress.md already
#                      records twice.
#
#  Why the split is not "put it all in `rust`": that job fires on
#  ^(rimed/|config/sysprofiles/|tests/). A PR touching only files/system/units
#  would skip it, and a skipped job passes. The units and the tripwire are
#  exactly what such a PR changes.
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_BINARY=0
[[ "${1:-}" == "--with-binary" ]] && WITH_BINARY=1

PASS=0 FAIL=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }
eq()  { [[ "$1" == "$2" ]] && ok "$3 == $1" || bad "$3: want '$1', got '$2'"; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

HEALTH="$REPO/files/system/libexec/rime-boot-health"
UNIT_HEALTH="$REPO/files/system/units/rime-boot-health.service"
UNIT_NOTICE="$REPO/files/system/units/rime-boot-notice.service"
BOOTRS="$REPO/rimed/rime/src/boot.rs"
BASECF="$REPO/Containerfile.base"
LOADER_GUID=4a67b082-0a4c-41cf-b6c7-440b29bb8c4f

for f in "$HEALTH" "$UNIT_HEALTH" "$UNIT_NOTICE" "$BOOTRS" "$BASECF"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done
# A non-executable helper makes every exit-code assertion below return 126,
# which reads as "the gate refused" unless the assertions demand rc=1. Both
# guards are kept: this one names the cause, and the rc=1 checks catch a
# regression this one would miss.
[[ -x "$HEALTH" ]] || { echo "FATAL: $HEALTH is not executable in the repo" >&2; exit 1; }

# ═════════════════════════════════════════════════════════════════════════════
sec "the opt-in is structural, not a claim in a comment"
# This is the property the whole section is most likely to lose silently. One
# image serves machines installed on either backend (AGENTS.md boot-path rule
# 5), so every unit shipped here must be incapable of doing anything on a GRUB
# boot.
# The switch is systemd-boot's own LoaderBootCountPath EFI variable, which is
# also what systemd-bless-boot-generator conditions on.
for u in "$UNIT_HEALTH" "$UNIT_NOTICE"; do
    n="$(basename "$u")"
    if grep -qx "ConditionPathExists=/sys/firmware/efi/efivars/LoaderBootCountPath-$LOADER_GUID" "$u"; then
        ok "$n is conditioned on LoaderBootCountPath"
    else
        bad "$n has no LoaderBootCountPath condition — it would run on a GRUB machine"
    fi
done

# RequiredBy vs WantedBy is the difference between "a failed health check
# blocks the blessing" and "a failed health check is a warning".
grep -qx 'RequiredBy=boot-complete.target' "$UNIT_HEALTH" \
    && ok "rime-boot-health is RequiredBy=boot-complete.target" \
    || bad "rime-boot-health must be RequiredBy=boot-complete.target, or a failed health check blesses anyway"
grep -qx 'Before=boot-complete.target' "$UNIT_HEALTH" \
    && ok "rime-boot-health is ordered Before=boot-complete.target" \
    || bad "rime-boot-health must be Before=boot-complete.target"
grep -qx 'WantedBy=boot-complete.target' "$UNIT_NOTICE" \
    && ok "rime-boot-notice is WantedBy (reporting must not fail a healthy boot)" \
    || bad "rime-boot-notice must be WantedBy=boot-complete.target, not RequiredBy"

# Containerfile.base must enable both, and must itself assert the condition.
grep -q 'systemctl enable rime-boot-health.service' "$BASECF" \
    && ok "Containerfile.base enables rime-boot-health.service" \
    || bad "Containerfile.base does not enable rime-boot-health.service"
grep -q 'boot-complete.target.requires/rime-boot-health.service' "$BASECF" \
    && ok "Containerfile.base checks the RequiredBy symlink systemctl created" \
    || bad "Containerfile.base does not verify the enablement it performed"


# ═════════════════════════════════════════════════════════════════════════════
sec "EFI variable payloads are read off a file that cannot be seeked"
# efivarfs files are not seekable. `tail -c +5` seeks, so on a real machine it
# printed nothing and said "cannot seek to relative offset 4: Illegal seek" —
# and rime-boot-health reported `entry unknown` on every systemd-boot boot.
# Seen in the lab serial logs and reproduced on the L16, whose LoaderInfo is
# present and reads "GRUB 2.12". The regression is invisible against a regular
# fixture file, so the control here is a FIFO: unseekable, like efivarfs.
EFIT="$TMP/efivars-seek"
mkdir -p "$EFIT"
# The function under test is the shipped one, lifted out of the shipped file,
# so this cannot drift into testing a copy.
sed -n '/^efivar_str() {/,/^}/p' "$HEALTH" > "$TMP/efivar_str.sh"
[[ -s "$TMP/efivar_str.sh" ]] \
    || bad "could not lift efivar_str out of rime-boot-health — the two checks below are vacuous"

# The 4-byte attribute prefix must really be four bytes: a bash variable
# cannot hold the three NULs, so printf writes them directly.
printf '\x07\x00\x00\x00rime-good.efi' > "$EFIT/LoaderEntrySelected-$LOADER_GUID"
got="$(
    set +u
    # shellcheck disable=SC1090
    . "$TMP/efivar_str.sh"
    EFIVARS_DIR="$EFIT" LOADER_GUID="$LOADER_GUID" efivar_str LoaderEntrySelected
)"
eq 'rime-good.efi' "$got" "an EFI string is read out of a regular fixture file"

# ── and the same read against something that cannot be seeked ──
FIFO="$TMP/LoaderEntrySelected-$LOADER_GUID"
mkfifo "$FIFO"
( printf '\x07\x00\x00\x00rime-new.efi' > "$FIFO" 2>/dev/null ) &
wpid=$!
gotfifo="$(
    set +u
    # shellcheck disable=SC1090
    . "$TMP/efivar_str.sh"
    EFIVARS_DIR="$TMP" LOADER_GUID="$LOADER_GUID" efivar_str LoaderEntrySelected
)"
wait "$wpid" 2>/dev/null || true
eq 'rime-new.efi' "$gotfifo" "the byte-wise read works on an UNSEEKABLE file (the efivarfs case)"

# The inverse control has to be a REAL efivarfs file, and it is worth saying
# why rather than quietly using a weaker one. A FIFO does not reproduce the
# defect: GNU tail sees S_ISFIFO and reads instead of seeking, so
# `tail -c +5 "$FIFO"` succeeds. An efivarfs entry is a regular file that
# reports a size, so tail tries lseek and gets ESPIPE. Only the real thing
# discriminates.
REALVAR=/sys/firmware/efi/efivars/LoaderInfo-$LOADER_GUID
if [[ -r "$REALVAR" ]]; then
    oldway="$(timeout 10 tail -c +5 "$REALVAR" 2>/dev/null | tr -d '\0' || true)"
    newway="$(
        set +u
        # shellcheck disable=SC1090
        . "$TMP/efivar_str.sh"
        EFIVARS_DIR=/sys/firmware/efi/efivars LOADER_GUID="$LOADER_GUID" \
            efivar_str LoaderInfo 2>/dev/null || true
    )"
    [[ -n "$newway" ]] \
        && ok "on this machine's real efivarfs the shipped reader returns '$newway'" \
        || bad "the shipped reader returned nothing from $REALVAR"
    [[ "$oldway" != "$newway" ]] \
        && ok "inverse control: 'tail -c +5' does NOT return that on real efivarfs" \
        || bad "inverse control: 'tail -c +5' agreed here, so this check cannot catch the defect"
else
    printf '  NOTE this machine has no %s — the definitive\n' "$(basename "$REALVAR")"
    printf '       inverse control needs a UEFI host and was NOT run. The FIFO case\n'
    printf '       above proves the byte-wise read works on unseekable input; it does\n'
    printf '       NOT prove the old one failed. See the L16 transcript in\n'
    printf '       ROADMAP/evidence/sdboot-image-20260920-decision.md.\n'
fi

# Executable lines only: the helper's own comment quotes the broken command to
# explain why it is broken, and a tripwire a comment can trip is a tripwire
# nobody can keep green — the same rule as the boot-path scan below.
grep -nE '^[^#]*tail -c \+' "$HEALTH" >/dev/null 2>&1 \
    && bad "rime-boot-health still reads efivars with a seeking command" \
    || ok "rime-boot-health does not seek an efivarfs file"

# ── and a real UTF-16LE payload, which is what sd-boot actually writes ──
printf '\x07\x00\x00\x00' > "$EFIT/LoaderInfo-$LOADER_GUID"
printf 'systemd-boot 258.10' | iconv -f UTF-8 -t UTF-16LE >> "$EFIT/LoaderInfo-$LOADER_GUID"
gotu16="$(
    set +u
    # shellcheck disable=SC1090
    . "$TMP/efivar_str.sh"
    EFIVARS_DIR="$EFIT" LOADER_GUID="$LOADER_GUID" efivar_str LoaderInfo
)"
eq 'systemd-boot 258.10' "$gotu16" "a UTF-16LE payload reads back as the string sd-boot wrote"

# ═════════════════════════════════════════════════════════════════════════════
sec "the boot counter Rime writes, because bootc writes none"
# bootc produces entry filenames with no +N-M suffix and a loader.conf whose
# timeout is commented out, so on a machine installed exactly as bootc leaves
# it LoaderBootCountPath never appears and everything above is inert. The
# counter has to be written into the STAGED entry: bootc-finalize-staged
# replaces the whole entries/ directory at shutdown, discarding anything
# written into a live filename. Measured, eleven guest boots, evidence in
# ROADMAP/evidence/sdboot-image-20260920-lab.md.
COUNT="$REPO/files/system/libexec/rime-boot-count"
UNIT_COUNT="$REPO/files/system/units/rime-boot-count.service"
BLESS_DROPIN="$REPO/files/system/units/10-rime-bless-boot-esp.conf"
SEPOL="$REPO/files/system/selinux/rime_sdboot.te"
for f in "$COUNT" "$UNIT_COUNT" "$BLESS_DROPIN" "$SEPOL"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done
[[ -x "$COUNT" ]] || bad "$COUNT is not executable in the repo"

BOOTED=aaaa1111
FRESH=bbbb2222

# Build an ESP fixture. `live` is what bootc has installed; `staged` is what it
# is about to install. Nothing under live/ may ever change.
mk_esp() {           # mk_esp <dir> <srel-contents> <staged entry name...>
    local d="$1" srel="$2"; shift 2
    mkdir -p "$d/loader/entries" "$d/loader/entries.staged"
    [[ "$srel" == none ]] || printf '%s\n' "$srel" > "$d/loader/entries.srel"
    printf 'title live\noptions rw composefs=%s\n' "$BOOTED" \
        > "$d/loader/entries/bootc_rime-43-1.conf"
    local e
    for e in "$@"; do
        local digest="${e##*:}" name="${e%%:*}"
        printf 'title staged\noptions rw quiet composefs=%s\n' "$digest" \
            > "$d/loader/entries.staged/$name"
    done
}
staged_ls() { ( cd "$1/loader/entries.staged" && ls -1 | sort | tr '\n' ' ' ); }
live_ls()   { ( cd "$1/loader/entries" && ls -1 | sort | tr '\n' ' ' ); }
run_count() {        # run_count <esp> <cmdline>
    local esp="$1" cl="$2"
    printf '%s\n' "$cl" > "$TMP/cmdline"
    RIME_BOOT_ESP="$esp" RIME_BOOT_CMDLINE="$TMP/cmdline" "$COUNT" stage 2>>"$TMP/count.log"
}

# ── the ordinary case: two staged entries, one of them new ──
E="$TMP/esp-normal"
mk_esp "$E" type1 "bootc_rime-43-0.conf:$BOOTED" "bootc_rime-43-1.conf:$FRESH"
live_before="$(live_ls "$E")"
run_count "$E" "rw quiet composefs=$BOOTED" && rc=0 || rc=$?
eq 0 "$rc" "rime-boot-count exits 0 on the ordinary case"
eq 'bootc_rime-43-0.conf bootc_rime-43-1+3-0.conf ' "$(staged_ls "$E")" \
   "the entry whose composefs digest is NOT the booted one gets the counter"
eq "$live_before" "$(live_ls "$E")" "the LIVE entries directory is untouched"

# ── idempotence: a second run must not reset a counter sd-boot has decremented ──
E2="$TMP/esp-counted"
mk_esp "$E2" type1 "bootc_rime-43-0.conf:$BOOTED" "bootc_rime-43-1+1-2.conf:$FRESH"
run_count "$E2" "rw composefs=$BOOTED" || true
eq 'bootc_rime-43-0.conf bootc_rime-43-1+1-2.conf ' "$(staged_ls "$E2")" \
   "an already-counted staged set is left exactly as it was"

# ── nothing new staged (the bootc-rollback-to-current and no-op cases) ──
E3="$TMP/esp-nothing-new"
mk_esp "$E3" type1 "bootc_rime-43-0.conf:$BOOTED"
run_count "$E3" "rw composefs=$BOOTED" || true
eq 'bootc_rime-43-0.conf ' "$(staged_ls "$E3")" \
   "a staged set that is all the booted deployment is left alone"

# ── ambiguity is refused, never guessed ──
E4="$TMP/esp-ambiguous"
mk_esp "$E4" type1 "bootc_rime-43-0.conf:cccc3333" "bootc_rime-43-1.conf:$FRESH"
run_count "$E4" "rw composefs=$BOOTED" || true
eq 'bootc_rime-43-0.conf bootc_rime-43-1.conf ' "$(staged_ls "$E4")" \
   "two entries differing from the booted one are refused, not guessed between"

# ── a GRUB machine: no entries.srel, so nothing happens even if a dir exists ──
E5="$TMP/esp-grub"
mk_esp "$E5" none "bootc_rime-43-1.conf:$FRESH"
run_count "$E5" "rw composefs=$BOOTED" || true
eq 'bootc_rime-43-1.conf ' "$(staged_ls "$E5")" \
   "without entries.srel nothing is renamed (the GRUB case)"

# ── a loader directory that is not Type #1 ──
E6="$TMP/esp-type2"
mk_esp "$E6" type2 "bootc_rime-43-1.conf:$FRESH"
run_count "$E6" "rw composefs=$BOOTED" || true
eq 'bootc_rime-43-1.conf ' "$(staged_ls "$E6")" \
   "entries.srel saying anything but type1 is refused"

# ── no composefs= on the command line: it cannot tell which entry is new ──
E7="$TMP/esp-nocfs"
mk_esp "$E7" type1 "bootc_rime-43-0.conf:$BOOTED" "bootc_rime-43-1.conf:$FRESH"
run_count "$E7" "rw quiet" || true
eq 'bootc_rime-43-0.conf bootc_rime-43-1.conf ' "$(staged_ls "$E7")" \
   "with no composefs= on the cmdline it refuses rather than guessing"

# ── and the fixture itself must be capable of showing a rename ──
# Without this control every eq above could be passing because the helper is
# broken in a way that renames nothing, ever. The ordinary case already proves
# one rename happened; this proves the two are the same helper and the same
# fixture shape.
E8="$TMP/esp-control"
mk_esp "$E8" type1 "bootc_rime-43-0.conf:$BOOTED" "bootc_rime-43-7.conf:$FRESH"
run_count "$E8" "rw composefs=$BOOTED" || true
if [[ "$(staged_ls "$E8")" == *'bootc_rime-43-7+3-0.conf'* ]]; then
    ok "positive control: the same helper does rename when the case is unambiguous"
else
    bad "positive control failed — the refusals above may be vacuous: $(staged_ls "$E8")"
fi

# ── the unit's condition and ordering ──
# It cannot use LoaderBootCountPath: that variable exists only once counting is
# already in effect, and this unit is what starts it. Worse, it works in
# ExecStop, and a unit whose START condition failed is never stopped.
grep -qx 'ConditionPathExists=/boot/loader/entries.srel' "$UNIT_COUNT" \
    && ok "rime-boot-count is conditioned on entries.srel (absent on a GRUB machine)" \
    || bad "rime-boot-count has no entries.srel condition — it would run on a GRUB machine"
grep -qx 'After=bootc-finalize-staged.service' "$UNIT_COUNT" \
    && ok "rime-boot-count starts After bootc-finalize-staged, so it STOPS before it" \
    || bad "rime-boot-count must be After=bootc-finalize-staged.service, or its ExecStop runs after the swap"
grep -q '^ExecStop=/usr/libexec/rime-boot-count stage' "$UNIT_COUNT" \
    && ok "rime-boot-count does its work in ExecStop" \
    || bad "rime-boot-count must work in ExecStop — the staged dir exists only at shutdown"
grep -qx 'WantedBy=multi-user.target' "$UNIT_COUNT" \
    && ok "rime-boot-count is WantedBy (a missing counter must not fail a boot)" \
    || bad "rime-boot-count must be WantedBy=multi-user.target, not RequiredBy"
grep -q 'systemctl enable rime-boot-count.service' "$BASECF" \
    && ok "Containerfile.base enables rime-boot-count.service" \
    || bad "Containerfile.base does not enable rime-boot-count.service"
# It renames a .conf on the same FAT ESP the blessing does, which makes the
# same drop-in look obvious. It is wrong: /usr/libexec is bin_t and the policy
# has `type_transition init_t bin_t:process unconfined_service_t`, so the
# helper is already unconfined, while bootupd_t cannot even read /proc/cmdline.
grep -q '^SELinuxContext=' "$UNIT_COUNT" \
    && bad "rime-boot-count must NOT set SELinuxContext — bootupd_t cannot read /proc/cmdline" \
    || ok "rime-boot-count stays unconfined_service_t, like every /usr/libexec helper"
grep -q 'allow bootupd_t bin_t:file' "$SEPOL" \
    && bad "rime_sdboot.te grants a bin_t entrypoint nothing needs" \
    || ok "the policy module stays one rule wide — only init_exec_t needs it"

# ═════════════════════════════════════════════════════════════════════════════
sec "the blessing can write a FAT ESP, or none of the above matters"
# On the composefs path the ESP IS /boot, so the entries systemd-bless-boot
# renames are dosfs_t. PID 1 running /usr/lib/systemd/systemd-bless-boot
# (init_exec_t) stays in init_t, which Fedora 43 allows no rename on dosfs_t —
# measured on the Rime image, with the AVC, in
# ROADMAP/evidence/sdboot-image-20260920-decision.md. Unrepaired, the counter
# above turns into a machine that rolls itself back on every fourth boot.
grep -qx 'SELinuxContext=-system_u:system_r:bootupd_t:s0' "$BLESS_DROPIN" \
    && ok "the blessing runs in bootupd_t, the domain allowed to write a FAT ESP" \
    || bad "the bless-boot drop-in does not set SELinuxContext to bootupd_t"
# The leading dash makes FAILING TO SET the context non-fatal — systemd.exec(5)
# is explicit that the execve can still be denied afterwards, so this covers
# SELinux disabled or a policy without bootupd_t and NOT the module going
# missing on an enforcing machine. That case is the cross-tier assertion below.
grep -q 'SELinuxContext=-' "$BLESS_DROPIN" \
    && ok "setting the context is non-fatal if it cannot be set (the leading dash)" \
    || bad "SELinuxContext has no leading dash — a permissive or SELinux-less machine would fail the unit"
grep -q 'allow bootupd_t init_exec_t:file' "$SEPOL" \
    && ok "the policy module grants the entrypoint the transition needs" \
    || bad "rime_sdboot.te does not grant bootupd_t an entrypoint on init_exec_t"
grep -q 'entrypoint' "$SEPOL" \
    && ok "…and specifically the entrypoint permission" \
    || bad "rime_sdboot.te never mentions entrypoint"
# The module NAME must equal the .pp basename or checkmodule refuses outright.
grep -qx 'module rime_sdboot 1.0.0;' "$SEPOL" \
    && ok "the module name matches the filename checkmodule will write" \
    || bad "rime_sdboot.te's module name must be rime_sdboot to match the .pp basename"
grep -q 'semodule -N -i rime_sdboot.pp' "$REPO/Containerfile.core" \
    && ok "Containerfile.core installs the policy module" \
    || bad "Containerfile.core does not install rime_sdboot.pp"
grep -q 'semodule -l | grep -qx rime_sdboot' "$BASECF" \
    && ok "Containerfile.base asserts across the tier boundary that it is there" \
    || bad "Containerfile.base does not check the core tier still ships the policy module"

# /dev/liveupdate's own type (rime_liveupdate.cil): declared like Fedora's
# device types, with no rule of its own, and installed + verified in core.
LUOPOL="$REPO/files/system/selinux/rime_liveupdate.cil"
grep -q '^(typeattributeset device_node (liveupdate_device_t))$' "$LUOPOL" \
    && grep -q '^(filecon "/dev/liveupdate" char (system_u object_r liveupdate_device_t ((s0) (s0))))$' "$LUOPOL" \
    && ok "/dev/liveupdate is a device_node of its own type" \
    || bad "rime_liveupdate.cil does not label /dev/liveupdate as a device_node type"
grep -Eq '^\((allow|typetransition|typeattributeset (domain|file_type)|roleattributeset)' "$LUOPOL" \
    && bad "rime_liveupdate.cil grants something beyond the type and its label" \
    || ok "rime_liveupdate.cil adds no rule of its own"
grep -q 'semodule -N -i /usr/share/rime-os/selinux/rime_liveupdate.cil' "$REPO/Containerfile.core" \
    && grep -q 'python3 /usr/share/rime-os/selinux/verify-rime-liveupdate.py' "$REPO/Containerfile.core" \
    && ok "Containerfile.core installs and verifies rime_liveupdate" \
    || bad "Containerfile.core does not install and verify rime_liveupdate"

# ═════════════════════════════════════════════════════════════════════════════
sec "nothing shipped into the image touches a real boot path"
# AGENTS.md boot-path rule 1. The match is on EXECUTABLE lines only: both units
# and the health helper contain comments naming the commands they refuse to
# run, and a tripwire a comment can trip is a tripwire nobody can keep green.
DANGER='(bootctl[[:space:]]+(install|update)|bootupctl|grub2-install|grub2-mkconfig|efibootmgr[[:space:]]+-[cBo])'
mapfile -t shipped < <(find "$REPO/files" -type f \
    \( -path '*/libexec/*' -o -name '*.service' -o -name '*.timer' \) | sort)
(( ${#shipped[@]} > 0 )) || bad "found no shipped units or helpers to scan — the scan is vacuous"
hits=0
for f in "${shipped[@]}"; do
    if grep -nE "^[^#]*\b$DANGER" "$f" >/dev/null 2>&1; then
        bad "$f runs a boot-path command on an executable line"
        hits=$((hits + 1))
    fi
done
(( hits == 0 )) && ok "${#shipped[@]} shipped units/helpers contain no boot-path command"

# The tripwire's own two controls. Without these the check above could be
# reporting "clean" because the pattern never matches anything at all — a
# failure mode this project has hit with a check script that did not exist and
# exited 127.
printf '#!/bin/sh\n# this comment mentions bootctl install and bootupctl on purpose\necho hi\n' \
    > "$TMP/comment-only"
if grep -nE "^[^#]*\b$DANGER" "$TMP/comment-only" >/dev/null 2>&1; then
    bad "inverse control: a comment mentioning 'bootctl install' trips the tripwire (false red)"
else
    ok "inverse control: a comment mentioning 'bootctl install' does not trip the tripwire"
fi
printf '#!/bin/sh\nbootctl install --esp-path=/boot/efi\n' > "$TMP/really-does-it"
if grep -nE "^[^#]*\b$DANGER" "$TMP/really-does-it" >/dev/null 2>&1; then
    ok "forward control: a real 'bootctl install' line does trip the tripwire"
else
    bad "forward control: a real 'bootctl install' line does NOT trip the tripwire — the scan proves nothing"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the health gate's exit codes are the rollback mechanism"
# rime-boot-health calls `systemctl` unqualified, so a fake earlier in PATH is
# enough to drive every branch. This tests the shipped script, not a copy of
# its logic.
mkfake() {  # mkfake DIR ACTIVE_UNITS...
    local dir="$1"; shift
    mkdir -p "$dir"
    {
        printf '#!/usr/bin/env bash\n'
        printf 'ACTIVE="%s"\n' "$*"
        printf 'case "$1" in\n'
        printf '  get-default) echo graphical.target;;\n'
        # `cat` decides which units EXIST. dbus-broker exists so the script
        # picks a real bus name; greetd exists so the display-manager branch is
        # actually exercised rather than skipped.
        printf '  cat) case "$2" in dbus-broker.service|greetd.service|rimed.service|systemd-logind.service) echo "[Unit]";; *) exit 1;; esac;;\n'
        printf '  is-active) for u in $ACTIVE; do [[ "$u" == "$2" ]] && { echo active; exit 0; }; done; echo inactive; exit 3;;\n'
        printf '  *) exit 1;;\n'
        printf 'esac\n'
    } > "$dir/systemctl"
    chmod +x "$dir/systemctl"
}

ALL_GOOD="graphical.target rimed.service systemd-logind.service dbus-broker.service greetd.service"

# (a) No LoaderBootCountPath: a GRUB machine. Must succeed and do nothing.
mkdir -p "$TMP/efivars-grub" "$TMP/state-grub"
mkfake "$TMP/bin-a" "$ALL_GOOD"
rc=0
PATH="$TMP/bin-a:$PATH" RIME_BOOT_EFIVARS="$TMP/efivars-grub" RIME_BOOT_STATE="$TMP/state-grub" \
    "$HEALTH" check >"$TMP/out-a" 2>&1 || rc=$?
eq 0 "$rc" "check on a GRUB machine exits"
grep -q 'boot counting is not in effect' "$TMP/out-a" \
    && ok "and says why" || bad "did not explain why it did nothing"
[[ ! -e "$TMP/state-grub/last-health.json" ]] \
    && ok "and wrote no verdict (a GRUB boot has nothing to bless)" \
    || bad "wrote a verdict on a machine with no boot counter"

# (b) Counting in effect, everything healthy.
mkdir -p "$TMP/efivars-sd" "$TMP/state-good"
printf '\x07\x00\x00\x00' > "$TMP/efivars-sd/LoaderBootCountPath-$LOADER_GUID"
printf '\x07\x00\x00\x00r\0i\0m\0e\0-\0n\0e\0w\0.\0e\0f\0i\0' \
    > "$TMP/efivars-sd/LoaderEntrySelected-$LOADER_GUID"
mkfake "$TMP/bin-b" "$ALL_GOOD"
rc=0
PATH="$TMP/bin-b:$PATH" RIME_BOOT_EFIVARS="$TMP/efivars-sd" RIME_BOOT_STATE="$TMP/state-good" \
    "$HEALTH" check >"$TMP/out-b" 2>&1 || rc=$?
eq 0 "$rc" "check with a healthy system exits"
eq good "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["verdict"])' \
           "$TMP/state-good/last-health.json" 2>/dev/null || echo MISSING)" \
   "the recorded verdict"
eq 'rime-new.efi' "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["entry"])' \
                     "$TMP/state-good/last-health.json" 2>/dev/null || echo MISSING)" \
   "the recorded entry, decoded from the UTF-16 EFI variable"

# (c) Each critical unit, dropped one at a time. A gate that only notices when
# everything is down is not a gate. `-ge`-style leniency here would mean a
# machine with no desktop and no rimed getting blessed.
for down in graphical.target rimed.service systemd-logind.service dbus-broker.service greetd.service; do
    active="${ALL_GOOD/$down/}"
    mkfake "$TMP/bin-c" "$active"
    mkdir -p "$TMP/state-c"
    rc=0
    PATH="$TMP/bin-c:$PATH" RIME_BOOT_EFIVARS="$TMP/efivars-sd" RIME_BOOT_STATE="$TMP/state-c" \
        "$HEALTH" check >"$TMP/out-c" 2>&1 || rc=$?
    # Exactly 1, not merely non-zero. An early version of this suite passed
    # here with rc=126 — "cannot execute", because the script was not
    # executable in the repo — and reported the gate as working.
    if (( rc == 1 )); then
        ok "check fails with rc=1 when $down is inactive"
    else
        bad "check returned $rc with $down inactive (want 1; 126 means it never ran)"
    fi
    grep -q "UNHEALTHY: $down is not active" "$TMP/out-c" \
        && ok "  and names $down" \
        || bad "  but did not name $down: $(cat "$TMP/out-c")"
    eq bad "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["verdict"])' \
              "$TMP/state-c/last-health.json" 2>/dev/null || echo MISSING)" \
       "  the recorded verdict with $down down"
done

# ═════════════════════════════════════════════════════════════════════════════
sec "the rollback notice, and the missing-key trap it would otherwise hit"
# bootctl OMITS triesLeft for an entry with no counter, which is what a blessed
# deployment looks like. Code that reads it as `get("triesLeft", 0) == 0` marks
# every healthy deployment as failed and fires the notice on a machine that
# never rolled back. This fixture is the real shape, taken from
# `bootctl list --json` run against an ESP a VM had actually booted.
cat > "$TMP/entries-rolledback.json" <<'JSON'
[
  {"type":"type2","id":"rime-good.efi","path":"/boot/EFI/Linux/rime-good.efi",
   "title":"Rime OS","isDefault":true},
  {"type":"type2","id":"rime-new.efi","path":"/boot/EFI/Linux/rime-new+0-3.efi",
   "title":"Rime OS","triesLeft":0,"triesDone":3,"isDefault":false}
]
JSON
cat > "$TMP/entries-allgood.json" <<'JSON'
[
  {"type":"type2","id":"rime-good.efi","path":"/boot/EFI/Linux/rime-good.efi",
   "title":"Rime OS","isDefault":true},
  {"type":"type2","id":"rime-new.efi","path":"/boot/EFI/Linux/rime-new.efi",
   "title":"Rime OS","isDefault":false}
]
JSON
cat > "$TMP/entries-ontrial.json" <<'JSON'
[
  {"type":"type2","id":"rime-good.efi","path":"/boot/EFI/Linux/rime-good.efi",
   "title":"Rime OS","isDefault":true},
  {"type":"type2","id":"rime-new.efi","path":"/boot/EFI/Linux/rime-new+2-1.efi",
   "title":"Rime OS","triesLeft":2,"triesDone":1,"isDefault":false}
]
JSON

mkdir -p "$TMP/efivars-good" "$TMP/state-n"
printf '\x07\x00\x00\x00' > "$TMP/efivars-good/LoaderBootCountPath-$LOADER_GUID"
printf '\x07\x00\x00\x00r\0i\0m\0e\0-\0g\0o\0o\0d\0.\0e\0f\0i\0' \
    > "$TMP/efivars-good/LoaderEntrySelected-$LOADER_GUID"

run_notice() {
    RIME_BOOT_EFIVARS="$TMP/efivars-good" RIME_BOOT_STATE="$TMP/state-n" \
    RIME_BOOT_BOOTCTL_JSON="$1" "$HEALTH" notice >"$TMP/out-n" 2>&1
}

rc=0; run_notice "$TMP/entries-rolledback.json" || rc=$?
eq 0 "$rc" "notice with an exhausted entry exits"
if [[ -f "$TMP/state-n/rollback-notice.json" ]]; then
    ok "wrote a rollback notice"
    eq 'rime-new.efi' \
       "$(python3 -c 'import json,sys;d=json.load(open(sys.argv[1]));print(d["failedEntries"][0]["id"])' \
          "$TMP/state-n/rollback-notice.json")" "the failed entry it names"
    eq 1 "$(python3 -c 'import json,sys;print(len(json.load(open(sys.argv[1]))["failedEntries"]))' \
            "$TMP/state-n/rollback-notice.json")" "the exact number of failed entries"
    eq 'rime-good.efi' \
       "$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["runningEntry"])' \
          "$TMP/state-n/rollback-notice.json")" "the entry it says is running"
else
    bad "no rollback notice written for an exhausted entry"
fi

# THE TRAP. Two blessed entries, neither carrying triesLeft at all.
rc=0; run_notice "$TMP/entries-allgood.json" || rc=$?
eq 0 "$rc" "notice with only blessed entries exits"
[[ ! -e "$TMP/state-n/rollback-notice.json" ]] \
    && ok "a missing triesLeft key is NOT read as 0 — no notice, and the stale one was cleared" \
    || bad "an entry with no triesLeft key was treated as exhausted: $(cat "$TMP/state-n/rollback-notice.json")"

# An entry mid-trial is not a failure either.
rc=0; run_notice "$TMP/entries-ontrial.json" || rc=$?
eq 0 "$rc" "notice with an entry still on trial exits"
[[ ! -e "$TMP/state-n/rollback-notice.json" ]] \
    && ok "triesLeft=2 is not reported as a rollback" \
    || bad "an entry with tries remaining was reported as a rollback"

# Unreadable entries must fail closed. An empty list would be
# indistinguishable from "nothing failed", which is the answer that hides a
# rollback from the user.
rc=0
RIME_BOOT_EFIVARS="$TMP/efivars-good" RIME_BOOT_STATE="$TMP/state-n" \
RIME_BOOT_BOOTCTL_JSON="$TMP/does-not-exist.json" "$HEALTH" notice >"$TMP/out-n2" 2>&1 || rc=$?
if (( rc == 1 )); then
    ok "notice fails closed with rc=1 when the boot entries cannot be read"
else
    bad "notice returned $rc for unreadable boot entries (want 1; 126 means it never ran)"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the state files have one schema, written in one place and read in another"
# rime-boot-health WRITES these files and rimed/rime/src/boot.rs READS them.
# They are in different languages in different directories, so nothing but a
# parity check couples them; a renamed key would leave `rime boot status`
# silently reporting "no verdict recorded" on a machine that had one.
for key in verdict entry target checkedAt failures; do
    grep -q "\"$key\"" "$HEALTH" \
        && grep -q "\"$key\"" "$BOOTRS" \
        && ok "last-health.json key '$key' is written and read" \
        || bad "last-health.json key '$key' is not in both rime-boot-health and boot.rs"
done
for key in rolledBack runningEntry failedEntries noticedAt; do
    grep -q "\"$key\"" "$HEALTH" \
        && grep -q "\"$key\"" "$BOOTRS" \
        && ok "rollback-notice.json key '$key' is written and read" \
        || bad "rollback-notice.json key '$key' is not in both rime-boot-health and boot.rs"
done
# triesDone is read out of the notice by boot.rs and copied into it by the
# health script from bootctl's own field name.
grep -q 'triesDone' "$HEALTH" && grep -q 'triesDone' "$BOOTRS" \
    && ok "bootctl's triesDone field name is used consistently" \
    || bad "triesDone is not consistent between the writer and the reader"

# ═════════════════════════════════════════════════════════════════════════════
sec "the build-time scripts refuse to write a real boot path"
# AGENTS.md boot-path rules 1 and 2: a guest ESP is always an image file, never
# the host's. These refusals run before anything is created, so the assertion
# is safe on the machine executing it — which is the whole point.
B="$REPO/files/scripts/boot-v2"
for target in /boot/rime-test.img /boot/efi/rime-test.img /efi/rime-test.img /usr/rime-test.img; do
    rc=0
    "$B/rime-mkesp" --disk "$target" --uki "x:0:$B/lib.sh" >"$TMP/out-esp" 2>&1 || rc=$?
    if (( rc == 0 )); then
        bad "rime-mkesp accepted --disk $target"
    else
        grep -q 'refusing to author an ESP' "$TMP/out-esp" \
            && ok "rime-mkesp refuses --disk $target" \
            || bad "rime-mkesp failed for --disk $target but not because it refused: $(cat "$TMP/out-esp")"
    fi
    [[ -e "$target" ]] && bad "rime-mkesp created $target" || true
done
rc=0
"$B/rime-sb-keys" /boot/keys >"$TMP/out-keys" 2>&1 || rc=$?
(( rc != 0 )) && grep -q 'refusing to write key material' "$TMP/out-keys" \
    && ok "rime-sb-keys refuses an output directory under /boot" \
    || bad "rime-sb-keys did not refuse /boot: $(cat "$TMP/out-keys")"

# ═════════════════════════════════════════════════════════════════════════════
sec "rime-mkuki fails closed on the inputs a UKI cannot be guessed from"
# A synthetic bzImage: the x86 boot protocol's HdrS magic at 0x202 and a
# kernel_version pointer at 0x20e. Enough for rime-mkuki's version read, and
# hermetic — a runner has no Rime kernel and pulling rime-os-core to get one
# would be a multi-gigabyte download for a refusal test.
python3 - "$TMP/fake-vmlinuz" <<'PY'
import sys
buf = bytearray(0x2000)
buf[0x202:0x206] = b'HdrS'
ver = b'6.99.0-rime-test (fake) #1 SMP\x00'
off = 0x1000
buf[off:off + len(ver)] = ver
buf[0x20e:0x210] = (off - 0x200).to_bytes(2, 'little')
open(sys.argv[1], 'wb').write(bytes(buf))
PY
printf 'not-a-real-initramfs' | gzip > "$TMP/fake-initrd.img"

mkuki_fails() {  # mkuki_fails DESCRIPTION EXPECTED_SUBSTRING ARGS...
    local what="$1" expect="$2"; shift 2
    local rc=0
    "$B/rime-mkuki" "$@" >"$TMP/out-uki" 2>&1 || rc=$?
    if (( rc == 0 )); then
        bad "rime-mkuki accepted $what"
    elif grep -qF -- "$expect" "$TMP/out-uki"; then
        ok "rime-mkuki refuses $what"
    else
        bad "rime-mkuki failed on $what for the wrong reason: $(tail -3 "$TMP/out-uki")"
    fi
}

# No microcode and no --allow-no-ucode. §22 puts microcode inside the UKI, and
# a UKI silently built without it would look identical to one that has it.
mkuki_fails "an initramfs with no microcode and no --allow-no-ucode" \
    "no microcode" \
    --output "$TMP/x.efi" --kernel "$TMP/fake-vmlinuz" --initrd "$TMP/fake-initrd.img" \
    --cmdline "root=/dev/nowhere"

# No --cmdline. A UKI's command line is signed into the image and cannot be
# edited at boot, so defaulting it would ship a guess.
mkuki_fails "a build with no --cmdline" \
    "--cmdline is required" \
    --output "$TMP/x.efi" --kernel "$TMP/fake-vmlinuz" --initrd "$TMP/fake-initrd.img" \
    --allow-no-ucode

# Two kernels under --from-root. Picking the newest silently is how a UKI gets
# paired with the wrong out-of-tree modules.
mkdir -p "$TMP/root2/usr/lib/modules/1.0-a" "$TMP/root2/usr/lib/modules/2.0-b"
cp "$TMP/fake-vmlinuz" "$TMP/root2/usr/lib/modules/1.0-a/vmlinuz"
cp "$TMP/fake-vmlinuz" "$TMP/root2/usr/lib/modules/2.0-b/vmlinuz"
mkuki_fails "a root containing two kernels" \
    "expected exactly 1 kernel" \
    --output "$TMP/x.efi" --from-root "$TMP/root2" --cmdline "root=/dev/nowhere" --allow-no-ucode

# A directory name that disagrees with the kernel's own version string.
mkdir -p "$TMP/root-mismatch/usr/lib/modules/9.9.9-wrong"
cp "$TMP/fake-vmlinuz" "$TMP/root-mismatch/usr/lib/modules/9.9.9-wrong/vmlinuz"
cp "$TMP/fake-initrd.img" "$TMP/root-mismatch/usr/lib/modules/9.9.9-wrong/initramfs.img"
cp /usr/lib/os-release "$TMP/root-mismatch/usr/lib/os-release" 2>/dev/null \
    || printf 'ID=test\nNAME=Test\nVERSION_ID=1\n' > "$TMP/root-mismatch/usr/lib/os-release"
mkuki_fails "a modules directory whose name disagrees with the kernel" \
    "kernel version mismatch" \
    --output "$TMP/x.efi" --from-root "$TMP/root-mismatch" \
    --cmdline "root=/dev/nowhere" --allow-no-ucode

# ═════════════════════════════════════════════════════════════════════════════
sec "the VM harness cannot report green for work it did not do"
# Three ways the boot lab has already reported a pass it had not earned, all
# three found by running it rather than reading it, and all three checked here
# because none of them can be caught by the harness's own scenarios — the
# scenarios are the thing being mis-reported.
#
#   1. A run that aborted printed NO summary line at all (fixed with an EXIT
#      trap; the end-to-end case below is that trap).
#   2. A run whose every check was a COULD-NOT-RUN printed "0 passed, 0 failed"
#      and exited 0, which every caller reads as a pass of a run that never ran.
#   3. A control decided with `cmp`, which the lab image does not ship.
#
# These run on any machine with bash: lib.sh has no side effects on source
# beyond two path variables, so the counters can be driven directly.
BOOTV2_LIB="$REPO/files/scripts/boot-v2/lib.sh"
[[ -f "$BOOTV2_LIB" ]] || { echo "FATAL: missing $BOOTV2_LIB" >&2; exit 1; }

# summary_case DESCRIPTION EXPECTED_RC SETUP_CODE — drives bootv2_summary in a
# subshell and reports its exit status and its line.
summary_case() {
    local what="$1" want="$2" setup="$3" rc=0 out
    out="$(bash -c '
        set -euo pipefail
        . "$1" >/dev/null 2>&1
        # Fixture chatter is suppressed deliberately: ok and bad print
        # "  ok  " and "  FAIL " lines, and a fixture FAIL scrolling past in
        # the output of this file would read as this file failing.
        eval "$2" 2>/dev/null
        bootv2_summary "case" 2>&1
    ' _ "$BOOTV2_LIB" "$setup")" || rc=$?
    SUMMARY_OUT="$out"
    eq "$want" "$rc" "$what: exit status"
}

summary_case "a run with nothing in it at all" 1 ':'
grep -q 'asserted nothing' <<<"$SUMMARY_OUT" \
    && ok "an empty run says so in words, not only in its exit status" \
    || bad "an empty run's report does not say it asserted nothing: $SUMMARY_OUT"

# THE CASE THAT WAS GREEN. Every check a could-not-run, nothing proved.
summary_case "a run whose every check was COULD-NOT-RUN" 1 \
    'cannot "no S3 on this kernel"; cannot "no second firmware build"'
grep -q 'COULD-NOT-RUN' <<<"$SUMMARY_OUT" \
    && ok "the all-could-not-run report still names the reasons it could not run" \
    || bad "the all-could-not-run report lost its reasons: $SUMMARY_OUT"

# And the other direction, which is what stops the fix above from turning
# `cannot` into `bad` — a run that proved something and ALSO could not do part
# of it is still a pass, because that is the distinction cannot() exists for.
summary_case "a run that proved something and could not do the rest" 0 \
    'ok "the volume unlocked"; cannot "no S3 on this kernel"'
grep -q 'COULD-NOT-RUN' <<<"$SUMMARY_OUT" \
    && ok "a passing run with a could-not-run still says COULD-NOT-RUN" \
    || bad "a passing run swallowed its could-not-run: $SUMMARY_OUT"
summary_case "a run with one passing check" 0 'ok "the volume unlocked"'
summary_case "a run with one failing check" 1 'bad "the volume did not unlock"'

# End to end, through the real entry point: run-scenarios outside the lab image
# dies on the missing kver file. EXACTLY ONE summary line, and exit 1.
rc=0
bash "$REPO/files/scripts/boot-v2/run-scenarios" --work "$TMP/harness" prereq \
    >"$TMP/harness.log" 2>&1 || rc=$?
eq 1 "$rc" "run-scenarios that cannot start exits 1"
n="$(grep -c '^== boot-v2 VM harness:' "$TMP/harness.log" || true)"
eq 1 "$n" "an aborted run prints exactly one summary line"
grep -q 'INCOMPLETE' "$TMP/harness.log" \
    && ok "the aborted run calls itself INCOMPLETE" \
    || bad "the aborted run printed a verdict that does not say it stopped early"

# A KILLED RUN MUST NOT REPORT A PASS.
#
# The EXIT trap alone does not cover this, and the comment in run-scenarios
# that once said it did cost a round. bash DOES run the EXIT trap on an
# untrapped SIGTERM, but `$?` reads 0 inside it, so the `rc != 0` INCOMPLETE
# branch never fires. MEASURED 2026-09-14: two detached scenario containers
# were SIGTERM'd part-way through their second boot and both logs ended
# `== boot-v2 VM harness: 6 passed, 0 failed ==`, which the next round was told
# to collect as a result.
#
# The real trap block is lifted out of run-scenarios rather than restated, so
# this test fails if someone deletes the signal traps from the script itself.
sed -n '/^BOOTV2_SUMMARY_PRINTED=0$/,/^trap bootv2_report EXIT$/p' \
    "$REPO/files/scripts/boot-v2/run-scenarios" >"$TMP/trapblock.sh"
[[ -s "$TMP/trapblock.sh" ]] \
    && ok "the trap block was found in run-scenarios to test against" \
    || bad "could not extract the trap block from run-scenarios"
{ printf '%s\n' 'set -uo pipefail' ". \"$BOOTV2_LIB\" >/dev/null 2>&1" \
      'ok "a check that ran before the kill"'
  cat "$TMP/trapblock.sh"
  printf '%s\n' 'sleep 300'
} >"$TMP/killme.sh"
bash "$TMP/killme.sh" >"$TMP/killed.log" 2>&1 &
killme=$!
# Wait for the fixture to reach its sleep, then signal it AND its child — a
# container kill hits the whole cgroup, and bash defers a trapped signal until
# the foreground child is gone.
for _ in $(seq 1 50); do
    child="$(pgrep -P "$killme" 2>/dev/null | head -1)" && [[ -n "$child" ]] && break
    sleep 0.1
done
kill -TERM "$killme" 2>/dev/null || true
[[ -n "${child:-}" ]] && kill -TERM "$child" 2>/dev/null
krc=0; wait "$killme" || krc=$?
eq 143 "$krc" "a SIGTERM'd run exits 143"
grep -q 'KILLED by SIGTERM' "$TMP/killed.log" \
    && ok "a SIGTERM'd run says it was killed" \
    || bad "a SIGTERM'd run did not name the signal: $(tail -2 "$TMP/killed.log" | tr '\n' ' ')"
grep -qE '^== boot-v2 VM harness: [0-9]+ passed, 0 failed ==' "$TMP/killed.log" \
    && bad "a SIGTERM'd run reported a clean pass — the killed-run defect is back" \
    || ok "a SIGTERM'd run does not report 0 failed"

# The lab image ships no diffutils (bootlab/Containerfile installs neither cmp
# nor diff and asserts neither present). `if cmp -s A B; then ...` with no cmp
# exits 127, which takes the same branch as "the files differ" — so a control
# written that way passes whatever the files hold. Both halves are checked, so
# the day diffutils IS installed this stops objecting rather than lying.
if grep -qw diffutils "$REPO/bootlab/Containerfile"; then
    ok "the boot lab image installs diffutils, so cmp/diff are available to it"
else
    uses="$(grep -nE '(^|[;&|(]|\b(if|then|else|elif|do|while|until|!)[[:space:]]+)[[:space:]]*(cmp|diff)[[:space:]]' \
        "$REPO"/files/scripts/boot-v2/* 2>/dev/null | grep -v '^[^:]*:[0-9]*:[[:space:]]*#' || true)"
    [[ -z "$uses" ]] \
        && ok "no boot-v2 script decides anything with cmp or diff, which the lab image does not ship" \
        || bad "boot-v2 scripts call cmp/diff, absent from the lab image (exit 127 reads as 'they differ'): $uses"
    # Both controls, because a scan that matches nothing anywhere reports the
    # same clean as a scan over clean files — the exact defect this section is
    # about, applied to this section.
    DIFFUTILS_RE='(^|[;&|(]|\b(if|then|else|elif|do|while|until|!)[[:space:]]+)[[:space:]]*(cmp|diff)[[:space:]]'
    printf '#!/bin/bash\nif cmp -s "$a" "$b"; then bad x; fi\n' > "$TMP/uses-cmp"
    grep -qE "$DIFFUTILS_RE" "$TMP/uses-cmp" \
        && ok "forward control: a real 'if cmp -s' line is found by the scan" \
        || bad "forward control: the cmp scan does not match 'if cmp -s' — it proves nothing"
    printf '#!/bin/bash\n# the two varstores differ, so do not cmp them\necho hi\n' > "$TMP/mentions-cmp"
    if grep -nE "$DIFFUTILS_RE" "$TMP/mentions-cmp" | grep -v '^[0-9]*:[[:space:]]*#' | grep -q .; then
        bad "inverse control: a comment mentioning cmp trips the scan (false red)"
    else
        ok "inverse control: a comment mentioning cmp does not trip the scan"
    fi
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "every scenario the harness defines can be asked for by name"
# A SCENARIO NOBODY CAN RUN IS A GATE THAT INSPECTS NOTHING, and it fails
# silently in the direction that reads as success: `run-scenarios` with no
# arguments runs ALL and reports green, and the scenario that was written and
# never registered is simply not in it. The two lists are compared as SETS in
# both directions, so the failure mode "registered under a name with no
# function" is caught too — that one dies at dispatch with "no such scenario",
# which at least is loud, but only if somebody asks for it.
RUNSC="$REPO/files/scripts/boot-v2/run-scenarios"
defined="$(grep -oE '^scenario_[a-z0-9_]+\(\)' "$RUNSC" | sed 's/()$//; s/^scenario_//; s/_/-/g' | sort)"
listed="$(bash "$RUNSC" --list | sort)"
[[ -n "$defined" ]] \
    && ok "run-scenarios defines $(wc -l <<<"$defined") scenario functions" \
    || bad "no scenario_* functions found in run-scenarios — the comparison below proves nothing"
[[ -n "$listed" ]] \
    && ok "run-scenarios --list names $(wc -l <<<"$listed") scenarios" \
    || bad "run-scenarios --list printed nothing"
unreachable="$(comm -23 <(printf '%s\n' "$defined") <(printf '%s\n' "$listed") | tr '\n' ' ')"
[[ -z "${unreachable// }" ]] \
    && ok "no scenario is defined and unreachable by name" \
    || bad "defined but not in --list, so a default run skips them silently: $unreachable"
phantom="$(comm -13 <(printf '%s\n' "$defined") <(printf '%s\n' "$listed") | tr '\n' ' ')"
[[ -z "${phantom// }" ]] \
    && ok "no scenario is listed without a function behind it" \
    || bad "listed but not defined, so asking for them dies at dispatch: $phantom"

sec "the two registers a firmware change moves, and the guest that reports them"
PROBE="$REPO/files/scripts/boot-v2/guest-luks-probe.sh"
# PCR 0 is the firmware CODE measurement and PCR 7 the Secure Boot POLICY
# register. luks-firmware-code seals a control volume to the value the guest
# prints for PCR 0; a probe that stopped printing it would make that control
# bind to whatever the TPM happened to hold, which is the failure the PCR 7
# control's own comment was written about.
for reg in 0 7 11; do
    grep -q "/sys/class/tpm/tpm0/pcr-sha256/$reg" "$PROBE" \
        && ok "the guest probe reads PCR $reg out of sysfs" \
        || bad "the guest probe no longer reads PCR $reg — a control sealed to it would bind to nothing"
done
# The recovery path must read the marker back. "A mapper appeared" says a
# keyslot was satisfied; only the marker says the user got their disk.
sed -n '/recovery-unlock=SUCCESS/,/^        else$/p' "$PROBE" > "$TMP/recovbranch.sh"
grep -q 'recovery-marker=found' "$TMP/recovbranch.sh" \
    && ok "the recovery branch reads the plaintext marker back" \
    || bad "the recovery branch reports SUCCESS without reading the marker: a keyslot opened is not a disk recovered"
# MARKER is set unconditionally, not inside the branch that was NOT taken. A
# variable that only exists on the TPM path expands to empty in the recovery
# path, and `case "" in "$MARKER"*)` matches everything — a recovery-marker
# check that can only say "found".
awk '/^MARKER=/{print NR; exit}' "$PROBE" > "$TMP/markerline"
awk '/tpm-unlock=SUCCESS/{print NR; exit}' "$PROBE" > "$TMP/successline"
mline="$(cat "$TMP/markerline")"; sline="$(cat "$TMP/successline")"
[[ -n "$mline" && -n "$sline" ]] && (( mline < sline )) \
    && ok "MARKER is defined before either unlock path, so the recovery check can fail" \
    || bad "MARKER is defined at line '${mline:-none}', not before the unlock paths (line '${sline:-none}'): the recovery-marker check would match an empty string and always pass"

sec "the alternate firmware is identified by its contents, not by its name"
# This unit's own scratch directory holds the trap: a build with Secure Boot
# and SMM compiled out, saved as `OVMF_CODE_4M.secboot.fd`. ovmf_build_id is
# what stops luks-firmware-code reading "Secure Boot was turned off" as "the
# firmware code changed", so it is tested against contents rather than trusted.
# lib.sh is driven through `bash -c` with its path as $1, the same way the
# summary cases above do it: a `.` on a variable is unfollowable by ShellCheck,
# and a directive would only silence the objection.
fwid() { bash -c '. "$1" >/dev/null 2>&1; ovmf_build_id "$2"' _ "$BOOTV2_LIB" "$1"; }
printf 'junk\x00/builddir/build/BUILD/edk2-20250812-build/edk2-d46aa46c8361/Build/OvmfX64/x\x00junk' > "$TMP/fw-a.fd"
printf 'junk\x00/builddir/build/BUILD/edk2-20260812-build/edk2-2970e5699ba6/Build/OvmfX64/x\x00junk' > "$TMP/fw-b.fd"
printf 'no revision string in here at all, edk2- and nothing after it' > "$TMP/fw-none.fd"
eq "edk2-d46aa46c8361" "$(fwid "$TMP/fw-a.fd")" "ovmf_build_id reads the revision out of a binary"
eq "edk2-2970e5699ba6" "$(fwid "$TMP/fw-b.fd")" "ovmf_build_id reads a different binary's revision"
eq "" "$(fwid "$TMP/fw-none.fd")" "ovmf_build_id says nothing rather than guessing"
eq "" "$(fwid "$TMP/does-not-exist.fd")" "ovmf_build_id on a missing file is empty, not an error string"
# The inverse control: two files whose NAMES are identical and whose contents
# are not must not be reported as the same build. This is the whole point.
cp "$TMP/fw-a.fd" "$TMP/same-name-a"; cp "$TMP/fw-b.fd" "$TMP/same-name-b"
[[ "$(fwid "$TMP/same-name-a")" != "$(fwid "$TMP/same-name-b")" ]] \
    && ok "two identically shaped files with different contents get different revisions" \
    || bad "ovmf_build_id cannot tell two different builds apart"

altrc() { bash -c '
    . "$1" >/dev/null 2>&1
    if [[ "$2" == UNSET ]]; then unset RIME_BOOTLAB_FW_ALT; else export RIME_BOOTLAB_FW_ALT="$3"; fi
    ovmf_code_alt >/dev/null 2>&1; printf "%s" "$?"' _ "$BOOTV2_LIB" "${1:+SET}${1-UNSET}" "${1-}"; }
altout() { bash -c '. "$1" >/dev/null 2>&1; RIME_BOOTLAB_FW_ALT="$2" ovmf_code_alt' _ "$BOOTV2_LIB" "$1"; }
# Three ways to not have a second firmware, and the scenario turns each into a
# different could-not-run. One shared failure code would make "nobody asked for
# this experiment" and "the path is wrong" the same sentence in the log.
eq 1 "$(altrc)"                   "ovmf_code_alt with the variable unset"
eq 1 "$(altrc '')"                "ovmf_code_alt with the variable empty"
eq 2 "$(altrc "$TMP/nope.fd")"    "ovmf_code_alt with a path that is not a file"
printf 'raw firmware' > "$TMP/alt-raw.fd"
eq 0 "$(altrc "$TMP/alt-raw.fd")" "ovmf_code_alt with a raw .fd"
eq "$TMP/alt-raw.fd" "$(altout "$TMP/alt-raw.fd")" "ovmf_code_alt hands a raw .fd straight back"

sec "the no-TPM scenario asserts that the guest came back, not only what it said"
# A guest that hangs waiting for a TPM that will never answer has stranded the
# user as thoroughly as one that refuses with no fallback. vm_boot's timeout
# kill is qemu rc=137, and the serial log written before the hang would still
# carry every string the scenario greps for — so the exit status has to be an
# assertion. Extracted from the scenario body rather than restated, so deleting
# the check fails this test.
sed -n '/^scenario_luks_no_tpm()/,/^}$/p' "$RUNSC" > "$TMP/notpm.sh"
[[ -s "$TMP/notpm.sh" ]] \
    && ok "scenario_luks_no_tpm was found to test against" \
    || bad "could not extract scenario_luks_no_tpm from run-scenarios"
grep -q 'luks_boot notpm --no-tpm' "$TMP/notpm.sh" \
    && ok "the second boot attaches no TPM device" \
    || bad "the no-TPM scenario no longer boots without a TPM, so both arms are the same guest"
grep -qE 'assert_eq 0 "\$rc" "with no TPM' "$TMP/notpm.sh" \
    && ok "the no-TPM boot asserts a clean poweroff, so a hang cannot pass" \
    || bad "the no-TPM boot does not assert its exit status: a guest killed at the timeout would pass every remaining check"
for want in 'tpm-device=absent' 'tpm-unlock=REFUSED' 'recovery-unlock=SUCCESS' 'recovery-marker=found'; do
    grep -q "$want" "$TMP/notpm.sh" \
        && ok "the no-TPM scenario asserts $want" \
        || bad "the no-TPM scenario no longer asserts $want"
done
# And the positive arm, without which none of the above has a control.
grep -q 'plaintext-marker=written' "$TMP/notpm.sh" \
    && ok "the with-TPM arm writes the marker the recovery arm reads back" \
    || bad "the with-TPM arm no longer writes the marker, so recovery-marker=found proves nothing about the data"

sec "L-003: the shipped enrolment path cannot bind what this program has ruled out"
# These run on EVERY pull request, with no lab, no swtpm and no qemu. The boot
# lab proves the BEHAVIOUR of files/system/libexec/rime-luks-enroll; this proves
# the properties that must hold in the source whether the lab ran or not — and
# the lab is path-filtered, so a PR that edits only this script would otherwise
# be gated by nothing at all.
ENROLL="$REPO/files/system/libexec/rime-luks-enroll"
[[ -f "$ENROLL" ]] \
    && ok "the shipped enrolment script exists" \
    || bad "no script at $ENROLL — every check below is vacuous"
[[ -x "$ENROLL" ]] \
    && ok "the shipped enrolment script is executable in the repo" \
    || bad "$ENROLL is not executable in the repo"
grep -q 'COPY --chmod=0755 files/system/libexec/rime-luks-enroll' "$BASECF" \
    && ok "Containerfile.base copies it into the image" \
    || bad "Containerfile.base does not ship rime-luks-enroll, so nothing on a machine can call it"

# ── the defect this whole item exists to prevent ───────────────────────────
# `systemd-cryptenroll --tpm2-device=auto` with no PCR selection exits 0 and
# seals to the TPM storage key and nothing else. MEASURED on systemd 258.10:
# tpm2-pcrs [], no tpm2-pcr-bank field, policy hash 32 zero bytes. The match is
# on EXECUTABLE lines only, because the script explains that defect at length
# and a tripwire a comment can trip is a tripwire nobody can keep green.
BARE_DEVICE='^[^#]*--tpm2-device='
BARE_OK='POLICY_ARGS|tpm2-pcrs|--tpm2-device=PATH|unlock-tpm2-device'
if grep -nE "$BARE_DEVICE" "$ENROLL" | grep -vE "$BARE_OK" >/dev/null 2>&1; then
    bad "an enrolment names a TPM2 device with no PCR selection beside it: $(grep -nE "$BARE_DEVICE" "$ENROLL" | grep -vE "$BARE_OK" | head -1)"
else
    ok "no executable line names a TPM2 device without a PCR selection"
fi
# Both controls on that tripwire, because a pattern that matches nothing at all
# reports exactly the same "clean".
printf '#!/bin/sh\n# a comment naming --tpm2-device=auto with no PCRs, on purpose\necho hi\n' \
    > "$TMP/enroll-comment-only"
if grep -nE "$BARE_DEVICE" "$TMP/enroll-comment-only" | grep -vE "$BARE_OK" >/dev/null 2>&1; then
    bad "inverse control: a comment naming --tpm2-device= trips the tripwire (false red)"
else
    ok "inverse control: a comment naming --tpm2-device= does not trip it"
fi
printf '#!/bin/sh\nsystemd-cryptenroll --tpm2-device=auto /dev/sda2\n' > "$TMP/enroll-bare"
if grep -nE "$BARE_DEVICE" "$TMP/enroll-bare" | grep -vE "$BARE_OK" >/dev/null 2>&1; then
    ok "positive control: a real bare enrolment DOES trip the tripwire"
else
    bad "positive control: a bare --tpm2-device=auto line did not trip the tripwire — it can never fire"
fi

# ── the registers this program has ruled out, and why ──────────────────────
#   PCR 0  moves on a firmware update and would strand a user on a BIOS update.
#   PCR 11 is 64 zeros on a machine with no sd-stub, and `tpm2_pcrextend 11`
#          succeeds from plain root while `tpm2_pcrreset 11` answers "bad
#          locality" — so binding to it there is a local denial of service.
# PCR 11 is allowed ONLY through the signed-policy probe, which requires a
# non-zero PCR 11 and an sd-stub boot before it will choose it. That is the
# seam the bootloader pivot needs, so the check names the guard rather than the
# register.
if grep -nE '^[^#]*--tpm2-(public-key-)?pcrs=[^ ]*\b0\b' "$ENROLL" >/dev/null 2>&1; then
    bad "the enrolment script binds PCR 0: $(grep -nE '^[^#]*--tpm2-(public-key-)?pcrs=[^ ]*\b0\b' "$ENROLL" | head -1)"
else
    ok "no executable line binds PCR 0"
fi
if grep -nE '^[^#]*--tpm2-public-key-pcrs=' "$ENROLL" >/dev/null 2>&1; then
    # It may only appear inside the probe that first checks PCR 11 is non-zero.
    sed -n '/^probe_signed_pcr11()/,/^}$/p' "$ENROLL" > "$TMP/enroll-p11.sh"
    if grep -q -- '--tpm2-public-key-pcrs=' "$TMP/enroll-p11.sh" \
       && grep -q 'pcr-sha256/11' "$TMP/enroll-p11.sh" \
       && grep -q 'StubInfo' "$TMP/enroll-p11.sh"; then
        ok "a PCR 11 binding exists only behind the sd-stub and non-zero-PCR-11 probes"
    else
        bad "a PCR 11 binding is reachable without proving sd-stub booted and PCR 11 was extended"
    fi
else
    ok "no PCR 11 binding is present at all"
fi

# ── no PIN by default ───────────────────────────────────────────────────────
# On real Intel PTT, MAX_AUTH_FAIL is 32, lockout 7200 s, recovery 86400 s, and
# a successful authorisation does NOT clear the counter. A default PIN risks
# locking a user out of their own TPM for a day, and on a dual-boot machine
# another OS holds lockoutAuth so Rime cannot clear it.
grep -qE '^WITH_PIN=0|WITH_PIN=0 ' "$ENROLL" \
    && ok "the PIN is off unless asked for" \
    || bad "the enrolment script does not default WITH_PIN to 0"

# ── the recovery key is enrolled BEFORE the TPM, not after ─────────────────
# Order, not presence. A script that enrolled the TPM first and the recovery key
# second would pass every "is there a recovery key" check while leaving a window
# in which the volume has a TPM binding and no way back — which is exactly the
# state that makes a firmware update a data-loss event.
rline="$(awk '/CRYPTENROLL" --recovery-key/{print NR; exit}' "$ENROLL")"
tline="$(awk '/enroll_args=\(--tpm2-device=/{print NR; exit}' "$ENROLL")"
if [[ -n "$rline" && -n "$tline" ]] && (( rline < tline )); then
    ok "the recovery key is enrolled at line $rline, before the TPM slot at line $tline"
else
    bad "the recovery key is enrolled at line '${rline:-none}' and the TPM slot at line '${tline:-none}': a TPM-only window exists"
fi

# ── the four scenarios are in the DEFAULT set, not behind a name ───────────
# The defined-vs-listed check above passes for a scenario in either array, so it
# cannot see a scenario moved from ALL into STAGED. STAGED scenarios are skipped
# by a default run, which is how a gate stops running while still being listed.
allblock="$(sed -n '/^ALL=(/,/)$/p' "$RUNSC")"
for s in enroll-sb-on enroll-sb-off enroll-no-tpm enroll-bare-policy; do
    grep -qw -- "$s" <<<"$allblock" \
        && ok "$s runs in a default boot-lab run" \
        || bad "$s is not in ALL, so a default run skips it and still reports success"
done

# ── the lab is actually triggered by a change to the shipped script ────────
# boot-v2.yml is path-filtered and a skipped job counts as success, so a path
# filter that does not name this file means the four scenarios never run on the
# PR that breaks them.
WF="$REPO/.github/workflows/boot-v2.yml"
grep -q 'files/system/libexec/rime-luks-enroll' "$WF" \
    && ok "boot-v2.yml runs the lab when the enrolment script changes" \
    || bad "boot-v2.yml has no path filter for rime-luks-enroll: the lab would be skipped, and a skipped job passes"

# ═════════════════════════════════════════════════════════════════════════════
if (( WITH_BINARY )); then
sec "rime boot status reports the state, and does not invent the parts it cannot see"
RIME_BIN="${RIME_BIN:-$REPO/rimed/target/debug/rime}"
# Dies rather than skipping. A skipped check counts as success.
[[ -x "$RIME_BIN" ]] || { echo "FATAL: no rime binary at $RIME_BIN (build it first)" >&2; exit 1; }

# (a) A GRUB machine — the state every published Rime image is in.
G="$TMP/fx-grub"
mkdir -p "$G/proc" "$G/sys/firmware/efi/efivars" "$G/var/lib/rime/boot"
printf 'BOOT_IMAGE=/ostree/default-abc/vmlinuz ostree=/ostree/boot.1/default/abc/0 root=UUID=x\n' \
    > "$G/proc/cmdline"
printf '\x06\x00\x00\x00\x01' > "$G/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
out="$(RIME_BOOT_ROOT="$G" "$RIME_BIN" boot status --json)"
# A dotted path into the report. Entry ids contain a literal '.' (they are
# filenames, "rime-good.efi"), so the separator is '/' and never '.' — an
# earlier version split on '.' and every entry-level assertion silently read
# `null`, which the exact-value checks caught and a truthiness check would not
# have.
j() { python3 -c 'import json,sys;d=json.load(sys.stdin)
for k in sys.argv[1].split("/"):
    d = d.get(k) if isinstance(d, dict) else None
print(json.dumps(d))' "$1" <<<"$out"; }
eq '"grub"'  "$(j bootloader)"             "GRUB fixture: bootloader"
eq 'false'   "$(j bootCounting/inEffect)"  "GRUB fixture: boot counting"
eq 'false'   "$(j bootedFromUki)"          "GRUB fixture: booted from a UKI"
eq 'true'    "$(j secureBoot/enabled)"     "GRUB fixture: Secure Boot"
eq 'null'    "$(j health)"                 "GRUB fixture: health verdict (never ran)"
eq 'null'    "$(j bootCounting/entries)"   "GRUB fixture: entries"
# Unavailable must come with a reason. A null with no reason is the shape that
# reads as "nothing failed".
[[ "$(j bootCounting/entriesUnavailable)" != null ]] \
    && ok "GRUB fixture: the entries are unavailable WITH a reason" \
    || bad "GRUB fixture: entries are null and no reason is given"

# (b) A systemd-boot machine mid-rollback. The bootctl document is the one
# `bootctl list --json` produced from an ESP a VM had actually booted four
# times, so the field shapes are real rather than invented.
S="$TMP/fx-sdboot"
mkdir -p "$S/proc" "$S/sys/firmware/efi/efivars" "$S/sys/class/tpm/tpm0" \
         "$S/run/systemd" "$S/var/lib/rime/boot"
E="$S/sys/firmware/efi/efivars"
printf 'root=UUID=x rw\n' > "$S/proc/cmdline"
printf '\x07\x00\x00\x00s\0y\0s\0t\0e\0m\0d\0-\0b\0o\0o\0t\0 \x002\x005\x008\0' \
    > "$E/LoaderInfo-$LOADER_GUID"
printf '\x07\x00\x00\x00s\0y\0s\0t\0e\0m\0d\0-\0s\0t\0u\0b\0' > "$E/StubInfo-$LOADER_GUID"
printf '\x07\x00\x00\x00\\\0E\0F\0I\0' > "$E/LoaderBootCountPath-$LOADER_GUID"
printf '\x07\x00\x00\x00r\0i\0m\0e\0-\0g\0o\0o\0d\0.\0e\0f\0i\0' \
    > "$E/LoaderEntrySelected-$LOADER_GUID"
printf '\x06\x00\x00\x00\x01' > "$E/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"
: > "$S/run/systemd/tpm2-pcr-signature.json"
cp "$TMP/entries-rolledback.json" "$S/bootctl-list.json"
cp "$TMP/state-good/last-health.json" "$S/var/lib/rime/boot/last-health.json"
RIME_BOOT_EFIVARS="$E" RIME_BOOT_STATE="$S/var/lib/rime/boot" \
    RIME_BOOT_BOOTCTL_JSON="$TMP/entries-rolledback.json" "$HEALTH" notice >/dev/null 2>&1

out="$(RIME_BOOT_ROOT="$S" "$RIME_BIN" boot status --json)"
eq '"systemd-boot"' "$(j bootloader)"            "sd-boot fixture: bootloader"
eq 'true'           "$(j bootCounting/inEffect)" "sd-boot fixture: boot counting"
eq 'true'           "$(j bootedFromUki)"         "sd-boot fixture: booted from a UKI"
eq 'true'           "$(j measuredBoot/tpmPresent)"   "sd-boot fixture: TPM"
eq 'true'           "$(j measuredBoot/pcrSignature)" "sd-boot fixture: signed PCR policy"
eq '"rime-good.efi"' "$(j bootCounting/selectedEntry)" "sd-boot fixture: selected entry"
eq 'null' "$(j bootCounting/entriesUnavailable)" "sd-boot fixture: no unavailability reason"
eq 2 "$(python3 -c 'import json,sys;print(len(json.load(sys.stdin)["bootCounting"]["entries"]))' <<<"$out")" \
   "sd-boot fixture: the exact number of entries"
eq 'true'  "$(j bootCounting/entries/rime-good.efi/blessed)" "sd-boot fixture: rime-good is blessed"
eq 'false' "$(j bootCounting/entries/rime-good.efi/exhausted)" "sd-boot fixture: rime-good is not exhausted"
eq 'true'  "$(j bootCounting/entries/rime-new.efi/exhausted)" "sd-boot fixture: rime-new is exhausted"
eq 'false' "$(j bootCounting/entries/rime-new.efi/blessed)"   "sd-boot fixture: rime-new is not blessed"
eq '"good"' "$(j health/verdict)" "sd-boot fixture: the health verdict is read back"
eq 'true'  "$(j rollbackNotice/rolledBack)" "sd-boot fixture: the rollback notice is surfaced"

# And the human report must actually say the words a user needs. A JSON-only
# assertion would pass with an empty text report.
text="$(RIME_BOOT_ROOT="$S" "$RIME_BIN" boot status)"
grep -q 'rolled back automatically' <<<"$text" \
    && ok "the human report announces the rollback" \
    || bad "the human report does not mention the rollback: $text"
grep -q 'OUT OF TRIES' <<<"$text" \
    && ok "the human report marks the exhausted entry" \
    || bad "the human report does not mark the exhausted entry"
textg="$(RIME_BOOT_ROOT="$G" "$RIME_BIN" boot status)"
# The wording moved when the in-place migration landed: a machine on GRUB is
# no longer "the default for every published image", it is a machine that has
# not migrated yet. Both halves are asserted — that it is not reported as a
# fault, and that it names the command that migrates it — because a report
# that says neither leaves the user with nothing to do.
grep -q 'Rime is moving to systemd-boot' <<<"$textg" \
    && ok "on a GRUB machine the report says where Rime is going" \
    || bad "the GRUB report does not mention the migration: $textg"
grep -q 'not a fault' <<<"$textg" \
    && ok "and that being on GRUB today is not a fault" \
    || bad "the GRUB report reads like a fault: $textg"
grep -q 'rime-boot-migrate precheck' <<<"$textg" \
    && ok "and names the command that says why a machine cannot migrate" \
    || bad "the GRUB report does not say how to find out why: $textg"

# Read-only means read-only. Nothing under the fixture root may change.
before="$(find "$G" -type f -printf '%p %s\n' | sort | sha256sum)"
RIME_BOOT_ROOT="$G" "$RIME_BIN" boot status --json >/dev/null
RIME_BOOT_ROOT="$G" "$RIME_BIN" boot status >/dev/null
after="$(find "$G" -type f -printf '%p %s\n' | sort | sha256sum)"
eq "$before" "$after" "rime boot status wrote nothing"
fi

printf '\n== test-boot-v2: %d passed, %d failed ==\n' "$PASS" "$FAIL"
(( FAIL == 0 ))
