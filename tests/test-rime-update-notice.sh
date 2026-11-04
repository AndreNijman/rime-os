#!/usr/bin/env bash
# test-rime-update-notice.sh — the "new Rime update available" notice.
#
# Drives rime-update-check against a stub bootc and rime-update-notice against a
# stub notify-send. No root, registry or session bus.
#
# Failures it exists to catch: a notice for an update that is not there (or is
# already staged); a repeat notice for the same release; no notice for a newer
# one; a failed check wiping a good answer; the units drifting into something
# that polls, runs at normal priority, or runs the notice as root.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK="$REPO/files/system/libexec/rime-update-check"
NOTICE="$REPO/files/system/libexec/rime-update-notice"
U="$REPO/files/system/units"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s %s\n' "$1" "${2:-}"; fail=$((fail+1)); }
sec() { printf '\n── %s ──\n' "$1"; }
for f in "$CHECK" "$NOTICE" "$U"/rime-update-{check.service,check.timer,notice.service,notice.path}; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
mkdir -p "$W/bin" "$W/home" "$W/run"
D1="sha256:$(printf 'a%.0s' {1..64})"; D2="sha256:$(printf 'b%.0s' {1..64})"

# Stub bootc: `status` prints $W/status, `upgrade --check` prints $W/check, exit $W/check.rc.
cat > "$W/bootc" <<STUB
#!/usr/bin/env bash
case "\$1" in
  status) cat "$W/status" ;;
  upgrade) cat "$W/check"; exit "\$(cat "$W/check.rc" 2>/dev/null || echo 0)" ;;
esac
STUB
chmod +x "$W/bootc"
cat > "$W/bin/notify-send" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$W/notified"
STUB
chmod +x "$W/bin/notify-send"

# Stub ostree: `refs` prints $W/ostree.refs; `show --print-detached-metadata-key=K REF` prints
# $W/ostree.md (the cached manifest digest) or $W/ostree.cfg (the cached config), as ostree prints a
# GVariant string. Stub skopeo prints $W/skopeo.out. Both log their argv.
cat > "$W/ostree" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$W/ostree.calls"
case "\$*" in
  *" refs "*) cat "$W/ostree.refs" 2>/dev/null ;;
  *manifest-digest*) [[ -f "$W/ostree.md" ]] || exit 1; cat "$W/ostree.md" ;;
  *cached.config*) [[ -f "$W/ostree.cfg" ]] || exit 1; cat "$W/ostree.cfg" ;;
esac
STUB
cat > "$W/skopeo" <<STUB
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$W/skopeo.calls"
cat "$W/skopeo.out" 2>/dev/null
STUB
chmod +x "$W/ostree" "$W/skopeo"
run_check()  { RIME_BOOTC_BIN="$W/bootc" RIME_UPDATE_DIR="$W/run" RIME_OSTREE_BIN="$W/ostree" RIME_OSTREE_REPO="$W" \
               RIME_SKOPEO_BIN="$W/skopeo" RIME_RELEASE_JSON="$W/release.json" bash "$CHECK" >/dev/null 2>&1; }
run_notice() { HOME="$W/home" XDG_STATE_HOME="$W/home/state" PATH="$W/bin:$PATH" RIME_UPDATE_STATE="$W/run/state" bash "$NOTICE" >/dev/null 2>&1; }
count() { [[ -f "$W/notified" ]] && wc -l < "$W/notified" || echo 0; }
avail() { printf 'Update available for: docker://ghcr.io/x/rime-os:daily\n  Version: 1\n  Digest: %s\n' "$1" > "$W/check"; echo 0 > "$W/check.rc"; }

sec "check"
echo '{"status":{"staged":null}}' > "$W/status"
avail "$D1"; run_check
grep -q '^state=available$' "$W/run/state" && grep -q "^digest=$D1\$" "$W/run/state" \
    && ok "update available is recorded with its digest" || bad "available"
