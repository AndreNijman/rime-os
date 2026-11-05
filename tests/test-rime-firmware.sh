#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-firmware.sh — executable assertions for roadmap §P2-015's
#  firmware readout.
#
#  ── The four failures this exists to catch ──────────────────────────────────
#
#  1. A HEALTHY MACHINE READ AS A BROKEN FIRMWARE SUBSYSTEM, or a broken one
#     read as healthy. fwupdmgr's exit status carries no information in either
#     direction, and both halves were measured on the development machine at
#     the same moment: `get-updates --json` with nothing to do exits 0;
#     `get-updates` WITHOUT --json exits 2 with a full device list; and
#     `get-releases --json <a device id that does not exist>` exits 0 with an
#     `Error` object in the body. So the document is the only truth.
#
#  2. A READOUT THAT IS MOSTLY SECURE BOOT BOOKKEEPING. Of 28 rows on this
#     machine, 12 are Secure Boot key and revocation stores, and five separate
#     rows are all called `UEFI Device Firmware`. The obvious filter — keep
#     what is flagged `updatable` — does not work: measured, NINE of eleven
#     updatable rows were certificate stores, and that list contained
#     `KEK CA` twice and `UEFI CA` twice.
#
#  3. A MOMENTARY CONDITION PRINTED AS A CAPABILITY. `updatable` is not a
#     property of a device. Two `get-devices` calls minutes apart on an idle
#     machine returned 11 and then 18 updatable rows, and the seven that
#     changed were EXACTLY the seven that had carried `require-ac-power` — the
#     laptop had been unplugged. On battery, an `[updatable]`/`[read-only]`
#     column reports the System Firmware and the NVMe as things that cannot be
#     updated at all.
#
#  4. A PERMANENT COMPLAINT ON A HEALTHY LAPTOP. Those same seven rows carry
#     `require-ac-power` while NOTHING has an update waiting, so reporting
#     blockers unconditionally puts seven warnings on a machine with nothing to
#     do, for as long as it is on battery. Same shape as warning about the
#     composefs root's 0 bytes free, which §48 already fixed once.
#
#  Two modes, the split the sibling suites use:
#
#    (no argument)     Structural checks with no toolchain.
#    --with-binary     Drives `rime firmware` against captured fwupd
#                      documents. It DIES if the binary is absent; a skipped
#                      assertion reports as a pass.
#
#  NOTHING HERE SPAWNS fwupdmgr. Under $RIME_FIRMWARE_ROOT every answer comes
#  from a file this script writes.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_BINARY=0
case "${1:-}" in
    "")            ;;
    --with-binary) WITH_BINARY=1 ;;
    *) echo "usage: ${0##*/} [--with-binary]" >&2; exit 2 ;;
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
hasnt() {
    if grep -qF -- "$1" "$2"; then
        bad "$3 — found '$1' in:"; sed 's/^/       /' "$2" >&2
    else ok "$3"; fi
}

# ── why no check below is `producer | grep -q` ───────────────────────────────
#
#  Under `set -o pipefail` that shape is a COIN FLIP. `grep -q` exits at its
#  first match and SIGPIPEs whatever is feeding it, so the pipeline's status is
#  141 rather than grep's 0, and an `if` reads a match as "no match". Measured
#  under bash against this repo's own `rimed-core/src/firmware.rs`: 40 trials
#  of `sed -E 's://.*$::' "$CORE" | grep -q uefi_capsule` returned **0 twenty
#  times and 141 twenty times**, and this suite's own structural half was
#  21/0 three times and 20/1 nine times out of twelve runs over a tree that
#  never changed.
#
#  Both directions are wrong and one of them is silent: where a match means
#  `bad`, a SIGPIPE turns a real regression into a pass, and no mutation can
#  ever be caught by that check again. So every check that has to look at
#  something other than a plain file CAPTURES the text first and greps the
#  text — command substitution reads its producer to EOF, so there is nobody
#  left to signal.
absent() {   # absent <text> <extended-regex> <message>
    if grep -qE -- "$2" <<<"$1"; then
        bad "$3 — matched /$2/ in:"; sed 's/^/       /' <<<"$1" >&2
    else ok "$3"; fi
}
present() {  # present <text> <extended-regex> <message>
    if grep -qE -- "$2" <<<"$1"; then ok "$3"
    else bad "$3 — no /$2/ in:"; sed 's/^/       /' <<<"$1" >&2; fi
}

