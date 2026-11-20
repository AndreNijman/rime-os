#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-crash.sh — the kernel-crash summary and its one-time notice.
#
#  Drives files/system/libexec/rime-crash-record against a fake systemd-pstore
#  archive (records shaped like the ones nix/tests/kernel-panic.nix collects
#  from a real panic on OVMF), and files/system/libexec/rime-crash-notice
#  against a stub notify-send. No root, no EFI, no session bus.
#
#  The failures it exists to catch:
#    1. the summary naming the flood instead of the panic (the L16's screen
#       did exactly that: 636,000 `C2H content:` lines and nothing else);
#    2. a driver blamed that the backtrace does not name, or none named when
#       it does;
#    3. a record from an EARLIER boot announced as this restart, the same
#       crash announced at every login, or a crash never announced (two before
#       a login, a failed notification, a run cut off, a full disk);
#    4. the kernel arguments drifting between bootc (kargs.d) and NixOS
#       (crash.nix reads the same file), and the units drifting in shape.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RECORD="$REPO/files/system/libexec/rime-crash-record"
NOTICE="$REPO/files/system/libexec/rime-crash-notice"
KARGS="$REPO/files/system/kargs.d/30-rime-crash.toml"
REC_UNIT="$REPO/files/system/units/rime-crash-record.service"
SVC="$REPO/files/system/units/rime-crash-notice.service"
TIMER="$REPO/files/system/units/rime-crash-notice.timer"
NIXMOD="$REPO/nix/modules/crash.nix"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }

for f in "$RECORD" "$NOTICE" "$KARGS" "$REC_UNIT" "$SVC" "$TIMER" "$NIXMOD"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/state" "$WORK/home"

NOW=$(date +%s)
BOOT=$((NOW - 120))     # this boot started two minutes ago

# A record directory as systemd-pstore 262 leaves it (measured in
# nix/tests/kernel-panic.nix): /var/lib/systemd/pstore/<seconds>/001/ holding
# the EFI parts written in that second (dmesg-efi_pstore-<id>, id = (seconds *
# 100 + part) * 1000 + count) and dmesg.txt, those parts joined oldest first.
# $1 dir under the archive, $2 body of dmesg.txt, $3 mtime (epoch), $4 the
# seconds the kernel wrote it, $5 first part number (default 1).
mkrecord() {
    local d="$WORK/pstore/$1" secs="${4:-$NOW}" p="${5:-1}"
    mkdir -p "$d"
    printf '%s\n' "$2" > "$d/dmesg.txt"
    : > "$d/dmesg-efi_pstore-$(( (secs * 100 + p) * 1000 + 1 ))"
    : > "$d/dmesg-efi_pstore-$(( (secs * 100 + p + 1) * 1000 + 1 ))"
    touch -d "@$3" "$d/dmesg.txt" "$d"/dmesg-efi_pstore-*
}

flood() {
    local i
    for i in $(seq 1 "$1"); do
        printf '<6>[  %d.000000] [   T812] rtl8xxxu 1-2:1.0: C2H content: 00000000: 07 1a 2b 3c 4d 5e 6f 70 81 92 a3 b4\n' "$((100 + i))"
    done
}

SYSRQ="Panic#1 Part2
$(flood 40)
Panic#1 Part1
<6>[  612.100000] [ T1203] sysrq: Trigger a crash
<0>[  612.100100] [ T1203] Kernel panic - not syncing: sysrq triggered crash
<4>[  612.100200] [ T1203] CPU: 1 UID: 0 PID: 1203 Comm: sh Not tainted 7.2.11-cachyos1-rime1 #1 PREEMPT(full)
<4>[  612.100300] [ T1203] Hardware name: QEMU Standard PC (Q35 + ICH9, 2009), BIOS unknown 2/2/2022
<4>[  612.100400] [ T1203] Call Trace:
<4>[  612.100500] [ T1203]  <TASK>
<4>[  612.100600] [ T1203]  dump_stack_lvl+0x5d/0x80
<4>[  612.100700] [ T1203]  vpanic+0x113/0x2c0
<4>[  612.100800] [ T1203]  panic+0x6b/0x6b
<4>[  612.100900] [ T1203]  sysrq_handle_crash+0x1a/0x20
<4>[  612.101000] [ T1203]  __handle_sysrq.cold+0x9b/0xe8
<4>[  612.101100] [ T1203]  write_sysrq_trigger+0x6a/0x90
<4>[  612.101200] [ T1203]  </TASK>
<0>[  612.101300] [ T1203] Kernel Offset: disabled"