echo 'No changes in: docker://ghcr.io/x/rime-os:daily' > "$W/check"; run_check
grep -q '^state=none$' "$W/run/state" && ok "no changes -> none" || bad "none"
avail "$D1"; run_check
echo '{"status":{"staged":{"image":{}}}}' > "$W/status"; run_check
grep -q '^state=none$' "$W/run/state" && ok "an already-staged update is not announced" || bad "staged"
echo '{"status":{"staged":null}}' > "$W/status"; avail "$D1"; run_check
echo 'boom' > "$W/check"; echo 1 > "$W/check.rc"; run_check
grep -q '^state=available$' "$W/run/state" && ok "a failed check keeps the last answer" || bad "failed check wiped state"
echo 'weird output' > "$W/check"; echo 0 > "$W/check.rc"; run_check
grep -q '^state=available$' "$W/run/state" && ok "unrecognised output keeps the last answer" || bad "unrecognised"

sec "check: the release on offer and the release running"
# release= / running= (the NixOS checker's keys): the image's org.rimeos.release.id, from ostree's cache
# of the config `bootc upgrade --check` fetched, else from the registry by digest; display data, so
# anything that is not a release ID is written as empty.
calls() { [[ -f "$W/$1.calls" ]] && wc -l < "$W/$1.calls" || echo 0; }
key() { sed -n "s/^$1=//p" "$W/run/state"; }
cfg_json() { printf '{"architecture":"amd64","config":{"Labels":{"containers.bootc":"1","org.rimeos.release.id":"%s"}}}' "$1"; }
# ostree prints a string metadata value as a GVariant text literal.
cfg_variant() { printf "'%s'\n" "$(cfg_json "$1")"; }
reset_lookups() { rm -f "$W/run/state" "$W"/{ostree,skopeo}.calls "$W"/ostree.{refs,md,cfg} "$W/skopeo.out"; }
echo '{"status":{"staged":null}}' > "$W/status"
printf '{\n  "schema": 1,\n  "id": "2026.11.03",\n  "notes": "https://rimeos.com/updates/2026.11.03"\n}\n' > "$W/release.json"

reset_lookups
printf 'docker_3A__2F__2F_ghcr_2E_io/x/apex-os_3A_old\ndocker_3A__2F__2F_ghcr_2E_io/x/rime-os_3A_daily\n' > "$W/ostree.refs"
printf "'%s'\n" "$D1" > "$W/ostree.md"; cfg_variant 2026.11.04.4 > "$W/ostree.cfg"
avail "$D1"; run_check
[[ "$(key state)" == available && "$(key digest)" == "$D1" && "$(key release)" == 2026.11.04.4 ]] \
    && ok "the release on offer comes from ostree's cache of the checked image" || bad "cached release" "$(cat "$W/run/state")"
[[ "$(key running)" == 2026.11.03 ]] && ok "the running release comes from the image's release.json" || bad "running" "$(key running)"
[[ "$(calls skopeo)" == 0 ]] && ok "a cached config costs no registry request" || bad "skopeo called with a cache"
grep -x 'release=2026.11.04.4' "$W/run/state" >/dev/null && [[ "$(head -n1 "$W/run/state")" == state=available ]] \
    && [[ "$(sed -n 2p "$W/run/state")" == "digest=$D1" ]] && ok "the keys keep the NixOS checker's order" || bad "order"

o=$(calls ostree); run_check
[[ "$(calls ostree)" == "$o" && "$(calls skopeo)" == 0 && "$(key release)" == 2026.11.04.4 ]] \
    && ok "the same digest is not looked up again" || bad "looked up again" "ostree $o -> $(calls ostree)"

reset_lookups
printf 'docker_3A__2F__2F_ghcr_2E_io/x/rime-os_3A_daily\n' > "$W/ostree.refs"
printf "'%s'\n" "$D2" > "$W/ostree.md"; cfg_variant 2026.01.01 > "$W/ostree.cfg"   # the cache is for another digest
cfg_json 2026.11.04.5 > "$W/skopeo.out"
avail "$D1"; run_check
[[ "$(key release)" == 2026.11.04.5 ]] && ok "a cache for another digest is not used; the registry answers" || bad "stale cache used" "$(key release)"
grep -F -- "--config docker://ghcr.io/x/rime-os@$D1" "$W/skopeo.calls" >/dev/null \
    && ok "the registry is asked for that digest, without the tag" || bad "skopeo ref" "$(cat "$W/skopeo.calls")"

