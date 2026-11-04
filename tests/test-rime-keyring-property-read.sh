#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-keyring-property-read.sh — reading a keyring property never kills
#  the login keyring.
#
#  gnome-keyring-daemon creates its record of each caller from a message
#  filter that only SCHEDULES the work as an idle. Now and then the idle GDBus
#  queued for a property read runs first: the PKCS#11 session lookup finds no
#  client, the collection getter returns NULL without an error, and GDBus
#  aborts the daemon. A plain read of the login collection's Locked property
#  (libsecret proxies make it) killed the --login daemon the login password
#  unlocked: 9 of 10 Fedora 45 gnome-keyring 51.1 daemons died within 5,000
#  reads each. files/system/src/gnome-keyring/ carries the fix and
#  Containerfile.base installs Fedora's gnome-keyring rebuilt with it.
#
#  Static (always; no network, no build, no root):
#    • the patch creates the client where it is needed and makes the lookup
#      set an error instead of returning NULL silently;
#    • the one build recipe pins the SRPM, refuses a spec it does not
#      understand, checks the prepared source and marks the release .rime1;
#    • the base build refuses a core whose gnome-keyring is not the pinned
#      Fedora build (Fedora moved it: a human re-checks whether upstream fixed
#      it), installs ONLY the rebuilt packages, and refuses an installed
#      gnome-keyring that is not Rime's build or lacks the fix;
#    • nothing installs gnome-keyring(-pam) from the repositories in base;
#    • a change to the patch or recipe builds an image.
#
#  The race (RIME_KEYRING_RACE=1): the gnome-keyring-daemon on PATH, on a
#  private bus with a throwaway HOME and password, K daemons × N reads of
#  collection/login's Locked property, each read a fresh connection (busctl).
#    RIME_KEYRING_RACE_EXPECT=survive (default) every daemon must survive all
#      N reads, and the daemon must carry the fix (its error string);
#    RIME_KEYRING_RACE_EXPECT=die     the mutant: a STOCK daemon must die at
#      least once (stops at the first death unless RIME_KEYRING_RACE_ALL=1).
#      Proves the race is measured.
#  RIME_KEYRING_RACE_DAEMONS (10) and RIME_KEYRING_RACE_READS (5000) set K, N.
#  Must run as a non-root user (that is how the daemon runs in a session);
#  needs dbus-daemon, busctl and gnome-keyring-daemon, else it SKIPS.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")/.." || exit 2

DIR=files/system/src/gnome-keyring
PATCH=$DIR/gnome-keyring-property-read-no-client.patch
RECIPE=$DIR/build-rpms.sh
BASE=Containerfile.base
WF=.github/workflows/build-image.yml
PRV=.github/workflows/pr-validation.yml
for f in "$PATCH" "$RECIPE" "$BASE" "$WF" "$PRV"; do [ -f "$f" ] || { echo "cannot find $f"; exit 2; }; done

