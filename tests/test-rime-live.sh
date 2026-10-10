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
#  Runs inside `unshare -Ur` (uid 0 in a user namespace) so the builder can
#  chown and create whiteouts. Nothing outside a temp directory is touched;
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
    if ! unshare -Ur true 2>/dev/null; then
        echo "SKIP: user namespaces are not available here"
        exit 0
    fi
    exec env RIME_LIVE_IN_NS=1 RIME_BIN="$RIME_BIN" unshare -Ur "$(pwd)/$(basename "$0")" "$@"
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
case "\$1" in
  status) cat "$T/bootc-status.json" ;;
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
    rm -rf "$R" "$T/calls" "$T/helper-rc" "$T/sysext.fail"
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
"staged":{"image":{"image":{"image":"ghcr.io/andrenijman/rime-os:daily","transport":"registry"},"imageDigest":"sha256:$(printf c%.0s $(seq 64))"},"ostree":{"checksum":"$S","deploySerial":0,"stateroot":"default"},"downloadOnly":false,"softRebootCapable":true},"rollback":null}}
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

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