CORE="$REPO/rimed/rimed-core/src/firmware.rs"
CLI="$REPO/rimed/rime/src/firmware.rs"
for f in "$CORE" "$CLI"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

sec "fwupd's exit status is never consulted"
# The status means "nothing to do" when non-zero and accompanies an explicit
# Error document when zero. A caller keeping the exit check and dropping
# --json reads a current machine as a broken one.
has 'pub fn parse_devices(text: &str) -> Result<Vec<Device>, String>' "$CORE" \
    "the parser takes no exit code, so it cannot be given one"
ASK="$(awk '/^fn ask\(/,/^}/' "$CLI" | sed -E 's://.*$::')"
[[ -n "$ASK" ]] || bad "fn ask() was not found in $CLI at all"
absent "$ASK" '\.status|success\(\)|\.code\(' \
    "and the spawn never looks at the status either"
has '"--json"' "$CLI" "--json is always passed, because the plain form's exit code inverts"

sec "an Error document is an error, although fwupd exits zero with one"
has 'v.get("Error")' "$CORE" "the Error object is looked for"
# No outer guard. The old one was `… | grep -n Error | head -1 | grep -q .`,
# and when that pipeline came back non-zero the whole `if` was skipped — so
# the assertion recorded neither a pass nor a failure and simply vanished from
# the count. An assertion that can disappear is worse than one that can fail.
PARSE="$(awk '/^pub fn parse_devices/,/^}/' "$CORE")"
e="$(grep -n '"Error"' <<<"$PARSE" | head -1 | cut -d: -f1)"
d="$(grep -n '"Devices"' <<<"$PARSE" | head -1 | cut -d: -f1)"
if [[ -n "$e" && -n "$d" && "$e" -lt "$d" ]]; then
    ok "and before the Devices list, so a failed request is never a device list"
else
    bad "Error is checked at line ${e:-<absent>} and Devices at ${d:-<absent>}"
fi

sec "a document with no Devices list is unreadable, not an empty machine"
has 'neither an Error nor a Devices list' "$CORE" "the reason exists and says what it means"
has 'a_document_with_no_devices_key_is_unreadable_and_not_an_empty_machine' "$CORE" \
    "and a test names the case"
has 'an_empty_devices_list_really_is_an_empty_list' "$CORE" \
    "while the real 'nothing to update' answer still parses"

sec "hardware is separated from Secure Boot stores by plugin, not by a flag"
has 'pub const SECURE_BOOT_PLUGINS' "$CORE" "the plugin list is one named constant"
# Scoped to the constant's own declaration, and that is the second bug fixed
# here: grepping the WHOLE file for `uefi_capsule` can never answer "is it in
# the Secure Boot list", because the string legitimately appears eight times
# in the test fixtures and in an assertion message. The check was a false
# alarm on correct code whenever it was not a coin flip.
PLUGINS="$(awk '/^pub const SECURE_BOOT_PLUGINS/,/;/' "$CORE")"
[[ -n "$PLUGINS" ]] || bad "SECURE_BOOT_PLUGINS's declaration could not be read"
present "$PLUGINS" 'uefi_dbx' "including the revocation list"
# uefi_capsule is the real system firmware and must not be filtered away with
# the certificates: six of this machine's rows are under it.
absent "$PLUGINS" 'uefi_capsule' \
    "and uefi_capsule is not in it, because that one is real system firmware"