DONGLE="Panic#1 Part2
$(flood 30)
<0>[ 4012.000000] [    C3] watchdog: BUG: soft lockup - CPU#3 stuck for 26s! [kworker/3:2:9918]
<4>[ 4012.000100] [    C3] Modules linked in: rtl8xxxu mac80211 cfg80211 amdgpu
<4>[ 4012.000200] [    C3] CPU: 3 UID: 0 PID: 9918 Comm: kworker/3:2 Tainted: G             L      7.2.11-cachyos1-rime1 #1
<4>[ 4012.000300] [    C3] Workqueue: events rtl8xxxu_c2hcmd_callback [rtl8xxxu]
<4>[ 4012.000400] [    C3] RIP: 0010:hex_dump_to_buffer+0x2c1/0x4b0
<4>[ 4012.000500] [    C3] Call Trace:
<4>[ 4012.000600] [    C3]  <IRQ>
<4>[ 4012.000700] [    C3]  print_hex_dump+0xd4/0x140
<4>[ 4012.000800] [    C3]  rtl8723bu_handle_c2h+0x1b8/0x2a0 [rtl8xxxu]
<4>[ 4012.000900] [    C3]  rtl8xxxu_c2hcmd_callback+0x7c/0x190 [rtl8xxxu]
Panic#1 Part1
<0>[ 4012.100000] [    C3] Kernel panic - not syncing: softlockup: hung tasks
<4>[ 4012.100100] [    C3] CPU: 3 UID: 0 PID: 9918 Comm: kworker/3:2 Tainted: G             L      7.2.11-cachyos1-rime1 #1
<4>[ 4012.100200] [    C3] Call Trace:
<4>[ 4012.100300] [    C3]  <IRQ>
<4>[ 4012.100400] [    C3]  dump_stack_lvl+0x5d/0x80
<4>[ 4012.100500] [    C3]  vpanic+0x113/0x2c0
<4>[ 4012.100600] [    C3]  watchdog_timer_fn.cold+0x3d/0x3d
<4>[ 4012.100700] [    C3]  </IRQ>"

