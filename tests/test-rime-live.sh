#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-live.sh — the live-update engine end to end, through the real
#  `rime` binary, against a fake sysroot.
#
#  What runs for real: the planner, the transaction record and audit log, the
#  live layer builder (copies, owners, modes, whiteouts, extension-release),
#  file verification after the merge, the activators' ordering, rollback, and
#  the status document the shell reads.
#
#  What is stubbed (on PATH): bootc, ostree, rpm, systemctl, systemd-sysext,
#  loginctl, systemd-run. The sysext stub "merges" by rebuilding the fake /usr
#  from the booted tree plus the layer, honouring whiteouts, so the engine's
#  own post-merge verification reads real files.
#
#  Runs inside `unshare -Urm` (uid 0 in a user and mount namespace) so the
#  builder can chown and create whiteouts, and so `rime update`'s absolute
#  helpers (/usr/bin/skopeo, the boot-path migration engine) can be replaced
#  by stubs for this process only: the image signature check then fails
#  closed, never reaching a registry. Nothing outside a temp directory is touched;
#  `RIME_LIVE_ROOT` prefixes every path the engine uses.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2
REPO="$(cd .. && pwd)"

if [ "${RIME_LIVE_IN_NS:-0}" != 1 ]; then
    RIME_BIN=${RIME_BIN:-$REPO/rimed/target/debug/rime}
    if [ ! -x "$RIME_BIN" ]; then
        echo "building the rime binary (not found at $RIME_BIN)…"
        ( cd "$REPO/rimed" && cargo build --locked --bin rime ) || { echo "FATAL: build failed"; exit 2; }
    fi
    command -v unshare >/dev/null || { echo "SKIP: no unshare"; exit 0; }
    if ! unshare -Urm true 2>/dev/null; then
        echo "SKIP: user namespaces are not available here"
        exit 0
    fi
    exec env RIME_LIVE_IN_NS=1 RIME_BIN="$RIME_BIN" unshare -Urm "$(pwd)/$(basename "$0")" "$@"
fi

T=$(mktemp -d /tmp/rime-live-test.XXXXXX)
trap 'rm -rf "$T"' EXIT
R="$T/root"
B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
S=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
BD="$R/sysroot/ostree/deploy/default/deploy/$B.0"
SD="$R/sysroot/ostree/deploy/default/deploy/$S.0"

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }

mkdir -p "$T/bin"
cat > "$T/bin/bootc" <<STUB
#!/bin/sh
echo "bootc \$*" >> "$T/calls"
# The staged deployment is download-only until --from-downloaded unlocks it.
case "\$*" in
  *--download-only*) echo true > "$T/dlonly" ;;
  *--from-downloaded*) echo false > "$T/dlonly" ;;
esac
case "\$1" in
  status) sed "s/@DLONLY@/\$(cat "$T/dlonly" 2>/dev/null || echo false)/; s/@SOFT@/\$(cat "$T/soft" 2>/dev/null || echo true)/" "$T/bootc-status.json" ;;
esac
STUB
cat > "$T/bin/ostree" <<STUB
#!/bin/sh
echo "ostree \$*" >> "$T/calls"
cat "$T/ostree-diff"
STUB
cat > "$T/bin/rpm" <<STUB
#!/bin/sh
case "\$*" in *"$S"*) cat "$T/rpm-staged" ;; *) cat "$T/rpm-booted" ;; esac
STUB
cat > "$T/bin/systemctl" <<STUB
#!/bin/sh
echo "systemctl \$*" >> "$T/calls"
case "\$*" in
  *list-units*) printf 'rimed.service loaded active running Rime\n' ;;
  *"is-active rimed.service"*) echo active ;;
  *"show -p MainPID --value rimed.service"*) echo 4242 ;;
esac
STUB
cat > "$T/bin/systemd-sysext" <<STUB
#!/bin/sh
echo "systemd-sysext \$*" >> "$T/calls"
[ -f "$T/sysext.fail" ] && exit 1
# A merge: /usr = booted /usr + the layer, whiteouts removing.
rm -rf "$R/usr"; cp -a "$BD/usr" "$R/usr"
L="$R/run/extensions/rime-live"
[ -d "\$L/usr" ] || exit 0
( cd "\$L" && find usr -mindepth 1 ) | while read -r p; do
  if [ -c "\$L/\$p" ]; then rm -rf "$R/\$p"
  elif [ -d "\$L/\$p" ] && [ ! -L "\$L/\$p" ]; then mkdir -p "$R/\$p"
  else rm -rf "$R/\$p"; cp -a "\$L/\$p" "$R/\$p"; fi
