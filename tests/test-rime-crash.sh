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
#    3. a record from an EARLIER boot announced as news, or the same crash
#       announced at every login;
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
if [[ ! -e "$WORK/state/last" ]]; then ok "a record from an earlier boot is history, not news"; else bad "old record announced"; fi

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

sec "the notice: once per crash, only in the boot that collected it"
cat > "$WORK/bin/notify-send" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$WORK/notified"
EOF
chmod +x "$WORK/bin/notify-send"
printf 'boot-a\n' > "$WORK/boot_id"
count() { if [[ -f "$WORK/notified" ]]; then wc -l < "$WORK/notified"; else echo 0; fi; }
notice() {
    HOME="$WORK/home" XDG_STATE_HOME="$WORK/home/.state" RIME_CRASH_LAST="$WORK/last" \
        RIME_BOOT_ID_FILE="$WORK/boot_id" PATH="$WORK/bin:$PATH" "$NOTICE" >/dev/null 2>&1
}
expect_count() {
    local want=$1 name=$2 got
    got=$(count)
    if [[ "$got" -eq "$want" ]]; then ok "$name"; else bad "$name" "notified $got times, wanted $want"; fi
}
notice; expect_count 0 "no summary: no notification"
printf 'id=1\nkind=panic\nreason=Kernel panic - not syncing: softlockup: hung tasks\nmodule=rtl8xxxu\nboot_id=boot-a\n' > "$WORK/last"
notice; expect_count 1 "a crash collected in this boot is announced"
if pipe_has -F 'Rime restarted after a system crash' "$WORK/notified" \
    && pipe_has -F 'The backtrace names the rtl8xxxu driver.' "$WORK/notified" \
    && pipe_has -F 'Details: rime crash' "$WORK/notified"; then
    ok "it says what happened, names the driver as the backtrace's, and points at rime crash"
else
    bad "notice text" "$(cat "$WORK/notified")"
fi
if pipe_has -F -- '--urgency=normal' "$WORK/notified"; then ok "normal urgency (the crash is over)"; else bad "urgency"; fi
notice; expect_count 1 "the same crash is not announced again at the next login"
printf 'boot-b\n' > "$WORK/boot_id"
printf 'id=2\nkind=panic\nreason=Kernel panic - not syncing: x\nboot_id=boot-a\n' > "$WORK/last"
notice; expect_count 1 "a crash collected in an earlier boot is not announced"
printf 'id=3\nkind=oops\nreason=BUG: y\nboot_id=boot-b\n' > "$WORK/last"
notice; expect_count 2 "a new crash in this boot is announced"
if tail -n 1 "$WORK/notified" | pipe_has -F 'The system hit a kernel error before this restart'; then
    ok "an oops is not called a crash restart"
else
    bad "oops title" "$(tail -n 1 "$WORK/notified")"
fi

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
pipe_has -x 'PrivateNetwork=yes' "$REC_UNIT" && ok "record has no network (never uploads)" || bad "record network"
pipe_has -x 'WantedBy=multi-user.target' "$REC_UNIT" && ok "record runs at every boot" || bad "record WantedBy"
pipe_has -x 'WantedBy=timers.target' "$TIMER" && ok "notice timer in the user session" || bad "timer WantedBy"
if pipe_has -E '^(User|DynamicUser)=' "$SVC"; then bad "notice drifted into a system unit"; else ok "notice is a user unit"; fi
pipe_has -x 'ExecStart=/usr/libexec/rime-crash-notice' "$SVC" && ok "notice runs the shipped script" || bad "notice ExecStart"

if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -S warning "$RECORD" "$NOTICE" "$0" >/dev/null; then ok "shellcheck clean"; else bad "shellcheck"; fi
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ $fail -eq 0 ]]
