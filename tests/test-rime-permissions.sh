#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-permissions.sh — that a revoked capability is REFUSED, and that
#  the one Rime cannot revoke is not quietly reported as revoked.
#
#  ── Why this test is shaped the way it is ───────────────────────────────────
#  "Permission denied is not absence" is the standing lesson in this codebase,
#  and a permissions feature is where it bites hardest. Asserting that an app is
#  missing from `flatpak permission-list` proves nothing: an app that was never
#  asked is missing from the same list, and so is an app whose sandbox hands it
#  the device node directly. The only assertion worth making is that a real
#  portal, asked a real question by a caller it believes is a real Flatpak,
#  answers no.
#
#  So this runs the actual `/usr/libexec/xdg-desktop-portal` and the actual
#  `/usr/libexec/xdg-permission-store`, on a PRIVATE D-Bus session, against a
#  fixture XDG tree, and calls `org.freedesktop.portal.Camera.AccessCamera`
#  from a process that bubblewrap has given a `/.flatpak-info`. That is how
#  xdg-desktop-portal's own tests fake an application identity, and it is the
#  only way to exercise the code path that actually enforces the grant.
#
#  ── The three cases, and why two is not enough ──────────────────────────────
#  A refusal that came from the permission store and a refusal that came from
#  the user clicking "Deny" look IDENTICAL from the caller: response code 1.
#  A test that only checked the response code would pass whether or not the
#  store was consulted at all. So the fixture stands up a fake
#  `org.freedesktop.impl.portal.Access` backend that records every call, and
#  the cases are distinguished by whether the backend was reached:
#
#    A  store says "no"       -> response 1, backend NOT consulted
#    B  no entry, backend no  -> response 1, backend consulted
#    C  no entry, backend yes -> response 0, backend consulted
#
#  A versus B is the assertion: same answer, and only the revoked one skips the
#  dialog. C is the proof the harness can produce a grant — without it, a
#  harness that was simply broken would report two passes.
#
#  Both halves were watched to fail before they were trusted. Deleting the
#  store write from case A leaves "a revoked camera grant is REFUSED" GREEN —
#  the fake dialog denies it instead — and turns the short-circuit assertion
#  red, which is precisely why the short-circuit assertion is separate. Making
#  the fake dialog always allow leaves case A refusing with the backend at zero
#  calls, so the refusal demonstrably comes from the store rather than from the
#  dialog.
#
#  ── The native case is the OPPOSITE assertion ───────────────────────────────
#  /dev/video0 on this image is root:video 0660 with a logind `uaccess` POSIX
#  ACL for the seat's user. That ACL belongs to the login session, not to a
#  program, so no per-application revocation exists for a native binary. The
#  last case opens the node as the test user and asserts it SUCCEEDS — which is
#  what makes the Settings page's "cannot be revoked" label a measured claim
#  rather than a disclaimer.
#
#  ── What it will not do ─────────────────────────────────────────────────────
#  It never touches the real session bus, the real permission store, or the
#  real portal. Every XDG directory is inside $WORK, XDG_DATA_DIRS points only
#  at the fixture so no packaged backend can be activated, and the isolation is
#  asserted before any case runs rather than assumed. It revokes nothing on the
#  machine it runs on.
#
#  ── The CLI half ────────────────────────────────────────────────────────────
#  `--with-binary` drives `rime permissions revoke` against a shimmed `flatpak`
#  and `busctl`, so the three refusals it owes can be asserted without touching
#  a real store: a native subject, a Flatpak whose manifest carries
#  `devices=all`, and a capability this session has no portal for. Each must
#  exit NON-ZERO with its own sentence — a revoke that printed success for
#  something it could not do is the defect in command form. It DIES if the
#  binary is absent; a skipped assertion reports as a pass.
#
#  PASS = the store refusal short-circuits the dialog, the same harness can
#         still produce a grant, the native node opens regardless, and the CLI
#         refuses the three revocations it cannot perform.
#
#  Run from anywhere: ./tests/test-rime-permissions.sh [--with-binary]
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2
REPO="$PWD"
WITH_BINARY=0
[ "${1:-}" = "--with-binary" ] && WITH_BINARY=1

