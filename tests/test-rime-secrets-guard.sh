#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-secrets-guard.sh — the image build refuses a second Secret
#  Service provider, and says which package brought it.
#
#  Runs the "Exactly one Secret Service" RUN block of Containerfile.base,
#  verbatim, against the system it is on. It installs and removes packages and
#  writes under /usr and /etc, so it runs ONLY as root inside a throwaway
#  container (CI: a fedora:45 container with gnome-keyring installed):
#
#    1. gnome-keyring alone, as the image ships it: the guard passes;
#    2. the provider file names another provider: refused;
#    3. a second D-Bus activation file for org.freedesktop.secrets: refused;
#    4. a systemd user dbus-org.freedesktop.secrets.service alias: refused;
#    5. a user unit with BusName=org.freedesktop.secrets: refused;
#    6. a second Secret portal backend: refused;
#    7. oo7-portal installed from the Fedora 45 repositories (it pulls in
#       oo7-daemon): refused, and the message names oo7-portal as what
#       required it;
#    8. oo7 removed again: passes again.
#
#  RIME_TEST_CONTAINERFILE runs the block from another file (a mutant).
#  SKIPS outside a container or without root, rpm or dnf.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
CF="${RIME_TEST_CONTAINERFILE:-$root/Containerfile.base}"
PROVIDER_SRC="$root/files/system/rime-secrets-provider"

pass=0 fail=0 skip=0
ok()   { echo "PASS  $1"; pass=$((pass + 1)); }
bad()  { echo "FAIL  $1${2:+  — $2}"; fail=$((fail + 1)); }
skp()  { echo "SKIP  $1${2:+  — $2}"; skip=$((skip + 1)); }
finish() { echo; echo "secrets-guard: $pass passed, $fail failed, $skip skipped"; [ "$fail" -eq 0 ]; exit $?; }

if [ "$(id -u)" != 0 ] || { [ ! -e /run/.containerenv ] && [ ! -e /.dockerenv ]; }; then
    skp "the build guard" "runs only as root inside a throwaway container (it installs packages)"; finish
fi
for t in rpm dnf; do command -v "$t" >/dev/null || { skp "the build guard" "no $t"; finish; }; done
rpm -q gnome-keyring >/dev/null || { skp "the build guard" "gnome-keyring is not installed"; finish; }

W="$(mktemp -d)"
cleanup() {
    rm -f /usr/local/share/dbus-1/services/rime-test-secrets.service \
          /etc/systemd/user/dbus-org.freedesktop.secrets.service \
          /etc/systemd/user/rime-test-secrets.service \
          /usr/share/xdg-desktop-portal/portals/rime-test.portal
    rm -rf "$W"
}
trap cleanup EXIT

# The RUN block after the header comment, up to its first line that does not
# continue. Every comment inside it is a `: '…'` command, so bash runs it as is.
awk '/^# ── Exactly one Secret Service/ {h = 1}
     h && /^RUN / {r = 1; sub(/^RUN /, "")}
     r {print; if ($0 !~ /\\$/) exit}' "$CF" > "$W/guard.sh"
[ -s "$W/guard.sh" ] || { bad "the guard block is found in $CF"; finish; }
ok "the guard block is found in Containerfile.base ($(wc -l < "$W/guard.sh") lines)"

install -D -m 0644 "$PROVIDER_SRC" /usr/share/rime/secrets-provider
guard() { bash "$W/guard.sh" >"$W/log" 2>&1; }
passes() {  # passes <desc>
    if guard; then ok "$1"; else bad "$1" "$(grep -m1 FATAL "$W/log" || tail -3 "$W/log")"; fi
}
refuses() {  # refuses <desc> <text the message must contain>
    if guard; then bad "$1" "the guard passed"
    elif grep -qF -- "$2" "$W/log"; then ok "$1"
    else bad "$1" "refused without saying '$2': $(grep -m3 FATAL "$W/log" | tr '\n' ' ')"; fi
}

# 1. As the image ships it.
rpm -q oo7-daemon >/dev/null 2>&1 && dnf -y -q remove oo7-daemon >/dev/null 2>&1
passes "gnome-keyring alone: one provider, one activation, one Secret portal"

# 2. A provider this image's login stack does not unlock.
sed -i 's/^provider=.*/provider=oo7/' /usr/share/rime/secrets-provider
refuses "the provider file naming oo7 is refused" "names oo7"
install -m 0644 "$PROVIDER_SRC" /usr/share/rime/secrets-provider

# 3. A second D-Bus activation file.
mkdir -p /usr/local/share/dbus-1/services
printf '[D-BUS Service]\nName=org.freedesktop.secrets\nExec=/bin/false\n' \
    > /usr/local/share/dbus-1/services/rime-test-secrets.service
refuses "a second D-Bus activation file is refused" "2 session D-Bus service files"
rm -f /usr/local/share/dbus-1/services/rime-test-secrets.service

# 4. A systemd user activation alias.
mkdir -p /etc/systemd/user
ln -s oo7-daemon.service /etc/systemd/user/dbus-org.freedesktop.secrets.service
refuses "a dbus-org.freedesktop.secrets.service alias is refused" "dbus-org.freedesktop.secrets.service"
rm -f /etc/systemd/user/dbus-org.freedesktop.secrets.service

# 5. A user unit claiming the bus name.
printf '[Service]\nType=dbus\nBusName=org.freedesktop.secrets\nExecStart=/bin/false\n' \
    > /etc/systemd/user/rime-test-secrets.service
refuses "a user unit with BusName=org.freedesktop.secrets is refused" "rime-test-secrets.service"
rm -f /etc/systemd/user/rime-test-secrets.service

# 6. A second Secret portal backend.
mkdir -p /usr/share/xdg-desktop-portal/portals
printf '[portal]\nDBusName=org.example.Secret\nInterfaces=org.freedesktop.impl.portal.Secret;\n' \
    > /usr/share/xdg-desktop-portal/portals/rime-test.portal
refuses "a second Secret portal backend is refused" "rime-test.portal"
rm -f /usr/share/xdg-desktop-portal/portals/rime-test.portal

# 7. The real drift: a package that requires oo7-daemon.
if dnf -y -q install --setopt=install_weak_deps=False --setopt=retries=5 oo7-portal >/dev/null 2>&1; then
    refuses "oo7-daemon in the image is refused" "oo7-daemon-"
    if grep -E 'required by:.*oo7-portal' "$W/log" >/dev/null; then ok "...and the message names what pulled it in (oo7-portal)"
    else bad "...and the message names what pulled it in (oo7-portal)" "$(grep -A1 FATAL "$W/log" | tr '\n' ' ')"; fi
    dnf -y -q remove oo7-portal oo7-daemon >/dev/null 2>&1
    # 8. And back.
    passes "with oo7 removed again the guard passes"
else
    skp "oo7-daemon in the image is refused" "dnf could not install oo7-portal"
    skp "...and the message names what pulled it in (oo7-portal)" "not installed"
    skp "with oo7 removed again the guard passes" "not installed"
fi
finish