done
STUB
cat > "$T/bin/loginctl" <<STUB
#!/bin/sh
case "\$1" in
  list-sessions) echo "4 1000 andre seat0 2684 user tty1 no -" ;;
  show-session) printf 'Type=wayland\nClass=user\nRemote=no\nState=active\nDesktop=hyprland\nService=greetd\nName=andre\nUser=1000\nLockedHint=%s\n' "\$(cat "$T/locked")" ;;
esac
STUB
cat > "$T/bin/systemd-run" <<STUB
#!/bin/sh
echo "systemd-run \$*" >> "$T/calls"
# helper-rc applies to the new revision only: putting the old shell back ("-") works.
case "\$*" in *" shell - "*|*" hypr - "*) rc=0 ;; *) rc=\$(cat "$T/helper-rc" 2>/dev/null || echo 0) ;; esac
echo "running shell revision new-shell (pid 77)"
exit \$rc
STUB
chmod +x "$T"/bin/*
export PATH="$T/bin:$PATH" RIME_LIVE_ROOT="$R"

mkfile() { mkdir -p "$(dirname "$1")"; printf '%s' "$2" > "$1"; }

setup() {
    rm -rf "$R" "$T/calls" "$T/helper-rc" "$T/sysext.fail" "$T/dlonly" "$T/soft"
    mkdir -p "$R/proc/sys/kernel/random" "$R/proc/4242" "$R/var/lib/rime/pkg" "$R/run/extensions"
    echo "boot-1" > "$R/proc/sys/kernel/random/boot_id"
    for d in "$BD" "$SD"; do
        mkfile "$d/usr/lib/os-release" $'ID=fedora\nVERSION_ID=45\n'
        mkfile "$d/usr/share/rime-shell/shell.qml" "old shell"
        mkfile "$d/usr/share/rime-shell/src/Gone.qml" "old"
        mkfile "$d/usr/share/rime-shell/.rime-shell-commit" "old-shell"
        mkfile "$d/usr/bin/rimed" "old rimed"
        mkfile "$d/usr/share/rime/release.json" '{"id":"2026.10.10"}'
        mkdir -p "$d/usr/lib/sysimage/rpm"
    done
    mkfile "$SD/usr/share/rime-shell/shell.qml" "new shell"
    mkfile "$SD/usr/share/rime-shell/src/New.qml" "new"
    rm "$SD/usr/share/rime-shell/src/Gone.qml"
    mkfile "$SD/usr/share/rime-shell/.rime-shell-commit" "new-shell"
    mkfile "$SD/usr/share/rime/release.json" '{"id":"2026.10.11"}'
    chmod 0755 "$SD/usr/bin/rimed"
    cp -a "$BD/usr" "$R/usr"
    ln -s /usr/bin/rimed "$R/proc/4242/exe"
    echo no > "$T/locked"
    printf 'systemd 0:262-3.fc45 x86_64\n' > "$T/rpm-booted"
    printf 'systemd 0:262-3.fc45 x86_64\n' > "$T/rpm-staged"
    cat > "$T/ostree-diff" <<EOF
M    /usr/share/rime-shell/shell.qml
M    /usr/share/rime-shell/.rime-shell-commit
A    /usr/share/rime-shell/src/New.qml
D    /usr/share/rime-shell/src/Gone.qml
M    /usr/share/rime/release.json
EOF
    cat > "$T/bootc-status.json" <<EOF
{"status":{"booted":{"image":{"image":{"image":"ghcr.io/andrenijman/rime-os:daily","transport":"registry"},"imageDigest":"sha256:$(printf b%.0s $(seq 64))"},"ostree":{"checksum":"$B","deploySerial":0,"stateroot":"default"},"downloadOnly":false,"softRebootCapable":true},
"staged":{"image":{"image":{"image":"ghcr.io/andrenijman/rime-os:daily","transport":"registry"},"imageDigest":"sha256:$(printf c%.0s $(seq 64))"},"ostree":{"checksum":"$S","deploySerial":0,"stateroot":"default"},"downloadOnly":@DLONLY@,"softRebootCapable":@SOFT@},"rollback":null}}
EOF
    # What `rime update` leaves after verifying the staged digest.
    mkdir -p "$R/var/lib/rime/live"
    cat > "$R/var/lib/rime/live/txn.json" <<EOF
{"schema":1,"id":"t0","boot_id":"boot-1","state":"deferred","booted_digest":"sha256:$(printf b%.0s $(seq 64))",
 "target_digest":"sha256:$(printf c%.0s $(seq 64))","target_release":null,"plan":null,"layer":null,"previous_layer":null,
 "outcomes":{},"history":[{"state":"discovered","at":1,"note":""},{"state":"verified","at":2,"note":""},
 {"state":"staged","at":3,"note":""},{"state":"planned","at":4,"note":""},{"state":"deferred","at":5,"note":""}]}
EOF
}

apply() { "$RIME_BIN" live apply > "$T/out" 2>&1; echo $?; }
jq_() { python3 -I -c "import json,sys; d=json.load(open('$R/run/rime-live/status.json')); print(eval(sys.argv[1]))" "$1"; }
comp_state() { jq_ "[c['state'] for c in d['components'] if c['component']=='$1'][0]"; }
L="$R/run/extensions/rime-live"

# 1. A shell-only release in an unlocked session activates live.
setup
rc=$(apply)
if [ "$rc" = 0 ] && [ "$(cat "$R/usr/share/rime-shell/shell.qml")" = "new shell" ]; then ok "shell release: new files are what /usr reads"; else bad "shell release" "rc=$rc $(cat "$T/out")"; fi
[ ! -e "$R/usr/share/rime-shell/src/Gone.qml" ] && [ -c "$L/usr/share/rime-shell/src/Gone.qml" ] && ok "removed file is an overlay whiteout" || bad "whiteout" "$(ls -l "$L/usr/share/rime-shell/src" 2>&1)"
[ ! -e "$L/usr/share/rime/release.json" ] && ok "release.json never enters the live layer" || bad "release.json in layer" ""
grep -q '^ID=fedora$' "$L/usr/lib/extension-release.d/extension-release.rime-live" && grep -q '^VERSION_ID=45$' "$L/usr/lib/extension-release.d/extension-release.rime-live" && ok "extension-release matches the booted OS" || bad "extension-release" ""
grep -q 'systemd-run --user --machine=andre@.host .*rime-live-session shell new-shell 4' "$T/calls" && ok "shell replaced through the session helper with the staged revision" || bad "helper call" "$(grep systemd-run "$T/calls")"
[ "$(jq_ "d['state']")" = active ] && [ "$(comp_state shell)" = active ] && ok "status.json: active, shell active" || bad "status" "$(cat "$R/run/rime-live/status.json")"
[ "$(stat -c %a "$R/run/rime-live/status.json")" = 644 ] && [ "$(stat -c %a "$R/var/lib/rime/live/txn.json")" = 600 ] && ok "status world-readable, record root-only" || bad "modes" ""
grep -q '"state":"active"' "$R/var/lib/rime/live/history.jsonl" && ok "audit log records the transaction" || bad "history" ""
grep -q 'bootc upgrade' "$T/calls" && bad "apply must not stage or queue" "$(grep bootc "$T/calls")" || ok "apply never runs bootc upgrade"

# 2. Live rollback removes the layer and puts the old shell back.
"$RIME_BIN" live rollback > "$T/out" 2>&1; rc=$?
if [ "$rc" = 0 ] && [ ! -e "$L" ] && [ "$(cat "$R/usr/share/rime-shell/shell.qml")" = "old shell" ]; then ok "rime live rollback restores the booted files"; else bad "rollback" "rc=$rc $(cat "$T/out")"; fi
grep -q 'rime-live-session shell - 4' "$T/calls" && ok "rollback restarts the old shell leniently" || bad "rollback helper" "$(grep systemd-run "$T/calls")"
[ "$(jq_ "d['state']")" = rolled-back ] && ok "status.json: rolled-back" || bad "rollback status" ""

# 3. Locked session: the shell is deferred, nothing is put in the layer.
setup; echo yes > "$T/locked"
rc=$(apply)
[ "$rc" = 0 ] && [ "$(comp_state shell)" = deferred ] && [ ! -e "$L/usr/share/rime-shell" ] && ! grep -q systemd-run "$T/calls" 2>/dev/null \
    && ok "locked: shell deferred, layer untouched, helper never run" || bad "locked" "rc=$rc $(cat "$T/out")"
[ "$(cat "$R/usr/share/rime-shell/shell.qml")" = "old shell" ] && ok "locked: /usr still reads the booted shell" || bad "locked files" ""

# 4. The helper fails: everything is undone.
setup; echo 1 > "$T/helper-rc"
rc=$(apply)
if [ "$rc" = 1 ] && [ ! -e "$L" ] && [ "$(cat "$R/usr/share/rime-shell/shell.qml")" = "old shell" ] && [ "$(comp_state shell)" = failed-rolled-back ]; then ok "failed activation rolls back"; else bad "rollback on failure" "rc=$rc $(cat "$T/out")"; fi
[ "$(jq_ "d['state']")" = rolled-back ] && ok "status.json: rolled-back after failure" || bad "fail status" "$(jq_ "d['state']")"

# 5. The helper reports the session locked at the last moment: deferred, undone, exit 0.
setup; echo 3 > "$T/helper-rc"
rc=$(apply)
[ "$rc" = 0 ] && [ ! -e "$L/usr/share/rime-shell" ] && [ "$(comp_state shell)" = deferred ] && ok "late lock: deferred and the shell files withdrawn" || bad "late lock" "rc=$rc $(cat "$T/out")"

# 6. The sysext merge fails: nothing claimed.
setup; : > "$T/sysext.fail"
rc=$(apply)
[ "$rc" = 1 ] && [ "$(comp_state shell)" != active ] && ok "merge failure is reported, not claimed" || bad "sysext fail" "rc=$rc"

# 7. rimed: daemon-reload, restart, and its running binary checked.
setup
mkfile "$SD/usr/bin/rimed" "new rimed"
printf 'M    /usr/bin/rimed\nM    /usr/share/rime/release.json\n' > "$T/ostree-diff"
rc=$(apply)
if [ "$rc" = 0 ] && grep -q 'systemctl restart rimed.service' "$T/calls" && [ "$(comp_state rime-daemon)" = active ]; then ok "rimed restarted live and verified"; else bad "rimed" "rc=$rc $(cat "$T/out")"; fi

# 8. A Rime binary whose libraries cannot be proven is deferred, with its dependents.
setup
cp /bin/true "$SD/usr/bin/rime"; cp /bin/true "$BD/usr/bin/rime"; printf 'x' >> "$SD/usr/bin/rime"
printf 'M    /usr/bin/rime\nM    /usr/share/rime-shell/shell.qml\n' > "$T/ostree-diff"
rc=$(apply)
[ "$(comp_state rime-tools)" = deferred ] && [ "$(comp_state shell)" = deferred ] && [ ! -e "$L/usr/bin/rime" ] \
    && ok "unproven libraries defer the tools and the shell that needs them" || bad "elf" "rc=$rc $(cat "$T/out")"

# 9. Kernel and systemd changes are reported with what they need, never acted on.
setup
printf 'M    /usr/lib/modules/7.2.10/vmlinuz\nM    /usr/lib/systemd/systemd\n' > "$T/ostree-diff"
mkfile "$SD/usr/lib/modules/7.2.10/vmlinuz" k; mkfile "$SD/usr/lib/systemd/systemd" s
rc=$(apply)
[ "$(comp_state kernel)" = pending ] && [ "$(comp_state systemd)" = pending ] && [ "$(jq_ "d['remaining']")" = kernel-transition ] \
    && ok "kernel/systemd: pending, remaining = kernel-transition" || bad "pending" "$(cat "$R/run/rime-live/status.json")"
grep -qE 'soft-reboot|reboot|kexec' "$T/calls" && bad "a disruptive command ran" "$(cat "$T/calls")" || ok "no reboot, soft-reboot or kexec was run"

# 10. Only a deployment `rime update` verified in this boot is ever activated.
setup; echo "boot-2" > "$R/proc/sys/kernel/random/boot_id"
rc=$(apply)
[ "$rc" = 1 ] && [ ! -e "$L" ] && grep -q 'not verified' "$T/out" && ok "unverified (other boot) staged deployment refused" || bad "verify gate" "rc=$rc $(cat "$T/out")"
setup; sed -i 's/cccc/dddd/' "$R/var/lib/rime/live/txn.json"
rc=$(apply)
[ "$rc" = 1 ] && [ ! -e "$L" ] && ok "a different staged digest is refused" || bad "digest gate" "rc=$rc"

# 11. An interrupted activation is undone on the next run.
setup
apply >/dev/null
python3 -I - "$R/var/lib/rime/live/txn.json" <<'EOF'
import json,sys
p=sys.argv[1]; t=json.load(open(p)); t["state"]="activating"; json.dump(t,open(p,"w"))
EOF
"$RIME_BIN" live apply --only rime-tools > "$T/out" 2>&1
grep -q '"state":"rolled-back"' "$R/var/lib/rime/live/history.jsonl" && grep -q 'interrupted mid-activation' "$T/out" \
    && ok "crash recovery undoes an interrupted activation" || bad "recovery" "$(cat "$T/out")"

# 12. status --json is the document, for anyone.
"$RIME_BIN" live status --json | python3 -I -c 'import json,sys; d=json.load(sys.stdin); assert d["schema"]==1' && ok "rime live status --json" || bad "status json" ""

# ── `rime update` itself ─────────────────────────────────────────────────────
# Both absolute helpers it may run are replaced, for this namespace only.
printf '#!/bin/sh\necho "skopeo stub: no registry in this test" >&2\nexit 1\n' > "$T/skopeo"
printf '#!/bin/sh\nexit 10\n' > "$T/boot-migrate"
chmod +x "$T/skopeo" "$T/boot-migrate"
[ -e /usr/bin/skopeo ] && mount --bind "$T/skopeo" /usr/bin/skopeo
[ -e /usr/libexec/rime-boot-migrate ] && mount --bind "$T/boot-migrate" /usr/libexec/rime-boot-migrate
export HOME="$T/home"; mkdir -p "$HOME"
upd() { "$RIME_BIN" update --force --fsync --skip-packages --skip-flatpak --skip-firmware "$@" > "$T/out" 2>&1; echo $?; }
nolayer_setup() { setup; rm -f "$R/var/lib/rime/live/txn.json"; }
bootc_seq() { grep '^bootc upgrade' "$T/calls" | sed 's/^bootc upgrade *//' | tr '\n' '|'; }