has 'the_updatable_flag_does_not_separate_hardware_from_secure_boot_stores' "$CORE" \
    "a test holds the measurement that ruled the flag out"
has 'a_plugin_nobody_here_has_heard_of_is_hardware' "$CORE" \
    "and an unknown plugin is visible rather than hidden"

sec "updatable is a momentary condition and is not printed as a capability"
has 'pub enum Writable' "$CORE" "three states, not a boolean"
has 'Blocked(Vec<String>)' "$CORE" "and the blocked one carries the reason"
has 'the_flag_and_the_problem_are_mutually_exclusive_in_this_documents_own_rows' "$CORE" \
    "with a test on the fact the three states rest on"
absent "$(sed -E 's://.*$::' "$CLI")" 'read-only' \
    "the CLI never says read-only, which would be false for a device on battery"

sec "rime doctor reports one condition once"
# The defect: the unavailable branch of `doctor_lines` used to fall through to
# the attention loop, whose sentence does not contain the words "update
# waiting" and so passed its filter — `rime doctor` printed one unreadable
# fwupd as a PASS and a WARN together. Fixed by returning, not by widening the
# filter, so the check is that the branch cannot fall through.
# Scoped to the pending match, and that scope is the point: `doctor_lines`
# has TWO `return vec![(true …)]` — one for a fwupd that could not be read at
# all, one for a pending list that could not be. A check against the whole
# function finds the first and passes while the second is gone, which is
# exactly what the battery caught it doing (F11: core 9/2, structural 24/0).
DOCTOR="$(awk '/^pub fn doctor_lines/,/^}/' "$CLI")"
[[ -n "$DOCTOR" ]] || bad "doctor_lines was not found in $CLI"
PENDING_ARM="$(awk '/let pending = match &r\.pending \{/,/^    \};/' "$CLI")"
[[ -n "$PENDING_ARM" ]] || bad "doctor_lines does not match on r.pending at all"
present "$PENDING_ARM" 'Reading::Unavailable' \
    "the unavailable pending list is its own arm"
present "$PENDING_ARM" 'return' \
    "and that arm RETURNS instead of falling through into the attention loop"
has 'an_unconsultable_fwupd_is_exactly_one_doctor_line_and_it_passes' "$CLI" \
    "and a test asserts it is exactly one line, and that it passes"
has 'a_healthy_machine_on_battery_is_one_passing_doctor_line' "$CLI" \
    "while an unplugged laptop with nothing to install stays green"

sec "a blocker is reported only against an update it is actually blocking"
has 'a_healthy_machine_with_blockers_and_no_updates_says_nothing_at_all' "$CORE" \
    "the permanent-false-alarm case has a test"
has 'a_blocker_is_matched_by_device_id_and_not_by_name' "$CORE" \
    "and blockers attach by DeviceId, because five rows share one name"

sec "fwupdmgr is spawned by absolute path, and the image installs it"
# Every absolute path in the Rust comes from the rime_paths crate, chosen at
# build time; the bootc image sets no RIME_PATH_* and so gets the default there.
PATHS_RS="$REPO/rimed/rime-paths/src/lib.rs"
has 'const FWUPDMGR: &str = rime_paths::FWUPDMGR;' "$CLI" "absolute, not from PATH"
has 'FWUPDMGR = "/usr/bin/fwupdmgr";' "$PATHS_RS" \
    "and rime_paths' bootc value for it is /usr/bin/fwupdmgr"
hasnt 'RIME_PATH_FWUPDMGR' "$REPO/Containerfile.base" "and the bootc image does not override it"
# Measured: nothing in any Containerfile installed fwupd and no package
# required it — it rode in from the base image, so `rime update`'s firmware
# pass depended on a binary the image did not own.
if grep -qE '^\s*fwupd\s*\\?$|\s fwupd\s' "$REPO/Containerfile.core"; then
    ok "fwupd is installed explicitly by Containerfile.core"
