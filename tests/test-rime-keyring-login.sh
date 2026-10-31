#!/usr/bin/env bash
# rime-keyring-login: the login password becomes the master of the keyring.
#
# Every keyring here is made by a REAL gnome-keyring-daemon, on a private
# D-Bus session with a private HOME and XDG_RUNTIME_DIR, and killed by PID —
# nothing reads or touches the running user's keyring. The helper's file
# parser is a reconstruction of gkm-secret-binary.c, so the only proof that
# counts is the daemon agreeing with it: after adopt() moves a keyring over,
# a fresh daemon given ONLY the login password (`--unlock`, which is what
# pam_gnome_keyring does) must hand back the secret with no prompter running.
#
# §6 runs the installed program the way pam_exec does, as root, against a
# throwaway account with a known password (CI). Without root it SKIPS, and a
# skip is counted, not hidden: CI requires skipped=0.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
HELPER="$root/files/system/libexec/rime-keyring-login"

pass=0 fail=0 skip=0
ok()   { echo "PASS  $1"; pass=$((pass + 1)); }
bad()  { echo "FAIL  $1${2:+  — $2}"; fail=$((fail + 1)); }
skp()  { echo "SKIP  $1${2:+  — $2}"; skip=$((skip + 1)); }
finish() { echo; echo "keyring-login: $pass passed, $fail failed, $skip skipped"; [ "$fail" -eq 0 ]; exit $?; }

for t in python3 gnome-keyring-daemon dbus-run-session secret-tool gdbus; do
    command -v "$t" >/dev/null || { skp "needs $t"; finish; }
done
python3 -c 'import gi; gi.require_version("Gio", "2.0")' 2>/dev/null || { skp "needs python3-gi"; finish; }

W="$(mktemp -d "${TMPDIR:-/tmp}/rkl-test.XXXXXX")"
trap 'rm -rf "$W"' EXIT

# ── fixtures: keyrings made by the daemon itself ────────────────────────────
cat >"$W/mk.py" <<'EOF'
import sys, gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
def call(path, iface, method, args, rtype=None):
    return bus.call_sync("org.freedesktop.secrets", path, iface, method, args,
                         GLib.VariantType(rtype) if rtype else None, Gio.DBusCallFlags.NONE, 5000, None)
svc = "/org/freedesktop/secrets"
_, sess = call(svc, "org.freedesktop.Secret.Service", "OpenSession",
               GLib.Variant("(sv)", ("plain", GLib.Variant("s", ""))), "(vo)").unpack()
label, pw = sys.argv[1], sys.argv[2].encode()
(coll,) = call(svc, "org.gnome.keyring.InternalUnsupportedGuiltRiddenInterface", "CreateWithMasterPassword",
               GLib.Variant("(a{sv}(oayays))", ({"org.freedesktop.Secret.Collection.Label": GLib.Variant("s", label)},
                                                (sess, b"", pw, "text/plain"))), "(o)").unpack()
if "default" in sys.argv[3:]:
    call(svc, "org.freedesktop.Secret.Service", "SetAlias", GLib.Variant("(so)", ("default", coll)))
props = {"org.freedesktop.Secret.Item.Label": GLib.Variant("s", "rime test item"),
         "org.freedesktop.Secret.Item.Attributes": GLib.Variant("a{ss}", {"rime-test": label})}
call(coll, "org.freedesktop.Secret.Collection", "CreateItem",
     GLib.Variant("(a{sv}(oayays)b)", (props, (sess, b"", b"s3cret-" + label.encode(), "text/plain"), True)), "(oo)")
EOF

# The private bus has NO service directories, so nothing on it is ever
# auto-activated. With the stock session config, a call that arrived before
# the test's daemon owned org.freedesktop.secrets activated a second daemon
# from the service file; that one outlived the bus, was reparented to the
# user's systemd, and held this script's output pipe open until killed by hand.
cat >"$W/bus.conf" <<BUSCONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=$W</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
BUSCONF