# 13. No signature, no --allow-unverified: nothing is pulled.
nolayer_setup
rc=$(upd)
[ "$rc" != 0 ] && ! grep -q '^bootc upgrade' "$T/calls" && [ ! -e "$L" ] && ok "update: unverifiable image refused before any pull" || bad "update refusal" "rc=$rc $(bootc_seq) $(tail -3 "$T/out")"

# 14. --plan: downloads to measure, never queues, never activates.
nolayer_setup
rc=$(upd --plan --allow-unverified)
[ "$rc" = 0 ] && [ "$(bootc_seq)" = "--download-only|" ] && [ ! -e "$L" ] && ! grep -q systemd-run "$T/calls" \
    && ok "update --plan: download-only, nothing queued or activated" || bad "plan" "rc=$rc seq=$(bootc_seq) $(tail -3 "$T/out")"
grep -q 'Rime Shell' "$T/out" && ok "update --plan prints the per-component plan" || bad "plan output" "$(cat "$T/out")"

# 15. --no-live: verified and queued for boot, nothing live.
nolayer_setup
rc=$(upd --no-live --allow-unverified)
[ "$rc" = 0 ] && [ "$(bootc_seq)" = "--download-only|--from-downloaded|" ] && [ ! -e "$L/usr/share/rime-shell" ] \
    && [ "$(jq_ "d['staged_for_boot']")" = True ] && ok "update --no-live: queued for boot, no live layer" || bad "no-live" "rc=$rc seq=$(bootc_seq) $(tail -3 "$T/out")"

