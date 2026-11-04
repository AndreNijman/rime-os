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
#  Containerfile.base installs Fedora's gnome-keyring rebuilt with it, following
#  whatever gnome-keyring core ships.
#
#  Static (always; no network, no build, no root):
#    • the patch creates the client where it is needed and makes the lookup
#      set an error instead of returning NULL silently;
#    • the one build recipe follows core's gnome-keyring (no pinned version or
#      checksum), accepts Fedora's SRPM only with rpmkeys' "digests signatures
#      OK" against Fedora's release key, marks the release .rime1, checks the
#      prepared source, and warns on a rebase and on an upstream fix;
#    • its decision, on source trees made from the patch itself (needs git):
#      the pre-image and the same code moved 200 lines (a new Fedora version)
#      are "patched", the post-image and a fix in other words are "upstream",
#      a changed context line and a half fix are "fail";
#    • the base build installs ONLY what the builder made (no repository),
#      checks it per mode (patched: Rime's build with the fix compiled in;
#      upstream: Fedora's build unchanged), records the decision in the
#      image, and its last word is still that build;
#    • nothing installs gnome-keyring(-pam) from the repositories in base;
#    • a change to the patch or recipe builds an image, and build-image puts
#      the decision in its summary.
#
#  The race (RIME_KEYRING_RACE=1): the gnome-keyring-daemon on PATH, on a
#  private bus with a throwaway HOME and password, K daemons × N reads of
#  collection/login's Locked property, each read a fresh connection (busctl).
#    RIME_KEYRING_RACE_EXPECT=survive (default) every daemon must survive all
#      N reads, and the daemon must carry the fix (its error string; set
#      RIME_KEYRING_RACE_MARKER=0 for a Fedora build that fixed it upstream);
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
check "recipe: follows the gnome-keyring core ships" \
    has -F "NVR=\"\$(rpm -q --qf '%{NAME}-%{VERSION}-%{RELEASE}' gnome-keyring)\"" "$RECIPE"
check "recipe: pins no version or checksum of its own" \
    bash -c '! grep -E "sha256sum|GNOME_KEYRING_(NVR|SRPM_SHA256)|^NVR=gnome-keyring-" "$0" >/dev/null' "$RECIPE"
check "recipe: uses Fedora's release key for that Fedora version" \
    has -F 'RPM-GPG-KEY-fedora-${DIST#fc}-primary' "$RECIPE"
check "recipe: accepts only \"digests signatures OK\" (an unsigned rpm says \"digests OK\")" \
    bash -c 'grep -F "rpmkeys --dbpath \"\$KR\" --checksig" "$0" >/dev/null && grep -F "*\"digests signatures OK\"*" "$0" >/dev/null' "$RECIPE"
check "recipe: Fedora's repositories first, then Koji (signed copy, then sigcache)" \
    bash -c 'grep -F "dnf5 -q download" "$0" >/dev/null && grep -F "/data/signed/\${KID}/" "$0" >/dev/null && grep -F "/data/sigcache/\${KID}/" "$0" >/dev/null' "$RECIPE"
check "recipe: the release is marked .rime1" \
    bash -c 'grep -F "%autorelease -e rime1/" "$0" >/dev/null && grep -F ".rime1%{?dist}/" "$0" >/dev/null' "$RECIPE"
check "recipe: the patch is added to the spec after Fedora's own" has -F 'a Patch9001:      ${PATCH}' "$RECIPE"
check "recipe: the prepared source is checked for both halves of the fix" \
    bash -c 'grep -F "gkd_secret_service_ensure_client (self, caller);" "$0" >/dev/null && grep -F "No session for the caller to look up" "$0" >/dev/null' "$RECIPE"
check "recipe: makes exactly gnome-keyring and gnome-keyring-pam" has -F 'for p in gnome-keyring gnome-keyring-pam; do' "$RECIPE"
check "recipe: says loudly when it rebased onto a new Fedora version" has -F '::warning title=gnome-keyring rebased::' "$RECIPE"
check "recipe: says the carried patch can be dropped when Fedora has the fix" has -F '::warning title=gnome-keyring fix is upstream::' "$RECIPE"
check "recipe: stops when the patch does not apply and the fix is not there" \
    has -F "patch does not apply to Fedora's \${NVR}, and that source does not already contain the fix" "$RECIPE"