pass=0; fail=0; skip=0
check() { # name, then a command that must succeed
    local name="$1"; shift
    if "$@"; then printf 'PASS  %s\n' "$name"; pass=$((pass+1)); else printf 'FAIL  %s\n' "$name"; fail=$((fail+1)); fi
}
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail+1)); }
skp() { printf 'SKIP  %s%s\n' "$1" "${2:+  — $2}"; skip=$((skip+1)); }
finish() { printf '\nkeyring-property-read: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"; [ "$fail" -eq 0 ]; exit $?; }
has() { grep "$@" >/dev/null; }

# ── the patch ────────────────────────────────────────────────────────────────
# The added lines of one file's diff, in order.
added() { awk -v f="$1" '/^diff --git /{on = index($0, " b/" f) > 0; next} on && /^\+[^+]/{print substr($0, 2)}' "$PATCH"; }
# The client is created BEFORE the lookup that asserts on it, in both functions.
ensure_before_lookup() {
    awk -v fn="$1" '/^diff --git /{on = index($0, "gkd-secret-service.c") > 0; next}
         on && /^@@ / {inf = index($0, fn) > 0}
         on && inf && /^\+.*gkd_secret_service_ensure_client \(self, caller\);/ {e = 1}
         on && inf && e && /^ .*client = g_hash_table_lookup \(self->clients, caller\);/ {ok = 1}
         END {exit !ok}' "$PATCH"
}
check "patch: the session lookup creates the caller's client first" ensure_before_lookup gkd_secret_service_get_pkcs11_session
check "patch: publishing a dispatch creates the caller's client first" ensure_before_lookup gkd_secret_service_publish_dispatch
check "patch: the object lookup sets an error when there is no session" \
    has -F 'g_set_error (error_out, G_DBUS_ERROR, G_DBUS_ERROR_FAILED,' <(added daemon/dbus/gkd-secret-objects.c)
check "patch: the silent NULL return is gone" \
    has -Fx -- '-	g_return_val_if_fail (session, FALSE);' "$PATCH"
check "patch: no AI attribution" bash -c '! grep -iE "co-authored-by|claude|generated with" "$0" >/dev/null' "$PATCH"

# ── the recipe ───────────────────────────────────────────────────────────────
check "recipe: the SRPM comes from Koji at the exact NVR" \
    has -F 'https://kojipkgs.fedoraproject.org/packages/gnome-keyring/${VER}/${REL}/src/${NVR}.src.rpm' "$RECIPE"
check "recipe: the SRPM hash is checked" has -F 'echo "${SHA}  $W/$NVR.src.rpm" | sha256sum -c -' "$RECIPE"
check "recipe: refuses a spec that now carries patches of its own" has -F 'now carries patches of its own' "$RECIPE"
check "recipe: refuses a changed Release line" has -F "Release line changed; re-check the .rime1 suffix" "$RECIPE"
check "recipe: the release is marked .rime1" has -F '%autorelease -e rime1/' "$RECIPE"
check "recipe: the patch is added to the spec" has -F 'a Patch0:         ${PATCH}' "$RECIPE"
check "recipe: the prepared source is checked for both halves of the fix" \
    bash -c 'grep -F "gkd_secret_service_ensure_client (self, caller);" "$0" >/dev/null && grep -F "No session for the caller to look up" "$0" >/dev/null' "$RECIPE"
check "recipe: makes exactly gnome-keyring and gnome-keyring-pam" has -F 'for p in gnome-keyring gnome-keyring-pam; do' "$RECIPE"
check "recipe: the strings it checks are the patch's" \
    bash -c 'grep -F "No session for the caller to look up" "$0" >/dev/null' "$PATCH"

# ── the base build ───────────────────────────────────────────────────────────
NVR="$(sed -n 's/^ARG GNOME_KEYRING_NVR=//p' "$BASE")"
SHA="$(sed -n 's/^ARG GNOME_KEYRING_SRPM_SHA256=//p' "$BASE")"
check "base: the Fedora build is pinned once, by NVR" test "$(printf '%s\n' "$NVR" | grep -c .)" = 1
check "base: the SRPM is pinned by sha256" bash -c '[[ "$0" =~ ^[0-9a-f]{64}$ ]]' "$SHA"
stage() { awk '/^FROM .* AS gnome-keyring-builder$/{on=1} on && /^FROM / && !/gnome-keyring-builder/{exit} on' "$BASE"; }
check "base: gnome-keyring-builder stage exists, built on core" has -E '^FROM \$\{CORE\} AS gnome-keyring-builder$' "$BASE"
check "base: the builder refuses a core whose gnome-keyring is not the pinned build" \
    has -F 'is not the pinned ${GNOME_KEYRING_NVR}' <(stage)
check "base: the builder runs the one recipe" \
    has -F '/build/build-rpms.sh "${GNOME_KEYRING_NVR}" "${GNOME_KEYRING_SRPM_SHA256}" /out/gnome-keyring' <(stage)
install_run() { awk '/^COPY --from=gnome-keyring-builder /{on=1} on{print} on && !/\\$/ && !/^COPY/ && !/^ARG/{exit}' "$BASE"; }
check "base: refuses to install over anything but the pinned Fedora build" \
    has -F 'is not the pinned ${GNOME_KEYRING_NVR}' <(install_run)
check "base: installs ONLY the rebuilt packages (no repository)" \
    has -F "dnf5 -y --nogpgcheck --disable-repo='*' install /tmp/rime-gnome-keyring/*.rpm" <(install_run)
check "base: refuses a gnome-keyring that is not Rime's build" \
    has -F "is not Rime's gnome-keyring build" <(install_run)
check "base: refuses a daemon without the fix compiled in" \
    has -F "grep -aF 'No session for the caller to look up' /usr/bin/gnome-keyring-daemon" <(install_run)
check "base: the image's last word is still Rime's gnome-keyring" \
    has -F "FATAL: a later step replaced Rime's gnome-keyring build" "$BASE"
check "base: no gnome-keyring(-pam) from the repositories" \
    bash -c '! grep -vE "^[[:space:]]*#" "$0" | grep -E "dnf5? .*install([^/]*[[:space:]])?gnome-keyring(-pam)?([[:space:]]|$|&)" >/dev/null' "$BASE"

# ── rebuild on change ────────────────────────────────────────────────────────
# files/** builds an image; the patch is base content, so core is not rebuilt.
check "build-image: a change under files/ builds an image" has -F -e '- "files/**"' "$WF"
check "build-image: the patch does not force a core rebuild" \
    bash -c '! grep -E "CORE_PATHS=.*gnome-keyring" "$0" >/dev/null' "$WF"
check "pr-validation: runs the race against the rebuilt and the stock daemon" \
    bash -c 'grep -F "RIME_KEYRING_RACE_EXPECT=die" "$0" >/dev/null && grep -F "RIME_KEYRING_RACE_EXPECT=survive" "$0" >/dev/null' "$PRV"

[ "${RIME_KEYRING_RACE:-0}" = 1 ] || finish

# ── the race, against a real daemon ──────────────────────────────────────────
EXPECT="${RIME_KEYRING_RACE_EXPECT:-survive}"
K="${RIME_KEYRING_RACE_DAEMONS:-10}"
N="${RIME_KEYRING_RACE_READS:-5000}"
case "$EXPECT" in survive|die) ;; *) echo "RIME_KEYRING_RACE_EXPECT must be survive or die"; exit 2;; esac
[ "$(id -u)" != 0 ] || { skp "the race" "run it as a non-root user"; finish; }
for t in dbus-daemon busctl gnome-keyring-daemon timeout; do
    command -v "$t" >/dev/null 2>&1 || { skp "the race" "no $t"; finish; }