# 16. --live-only: activates now, leaves the deployment download-only.
nolayer_setup
rc=$(upd --live-only --allow-unverified)
[ "$rc" = 0 ] && [ "$(bootc_seq)" = "--download-only|" ] && [ "$(cat "$R/usr/share/rime-shell/shell.qml")" = "new shell" ] \
    && [ "$(jq_ "d['staged_for_boot']")" = False ] && ok "update --live-only: live, not queued" || bad "live-only" "rc=$rc seq=$(bootc_seq) $(tail -3 "$T/out")"

# 17. Default: download, verify, queue, then activate, in that order.
nolayer_setup
rc=$(upd --allow-unverified)
order="$(grep -nE '^bootc upgrade|^systemd-run' "$T/calls" | sed 's/:.*rime-live-session shell.*/:helper/; s/:bootc upgrade /:/' | cut -d: -f2 | tr '\n' ' ')"
[ "$rc" = 0 ] && [ "$order" = "--download-only --from-downloaded helper " ] && [ "$(comp_state shell)" = active ] \
    && ok "update: download → verify → queue → activate" || bad "update order" "rc=$rc order=$order $(tail -3 "$T/out")"

# 18. An overridden verification is recorded as such and never trusted later.
[ "$(jq_ "d['verified']")" = False ] && grep -q 'did NOT verify' "$R/var/lib/rime/live/history.jsonl" \
    && ok "--allow-unverified is recorded, not called verified" || bad "override record" "$(jq_ "d.get('verified')")"