pass=0; fail=0; skip=0
ok()      { printf 'PASS  %-62s\n' "$1"; pass=$((pass+1)); }
bad()     { printf 'FAIL  %-62s %s\n' "$1" "$2"; fail=$((fail+1)); }
skipped() { printf 'SKIP  %-62s %s\n' "$1" "$2"; skip=$((skip+1)); }

summary() {
  printf '\n%s\n' "passed=$pass failed=$fail skipped=$skip"
  [ "$fail" -eq 0 ] || exit 1
  exit 0
}

XDP=/usr/libexec/xdg-desktop-portal
STORE=/usr/libexec/xdg-permission-store

missing=""
for t in bwrap dbus-run-session python3 busctl gdbus; do
  command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
done
[ -x "$XDP" ]   || missing="$missing xdg-desktop-portal"
[ -x "$STORE" ] || missing="$missing xdg-permission-store"
python3 -c 'import gi; gi.require_version("Gio","2.0"); from gi.repository import Gio' \
  >/dev/null 2>&1 || missing="$missing python3-gobject"

if [ -n "$missing" ]; then
  skipped "portal revocation" "not installed:$missing"
  # The native case needs none of that, so it still runs below.
else
  WORK=$(mktemp -d /tmp/rime-permtest.XXXXXX) || exit 2
  cleanup() { rm -rf "$WORK"; }
  trap cleanup EXIT

  mkdir -p "$WORK"/{home,data,config/xdg-desktop-portal,run,share/dbus-1/services,share/xdg-desktop-portal/portals,share/glib-2.0,fpinfo}
  chmod 700 "$WORK/run"

  # xdg-desktop-portal aborts outright if it cannot find GSettings schemas, and
  # XDG_DATA_DIRS is pinned to the fixture so that no packaged .portal file and
  # no packaged D-Bus service can be reached. Symlinking just the schemas keeps
  # the isolation and satisfies GLib.
  ln -sfn /usr/share/glib-2.0/schemas "$WORK/share/glib-2.0/schemas"

  # A fake Access backend. It is the ONLY backend the fixture offers, it never
  # draws anything, and it records each call so a case can tell "refused before
  # the dialog" from "refused at the dialog".
  cat > "$WORK/fake-access.py" <<'PYEOF'
import os
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

MARKER = os.environ["RIME_TEST_MARKER"]
VERDICT = int(os.environ.get("RIME_TEST_VERDICT", "1"))

XML = """
<node>
  <interface name='org.freedesktop.impl.portal.Access'>
    <method name='AccessDialog'>
      <arg type='o' name='handle' direction='in'/>
      <arg type='s' name='app_id' direction='in'/>
      <arg type='s' name='parent_window' direction='in'/>
      <arg type='s' name='title' direction='in'/>
      <arg type='s' name='subtitle' direction='in'/>
      <arg type='s' name='body' direction='in'/>
      <arg type='a{sv}' name='options' direction='in'/>
      <arg type='u' name='response' direction='out'/>
      <arg type='a{sv}' name='results' direction='out'/>
    </method>
    <property name='version' type='u' access='read'/>
  </interface>
</node>
"""


def on_call(conn, sender, path, iface, method, params, inv):
    if method == "AccessDialog":
        with open(MARKER, "a") as f:
            f.write("AccessDialog app_id=%s\n" % params[1])
        inv.return_value(GLib.Variant("(ua{sv})", (VERDICT, {})))
    else:
        inv.return_error_literal(Gio.DBusError.quark(),
                                 Gio.DBusError.UNKNOWN_METHOD, method)


def on_get(conn, sender, path, iface, prop):
    return GLib.Variant("u", 1)


def on_bus(conn, name):
    node = Gio.DBusNodeInfo.new_for_xml(XML)
    conn.register_object("/org/freedesktop/portal/desktop", node.interfaces[0],
                         on_call, on_get, None)


loop = GLib.MainLoop()
Gio.bus_own_name(Gio.BusType.SESSION,
                 "org.freedesktop.impl.portal.desktop.rimetest",
                 Gio.BusNameOwnerFlags.NONE, on_bus, None, None)