# daemon <home> <password> <script...>: run a private daemon unlocked with
# <password> (it creates `login` if absent, as pam_gnome_keyring's does), run
# the script on the same private bus, kill the daemon by PID.
daemon() {
    local home="$1" pw="$2"; shift 2
    mkdir -p "$home/run" && chmod 700 "$home/run"
    HOME="$home" XDG_DATA_HOME="$home/.local/share" XDG_CONFIG_HOME="$home/.config" \
    XDG_RUNTIME_DIR="$home/run" RKL_PW="$pw" RKL_SCRIPT="$*" \
    dbus-run-session --config-file="$W/bus.conf" -- bash -c '
        printf "%s" "$RKL_PW" | gnome-keyring-daemon --foreground --unlock --components=secrets >/dev/null 2>&1 &
        gkd=$!
        # NameHasOwner asks the bus itself, so waiting activates nothing.
        for _ in $(seq 100); do
            gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
                --method org.freedesktop.DBus.NameHasOwner org.freedesktop.secrets 2>/dev/null \
                | grep -q true && break
            sleep 0.05
        done
        eval "$RKL_SCRIPT"; rc=$?
        kill "$gkd" 2>/dev/null; wait "$gkd" 2>/dev/null
        exit $rc'
}

# fixture <name> — Andre's shape: "Default keyring" (the gcr prompt's) as the
# default alias, holding one secret, password login-pw; "Other" with its own
# password; and the login keyring the --unlock made, REMOVED, because no Rime
# machine before 2026.09.29.2 ever had one.
fixture() {
    local h="$W/$1"
    daemon "$h" "login-pw" "python3 '$W/mk.py' 'Default keyring' login-pw default && python3 '$W/mk.py' Other other-pw" \
        || { bad "fixture $1 could not be made"; return 1; }
    rm -f "$h/.local/share/keyrings/login.keyring"
    echo "$h/.local/share/keyrings"
}

# lookup <home> <password> <label>: the secret a fresh daemon unlocked with
# <password> hands back for that fixture's item, or nothing. No prompter
# exists here, so a keyring that is still locked cannot answer.
lookup() {
    daemon "$1" "$2" "timeout 5 secret-tool lookup rime-test '$3' 2>/dev/null"
}

sums() { (cd "$1" && sha256sum -- * 2>/dev/null | sort); }

cat >"$W/drive.py" <<EOF
import importlib.machinery, importlib.util, sys
loader = importlib.machinery.SourceFileLoader("rkl", "$HELPER")
spec = importlib.util.spec_from_loader("rkl", loader)
m = importlib.util.module_from_spec(spec); loader.exec_module(m)
if "--mutant-always-opens" in sys.argv:
    m.Keyring.opens_with = lambda self, pw: self.ok
changed, verdict = m.adopt(sys.argv[1], sys.argv[2].encode(), now=1790000000)
print("changed" if changed else "kept", "|", verdict)
EOF
adopt() { python3 "$W/drive.py" "$@"; }

# ── §1 the parser agrees with the daemon ────────────────────────────────────
echo "── §1 the parser, against daemon-made files"
K="$(fixture p1)" || finish
out="$(python3 - "$HELPER" "$K" <<'EOF'
import importlib.machinery, importlib.util, os, sys
loader = importlib.machinery.SourceFileLoader("rkl", sys.argv[1])
spec = importlib.util.spec_from_loader("rkl", loader); m = importlib.util.module_from_spec(spec); loader.exec_module(m)
d = sys.argv[2]
for name, pw in [("Default_keyring", "login-pw"), ("Default_keyring", "login-pX"), ("Default_keyring", ""),
                 ("Other", "other-pw"), ("Other", "login-pw")]:
    k = m._read_keyring(os.path.join(d, name + ".keyring"))
    print(name, pw or "<empty>", k.ok, k.items, k.opens_with(pw.encode()))