else
    bad "nothing in Containerfile.core installs fwupd"
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
FIX="$TMP/fw"
export RIME_FIRMWARE_ROOT="$FIX"
mkdir -p "$FIX/.fixture"
OUT="$TMP/out"

# Every shape measured on the development machine, in miniature: a row with no
# Name key and no Version key, a row with a Name and no Version, a hex
# version, two rows sharing a name, six Secure Boot store rows across four
# plugins, a uefi_capsule row that must NOT be filtered with them, and
# `require-ac-power` on a row that consequently has no `updatable` flag.
devices_on_battery() {
    cat > "$FIX/.fixture/get-devices.json" <<'EOF'
{"Devices": [
 {"DeviceId": "aec1a869eb0df71b7cea6b3ac71d39b830faf164",
  "Plugin": "linux_display", "Flags": ["can-emulation-tag"]},
 {"DeviceId": "f685512aa07369c9e77742acef941d779d31e766",
  "Name": "GPIO controller", "Plugin": "gpio", "Flags": ["can-emulation-tag"]},
 {"DeviceId": "a363c73cb1f37be672fa2fdd1b31bfc5c84f24d8",
  "Name": "06DA:00 04F3:320B", "Version": "0x0006", "VersionFormat": "hex",
  "Vendor": "Elan", "Plugin": "elantp",
  "Flags": ["internal", "updatable", "unsigned-payload"]},
 {"DeviceId": "e2f710f8ff0d3b0a5a3f9d0b7c8e1f2a3b4c5d6e",
  "Name": "System Firmware", "Version": "0.1.17", "VersionFormat": "triplet",
  "Plugin": "uefi_capsule", "Flags": ["internal", "needs-reboot"],
  "Problems": ["require-ac-power"]},
 {"DeviceId": "8458ebba1111111111111111111111111111111a",
  "Name": "UEFI Device Firmware", "Version": "4116", "VersionFormat": "number",
  "Plugin": "uefi_capsule", "Flags": ["internal", "needs-reboot"],
  "Problems": ["require-ac-power"]},
 {"DeviceId": "811cd1b42222222222222222222222222222222b",
  "Name": "UEFI Device Firmware", "Version": "6", "VersionFormat": "number",
  "Plugin": "uefi_capsule", "Flags": ["internal", "needs-reboot"],
  "Problems": ["require-ac-power"]},
 {"DeviceId": "3333333333333333333333333333333333333333",
  "Name": "KEK CA", "Version": "2023", "VersionFormat": "number",
  "Plugin": "uefi_kek", "Flags": ["internal", "updatable", "needs-reboot"]},
 {"DeviceId": "4444444444444444444444444444444444444444",
  "Name": "KEK CA", "Version": "2012", "VersionFormat": "number",
  "Plugin": "uefi_kek", "Flags": ["internal", "updatable", "needs-reboot"]},
 {"DeviceId": "5555555555555555555555555555555555555555",
  "Name": "UEFI dbx", "Version": "20260402", "VersionFormat": "number",
  "Plugin": "uefi_dbx", "Flags": ["internal", "updatable", "needs-reboot"]},
 {"DeviceId": "6666666666666666666666666666666666666666",
  "Name": "SBAT", "Version": "1.5.4", "VersionFormat": "triplet",
  "Plugin": "uefi_sbat", "Flags": ["updatable", "needs-reboot"]},
 {"DeviceId": "7777777777777777777777777777777777777777",
  "Name": "PK CA", "Version": "2012", "Plugin": "uefi_pk", "Flags": ["internal"]},
 {"DeviceId": "8888888888888888888888888888888888888888",
  "Name": "UEFI Key Exchange Key", "Plugin": "uefi_kek", "Flags": ["internal"]}
]}
EOF
}
no_updates() { printf '{"Devices": []}\n' > "$FIX/.fixture/get-updates.json"; }