reset_lookups; cfg_json 2026.11.04.6 > "$W/skopeo.out"
printf 'Update available for: docker://localhost:5000/rime/rime-os:edge\n  Digest: %s\n' "$D2" > "$W/check"; echo 0 > "$W/check.rc"
run_check
grep -F -- "docker://localhost:5000/rime/rime-os@$D2" "$W/skopeo.calls" >/dev/null && [[ "$(key release)" == 2026.11.04.6 ]] \
    && ok "a registry with a port keeps its port" || bad "port" "$(cat "$W/skopeo.calls" 2>/dev/null)"

shapes_bad=0
for v in dev 2026.11.04.1 2026.11.4 ../../updates 'rel"ease' $'2026.11.04\nstate=none' ' 2026.11.04'; do
    reset_lookups; cfg_json "$v" > "$W/skopeo.out"; avail "$D1"; run_check
    [[ "$(key state)" == available && "$(key release)" == "" && "$(grep -c '^state=' "$W/run/state")" == 1 ]] \
        || { bad "a label that is not a release ID is dropped" "$(printf '%q' "$v") -> $(cat "$W/run/state")"; shapes_bad=1; }
done
[[ "$shapes_bad" -eq 0 ]] && ok "a label that is not a release ID is written as empty (7 shapes)"

reset_lookups; printf 'garbage' > "$W/skopeo.out"; avail "$D1"; run_check
[[ "$(key state)" == available && "$(key release)" == "" ]] && ok "an unreadable config still reports the update" || bad "garbage config"

reset_lookups; cfg_json 2026.11.04.4 > "$W/skopeo.out"
printf 'Update available for: docker://ghcr.io/x/rime-os:daily\n' > "$W/check"; echo 0 > "$W/check.rc"; run_check
[[ "$(key digest)" == text:* && "$(key release)" == "" && "$(calls skopeo)" == 0 ]] \
    && ok "no digest in the output: nothing is looked up" || bad "text digest lookup"

reset_lookups
printf 'Update available for: oci:/var/tmp/img\n  Digest: %s\n' "$D1" > "$W/check"; run_check
[[ "$(key state)" == available && "$(key release)" == "" && "$(calls skopeo)" == 0 ]] \
    && ok "a non-registry image is not sent to the registry" || bad "oci transport"

reset_lookups; echo 'No changes in: docker://ghcr.io/x/rime-os:daily' > "$W/check"; run_check
[[ "$(key state)" == none && "$(key release)" == "" && "$(key running)" == 2026.11.03 ]] \
    && ok "none: no release on offer, the running one still named" || bad "none keys" "$(cat "$W/run/state")"

printf '{"schema": 1, "id": "dev", "notes": ""}' > "$W/release.json"; run_check
[[ "$(key running)" == "" ]] && ok "a dev image names no running release" || bad "dev running"
rm -f "$W/release.json"; run_check
[[ "$(key running)" == "" && "$(key state)" == none ]] && ok "no release.json: running is empty" || bad "no release.json"

reset_lookups; printf 'docker_3A__2F__2F_ghcr_2E_io/x/rime-os_3A_daily\n' > "$W/ostree.refs"
printf "'%s'\n" "$D1" > "$W/ostree.md"; cfg_variant 2026.11.04.4 > "$W/ostree.cfg"
avail "$D1"
RIME_BOOTC_BIN="$W/bootc" RIME_UPDATE_DIR="$W/run" RIME_OSTREE_BIN="$W/nonexistent" RIME_SKOPEO_BIN="$W/nonexistent" \
    RIME_RELEASE_JSON="$W/release.json" bash "$CHECK" >/dev/null 2>&1
[[ "$(key state)" == available && "$(key release)" == "" ]] && ok "without ostree or skopeo the update is still reported" || bad "no tools"