check "recipe: records its decision" has -F "printf 'MODE=%s\\nNVR=%s\\nBUILT=%s\\nPROVEN=%s\\n'" "$RECIPE"
check "recipe: the strings it checks are the patch's" \
    bash -c 'grep -F "No session for the caller to look up" "$0" >/dev/null' "$PATCH"

# ── the recipe's decision, on source trees made from the patch ──────────────
# mkfix MODE OUTDIR SHIFT: each file the patch touches, holding every hunk's
# pre-image (MODE=pre) or post-image (post), with SHIFT more filler lines
# before each hunk than the patch's own line numbers expect.
mkfix() {
    awk -v mode="$1" -v out="$2" -v shift="$3" '
        /^diff --git / { f = $4; sub(/^b\//, "", f); file = out "/" f; d = file; sub(/\/[^\/]*$/, "", d)
                         system("mkdir -p \"" d "\""); inh = 0; next }
        /^-- $/        { inh = 0; next }
        /^@@ /         { inh = 1; for (i = 0; i < shift + 3; i++) print "/* filler " (++c) " */" > file; next }
        !inh           { next }
        /^ /           { print substr($0, 2) > file; next }
        /^$/           { print "" > file; next }
        /^-/           { if (mode == "pre")  print substr($0, 2) > file; next }
        /^\+/          { if (mode == "post") print substr($0, 2) > file; next }
                       { inh = 0 }' "$PATCH"
}
decides() { # expected, fixture dir
    local got
    got="$("$RECIPE" --classify "$2" 2>/dev/null)"
    [ "$got" = "$1" ] || { echo "    got '$got'"; return 1; }
}
if command -v git >/dev/null 2>&1; then
    FX="$(mktemp -d "${TMPDIR:-/tmp}/rime-gkfx.XXXXXX")"
    mkfix pre "$FX/pristine" 0
    mkfix pre "$FX/moved" 200
    mkfix post "$FX/fixed" 0
    mkfix pre "$FX/drifted" 0
    sed -i 's/^\t\/\* The session we.re using to access the object \*\/$/\t\/* The session this caller uses *\//' \
        "$FX/drifted/daemon/dbus/gkd-secret-objects.c"
    # Fedora (or GNOME) fixed it in other words: both halves, none of the patch's text.
    mkdir -p "$FX/reworded/daemon/dbus" "$FX/half/daemon/dbus"
    printf '%s\n' 'GckSession *' 'gkd_secret_service_get_pkcs11_session (GkdSecretService *self, const gchar *caller)' '{' \
        '	gkd_secret_service_ensure_client (self, caller);' '	client = g_hash_table_lookup (self->clients, caller);' '}' \
        > "$FX/reworded/daemon/dbus/gkd-secret-service.c"
    printf '%s\n' 'static GckObject *' 'secret_objects_lookup_gck_object_for_path (GkdSecretObjects *self, const gchar *sender,' '{' \
        '	session = gkd_secret_service_get_pkcs11_session (self->service, sender);' \
        '	if (!session) {' '		g_set_error_literal (error_out, G_DBUS_ERROR, G_DBUS_ERROR_FAILED, "no session");' '		return NULL;' '	}' '}' \
        > "$FX/reworded/daemon/dbus/gkd-secret-objects.c"
    # Only the client half fixed: the getter can still return NULL silently.
    cp "$FX/reworded/daemon/dbus/gkd-secret-service.c" "$FX/half/daemon/dbus/"
    printf '%s\n' 'static GckObject *' 'secret_objects_lookup_gck_object_for_path (GkdSecretObjects *self, const gchar *sender,' '{' \
        '	session = gkd_secret_service_get_pkcs11_session (self->service, sender);' '	g_return_val_if_fail (session, FALSE);' '}' \
        > "$FX/half/daemon/dbus/gkd-secret-objects.c"
    check "decision: Fedora's source as the patch expects it -> patched" decides patched "$FX/pristine"
    check "decision: the same code moved 200 lines (a new Fedora version) -> patched" decides patched "$FX/moved"
    check "decision: the fix already in Fedora's source (patch reverse-applies) -> upstream" decides upstream "$FX/fixed"
    check "decision: the fix upstream in other words -> upstream" decides upstream "$FX/reworded"
    check "decision: a context line changed and no fix -> fail" decides fail "$FX/drifted"
    check "decision: only half the fix upstream -> fail" decides fail "$FX/half"
    check "decision: fail exits non-zero (the build stops)" bash -c '! "$0" --classify "$1" >/dev/null 2>&1' "$RECIPE" "$FX/drifted"
    check "decision: the source trees were left untouched" \
        bash -c '! grep -rF "No session for the caller" "$0" "$1" >/dev/null' "$FX/pristine" "$FX/moved"
    rm -rf "$FX"
else
    skp "the recipe's decision" "no git"
fi

# ── the base build ───────────────────────────────────────────────────────────
check "base: no gnome-keyring version or checksum pinned" \
    bash -c '! grep -E "GNOME_KEYRING_(NVR|SRPM_SHA256)" "$0" >/dev/null' "$BASE"
stage() { awk '/^FROM .* AS gnome-keyring-builder$/{on=1} on && /^FROM / && !/gnome-keyring-builder/{exit} on' "$BASE"; }
check "base: gnome-keyring-builder stage exists, built on core" has -E '^FROM \$\{CORE\} AS gnome-keyring-builder$' "$BASE"
check "base: the builder runs the one recipe" has -F '/build/build-rpms.sh /out/gnome-keyring;' <(stage)
check "base: the builder accepts only a patched or upstream decision" \
    has -F "grep -E '^MODE=(patched|upstream)\$' /out/gnome-keyring/result" <(stage)
install_run() { awk '/^COPY --from=gnome-keyring-builder /{on=1} on{print} on && !/\\$/ && !/^COPY/{exit}' "$BASE"; }
check "base: the install acts on the builder's decision" has -F '. /tmp/rime-gnome-keyring/result;' <(install_run)
check "base: refuses a core other than the one the builder followed" \
    has -F 'but gnome-keyring-builder followed $NVR' <(install_run)
check "base: installs ONLY the builder's packages (no repository)" \
    has -F "dnf5 -y --nogpgcheck --disable-repo='*' install /tmp/rime-gnome-keyring/*.rpm" <(install_run)
check "base: patched refuses a gnome-keyring that is not Rime's build" \
    has -F "is not Rime's gnome-keyring build" <(install_run)
check "base: patched refuses a daemon without the fix compiled in" \
    has -F "grep -aF 'No session for the caller to look up' /usr/bin/gnome-keyring-daemon" <(install_run)
check "base: upstream keeps Fedora's build and warns" \
    bash -c 'grep -F "::warning title=gnome-keyring fix is upstream::" <<<"$0" >/dev/null && grep -F "= \"\$NVR\" ;;" <<<"$0" >/dev/null' "$(install_run)"
check "base: the decision is recorded in the image" \
    has -F 'install -D -m 0644 /tmp/rime-gnome-keyring/result /usr/share/rime/gnome-keyring-fix' <(install_run)
check "base: the image's last word is still the build the fix decided on" \
    bash -c 'grep -F ". /usr/share/rime/gnome-keyring-fix;" "$0" >/dev/null && grep -F "FATAL: a later step replaced gnome-keyring:" "$0" >/dev/null' "$BASE"
check "base: no gnome-keyring(-pam) from the repositories" \
    bash -c '! grep -vE "^[[:space:]]*#" "$0" | grep -E "dnf5? .*install([^/]*[[:space:]])?gnome-keyring(-pam)?([[:space:]]|$|&|;)" >/dev/null' "$BASE"

# ── rebuild on change ────────────────────────────────────────────────────────
# files/** builds an image; the patch is base content, so core is not rebuilt.
check "build-image: a change under files/ builds an image" has -F -e '- "files/**"' "$WF"
check "build-image: the patch does not force a core rebuild" \
    bash -c '! grep -E "CORE_PATHS=.*gnome-keyring" "$0" >/dev/null' "$WF"
check "build-image: the summary says what the gnome-keyring fix did" \
    bash -c 'grep -F "cat /usr/share/rime/gnome-keyring-fix" "$0" >/dev/null && grep -F "### gnome-keyring" "$0" >/dev/null' "$WF"
check "pr-validation: runs the race against the rebuilt and the stock daemon" \
    bash -c 'grep -F "RIME_KEYRING_RACE_EXPECT=die" "$0" >/dev/null && grep -F "RIME_KEYRING_RACE_EXPECT=survive" "$0" >/dev/null' "$PRV"
check "pr-validation: the race follows the recipe's decision" \
    has -F 'files/system/src/gnome-keyring/build-rpms.sh /tmp/rime-gk' "$PRV"

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
if [ "$EXPECT" = survive ] && [ "${RIME_KEYRING_RACE_MARKER:-1}" = 0 ]; then
    echo "daemon under test: Fedora's own build (fixed upstream; its error string is not checked)"
elif [ "$EXPECT" = survive ]; then
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