run() { "$Rime" firmware status "$@" > "$OUT" 2>&1; RC=$?; }

devices_on_battery; no_updates

sec "a machine with nothing to do says so, and complains about nothing"
run
[[ "$RC" -eq 0 ]] && ok "exit 0 with nothing waiting" || bad "exit was $RC: $(cat "$OUT")"
has 'no firmware updates are waiting' "$OUT" "and says it plainly"
# THE regression this suite exists for. Seven rows on the real machine carry
# require-ac-power with nothing to install; complaining about them makes the
# command permanently non-zero on a laptop that is simply unplugged.
hasnt '  - ' "$OUT" "no attention rows on a healthy machine that happens to be on battery"

sec "the readout is hardware first and Secure Boot bookkeeping second"
has 'Secure Boot keys and revocation lists' "$OUT" "the stores are a separate section"
has '6 rows' "$OUT" "and counted"
# uefi_capsule is real firmware; if it were filtered by the `updatable` flag
# it would vanish here, because on battery it does not carry the flag.
has 'System Firmware' "$OUT" "uefi_capsule is listed as hardware"
has 'UEFI Device Firmware' "$OUT" "and so are the rows that share a name"

sec "a momentary condition is not printed as a capability"
has '[blocked]' "$OUT" "a device fwupd will not write right now is blocked"
hasnt 'read-only' "$OUT" "and never read-only, which would be false"
has '3 device(s) cannot be written at the moment' "$OUT" "the reason is grouped and counted"
has 'plug the machine in' "$OUT" "and it is the reason fwupd actually gave"
has '[not offered]' "$OUT" "a device with no flag and no problem is distinguished from both"
has '[updatable]' "$OUT" "and a writable one still says so"

sec "a row fwupd did not name still appears, and a missing version is a reason"
has 'unnamed linux_display device' "$OUT" "the unnamed row is labelled by its plugin"
has 'no version reported' "$OUT" "and a missing version says so rather than printing a blank"
has '0x0006 (hex)' "$OUT" "a hex version travels with the format fwupd declared"

sec "five rows sharing a name are told apart"
has '8458ebba' "$OUT" "by the head of the device id"
has '811cd1b4' "$OUT" "for each of them"

sec "an update waiting is an attention row, and its blocker comes with it"
cat > "$FIX/.fixture/get-updates.json" <<'EOF'
{"Devices": [
 {"DeviceId": "e2f710f8ff0d3b0a5a3f9d0b7c8e1f2a3b4c5d6e",
  "Name": "System Firmware", "Version": "0.1.17", "VersionFormat": "triplet",
  "Plugin": "uefi_capsule", "Flags": ["internal", "needs-reboot"]}
]}
EOF
run
[[ "$RC" -eq 1 ]] && ok "exit 1 when something is waiting" || bad "exit was $RC: $(cat "$OUT")"
has 'System Firmware' "$OUT" "the device is named"
has 'has a firmware update waiting' "$OUT" "and the sentence says what is true"
has 'a restart applies it' "$OUT" "needs-reboot reaches the reader"
# The blocker is worth saying HERE and only here: it is now standing between
# this machine and an update it is being offered. Checked on the attention
# rows, so that the device-list summary cannot satisfy it by accident.
grep '^  - ' "$OUT" > "$TMP/attn" || true
has 'plug the machine in' "$TMP/attn" "and the blocker that stands in its way comes with it"