OOPS="Oops#1 Part1
<1>[  77.000000] [  T433] BUG: kernel NULL pointer dereference, address: 0000000000000008
<4>[  77.000100] [  T433] Oops: Oops: 0000 [#1] SMP NOPTI
<4>[  77.000200] [  T433] CPU: 0 UID: 0 PID: 433 Comm: modprobe Tainted: G           OE      7.2.11-cachyos1-rime1 #1
<4>[  77.000300] [  T433] RIP: 0010:demo_probe+0x22/0x80 [demo_wifi]
<4>[  77.000400] [  T433] Call Trace:
<4>[  77.000500] [  T433]  really_probe+0xdb/0x340
<4>[  77.000600] [  T433]  demo_init+0x10/0x1000 [demo_wifi]"

record() {
    RIME_PSTORE_ARCHIVE="$WORK/pstore" RIME_CRASH_STATE="$WORK/state" \
        RIME_BOOT_TIME="$BOOT" RIME_BOOT_ID="${BOOTID:-boot-a}" "$RECORD" 2>"$WORK/record.err"
}
field() { sed -n "s/^$1=//p" "$WORK/state/last" | head -n 1; }
# A new boot: everything archived so far was collected in an earlier one.
newboot() { find "$WORK/pstore" -exec touch -d "@$((BOOT - 3600))" {} +; }
expect_field() {
    local k=$1 want=$2 name=$3 got
    got=$(field "$k")
    if [[ "$got" == "$want" ]]; then ok "$name"; else bad "$name" "$k='$got', wanted '$want'"; fi
}

sec "nothing to say"
record; rc=$?
if [[ $rc -eq 0 && ! -e "$WORK/state/last" ]]; then ok "no archive: exit 0, no summary"; else bad "no archive" "rc=$rc"; fi
mkdir -p "$WORK/pstore"
mkrecord old/001 "$SYSRQ" $((BOOT - 86400)) $((BOOT - 86400))
record
# No run ever finished here, so a record from an earlier boot is written down
# (a first crash whose run was cut off must not be lost), but without a boot
# id: nothing claims this boot restarted from it.
expect_field id old/001 "a record no finished run wrote down is written down"
expect_field boot_id "" "without a boot id (it is not this boot's)"

sec "a flood, then a sysrq panic"
# One dump, two directories: EFI writes are slow, so the parts of one panic
# land in consecutive seconds. The panic is in the FIRST (Part1-2, written
# first); the later one holds only older log and is the newer file.
mkrecord "$((NOW - 200))/001" "$SYSRQ" $((NOW - 60)) $((NOW - 200)) 1
mkrecord "$((NOW - 199))/001" "Panic#1 Part12
$(flood 30)" $((NOW - 59)) $((NOW - 199)) 12
record
expect_field kind panic "kind is panic"
expect_field reason "Kernel panic - not syncing: sysrq triggered crash" "the reason is the panic line, not the flood"
expect_field kernel "7.2.11-cachyos1-rime1" "the kernel release from the CPU line"
expect_field module "" "no module blamed when the backtrace names none"
expect_field id "$((NOW - 200))/001" "id is the directory holding the panic, not the newest one"
expect_field record "$WORK/pstore/$((NOW - 200))/001/dmesg.txt" "the record path"
expect_field boot_id "boot-a" "the boot it was collected in"
expect_field time "$(date -u -d "@$((NOW - 200))" +%Y-%m-%dT%H:%M:%SZ)" "time from the first efi part id, not the file time"
if pipe_has -F 'C2H content' "$WORK/state/last"; then bad "the summary quotes the flood"; else ok "the summary never quotes the flood"; fi
perm=$(stat -c %a "$WORK/state/last")
[[ "$perm" == 644 ]] && ok "summary is world-readable (rime crash needs no root)" || bad "summary mode" "$perm"
if ls -A "$WORK/state" | pipe_has '^\.last\.'; then bad "a temp file was left"; else ok "written atomically, nothing left over"; fi

sec "the same record twice is one crash"
before=$(cat "$WORK/state/last")
BOOTID=boot-b record
after=$(cat "$WORK/state/last")
[[ "$before" == "$after" ]] && ok "an already-recorded id is not rewritten" || bad "rewritten on a second run"

sec "the dongle: soft lockup in rtl8xxxu, then the panic"
newboot
mkrecord "$((NOW - 100))/001" "$DONGLE" $((NOW - 30)) $((NOW - 100))
record
expect_field reason "Kernel panic - not syncing: softlockup: hung tasks" "the reason is the panic line"
expect_field module rtl8xxxu "the driver the backtrace names (the report before the panic)"
expect_field kernel "7.2.11-cachyos1-rime1" "kernel from a Tainted: line"

sec "a dump split over seconds: older parts carry an older BUG"
# The dongle again, as EFI writes it: the panic in Part1-2 (first second),
# the soft lockup reports in Part12-13 (a later second, a newer file).
newboot
mkrecord "$((NOW - 90))/001" "Panic#1 Part2
$(flood 5)
Panic#1 Part1
<0>[ 4012.100000] [    C3] Kernel panic - not syncing: softlockup: hung tasks
<4>[ 4012.100100] [    C3] CPU: 3 UID: 0 PID: 9918 Comm: kworker/3:2 Tainted: G             L      7.2.11-cachyos1-rime1 #1" \
    $((NOW - 25)) $((NOW - 90)) 1
mkrecord "$((NOW - 89))/001" "Panic#1 Part13
<0>[ 3986.000000] [    C3] watchdog: BUG: soft lockup - CPU#3 stuck for 22s! [kworker/3:2:9918]
<4>[ 3986.000100] [    C3]  rtl8xxxu_c2hcmd_callback+0x7c/0x190 [rtl8xxxu]
Panic#1 Part12
$(flood 5)" $((NOW - 24)) $((NOW - 89)) 12
record
expect_field reason "Kernel panic - not syncing: softlockup: hung tasks" "the panic, not the older soft lockup in a later directory"
expect_field id "$((NOW - 90))/001" "the directory holding the panic"

sec "an oops that did not stop the machine"
newboot
mkrecord "$((NOW - 50))/001" "$OOPS" $((NOW - 10)) $((NOW - 50))
record
expect_field kind oops "kind is oops"
expect_field reason "BUG: kernel NULL pointer dereference, address: 0000000000000008" "the reason is the BUG line"
expect_field module demo_wifi "the driver of the RIP line"

sec "an oops split over seconds: Part1 holds the newest report"
newboot
mkrecord "$((NOW - 40))/001" "$OOPS" $((NOW - 8)) $((NOW - 40)) 1
mkrecord "$((NOW - 39))/001" "Oops#1 Part12
<4>[  30.000000] [  T120] watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [kworker/1:0:120]
<4>[  30.000100] [  T120]  old_work+0x10/0x20 [old_mod]" $((NOW - 7)) $((NOW - 39)) 12
record
expect_field reason "BUG: kernel NULL pointer dereference, address: 0000000000000008" "the newest report (Part1), not an older BUG in a later directory"
expect_field id "$((NOW - 40))/001" "the directory holding Part1"

sec "a soft lockup report on its own"
newboot
mkrecord "$((NOW - 20))/001" "Oops#1 Part1
<4>[  30.000000] [  T120] watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [kworker/1:0:120]
<4>[  30.000100] [  T120] RIP: 0010:busy_loop+0x10/0x20 [demo_wifi]" $((NOW - 6)) $((NOW - 20)) 1
record
expect_field reason "watchdog: BUG: soft lockup - CPU#1 stuck for 23s! [kworker/1:0:120]" "the watchdog's BUG line is a reason"
expect_field module demo_wifi "and its RIP names the driver"

sec "a hostile record cannot break the summary"
newboot
mkrecord "$((NOW - 5))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: $(printf 'x%.0s' $(seq 1 600))$(printf '\033[2J')
id=forged" $((NOW - 5)) $((NOW - 5))
record
expect_field id "$((NOW - 5))/001" "a line in the record cannot forge a field"
len=$(field reason | wc -c)
(( len <= 301 )) && ok "a long reason is cut to a sentence ($len)" || bad "reason length" "$len"
if pipe_has -P '\x1b' "$WORK/state/last"; then bad "control characters kept"; else ok "control characters dropped"; fi

sec "a dump split over directories: the backtrace is read with the panic"
# EFI writes are slow: the panic line closes Part2, written a second after
# Part1, which holds the end of the log (the panic's own backtrace).
newboot
T=$((NOW - 70))
mkrecord "$T/001" "Panic#1 Part1
<4>[  90.100100] [  T433] CPU: 0 UID: 0 PID: 433 Comm: irq/45 Tainted: G      D    OE      7.2.11-cachyos1-rime1 #1
<4>[  90.100200] [  T433] Call Trace:
<4>[  90.100300] [  T433]  dump_stack_lvl+0x5d/0x80
<4>[  90.100400] [  T433]  vpanic+0x113/0x2c0
<4>[  90.100500] [  T433]  oops_end.cold+0x2d/0x2d
<4>[  90.100600] [  T433]  demo_irq+0x22/0x80 [demo_wifi]
<0>[  90.100700] [  T433] Kernel Offset: disabled" $((NOW - 30)) "$T" 1
mkrecord "$((T + 1))/001" "Panic#1 Part3
$(flood 5)
Panic#1 Part2
$(flood 5)
<0>[  90.100000] [  T433] Kernel panic - not syncing: Fatal exception in interrupt" $((NOW - 29)) "$((T + 1))" 2
record
expect_field reason "Kernel panic - not syncing: Fatal exception in interrupt" "the panic line, from the later directory"
expect_field module demo_wifi "the driver its backtrace names, from the neighbouring directory"
expect_field id "$((T + 1))/001" "the id is the directory holding the panic line"
expect_field time "$(date -u -d "@$T" +%Y-%m-%dT%H:%M:%SZ)" "the time is the dump's first part"

sec "a run cut off before it wrote anything is recovered at the next boot"
# The previous boot (from BOOT-600) collected a panic and its run never
# finished; the last finished run was the boot before (BOOT-1200).
newboot
printf '%s\n' $((BOOT - 1200)) > "$WORK/state/checked"
mkrecord "$((BOOT - 1300))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: older than the last finished run" $((BOOT - 1300)) $((BOOT - 1300))
mkrecord "$((BOOT - 560))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: collected by a run that was cut off" $((BOOT - 500)) $((BOOT - 560))
mkrecord "$((NOW - 15))/001" "$OOPS" $((NOW - 10)) $((NOW - 15))
record
hist_has() { [[ -r "$WORK/state/history" ]] && awk -F '\t' -v id="$1" '$1 == id { n++ } END { exit !(n == 1) }' "$WORK/state/history"; }
hist_field() { awk -F '\t' -v id="$1" -v c="$2" '$1 == id { print $c }' "$WORK/state/history"; }
if hist_has "$((BOOT - 560))/001"; then ok "the cut-off run's crash is written down"; else bad "not recovered" "$(cat "$WORK/state/history" 2>/dev/null)"; fi
if [[ -z "$(hist_field "$((BOOT - 560))/001" 4)" ]]; then ok "with no boot id: nothing says this boot restarted from it"; else bad "recovered boot id" "$(hist_field "$((BOOT - 560))/001" 4)"; fi
if hist_has "$((BOOT - 1300))/001"; then bad "a record older than the last finished run was announced"; else ok "a record older than the last finished run stays history"; fi
expect_field id "$((NOW - 15))/001" "this boot's crash is the newest summary"
expect_field boot_id boot-a "and carries this boot"
if [[ "$(cat "$WORK/state/checked")" == "$BOOT" ]]; then ok "the finished run moves the window to this boot"; else bad "checked" "$(cat "$WORK/state/checked")"; fi
record
if hist_has "$((BOOT - 560))/001" && hist_has "$((NOW - 15))/001"; then ok "a second run writes nothing twice"; else bad "history repeats" "$(cat "$WORK/state/history")"; fi

# A separate archive and state for the cases below, so earlier records do not
# answer for them.
record_in() {
    RIME_PSTORE_ARCHIVE="$1" RIME_CRASH_STATE="$2" RIME_BOOT_TIME="$BOOT" RIME_BOOT_ID=boot-a "$RECORD" 2>/dev/null
}
in_hist() { awk -F '\t' -v id="$2" '$1 == id { n++ } END { exit !(n == 1) }' "$1/history" 2>/dev/null; }
mkpart() { # archive dir body mtime
    mkdir -p "$1/$2"
    printf '%s\n' "$3" > "$1/$2/dmesg.txt"
    : > "$1/$2/dmesg-efi_pstore-$(( (${2%%/*} * 100 + 1) * 1000 + 1 ))"
    touch -d "@$4" "$1/$2/dmesg.txt" "$1/$2"/dmesg-efi_pstore-*
}

sec "the first crash on a machine, its run cut off, is not lost"
A="$WORK/first/pstore" S="$WORK/first/state"
mkpart "$A" "$((BOOT - 40))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: the first one" $((BOOT - 30))
mkdir -p "$S"
record_in "$A" "$S"
if in_hist "$S" "$((BOOT - 40))/001"; then ok "no finished run yet: the earlier boot's record is written down"; else bad "first crash lost" "$(cat "$S/history" 2>/dev/null)"; fi

sec "a window holding a recorded panic still finds the oops beside it"
A="$WORK/beside/pstore" S="$WORK/beside/state"
mkpart "$A" "$((BOOT - 1100))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: recorded by its own boot" $((BOOT - 1100))
mkpart "$A" "$((BOOT - 520))/001" "$OOPS" $((BOOT - 500))
mkdir -p "$S"
printf '%s\n' $((BOOT - 1200)) > "$S/checked"
printf '%s\t2026-01-01T00:00:00Z\tpanic\tboot-0\tKernel panic - not syncing: recorded by its own boot\t\t/x\t%s\n' \
    "$((BOOT - 1100))/001" "$((BOOT - 1100))/001" > "$S/history"
record_in "$A" "$S"
if in_hist "$S" "$((BOOT - 520))/001"; then ok "the oops of the cut-off boot is written down"; else bad "oops behind a recorded panic lost" "$(cat "$S/history")"; fi
if in_hist "$S" "$((BOOT - 1100))/001"; then ok "and the panic is still one line"; else bad "panic line repeated" "$(cat "$S/history")"; fi

sec "two cut-off boots in a row: both crashes are written down"
A="$WORK/two/pstore" S="$WORK/two/state"
mkpart "$A" "$((BOOT - 900))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: the first cut-off boot" $((BOOT - 890))
mkpart "$A" "$((BOOT - 400))/001" "Panic#1 Part1
<0>[ 1.0] Kernel panic - not syncing: the second cut-off boot" $((BOOT - 390))
mkdir -p "$S"
printf '%s\n' $((BOOT - 1200)) > "$S/checked"
record_in "$A" "$S"
if in_hist "$S" "$((BOOT - 900))/001" && in_hist "$S" "$((BOOT - 400))/001"; then ok "both"; else bad "one of two lost" "$(cat "$S/history" 2>/dev/null)"; fi

sec "the history keeps the last 50"
newboot
for i in $(seq 1 60); do printf 'filler/%d\t2026-01-01T00:00:00Z\tpanic\t\tx\t\t/x\n' "$i"; done > "$WORK/state/history"
mkrecord "$((NOW - 12))/001" "$SYSRQ" $((NOW - 9)) $((NOW - 12))
record
if [[ "$(wc -l < "$WORK/state/history")" -eq 50 ]] && tail -n 1 "$WORK/state/history" | pipe_has -F "$((NOW - 12))/001"; then
    ok "50 lines, the new crash last"
else
    bad "history cap" "$(wc -l < "$WORK/state/history") lines"
fi

sec "a full disk keeps the previous summary"
# A tmpfs of three pages in a private mount namespace: the old summary and
# history take two, a filler the third. The run must fail and change nothing.
if unshare -Urm true 2>/dev/null; then
    mkdir -p "$WORK/full"
    newboot
    mkrecord "$((NOW - 11))/001" "$SYSRQ" $((NOW - 8)) $((NOW - 11))
    out="$(unshare -Urm bash -c '
        mount -t tmpfs -o size=12k tmpfs "$1" || { echo "mount failed"; exit 0; }
        printf "id=prev\nkind=panic\nreason=the previous crash\n" > "$1/last"
        printf "prev\t2026-01-01T00:00:00Z\tpanic\t\tthe previous crash\t\t/x\n" > "$1/history"
        dd if=/dev/zero of="$1/fill" bs=1k count=64 2>/dev/null
        RIME_PSTORE_ARCHIVE="$2" RIME_CRASH_STATE="$1" RIME_BOOT_TIME="$3" RIME_BOOT_ID=boot-a "$4" 2>/dev/null
        echo "rc=$?"
        cat "$1/last"
        echo "--"
        ls -A "$1"
    ' _ "$WORK/full" "$WORK/pstore" "$BOOT" "$RECORD")"
    if [[ "$out" == *"mount failed"* ]]; then
        bad "tmpfs mount in a user namespace failed"
    else
        if [[ "$out" != *"rc=0"* ]]; then ok "the run fails"; else bad "a full disk reported success"; fi
        if [[ "$out" == *"id=prev"$'\n'"kind=panic"$'\n'"reason=the previous crash"* ]]; then
            ok "the previous summary is untouched"
        else
            bad "summary replaced on a full disk" "$out"
        fi
        if [[ "${out#*--}" == *last.* || "${out#*--}" == *history.* ]]; then bad "a temporary file was left" "${out#*--}"; else ok "no temporary file left"; fi
        if [[ "${out#*--}" == *checked* ]]; then bad "the window moved past a failed run"; else ok "the window stays, so the next boot tries again"; fi
    fi
elif [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    bad "unshare -Urm refused (CI must allow user namespaces for this test)"
else
    printf 'SKIP  a full disk: no user namespaces here\n'
fi

sec "the notice: every crash once, after it got through"
cat > "$WORK/bin/notify-send" <<EOF
#!/usr/bin/env bash
[[ -e "$WORK/notify-fails" ]] && exit 1
printf '%s\n' "\$*" >> "$WORK/notified"
EOF
chmod +x "$WORK/bin/notify-send"
printf 'boot-a\n' > "$WORK/boot_id"
HIST="$WORK/history"
count() { if [[ -f "$WORK/notified" ]]; then wc -l < "$WORK/notified"; else echo 0; fi; }
notice() {
    HOME="$WORK/home" XDG_STATE_HOME="$WORK/home/.state" RIME_CRASH_HISTORY="$HIST" \
        RIME_BOOT_ID_FILE="$WORK/boot_id" PATH="$WORK/bin:$PATH" "$NOTICE" >/dev/null 2>&1
}
# hist ID KIND BOOT REASON [MODULE] [AGE-SECONDS]
hist() {
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$1" "$(date -u -d "@$((NOW - ${6:-60}))" +%Y-%m-%dT%H:%M:%SZ)" \
        "$2" "$3" "$4" "${5:-}" "/var/lib/systemd/pstore/$1/dmesg.txt" >> "$HIST"
}
expect_count() {
    local want=$1 name=$2 got
    got=$(count)
    if [[ "$got" -eq "$want" ]]; then ok "$name"; else bad "$name" "notified $got times, wanted $want"; fi
}
last_says() { tail -n 1 "$WORK/notified" 2>/dev/null | pipe_has -F -- "$1"; }
notice; expect_count 0 "no history: no notification"
hist 1 panic boot-a "Kernel panic - not syncing: softlockup: hung tasks" rtl8xxxu
notice; expect_count 1 "a crash collected in this boot is announced"
if last_says 'Rime restarted after a system crash' && last_says 'The backtrace names the rtl8xxxu driver.' \
    && last_says 'Details: rime crash'; then
    ok "it says what happened, names the driver as the backtrace's, and points at rime crash"
else
    bad "notice text" "$(cat "$WORK/notified")"
fi
if last_says '--urgency=normal'; then ok "normal urgency (the crash is over)"; else bad "urgency"; fi
notice; expect_count 1 "the same crash is not announced again at the next login"

printf 'boot-b\n' > "$WORK/boot_id"
hist 2 panic boot-a "Kernel panic - not syncing: x"
notice; expect_count 2 "a crash nobody was told about in its own boot is announced later"
if last_says 'Rime crashed and restarted earlier'; then ok "as earlier, not as this restart"; else bad "earlier title" "$(tail -n 1 "$WORK/notified")"; fi
hist 3 oops boot-b "BUG: y"
notice; expect_count 3 "a new crash in this boot is announced"
if last_says 'The system hit a kernel error before this restart'; then ok "an oops is not called a crash restart"; else bad "oops title" "$(tail -n 1 "$WORK/notified")"; fi

printf 'boot-c\n' > "$WORK/boot_id"
hist 4 panic boot-c "Kernel panic - not syncing: this boot's"
hist 5 panic boot-b "Kernel panic - not syncing: an earlier one"
notice; expect_count 4 "two crashes before a login: one notice"
if last_says "this boot's" && last_says 'Rime restarted after a system crash' \
    && last_says 'There was 1 more kernel crash or error since the last notice.'; then
    ok "it tells this boot's crash and counts the other"
else
    bad "two crashes" "$(tail -n 1 "$WORK/notified")"
fi
notice; expect_count 4 "and neither again"

hist 6 panic "" "Kernel panic - not syncing: recovered" demo_wifi
notice; expect_count 5 "a recovered crash (no boot id) is announced"
if last_says 'The backtrace names the demo_wifi driver.' && last_says 'earlier'; then
    ok "its empty boot id does not shift the other fields"
else
    bad "empty field" "$(tail -n 1 "$WORK/notified")"
fi
hist 7 panic boot-a "Kernel panic - not syncing: long ago" "" $((8 * 86400))
notice; expect_count 5 "nothing older than a week is announced"

hist 8 panic boot-c "Kernel panic - not syncing: no notification server yet"
: > "$WORK/notify-fails"
notice; rc=$?
rm -f "$WORK/notify-fails"
if [[ $rc -ne 0 ]]; then ok "a notice that did not get through fails the unit (it retries)"; else bad "notify-send failure exit 0"; fi
notice; expect_count 6 "and is announced on the next try"
if last_says 'no notification server yet'; then ok "the same crash, not lost"; else bad "retry text" "$(tail -n 1 "$WORK/notified")"; fi

sec "one set of kernel arguments for both systems"
line='kargs = ["panic=30", "drm.panic_screen=user", "efi_pstore.pstore_disable=0"]'
if pipe_has -Fx "$line" "$KARGS"; then ok "kargs.d: panic=30, the user panic screen, efi_pstore on"; else bad "kargs.d line"; fi
if pipe_has -F 'builtins.readFile ../../files/system/kargs.d/30-rime-crash.toml' "$NIXMOD"; then
    ok "NixOS reads the same file (no second copy to drift)"
else
    bad "crash.nix does not read the kargs.d file"
fi
for k in panic_on_oops softlockup_panic hung_task_panic hardlockup_panic; do
    if pipe_has -E "^kargs.*$k" "$KARGS"; then bad "$k is set (left off on purpose: docs/kernel-crash.md)"; else ok "$k left off"; fi
done
if pipe_has -E 'rtl8xxxu' "$KARGS" || grep -rn 'blacklist.*rtl8xxxu' "$REPO/files" "$REPO/nix/modules" >/dev/null 2>&1; then
    bad "rtl8xxxu is blacklisted"
else
    ok "rtl8xxxu is not blacklisted"
fi

sec "unit shapes"
pipe_has -x 'After=systemd-pstore.service local-fs.target' "$REC_UNIT" && ok "record runs after systemd-pstore" || bad "record After="
pipe_has -x 'ConditionDirectoryNotEmpty=/var/lib/systemd/pstore' "$REC_UNIT" && ok "record skipped when the archive is empty" || bad "record condition"
pipe_has -x 'Before=systemd-user-sessions.service' "$REC_UNIT" && ok "record done before any login (the notice never reads a stale summary)" || bad "record Before="
pipe_has -F 'before = [ "systemd-user-sessions.service" ];' "$NIXMOD" && ok "the same order on NixOS" || bad "crash.nix before"
pipe_has -x 'PrivateNetwork=yes' "$REC_UNIT" && ok "record has no network (never uploads)" || bad "record network"
pipe_has -x 'WantedBy=multi-user.target' "$REC_UNIT" && ok "record runs at every boot" || bad "record WantedBy"
pipe_has -x 'WantedBy=timers.target' "$TIMER" && ok "notice timer in the user session" || bad "timer WantedBy"
if pipe_has -E '^(User|DynamicUser)=' "$SVC"; then bad "notice drifted into a system unit"; else ok "notice is a user unit"; fi
pipe_has -x 'ExecStart=/usr/libexec/rime-crash-notice' "$SVC" && ok "notice runs the shipped script" || bad "notice ExecStart"
pipe_has -x 'ConditionPathExists=/var/lib/rime/crash/history' "$SVC" && ok "notice reads the history" || bad "notice condition"
pipe_has -x 'Restart=on-failure' "$SVC" && pipe_has -F 'Restart = "on-failure";' "$NIXMOD" \
    && ok "a notice that did not get through is tried again (both systems)" || bad "notice Restart="

if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -S warning "$RECORD" "$NOTICE" "$0" >/dev/null; then ok "shellcheck clean"; else bad "shellcheck"; fi
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail -eq 0 ]]