sec "notice"
rm -f "$W/notified"
echo 'state=none' > "$W/run/state"; run_notice
[[ "$(count)" == 0 ]] && ok "nothing to say -> silent" || bad "silent"
printf 'state=available\ndigest=%s\n' "$D1" > "$W/run/state"; run_notice
[[ "$(count)" == 1 ]] && grep -q 'sudo rime update' "$W/notified" && ok "announces once and names the command" || bad "first notice"
run_notice; run_notice
[[ "$(count)" == 1 ]] && ok "same release is never repeated" || bad "repeat"
printf 'state=available\ndigest=%s\n' "$D2" > "$W/run/state"; run_notice
[[ "$(count)" == 2 ]] && ok "a newer release is announced again" || bad "newer"
printf 'state=available\ndigest=%s\nrelease=2026.11.04.4\nrunning=2026.11.03\n' "$D2" > "$W/run/state"; run_notice
[[ "$(count)" == 2 ]] && ok "release= and running= do not make the same digest new" || bad "new keys re-announced"
printf 'state=available\n' > "$W/run/state"; rm -f "$W/home/state/rime/update-notice.seen"; run_notice
[[ "$(count)" == 2 ]] && ok "a partial write is ignored" || bad "partial"
rm -f "$W/run/state"; run_notice; [[ "$(count)" == 2 ]] && ok "missing state file is silent" || bad "missing"

sec "units"
grep -q '^OnBootSec=' "$U/rime-update-check.timer" && grep -q '^OnUnitActiveSec=' "$U/rime-update-check.timer" \
    && ok "check runs after boot and on an interval" || bad "timer"
grep -q '^CPUSchedulingPolicy=idle' "$U/rime-update-check.service" && grep -q '^IOSchedulingClass=idle' "$U/rime-update-check.service" \
    && grep -q '^Nice=19' "$U/rime-update-check.service" && ok "check runs at idle priority" || bad "priority"
grep -q '^PathChanged=/run/rime-update/state' "$U/rime-update-notice.path" && ok "notice is woken by inotify, not polling" || bad "path"
! grep -qE '^(User|OnCalendar|OnUnitActiveSec)=' "$U/rime-update-notice.service" "$U/rime-update-notice.path" \
    && ok "notice units poll nothing" || bad "notice polls"

sec "live update results"
rm -f "$W/run/state" "$W/notified"
run_live() { HOME="$W/home" XDG_STATE_HOME="$W/home/state" PATH="$W/bin:$PATH" RIME_UPDATE_STATE="$W/run/state" RIME_LIVE_STATUS="$W/run/live.json" bash "$NOTICE" >/dev/null 2>&1; }
live_doc() { printf '{"schema":1,"txn":"%s","state":"%s","summary":"%s","components":[{"component":"shell","state":"%s"}]}' "$1" "$2" "$3" "${4:-active}" > "$W/run/live.json"; }
sent_n() { [[ -f "$W/notified" ]] && wc -l < "$W/notified" || echo 0; }
live_doc t1 planned "plan only"; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "a plan is not announced" || bad "plan announced"
live_doc t1 deferred "everything waits for a restart"; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "a deferred update is not announced" || bad "deferred announced"
live_doc t1 active "Up to date." pending; run_live
[[ "$(sent_n)" -eq 0 ]] && ok "active with nothing activated is not announced" || bad "empty active announced"
live_doc t2 active "Rime Shell is active now."; run_live
[[ "$(sent_n)" -eq 1 ]] && grep -q 'Rime update applied' "$W/notified" && grep -q 'Rime Shell is active now.' "$W/notified" \
    && ok "an applied update is announced in the engine's words" || bad "active" "$(cat "$W/notified" 2>/dev/null)"
run_live
[[ "$(sent_n)" -eq 1 ]] && ok "once per transaction" || bad "repeat"
live_doc t3 failed "Live activation failed; see rime live doctor." failed; run_live
[[ "$(sent_n)" -eq 2 ]] && grep -q 'rime live doctor' "$W/notified" && ok "a failure names rime live doctor" || bad "failed" "$(cat "$W/notified")"
printf '{"schema":2,"txn":"t4","state":"active","summary":"x"}' > "$W/run/live.json"; run_live
printf 'not json' > "$W/run/live.json"; run_live
[[ "$(sent_n)" -eq 2 ]] && ok "unknown schema or garbage is ignored" || bad "garbage"
grep -q '^PathChanged=/run/rime-live/status.json' "$U/rime-update-notice.path" && ok "notice is woken by a live result" || bad "live path"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