sec "a blocker is attached by device id and not by name"
# The pending row is the OTHER UEFI Device Firmware — the one whose sibling
# carries the problem. Matching on the name would blame the wrong device.
cat > "$FIX/.fixture/get-updates.json" <<'EOF'
{"Devices": [
 {"DeviceId": "9999999999999999999999999999999999999999",
  "Name": "UEFI Device Firmware", "Version": "7", "Plugin": "uefi_capsule",
  "Flags": ["internal"]}
]}
EOF
run
has 'has a firmware update waiting' "$OUT" "the pending row is reported"
# Only the ATTENTION rows, which are the '  - ' lines. The device-list summary
# names the same reason for the devices that really do carry it, and grepping
# the whole output cannot tell the two apart.
grep '^  - ' "$OUT" > "$TMP/attn" || true
hasnt 'plug the machine in' "$TMP/attn" "and a namesake's problem is not attached to it"
has 'has a firmware update waiting' "$TMP/attn" "while the update itself is an attention row"

sec "fwupd having nothing to say is never silence"
no_updates
rm -f "$FIX/.fixture/get-updates.json"
run
# The exit code, not only the text: a mutant that drops the unavailable row
# from `attention()` leaves the sentence in the device summary and turns the
# command green on a machine nobody could ask. Rust catches that; so should
# the binary half.
[[ "$RC" -eq 1 ]] && ok "exit 1, because a machine nobody could ask is not a machine with nothing to do" \
                  || bad "exit was $RC: $(cat "$OUT")"
has '[unavailable] firmware updates' "$OUT" "an unreadable pending list is reported"
has 'did not capture' "$OUT" "with the reason"
hasnt 'no firmware updates are waiting' "$OUT" \
    "and is never rendered as a machine with nothing to update"
no_updates

sec "an Error document is a failure, although fwupd exits zero with one"
cat > "$FIX/.fixture/get-devices.json" <<'EOF'
{"Error": {"Domain": "FwupdError", "Code": 8, "Message": "failed to find deadbeef"}}
EOF
run
[[ "$RC" -eq 2 ]] && ok "exit 2 for a request fwupd refused" || bad "exit was $RC: $(cat "$OUT")"
has 'failed to find deadbeef' "$OUT" "and fwupd's own message reaches the user"
has 'code 8' "$OUT" "with its code"

sec "a document with no Devices list is not a machine with no devices"
printf '{}\n' > "$FIX/.fixture/get-devices.json"
run
[[ "$RC" -eq 2 ]] && ok "exit 2, not a clean empty report" || bad "exit was $RC: $(cat "$OUT")"
has 'neither an Error nor a Devices list' "$OUT" "and says exactly what was wrong"

sec "output that is not JSON at all is reported as such"
# Measured: without --json, fwupdmgr prints `Idle…: 0%` and prose.
printf 'Idle…: 0%%\nfailed to find x\n' > "$FIX/.fixture/get-devices.json"
run
[[ "$RC" -eq 2 ]] && ok "exit 2 for prose where JSON was asked for" || bad "exit was $RC"
has 'did not return JSON' "$OUT" "with the reason"

sec "the JSON surface carries the reasons, not only the values"
devices_on_battery; no_updates
run --json
[[ "$RC" -eq 0 ]] && ok "--json exits 0 on a healthy machine" || bad "exit was $RC: $(cat "$OUT")"
has '"version_unavailable"' "$OUT" "a missing version's reason is a field"
has '"blocked"' "$OUT" "and so is the blocked state"
has '"writable"' "$OUT" "beside the flag fwupd literally set"
has '"secure_boot"' "$OUT" "the stores are their own array"
python3 - "$OUT" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
hw = {x["label"]: x for x in d["hardware"]}
assert len(d["secure_boot"]) == 6, d["secure_boot"]
assert hw["System Firmware"]["writable"]["blocked"], hw["System Firmware"]
assert hw["GPIO controller"]["writable"] == "not-offered", hw["GPIO controller"]
assert hw["06DA:00 04F3:320B"]["writable"] == "now"
assert d["attention"] == [], d["attention"]
assert d["pending"] == []
assert d["pending_unavailable"] is None
PY
[[ "$?" -eq 0 ]] && ok "and the whole document parses with the three states in place" \
                 || bad "the JSON document did not hold up"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[[ "$FAIL" -eq 0 ]] || exit 1
exit 0