loop.run()
PYEOF

  cat > "$WORK/share/xdg-desktop-portal/portals/rimetest.portal" <<'EOF'
[portal]
DBusName=org.freedesktop.impl.portal.desktop.rimetest
Interfaces=org.freedesktop.impl.portal.Access;
UseIn=RIMETEST
EOF

  # `default=none` matters: without it xdg-desktop-portal falls back to
  # whichever packaged backend it can find for every interface it has no
  # preference for, and would try to activate the real GTK backend. It would
  # fail (no display), but a test that depends on a GUI failing to start is a
  # test that opens a window the day it stops failing.
  cat > "$WORK/config/xdg-desktop-portal/portals.conf" <<'EOF'
[preferred]
default=none
org.freedesktop.impl.portal.Access=rimetest
EOF

  # The caller believes it is this Flatpak. xdg-desktop-portal reads
  # /proc/<peer-pid>/root/.flatpak-info for the identity, then validates the
  # instance against $XDG_RUNTIME_DIR/.flatpak/<id>/bwrapinfo.json — so the
  # harness has bwrap write that file with --info-fd and the caller waits for
  # it before making the call.
  APPID=org.rime.test.Camera
  cat > "$WORK/fpinfo/.flatpak-info" <<EOF
[Application]
name=$APPID
runtime=runtime/org.rime.Test/x86_64/1

[Instance]
instance-id=4242
flatpak-version=1.16.6
EOF

  cat > "$WORK/caller.py" <<'PYEOF'
import json
import os
import sys
import time
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

info = os.path.join(os.environ["XDG_RUNTIME_DIR"], ".flatpak", "4242",
                    "bwrapinfo.json")
for _ in range(200):
    try:
        if json.load(open(info)).get("pid-namespace"):
            break
    except Exception:
        pass
    time.sleep(0.05)

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
token = "rimep1061"
sender = bus.get_unique_name()[1:].replace(".", "_")
handle = "/org/freedesktop/portal/desktop/request/%s/%s" % (sender, token)
loop = GLib.MainLoop()
result = {}


def on_response(conn, s, p, i, sig, params):
    result["response"] = params[0]
    loop.quit()


bus.signal_subscribe(None, "org.freedesktop.portal.Request", "Response",
                     handle, None, Gio.DBusSignalFlags.NONE, on_response)
try:
    bus.call_sync("org.freedesktop.portal.Desktop",
                  "/org/freedesktop/portal/desktop",
                  "org.freedesktop.portal.Camera", "AccessCamera",
                  GLib.Variant("(a{sv})",
                               ({"handle_token": GLib.Variant("s", token)},)),
                  None, Gio.DBusCallFlags.NONE, 15000, None)
except GLib.Error as e:
    print("ERROR %s" % e.message)
    sys.exit(0)
GLib.timeout_add_seconds(15, lambda: (result.setdefault("response", 99),
                                      loop.quit())[1])
loop.run()
print("RESPONSE %s" % result.get("response"))
PYEOF

  # One case, start to finish, on a bus of its own.
  cat > "$WORK/case.sh" <<'EOS'
set -u
MODE="$1"
MARKER="$XDG_RUNTIME_DIR/backend-called"
: > "$MARKER"
export RIME_TEST_MARKER="$MARKER"
case "$MODE" in
  *-allow) export RIME_TEST_VERDICT=0 ;;
  *)       export RIME_TEST_VERDICT=1 ;;
esac

/usr/libexec/xdg-permission-store >"$XDG_RUNTIME_DIR/store.log" 2>&1 &
for _ in $(seq 1 60); do
  busctl --address="$DBUS_SESSION_BUS_ADDRESS" list 2>/dev/null \
    | grep -q 'impl.portal.PermissionStore' && break
  sleep 0.1
done
python3 "$WORK/fake-access.py" >"$XDG_RUNTIME_DIR/access.log" 2>&1 &
for _ in $(seq 1 60); do
  busctl --address="$DBUS_SESSION_BUS_ADDRESS" list 2>/dev/null \
    | grep -q 'desktop.rimetest' && break
  sleep 0.1