EOF
)"
chk() { grep -qx "$2" <<<"$out" && ok "$1" || bad "$1" "$(grep "^${2%% *} ${2#* }" <<<"$out" | head -1)"; }
chk "the default keyring opens with its own password"       "Default_keyring login-pw True 1 True"
chk "…and not with one character wrong"                      "Default_keyring login-pX True 1 False"
chk "…and not with an empty password"                        "Default_keyring <empty> True 1 False"
chk "another keyring opens with its own password"           "Other other-pw True 1 True"
chk "…and not with the login password"                       "Other login-pw True 1 False"
[ "$(cat "$K/default")" = "Default_keyring" ] && ok "the fixture's default alias is the gcr keyring" \
    || bad "the fixture's default alias is the gcr keyring" "$(cat "$K/default")"

# ── §2 the login password moves the default keyring over ────────────────────
echo "── §2 adoption, then a real unlock with only the login password"
K="$(fixture p2)" || finish
before="$(lookup "$W/p2" "login-pw" "Default keyring")"
[ -z "$before" ] && ok "before: the login password does not reach the default keyring" \
    || bad "before: the login password does not reach the default keyring" "got [$before]"
rm -f "$K/login.keyring"   # the lookup's --unlock made one; a real machine has none
r="$(adopt "$K" login-pw)"
[[ "$r" == changed* ]] && ok "adopt() moves the default keyring over" || bad "adopt() moves the default keyring over" "$r"
[ -f "$K/login.keyring" ] && [ ! -e "$K/Default_keyring.keyring" ] && ok "…by renaming it to login.keyring" \
    || bad "…by renaming it to login.keyring" "$(ls "$K" | tr '\n' ' ')"
[ "$(cat "$K/default")" = "login" ] && ok "…and the default alias is login, with no newline" \
    || bad "…and the default alias is login, with no newline" "$(od -c "$K/default" | head -1)"
[ "$(stat -c %a "$K/default")" = "600" ] && ok "…written 0600" || bad "…written 0600" "$(stat -c %a "$K/default")"
after="$(lookup "$W/p2" "login-pw" "Default keyring")"
[ "$after" = "s3cret-Default keyring" ] && ok "after: a daemon given only the login password returns the secret" \
    || bad "after: a daemon given only the login password returns the secret" "got [$after]"
[ -z "$(lookup "$W/p2" "login-pw" "Other")" ] && ok "a keyring with its own password is still not opened by the login one" \
    || bad "a keyring with its own password is still not opened by the login one"
r="$(adopt "$K" login-pw)"
[[ "$r" == kept* ]] && ok "a second run changes nothing" || bad "a second run changes nothing" "$r"

# ── §3 what must leave everything as it was ─────────────────────────────────
echo "── §3 refusals leave every file byte for byte"
refuse() {  # refuse <desc> <keyrings> <password> [drive args]
    local desc="$1" k="$2" pw="$3"; shift 3
    local s0; s0="$(sums "$k")"
    local r; r="$(adopt "$k" "$pw" "$@")"
    if [[ "$r" == kept* ]] && [ "$(sums "$k")" = "$s0" ]; then ok "$desc"; else bad "$desc" "$r"; fi
}
K="$(fixture p3)" || finish
refuse "a wrong password moves nothing"                   "$K" login-pX
refuse "the other keyring's password moves nothing"       "$K" other-pw
printf 'Other' >"$K/default"
refuse "a default keyring with a different password moves nothing" "$K" login-pw
K="$(fixture p3b)" || finish
daemon "$W/p3b" "login-pw" "true"   # --unlock makes a login keyring …
cp "$K/Other.keyring" "$K/login.keyring"   # … give it contents of its own
refuse "a login keyring with contents is never replaced"  "$K" login-pw
K="$(fixture p3c)" || finish
rm -f "$K/default"
refuse "no alias and two candidate keyrings: nothing is guessed" "$K" login-pw
printf '../evil' >"$K/default"
refuse "a default alias that is not a plain name is refused" "$K" login-pw
K="$(fixture p3d)" || finish
refuse "the mutant check: nothing moves for a wrong password"  "$K" login-pX
r="$(adopt "$K" login-pX --mutant-always-opens)"
[[ "$r" == changed* ]] && ok "…and a parser that says yes to everything WOULD have moved it (the check can fail)" \
    || bad "…and a parser that says yes to everything WOULD have moved it (the check can fail)" "$r"

# ── §4 the state 2026.09.29.2 can leave: PAM's own empty login keyring ──────
echo "── §4 an empty login keyring beside the real one"
K="$(fixture p4)" || finish
daemon "$W/p4" "login-pw" "true"      # the first login on .2: --unlock makes an empty one
[ -f "$K/login.keyring" ] && ok "fixture: an empty login keyring exists beside the default one" \
    || bad "fixture: an empty login keyring exists beside the default one"
r="$(adopt "$K" login-pw)"
[[ "$r" == changed*"moved to login.keyring.rime-empty-1790000000"* ]] && ok "the empty one is moved aside and the default takes its place" \
    || bad "the empty one is moved aside and the default takes its place" "$r"
[ -f "$K/login.keyring.rime-empty-1790000000" ] && ok "…the empty one is kept, not deleted" || bad "…the empty one is kept, not deleted"
[ "$(lookup "$W/p4" "login-pw" "Default keyring")" = "s3cret-Default keyring" ] \
    && ok "…and the login password opens the real secrets" || bad "…and the login password opens the real secrets"
K="$(fixture p4b)" || finish
daemon "$W/p4b" "someone-else" "true"   # an empty login keyring with ANOTHER password
refuse "an empty login keyring this password cannot open is not moved" "$K" login-pw

K="$(fixture p4c)" || finish
rm -f "$K/default" "$K/Other.keyring"
r="$(adopt "$K" login-pw)"
[[ "$r" == changed* ]] && [ "$(cat "$K/default")" = "login" ] && ok "no alias and exactly one keyring: that one is adopted" \
    || bad "no alias and exactly one keyring: that one is adopted" "$r"

# ── §5 main(): fails open, and only for a real login ────────────────────────
echo "── §5 main() never fails a login"
cat >"$W/main.py" <<EOF
import importlib.machinery, importlib.util, os, sys
loader = importlib.machinery.SourceFileLoader("rkl", "$HELPER")
spec = importlib.util.spec_from_loader("rkl", loader)
m = importlib.util.module_from_spec(spec); loader.exec_module(m)
m.syslog.syslog = lambda *a: print("LOG", *a[1:], file=sys.stderr)
case = sys.argv[1]
if case == "chkpwd-fails":
    m.CHKPWD = "/bin/false"
if case == "chkpwd-missing":
    m.CHKPWD = "/nonexistent/unix_chkpwd"
if case == "adopt-raises":
    m._password_is_the_accounts = lambda u, p: True
    m.os.setgroups = m.os.setresgid = m.os.setresuid = lambda *a: None
    m._daemon_running = lambda uid: False
    def boom(*a, **k): raise RuntimeError("boom")
    m.adopt = boom
sys.exit(m.main())
EOF
me="$(id -un)"
for c in "session:PAM_TYPE=session" "unknown-user:PAM_USER=no-such-user-rkl" "chkpwd-fails:" "chkpwd-missing:" "adopt-raises:"; do
    name="${c%%:*}" envs="${c#*:}"
    s0="$(sums "$K")"
    err="$(printf 'login-pw\0' | env PAM_TYPE=auth PAM_USER="$me" $envs python3 "$W/main.py" "$name" 2>&1 >/dev/null)"; rc=$?
    if [ "$rc" -eq 0 ] && [ "$(sums "$K")" = "$s0" ]; then ok "main() exits 0 and changes nothing: $name"
    else bad "main() exits 0 and changes nothing: $name" "rc=$rc $err"; fi
    grep -q "login-pw" <<<"$err" && bad "…and the password never reaches the log: $name"
done
grep -q 'syslog.syslog(syslog.LOG_WARNING, "stopped without changing anything: %s" % type(e).__name__)' "$HELPER" \
    && ok "an unexpected error is logged by its type only" || bad "an unexpected error is logged by its type only"
grep -q 'signal.alarm(DEADLINE_SECS)' "$HELPER" && ok "a hard deadline stops a helper that hangs" \
    || bad "a hard deadline stops a helper that hangs"

# ── §5b only when gnome-keyring is the provider ─────────────────────────────
# Fedora 45 moved its desktops to oo7-daemon. This helper renames
# gnome-keyring's files, so where gnome-keyring is not the image's provider, or
# not installed, it must do nothing, say so, and still exit 0.
echo "── §5b gnome-keyring is the provider, or nothing happens"
cat >"$W/gate.py" <<EOF
import importlib.machinery, importlib.util, os, sys
loader = importlib.machinery.SourceFileLoader("rkl", "$HELPER")
spec = importlib.util.spec_from_loader("rkl", loader)
m = importlib.util.module_from_spec(spec); loader.exec_module(m)
logged = []
m.syslog.syslog = lambda *a: logged.append(a[-1])
# An ordinary account, whoever runs the suite: main() leaves uid < 1000 alone,
# so under sudo (CI's root run) the real PAM_USER, root, never reached the gate.
_pw = type("pw", (), {"pw_uid": 4242, "pw_gid": 4242, "pw_dir": "/nonexistent"})
m.pwd.getpwnam = lambda u: _pw
m.PROVIDER_FILE = sys.argv[2]
if sys.argv[1] == "no-daemon":
    m.GNOME_KEYRING_DAEMON = "/nonexistent/gnome-keyring-daemon"
reached = []
m._password_is_the_accounts = lambda u, p: reached.append(1) or False
rc = m.main()
print("rc=%d reached=%d log=%s" % (rc, len(reached), " | ".join(logged)))
EOF
printf '# test\nprovider=oo7\nportal=oo7-portal\n' >"$W/prov-oo7"
printf 'provider=gnome-keyring\nportal=gnome-keyring\n' >"$W/prov-gk"
gate() { printf 'login-pw\0' | env PAM_TYPE=auth PAM_USER=rkl-gate-user python3 "$W/gate.py" "$@" 2>&1; }
r="$(gate provider "$W/prov-oo7")"
[[ "$r" == "rc=0 reached=0 log=skipped: the Secret Service provider is oo7, not gnome-keyring; nothing moved" ]] \
    && ok "provider oo7: exit 0, nothing checked or moved, and the journal says why" || bad "provider oo7: exit 0, nothing checked or moved, and the journal says why" "$r"
r="$(gate no-daemon "$W/prov-gk")"
[[ "$r" == "rc=0 reached=0 log=skipped: gnome-keyring is not installed; nothing moved" ]] \
    && ok "gnome-keyring not installed: exit 0 and a logged skip" || bad "gnome-keyring not installed: exit 0 and a logged skip" "$r"
r="$(gate provider "$W/prov-gk")"
[[ "$r" == "rc=0 reached=1 "* ]] && ok "provider gnome-keyring: it goes on to check the password" \
    || bad "provider gnome-keyring: it goes on to check the password" "$r"
r="$(gate provider "$W/no-such-provider-file")"
[[ "$r" == "rc=0 reached=1 "* ]] && ok "no provider file (every release before it): gnome-keyring, as before" \
    || bad "no provider file (every release before it): gnome-keyring, as before" "$r"
grep -q 'login-pw' <<<"$r" && bad "…and the password never reaches the log"

# A store in another format: the same header, version 1.0. The magic and the
# version (the first 18 bytes) are what oo7-daemon 0.7.0 wrote to
# keyrings/v1/login.keyring on Fedora 45 once an item was stored (--login
# alone writes no file); the rest is zeros. oo7 never writes it HERE, where
# this helper reads: this checks the verdict, the skip itself predates it.
K="$W/p5b/.local/share/keyrings"; mkdir -p "$K"
printf 'GnomeKeyring\n\r\0\n\1\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0' >"$K/Default_keyring.keyring"
printf 'Default_keyring' >"$K/default"
s0="$(sums "$K")"
r="$(adopt "$K" login-pw)"
if [[ "$r" == "kept | the default keyring 'Default_keyring' is not in gnome-keyring's file format (version 1.0); left alone" ]] \
    && [ "$(sums "$K")" = "$s0" ]; then ok "an oo7-format store is left alone, and the verdict says it is another format"
else bad "an oo7-format store is left alone, and the verdict says it is another format" "$r"; fi

# ── §6 the real program, as pam_exec runs it (root) ─────────────────────────
echo "── §6 as root, for a real account, through unix_chkpwd"
if [ "$(id -u)" -ne 0 ] || [ "${RIME_KEYRING_TEST_ACCOUNT:-}" != 1 ]; then
    skp "the real-account run" "needs root and RIME_KEYRING_TEST_ACCOUNT=1 (it creates and deletes a user)"
else
    u="rkltest$$"
    useradd -m "$u" && printf '%s:%s\n' "$u" "login-pw" | chpasswd || { bad "could not make the test account"; finish; }
    uhome="$(getent passwd "$u" | cut -d: -f6)"
    K="$(fixture p6)" || finish
    install -d -o "$u" -g "$u" -m 0700 "$uhome/.local" "$uhome/.local/share" "$uhome/.local/share/keyrings"
    cp -a "$K/." "$uhome/.local/share/keyrings/" && chown -R "$u:$u" "$uhome/.local/share/keyrings"
    UK="$uhome/.local/share/keyrings"
    s0="$(sums "$UK")"
    printf 'login-pX\0' | PAM_TYPE=auth PAM_USER="$u" "$HELPER"; rc=$?
    [ "$rc" -eq 0 ] && [ "$(sums "$UK")" = "$s0" ] && ok "a wrong login password: exit 0, nothing moved" \
        || bad "a wrong login password: exit 0, nothing moved" "rc=$rc"
    printf 'other-pw\0' | PAM_TYPE=auth PAM_USER="$u" "$HELPER"; rc=$?
    [ "$rc" -eq 0 ] && [ "$(sums "$UK")" = "$s0" ] && ok "a keyring's own password that is not the account's: nothing moved" \
        || bad "a keyring's own password that is not the account's: nothing moved" "rc=$rc"
    t0=$(date +%s%N)
    printf 'login-pw\0' | PAM_TYPE=auth PAM_USER="$u" "$HELPER"; rc=$?
    ms=$(( ($(date +%s%N) - t0) / 1000000 ))
    [ "$rc" -eq 0 ] && [ -f "$UK/login.keyring" ] && [ "$(cat "$UK/default")" = login ] \
        && ok "the account's password: the default keyring is the login keyring" \
        || bad "the account's password: the default keyring is the login keyring" "rc=$rc $(ls "$UK" | tr '\n' ' ')"
    [ "$(stat -c %U "$UK/login.keyring")" = "$u" ] && [ "$(stat -c %U "$UK/default")" = "$u" ] \
        && ok "…and every file it wrote belongs to the user, not root" \
        || bad "…and every file it wrote belongs to the user, not root" "$(stat -c '%U %n' "$UK"/*)"
    [ "$ms" -lt 2000 ] && ok "…in ${ms} ms (a login waits for it)" || bad "…in ${ms} ms (a login waits for it)"
    userdel -r "$u" 2>/dev/null
fi

finish