"$RIME_BIN" live rollback >/dev/null 2>&1
rc=$(apply)
[ "$rc" = 1 ] && [ ! -e "$L" ] && ok "live apply refuses a deployment that only --allow-unverified let through" || bad "override trusted" "rc=$rc $(cat "$T/out")"

# 19. Soft reboot: only on request, only verified, only when bootc says it can.
setup
"$RIME_BIN" live soft-reboot > "$T/out" 2>&1; rc=$?
[ "$rc" = 2 ] && ! grep -q -- '--apply' "$T/calls" 2>/dev/null && grep -q -- '--yes' "$T/out" && ok "soft-reboot without --yes explains and does nothing" || bad "soft-reboot confirm" "rc=$rc $(cat "$T/out")"
setup; echo false > "$T/soft"
"$RIME_BIN" live soft-reboot --yes > "$T/out" 2>&1; rc=$?
[ "$rc" = 1 ] && ! grep -q -- '--apply' "$T/calls" 2>/dev/null && ok "soft-reboot refused when bootc says it cannot apply the update" || bad "soft-reboot capable" "rc=$rc $(cat "$T/out")"
setup; echo "boot-2" > "$R/proc/sys/kernel/random/boot_id"
"$RIME_BIN" live soft-reboot --yes > "$T/out" 2>&1; rc=$?
[ "$rc" = 1 ] && ! grep -q -- '--apply' "$T/calls" 2>/dev/null && ok "soft-reboot refused for an unverified deployment" || bad "soft-reboot verify" "rc=$rc"
setup; apply >/dev/null; : > "$T/calls"
"$RIME_BIN" live soft-reboot --yes > "$T/out" 2>&1; rc=$?
[ "$rc" = 0 ] && [ ! -e "$L" ] && grep -qx 'bootc upgrade --apply --soft-reboot=required' "$T/calls" \
    && ok "soft-reboot --yes: live layer removed, bootc applies the queued deployment" || bad "soft-reboot" "rc=$rc $(cat "$T/calls") $(cat "$T/out")"
grep -q '"state":"superseded"' "$R/var/lib/rime/live/history.jsonl" && ok "soft-reboot closes the transaction" || bad "soft-reboot txn" ""

# 20. After a soft reboot (same boot id, another deployment) a stale layer is dropped.
setup; apply >/dev/null
sed -i "s/\"checksum\":\"$B\"/\"checksum\":\"$S\"/; s/\"imageDigest\":\"sha256:b\{64\}\"/\"imageDigest\":\"sha256:$(printf e%.0s $(seq 64))\"/" "$T/bootc-status.json"
"$RIME_BIN" live apply > "$T/out" 2>&1
[ ! -e "$L" ] && grep -q 'another deployment' "$R/var/lib/rime/live/history.jsonl" && ok "a new deployment in the same boot supersedes the old layer" || bad "soft-reboot recovery" "$(cat "$T/out")"

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