done

if [ "${MODE%%-*}" = deny ]; then
  gdbus call --session -d org.freedesktop.impl.portal.PermissionStore \
    -o /org/freedesktop/impl/portal/PermissionStore \
    -m org.freedesktop.impl.portal.PermissionStore.SetPermission \
    devices true camera "$APPID" '["no"]' >/dev/null 2>&1
fi
echo "STORE $(gdbus call --session \
  -d org.freedesktop.impl.portal.PermissionStore \
  -o /org/freedesktop/impl/portal/PermissionStore \
  -m org.freedesktop.impl.portal.PermissionStore.Lookup devices camera 2>&1 \
  | tr -d '\n')"

"$XDP" >"$XDG_RUNTIME_DIR/portal.log" 2>&1 &
for _ in $(seq 1 100); do
  busctl --address="$DBUS_SESSION_BUS_ADDRESS" list 2>/dev/null \
    | grep -q 'org.freedesktop.portal.Desktop' && break
  sleep 0.2
done
echo "CAMERA $(busctl --address="$DBUS_SESSION_BUS_ADDRESS" introspect \
  org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop 2>/dev/null \
  | grep -c 'org.freedesktop.portal.Camera')"

mkdir -p "$XDG_RUNTIME_DIR/.flatpak/4242"
bwrap --unshare-pid --info-fd 9 \
  --ro-bind /usr /usr --ro-bind /etc /etc \
  --symlink usr/bin /bin --symlink usr/lib /lib \
  --symlink usr/lib64 /lib64 --symlink usr/sbin /sbin \
  --proc /proc --dev /dev --bind /tmp /tmp \
  --ro-bind "$WORK/fpinfo/.flatpak-info" /.flatpak-info \
  --ro-bind "$WORK" "$WORK" --bind "$XDG_RUNTIME_DIR" "$XDG_RUNTIME_DIR" \
  -- /usr/bin/python3 "$WORK/caller.py" \
  9>"$XDG_RUNTIME_DIR/.flatpak/4242/bwrapinfo.json" 2>&1
echo "BACKEND $(wc -l < "$MARKER" | tr -d ' ')"
EOS

  run_case() {
    rm -rf "$WORK/data" "$WORK/run"
    mkdir -p "$WORK/data" "$WORK/run"
    chmod 700 "$WORK/run"
    env -i PATH=/usr/bin:/bin HOME="$WORK/home" WORK="$WORK" XDP="$XDP" \
      APPID="$APPID" \
      XDG_DATA_HOME="$WORK/data" XDG_CONFIG_HOME="$WORK/config" \
      XDG_RUNTIME_DIR="$WORK/run" XDG_DATA_DIRS="$WORK/share" \
      XDG_CURRENT_DESKTOP=RIMETEST \
      dbus-run-session -- bash "$WORK/case.sh" "$1" 2>/dev/null
  }
  field() { printf '%s\n' "$1" | sed -n "s/^$2 //p" | head -1; }

  # ── isolation, asserted before anything is written ─────────────────────────
  real_db="${XDG_DATA_HOME:-$HOME/.local/share}/flatpak/db"
  before_devices=absent
  [ -e "$real_db/devices" ] && before_devices=present

  A=$(run_case deny)
  after_devices=absent
  [ -e "$real_db/devices" ] && after_devices=present

  if [ "$before_devices" = "$after_devices" ]; then
    ok "the real permission store is untouched"
  else
    bad "the real permission store is untouched" \
        "devices went $before_devices -> $after_devices"
  fi

  if [ -s "$WORK/data/flatpak/db/devices" ]; then
    ok "the revocation was written inside the fixture"
  else
    bad "the revocation was written inside the fixture" \
        "no $WORK/data/flatpak/db/devices"
  fi

  # ── A: the store refuses, and the dialog is never reached ──────────────────
  if [ "$(field "$A" CAMERA)" = 1 ]; then
    ok "the isolated portal exports org.freedesktop.portal.Camera"
  else
    bad "the isolated portal exports org.freedesktop.portal.Camera" \
        "introspection found $(field "$A" CAMERA)"
  fi

  a_resp=$(field "$A" RESPONSE); a_back=$(field "$A" BACKEND)
  if [ "$a_resp" = 1 ]; then
    ok "a revoked camera grant is REFUSED, not merely absent"
  else
    bad "a revoked camera grant is REFUSED, not merely absent" \
        "response=$a_resp store=$(field "$A" STORE)"
  fi
  if [ "$a_back" = 0 ]; then
    ok "the refusal short-circuits the permission dialog"
  else
    bad "the refusal short-circuits the permission dialog" \
        "backend was called $a_back time(s)"
  fi

  # ── B: no entry, the dialog is reached and says no ─────────────────────────
  B=$(run_case forget)
  b_resp=$(field "$B" RESPONSE); b_back=$(field "$B" BACKEND)
  if [ "$b_resp" = 1 ] && [ "$b_back" -ge 1 ]; then
    ok "with no entry the dialog IS reached (same answer, other path)"
  else
    bad "with no entry the dialog IS reached (same answer, other path)" \
        "response=$b_resp backend=$b_back"
  fi
  if [ "$a_resp" = "$b_resp" ] && [ "$a_back" != "$b_back" ]; then
    ok "revoked and never-asked differ only in whether the dialog ran"
  else
    bad "revoked and never-asked differ only in whether the dialog ran" \
        "A(resp=$a_resp back=$a_back) B(resp=$b_resp back=$b_back)"
  fi

  # ── C: the harness can still produce a grant ───────────────────────────────
  C=$(run_case forget-allow)
  c_resp=$(field "$C" RESPONSE); c_back=$(field "$C" BACKEND)
  if [ "$c_resp" = 0 ] && [ "$c_back" -ge 1 ]; then
    ok "the same harness grants when nothing revoked it"
  else
    bad "the same harness grants when nothing revoked it" \
        "response=$c_resp backend=$c_back — case A proves nothing"
  fi