done
GKD="$(command -v gnome-keyring-daemon)"
if grep -aF 'No session for the caller to look up' "$GKD" >/dev/null; then fixed=1; else fixed=0; fi
if [ "$EXPECT" = survive ]; then
    check "the daemon under test carries the fix" test "$fixed" = 1
else
    check "the mutant daemon under test is a stock build" test "$fixed" = 0
fi
command -v rpm >/dev/null 2>&1 && echo "daemon under test: $(rpm -qf "$GKD" 2>/dev/null || echo "$GKD")"

READ=(busctl --user get-property org.freedesktop.secrets /org/freedesktop/secrets/collection/login
      org.freedesktop.Secret.Collection Locked)
deaths=0; reads=0; failed_reads=0
for k in $(seq 1 "$K"); do
    W="$(mktemp -d "${TMPDIR:-/tmp}/rime-gkr.XXXXXX")"
    (
        export HOME="$W/home" XDG_RUNTIME_DIR="$W/run" XDG_DATA_HOME="$W/home/.local/share" XDG_CONFIG_HOME="$W/home/.config"
        mkdir -p "$HOME" "$XDG_RUNTIME_DIR"; chmod 700 "$XDG_RUNTIME_DIR"
        cat > "$W/bus.conf" <<CONF
<busconfig><type>session</type><listen>unix:path=$W/bus</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
        dbus-daemon --config-file="$W/bus.conf" --nofork --nopidfile >"$W/bus.log" 2>&1 & BUS=$!
        for _ in $(seq 50); do [ -S "$W/bus" ] && break; sleep 0.1; done
        export DBUS_SESSION_BUS_ADDRESS="unix:path=$W/bus"
        # The login password unlocks the login keyring, as pam_gnome_keyring does.
        printf 'rime-test-pass' | gnome-keyring-daemon --unlock --components=secrets --foreground >"$W/gk.log" 2>&1 & GKP=$!
        for _ in $(seq 100); do timeout 2 busctl --user status org.freedesktop.secrets >/dev/null 2>&1 && break; sleep 0.1; done
        timeout 10 busctl --user get-property org.freedesktop.secrets /org/freedesktop/secrets \
            org.freedesktop.Secret.Service Collections >/dev/null 2>&1
        i=0; f=0; died=0
        while [ "$i" -lt "$N" ]; do
            timeout 10 "${READ[@]}" >/dev/null 2>&1 || f=$((f+1))
            i=$((i+1))
            kill -0 "$GKP" 2>/dev/null || { died=1; break; }
        done
        kill "$GKP" "$BUS" 2>/dev/null; wait 2>/dev/null
        echo "$i $f $died" > "$W/result"
        if [ "$died" = 1 ]; then
            { grep -E 'assertion|ERROR' "$W/gk.log" | head -n 3; } > "$W/why"
        fi
    )
    read -r i f died < "$W/result" 2>/dev/null || { i=0; f=0; died=1; }
    reads=$((reads+i)); failed_reads=$((failed_reads+f))
    if [ "$died" = 1 ]; then
        deaths=$((deaths+1))
        echo "daemon $k: DIED after read $i"; sed 's/^/    /' "$W/why" 2>/dev/null
    else
        echo "daemon $k: alive after $N reads ($f reads failed)"
    fi
    rm -rf "$W"
    [ "$EXPECT" = die ] && [ "${RIME_KEYRING_RACE_ALL:-0}" != 1 ] && [ "$deaths" -gt 0 ] && break
done
echo "measured: daemons=$k deaths=$deaths reads=$reads failed_reads=$failed_reads"
if [ "$EXPECT" = survive ]; then
    if [ "$deaths" = 0 ]; then ok "every daemon survived $N reads of the login collection ($K daemons, $reads reads)"
    else bad "every daemon survived $N reads of the login collection" "$deaths of $K died"; fi
    # A read the daemon refuses is not a pass either: the user's apps make them.
    if [ "$failed_reads" = 0 ]; then ok "every read was answered"
    else bad "every read was answered" "$failed_reads of $reads failed"; fi
else
    if [ "$deaths" -gt 0 ]; then ok "the stock daemon dies under the same reads (the race is measured)"
    else bad "the stock daemon dies under the same reads" "none of $K died in $reads reads"; fi
fi
finish
