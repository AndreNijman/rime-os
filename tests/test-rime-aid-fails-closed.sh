#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-aid-fails-closed.sh — rime-aid refuses to start without its sandbox
#
#  rime-aid runs every inference backend inside `bwrap --unshare-net`. It used
#  to run the backend UNCONFINED when /usr/bin/bwrap was missing and only note
#  it in `rime ai status`. Now the daemon checks the sandbox before it creates
#  anything and refuses to start when the sandbox cannot be applied.
#
#  This drives the BUILT DAEMON as a process, in a private user + mount
#  namespace (`unshare -Urm`), with /usr/bin/bwrap replaced underneath it:
#
#    absent          /usr/bin hidden by an empty tmpfs     -> refuses, no sockets
#    not executable  a 0644 file bound over bwrap          -> refuses, no sockets
#    broken          a bwrap that cannot build namespaces  -> refuses, says why
#    real bwrap      the positive control                  -> starts, both sockets
#
#  The positive control is what makes the refusals mean something: the same
#  environment with a working bwrap must start, or "it exited" proves nothing.
#
#  Nothing here touches the machine. No client ever connects, so no backend
#  is started; the runtime directory, HOME and the model store are temporary.
#
#  Without /usr/bin/bwrap on the host the bind-mount cases cannot be set up and
#  SKIP; RIME_TEST_REQUIRE_BWRAP=1 (CI) turns that skip into a failure.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
cd "$(dirname "$0")" || exit 2
REPO="$(cd .. && pwd)"

if [ "${RIME_AID_IN_NS:-0}" != 1 ]; then
    AID_BIN=${AID_BIN:-$REPO/rimed/target/debug/rime-aid}
    if [ ! -x "$AID_BIN" ]; then
        echo "building rime-aid (not found at $AID_BIN)…"
        ( cd "$REPO/rimed" && cargo build --locked --bin rime-aid ) \
            || { echo "FATAL: build failed"; exit 2; }
    fi
    if ! command -v unshare >/dev/null || ! unshare -Urm true 2>/dev/null; then
        if [ "${RIME_TEST_REQUIRE_BWRAP:-0}" = 1 ]; then
            echo "FATAL: user namespaces are not available, so nothing here can run"
            exit 1
        fi
        echo "SKIP: user namespaces are not available here"
        exit 0
    fi
    exec env RIME_AID_IN_NS=1 AID_BIN="$AID_BIN" unshare -Urm "$(pwd)/$(basename "$0")" "$@"
fi

T=$(mktemp -d /tmp/rime-aid-failclosed.XXXXXX)
trap '[ -n "${KEEP:-}" ] || rm -rf "$T"' EXIT

pass=0; fail=0; skip=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass+1)); }
bad()  { printf 'FAIL  %s  %s\n' "$1" "$2"; fail=$((fail+1)); }
skp()  {
    if [ "${RIME_TEST_REQUIRE_BWRAP:-0}" = 1 ]; then
        bad "$1" "skipped, and RIME_TEST_REQUIRE_BWRAP=1: $2"
    else
        printf 'SKIP  %s  %s\n' "$1" "$2"; skip=$((skip+1))
    fi
}

# A 0644 file, and a bwrap that fails the way a kernel refusing unprivileged
# user namespaces makes the real one fail.
: > "$T/fx-noexec"
chmod 0644 "$T/fx-noexec"
cat > "$T/fx-broken-bwrap" <<'EOF'
#!/bin/sh
echo "bwrap: No permissions to create new namespace" >&2
exit 1
EOF
chmod 0755 "$T/fx-broken-bwrap"