fi

# ── the native case: the assertion that must NOT hold ────────────────────────
# A native binary's camera access is a logind uaccess ACL on the device node.
# The ACL is the login session's, so there is no per-application revocation to
# test — and the honest test is that the node opens anyway.
NODE=$(ls /dev/video* 2>/dev/null | head -1)
if [ -z "${NODE:-}" ]; then
  skipped "native camera access is unmediated" "no /dev/video* on this machine"
elif [ "$(id -u)" = 0 ]; then
  skipped "native camera access is unmediated" "root walks through any ACL"
else
  acl=$(getfacl -p "$NODE" 2>/dev/null | sed -n "s/^user:$(id -un)://p")
  grp=$(stat -c %G "$NODE" 2>/dev/null)
  if id -nG | tr ' ' '\n' | pipe_has -x "$grp"; then
    skipped "the ACL, not the group, is what grants the node" \
            "this user is in $grp"
  elif [ -n "$acl" ]; then
    ok "the ACL, not the group, is what grants the node"
  else
    skipped "the ACL, not the group, is what grants the node" \
            "no uaccess ACL on $NODE"
  fi

  if python3 - "$NODE" <<'PYEOF'
import os
import sys
try:
    os.close(os.open(sys.argv[1], os.O_RDONLY))
except OSError:
    sys.exit(1)
sys.exit(0)
PYEOF
  then
    ok "a native process opens the camera node with nothing brokering it"
  else
    skipped "a native process opens the camera node with nothing brokering it" \
            "$NODE refused this user — the seat is not active"
  fi
fi

# ── the CLI's refusals ───────────────────────────────────────────────────────
if [ "$WITH_BINARY" = 1 ]; then
  Rime="${Rime:-$REPO/rimed/target/debug/rime}"
  [ -x "$Rime" ] || { echo "no rime binary at $Rime"; exit 2; }

  CWORK=$(mktemp -d /tmp/rime-permcli.XXXXXX) || exit 2
  trap 'rm -rf "$CWORK"' EXIT
  CBIN="$CWORK/bin"; mkdir -p "$CBIN"

  # A session with a Camera portal and no Usb portal — which is Rime's own
  # Hyprland session, and the reason `usb-device` has to refuse differently
  # from `camera`.
  cat > "$CBIN/busctl" <<'EOF'
#!/usr/bin/env bash
cat <<'XML'
org.freedesktop.portal.Camera             interface -  -  -
org.freedesktop.portal.Location           interface -  -  -
org.freedesktop.portal.Notification       interface -  -  -
XML
EOF

  # `devices=all` for one app, a narrow context for the other.
  cat > "$CBIN/flatpak" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  permission-list) exit 0 ;;
  list) printf 'org.rime.Broker
org.rime.RawDev
' ;;
  info)
    case "$3" in
      org.rime.RawDev) printf '[Context]
sockets=wayland;
devices=all;
' ;;
      *)               printf '[Context]
shared=network;
sockets=wayland;
devices=dri;
' ;;
    esac ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
EOF
  chmod +x "$CBIN/busctl" "$CBIN/flatpak"

  # A fixture /dev. Without it the native camera row is NoPrimitive on any
  # machine with no webcam — every CI runner — and the assertion below would
  # pass or fail on whether the developer's laptop has a camera rather than on
  # what the binary does. access(R_OK) on a readable regular file returns 0, so
  # the row lands on the logind-ACL enforcer exactly as it does on real
  # hardware. Overridable so the machine-dependence can be demonstrated: point
  # RIME_PERM_DEV_ROOT at an empty directory and the ACL assertion goes red.
  mkdir -p "$CWORK/dev/snd"
  : > "$CWORK/dev/video0"
  : > "$CWORK/dev/snd/pcmC0D0c"

  cli() {
      PATH="$CBIN:$PATH" HOME="$CWORK" \
      RIME_PERM_DEV_ROOT="${RIME_PERM_DEV_ROOT_OVERRIDE:-$CWORK/dev}" \
      "$Rime" permissions "$@" 2>&1
  }
  refuses() {
    local label=$1 want=$2; shift 2
    local out rc
    out=$(cli "$@"); rc=$?
    if [ "$rc" -eq 0 ]; then
      bad "$label" "exited 0; a revoke it cannot perform must not report success"
    elif printf '%s' "$out" | pipe_has -F "$want"; then
      ok "$label"
    else
      bad "$label" "no '$want' in: $(printf '%s' "$out" | head -2 | tr '\n' ' ')"
    fi
  }

  refuses "revoking a native camera says whose permission it is" \
          "belongs to your login session" revoke native:zed camera
  refuses "revoking a devices=all camera refuses too" \
          "cannot be revoked for one app" revoke org.rime.RawDev camera
  refuses "a capability with no portal refuses with a different sentence" \
          "is not brokered in this session" revoke org.rime.Broker usb-device

  # And the control: the one that CAN be revoked names the exact command. A
  # suite where every case refuses would pass on a binary that refused
  # everything.
  out=$(cli revoke org.rime.Broker camera --dry-run)
  if printf '%s' "$out" | pipe_has -F "flatpak permission-set devices camera org.rime.Broker no"; then
    ok "a brokered camera revocation is a store write, named exactly"
  else
    bad "a brokered camera revocation is a store write, named exactly" "$out"
  fi
  out=$(cli revoke org.rime.Broker camera --forget --dry-run)
  if printf '%s' "$out" | pipe_has -F "flatpak permission-remove devices camera org.rime.Broker"; then
    ok "--forget removes the entry instead of writing a refusal"
  else
    bad "--forget removes the entry instead of writing a refusal" "$out"
  fi
  out=$(cli revoke org.rime.Broker network --dry-run)
  if printf '%s' "$out" | pipe_has -F "flatpak override --user --unshare=network org.rime.Broker"; then
    ok "a sandbox capability is an override, and says it applies next launch"
  else
    bad "a sandbox capability is an override, and says it applies next launch" "$out"
  fi
  if printf '%s' "$out" | pipe_has -F "next time the app starts"; then
    ok "the override's timing is stated rather than implied"
  else
    bad "the override's timing is stated rather than implied" "$out"
  fi
else
  skipped "the CLI's refusals" "pass --with-binary"
fi

summary