# run_aid NAME SETUP: start the daemon in a nested mount namespace after SETUP
# (a shell fragment run as root-in-the-namespace), with a private HOME, config,
# store and runtime directory. Waits until it either exits or binds its control
# socket, up to 20 s. Leaves $T/NAME.out, $T/NAME.rc (if it exited) and $T/NAME.pid.
run_aid() {
    local name=$1 setup=$2
    mkdir -p "$T/$name/home" "$T/$name/store"
    env -i \
        HOME="$T/$name/home" \
        XDG_CONFIG_HOME="$T/$name/home/.config" \
        XDG_RUNTIME_DIR="$T/$name/run" \
        RIME_AI_STORE="$T/$name/store" \
        PATH=/usr/bin:/usr/sbin \
        AID_BIN="$AID_BIN" SETUP="$setup" \
        unshare -m --propagation private \
        /bin/sh -c 'eval "$SETUP" || exit 99; exec "$AID_BIN"' \
        > "$T/$name.out" 2>&1 &
    local pid=$!
    echo "$pid" > "$T/$name.pid"
    local i=0
    while [ "$i" -lt 200 ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid"
            echo "$?" > "$T/$name.rc"
            return
        fi
        [ -S "$T/$name/run/rime-ai/control.sock" ] && return
        sleep 0.1
        i=$((i+1))
    done
}

# assert_refused NAME REASON: exited non-zero, said it refuses and why, and
# left no runtime directory, let alone a socket.
assert_refused() {
    local name=$1 reason=$2
    if [ ! -f "$T/$name.rc" ]; then
        bad "$name: refuses to start" "the daemon is still running without its sandbox"
        kill -TERM "$(cat "$T/$name.pid")" 2>/dev/null
        wait "$(cat "$T/$name.pid")" 2>/dev/null
        return
    fi
    local rc
    rc=$(cat "$T/$name.rc")
    if [ "$rc" = 99 ]; then
        bad "$name: refuses to start" "the test could not set up the case: $(cat "$T/$name.out")"
        return
    fi
    if [ "$rc" != 0 ]; then ok "$name: exits non-zero ($rc)"
    else bad "$name: exits non-zero" "exited 0: $(cat "$T/$name.out")"; fi
    if grep -F "refusing to start" "$T/$name.out" >/dev/null \
        && grep -F "never runs a model unsandboxed" "$T/$name.out" >/dev/null; then
        ok "$name: says it refuses, and that nothing ran unsandboxed"
    else
        bad "$name: says it refuses" "$(cat "$T/$name.out")"
    fi
    if grep -F -- "$reason" "$T/$name.out" >/dev/null; then ok "$name: names the reason ($reason)"
    else bad "$name: names the reason" "wanted '$reason' in: $(cat "$T/$name.out")"; fi
    if [ ! -e "$T/$name/run/rime-ai" ]; then ok "$name: created no runtime directory or socket"
    else bad "$name: created nothing" "$(ls -la "$T/$name/run/rime-ai")"; fi
}

# 1. Absent. An empty tmpfs over /usr/bin hides bwrap for the daemon alone; the
#    daemon itself is in the repository, and the shell that mounts it is
#    already running.
run_aid absent 'mount -t tmpfs none /usr/bin'
assert_refused absent "/usr/bin/bwrap is not installed"

if [ -f /usr/bin/bwrap ]; then
    # 2. Present but not executable.
    run_aid noexec "mount --bind '$T/fx-noexec' /usr/bin/bwrap"
    assert_refused noexec "/usr/bin/bwrap is not executable"

    # 3. Present, executable, and unable to build the sandbox.
    run_aid broken "mount --bind '$T/fx-broken-bwrap' /usr/bin/bwrap"
    assert_refused broken "No permissions to create new namespace"

    # 4. The positive control: the real bwrap, the same environment.
    run_aid real ':'
    if [ -f "$T/real.rc" ]; then
        bad "real bwrap: the daemon starts" "it exited $(cat "$T/real.rc"): $(cat "$T/real.out")"
    else
        if [ -S "$T/real/run/rime-ai/control.sock" ] && [ -S "$T/real/run/rime-ai/api.sock" ]; then
            ok "real bwrap: the daemon starts and binds both sockets"
        else
            bad "real bwrap: the daemon starts" "no sockets after 20 s: $(cat "$T/real.out")"
        fi
        if grep -F "backends confined with bwrap" "$T/real.out" >/dev/null; then
            ok "real bwrap: the startup line says backends are confined"
        else
            bad "real bwrap: startup line" "$(cat "$T/real.out")"
        fi
        pid=$(cat "$T/real.pid")
        kill -TERM "$pid" 2>/dev/null
        wait "$pid"
        rc=$?
        if [ "$rc" = 0 ]; then ok "real bwrap: SIGTERM stops it cleanly"
        else bad "real bwrap: SIGTERM stops it" "exit $rc"; fi
    fi
else
    skp "not executable / broken / real bwrap" "/usr/bin/bwrap is not on this machine to bind over"
fi

echo
echo "rime-aid fails closed: $pass passed, $fail failed, $skip skipped"
[ "$fail" -eq 0 ]
