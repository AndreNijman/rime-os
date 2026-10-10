#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-blueprint.sh — assertions against the REAL `rime` binary for
#  roadmap §10: the declarative blueprint, `rime apply` and `rime sync`.
#
#  ── Why this file exists ────────────────────────────────────────────────────
#  `rime apply` is the first Rime verb whose entire job is to change the
#  machine, and the three things it changes are the three that hurt most when a
#  test reaches them: the session the greeter offers, what is installed, and the
#  user's own configuration files. This repository has a history there:
#
#    * a game-mode test applied `ScxSwitch` through a live writer, which is a
#      D-Bus call to scx_loader whose polkit action is not passwordless. It
#      raised a burst of authentication prompts on the developer's desktop and
#      blocked for 177 seconds waiting on a password.
#    * a display test with an isolated HOME still reconfigured the live session,
#      because `hyprctl` does not care what HOME is set to.
#    * a compositor-facade test called `setGaps(0, 0)` to check that a capable
#      action returns true, and set the developer's live Hyprland gaps to zero.
#
#  So the design of `apply` puts THREE independent things between this suite and
#  the machine, and this file asserts all three rather than trusting any one:
#
#    1. `apply` never runs sudo. It converges the privilege domain it is already
#       in and reports the other, so a non-root run — which is all this suite
#       ever performs — cannot reach the session, the package engine or
#       anything outside the isolated HOME. Fake `sudo`, `pkexec`, `secret-tool`
#       and friends sit first on PATH and the suite FAILS if any is invoked.
#    2. `RealConverger::for_apply()` is the only constructor with effects, after
#       `RealWriter::for_daemon`. CI has a static check for that; this file
#       checks the behaviour.
#    3. `RIME_BLUEPRINT_NO_APPLY` refuses outright.
#
#  Because of (1), even a total failure of (2) and (3) could only write inside
#  the throwaway HOME this suite builds. That is the property that makes it safe
#  to assert the live path at all — and asserting it is the point, because a
#  dry run that is never compared against a real run is just a second opinion
#  from the same code.
#
#  ── The one step that reaches a program by absolute path ────────────────────
#  `[development] languages` converges by running `/usr/libexec/rime-env
#  provision <language>`. That is an ABSOLUTE path, so the PATH fakes above
#  cannot intercept it — the same hole the root-domain steps have, except a
#  user-domain step has no domain filtering to fall back on. `RIME_ENV_ENGINE`
#  is what closes it: the language section points it at a recording stub, which
#  is what makes the live convergence path testable without a container. See
#  its doc comment in rimed/rime/src/blueprint.rs for why it is safe to be
#  overridable when rime-pkg's own ENV_ENGINE deliberately is not.
#
#  ── What it deliberately does NOT do ────────────────────────────────────────
#  No root, no network, no polkit, no keyring, no package installs, no session
#  changes. Every `rime apply` here runs as the ordinary user against a HOME
#  under a temp directory, so the only files any of it can write are ones this
#  script created and deletes on exit.
#
#  PASS = every case reports what it should, with the exit code it should, and
#         the recorded tool-call log contains nothing that changes anything.
#
#  Run from anywhere:  ./tests/test-rime-blueprint.sh
#  Uses $RIME_BIN if set; otherwise builds the binary with cargo.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e`, for the same reason as every other suite here: this one COUNTS
# failures instead of aborting, and many assertions run commands that exit
# non-zero on purpose. GitHub Actions invokes a script as `bash -e {0}`, and
# under `-e` the first such command ends the script — silently truncating the
# run rather than reporting anything, which is worse than a failure.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d /tmp/rime-blueprint-test.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0

ok()      { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()     { printf 'FAIL  %s%s\n' "$1" "${2:+  — $2}"; fail=$((fail + 1)); }
section() { printf '\n── %s ──────────────────────────────────────\n' "$1"; }

# ── the binary under test ────────────────────────────────────────────────────
#
# There is no skip path here on purpose. A suite that cannot run must FAIL, not
# report zero assertions and a green tick — that has happened three times in
# this repository, most recently when the labwc keybind suite reported
# "passed=0 failed=0" on its first CI run having asserted nothing at all.
Rime="${RIME_BIN:-}"
if [ -z "$Rime" ]; then
    if ! command -v cargo >/dev/null 2>&1; then
        echo "FATAL: no \$RIME_BIN and no cargo; this suite cannot test anything" >&2
        exit 2
    fi
    echo "building rime (set \$RIME_BIN to skip this)…"
    if ! cargo build -p rime --locked --manifest-path "${ROOT}/rimed/Cargo.toml" >&2; then
        echo "FATAL: could not build the rime binary" >&2
        exit 2
    fi
    Rime="${ROOT}/rimed/target/debug/rime"
fi
if [ ! -x "$Rime" ]; then
    echo "FATAL: $Rime is not an executable" >&2
    exit 2
fi
echo "testing $Rime"

# ── fake tools: the outermost safety net, and an assertion in its own right ──
#
# Everything here is resolved through PATH, so these shadow the real ones. Two
# jobs, as in tests/test-rime-display.sh:
#
#   * determinism — `flatpak list` must not answer from whatever the developer
#     happens to have installed, or the diff changes machine to machine;
#   * proof of isolation — anything that could prompt for a password, unlock a
#     keyring, or change the system is recorded as FORBIDDEN, and the suite
#     fails at the end if the log contains one. `apply` is designed never to
#     invoke any of them; this is what turns "designed never to" into a checked
#     fact.
BIN="${WORK}/bin"; mkdir -p "$BIN"
CALLS="${WORK}/calls.log"; : > "$CALLS"
export RIME_TEST_CALLS="$CALLS"

# git: read-only in every path `rime` reaches it through (`sync export` asks a
# project for its origin remote). Recorded and answered emptily so the bundle's
# contents do not depend on the developer's checkout.
cat > "${BIN}/git" <<'FAKE'
#!/bin/sh
echo "git $*" >> "$RIME_TEST_CALLS"
exit 0
FAKE
chmod +x "${BIN}/git"

# flatpak: `list` and `remotes` are reads; anything else changes the machine.
cat > "${BIN}/flatpak" <<'FAKE'
#!/bin/sh
echo "flatpak $*" >> "$RIME_TEST_CALLS"
case "${1:-}" in
    list|remotes|info) exit 0 ;;
esac
echo "FORBIDDEN flatpak $*" >> "$RIME_TEST_CALLS"
exit 1
FAKE
chmod +x "${BIN}/flatpak"

# Anything that escalates privilege, unlocks a secret, or reconfigures the
# session. `rime apply` must never invoke one of these, so any call at all is a
# failure — not a mode, not an argument, the invocation itself.
for tool in sudo pkexec su doas systemctl scxctl secret-tool \
            gnome-keyring-daemon kwallet-query hyprctl wlr-randr \
            rpm-ostree dnf dnf5 loginctl; do
    cat > "${BIN}/${tool}" <<FAKE
#!/bin/sh
echo "FORBIDDEN ${tool} \$*" >> "\$RIME_TEST_CALLS"
exit 1
FAKE
    chmod +x "${BIN}/${tool}"
done

# Prove the trap itself works before relying on it. Without this, a typo in the
# loop above would make every "nothing forbidden was called" assertion below
# pass for the wrong reason — which is exactly the shape of a vacuous test.
PATH="${BIN}:${PATH}" sudo -n true >/dev/null 2>&1
if grep -q '^FORBIDDEN sudo' "$CALLS"; then
    ok "the forbidden-tool trap records a call (self-test)"
else
    bad "the forbidden-tool trap records a call (self-test)" \
        "the fakes are not on PATH; every isolation assertion below would be vacuous"
fi
: > "$CALLS"

# ── running rime against a throwaway machine ─────────────────────────────────
#
# Each home is a complete, isolated XDG environment. HOME, XDG_CONFIG_HOME and
# XDG_STATE_HOME are all redirected, because the blueprint lives under the
# first, the agent configuration under the second and the generated record
# under the third — isolating only HOME would leave two of the three pointing
# at the developer's own files.
newhome() {
    local h="${WORK}/$1"
    mkdir -p "${h}/.config/rime" "${h}/.local/state"
    printf '%s' "$h"
}

# rime_in <home> <args...>
rime_in() {
    local h="$1"; shift
    env -i \
        HOME="$h" \
        XDG_CONFIG_HOME="${h}/.config" \
        XDG_STATE_HOME="${h}/.local/state" \
        PATH="${BIN}:/usr/bin:/bin" \
        RIME_TEST_CALLS="$CALLS" \
        "$Rime" "$@"
}

# Same, with extra VAR=VALUE settings before the binary.
# rime_env <home> <VAR=VALUE...> -- <args...>
rime_env() {
    local h="$1"; shift
    local -a extra=()
    while [ "${1:-}" != "--" ]; do extra+=("$1"); shift; done
    shift
    env -i \
        HOME="$h" \
        XDG_CONFIG_HOME="${h}/.config" \
        XDG_STATE_HOME="${h}/.local/state" \
        PATH="${BIN}:/usr/bin:/bin" \
        RIME_TEST_CALLS="$CALLS" \
        "${extra[@]}" \
        "$Rime" "$@"
}

# Assert that running rime with these arguments refuses: non-zero exit AND the
# expected text somewhere in its output. A message with exit 0 would let a
# script carry on as though nothing were wrong.
refuses() {
    local name=$1 home=$2 want=$3; shift 3
    local out rc
    out="$(rime_in "$home" "$@" 2>&1 </dev/null)"; rc=$?
    if [ "$rc" = 0 ]; then
        bad "$name" "exited 0; expected a refusal"
        return
    fi
    if grep -qF -- "$want" <<<"$out"; then ok "$name"
    else bad "$name" "expected $(printf '%q' "$want"), got: $(head -2 <<<"$out" | tr '\n' ' ')"; fi
}

exits() {
    local name=$1 want=$2 home=$3; shift 3
    local rc
    rime_in "$home" "$@" >/dev/null 2>&1 </dev/null; rc=$?
    if [ "$rc" = "$want" ]; then ok "$name"
    else bad "$name" "exit $rc, expected $want"; fi
}

# Install a blueprint into a home, from a heredoc on stdin.
#
# A heredoc rather than a committed fixture file, deliberately: the `static` CI
# job runs `tomllib.load` over every *.toml in the tree, so a deliberately
# malformed fixture committed next to this script would fail that job for a
# reason that has nothing to do with the change.
writes_blueprint() {
    cat > "${1}/.config/rime/blueprint.toml"
}

# ═════════════════════════════════════════════════════════════════════════════
section "the CLI advertises what it does"

H_HELP="$(newhome help)"
help="$(rime_in "$H_HELP" blueprint --help 2>&1)"
for want in show diff init; do
    if grep -qE "^\s+$want" <<<"$help"; then ok "blueprint --help lists: $want"
    else bad "blueprint --help lists: $want" "absent"; fi
done

apply_help="$(rime_in "$H_HELP" apply --help 2>&1)"
# The guard has to be discoverable, or the only people who know about it are
# the ones who read the source.
for want in RIME_BLUEPRINT_NO_APPLY --dry-run; do
    if grep -qF -- "$want" <<<"$apply_help"; then ok "apply --help mentions: $want"
    else bad "apply --help mentions: $want" "absent"; fi
done
# …and the two properties the roadmap asked for by name.
for want in Idempotent "never removed"; do
    if grep -qiF -- "$want" <<<"$apply_help"; then ok "apply --help states: $want"
    else bad "apply --help states: $want" "absent"; fi
done

sync_help="$(rime_in "$H_HELP" sync --help 2>&1)"
for want in export import show; do
    if grep -qE "^\s+$want" <<<"$sync_help"; then ok "sync --help lists: $want"
    else bad "sync --help lists: $want" "absent"; fi
done

# ═════════════════════════════════════════════════════════════════════════════
section "a typo is loud, not ignored"

# The whole value of the file. A blueprint that silently ignored an unknown key
# would report a converged machine that never changed — the failure a
# declarative model exists to prevent.
H_BAD="$(newhome bad)"

printf '[desktop]\ncompositer = "niri"\n' > "${WORK}/bad-key.toml"
refuses "an unknown key is refused, and named" "$H_BAD" "compositer" \
        blueprint show --file "${WORK}/bad-key.toml"

printf '[deskotp]\ncompositor = "niri"\n' > "${WORK}/bad-table.toml"
refuses "an unknown table is refused, and named" "$H_BAD" "deskotp" \
        blueprint show --file "${WORK}/bad-table.toml"

printf '[desktop]\ncompositor = "hyperland"\n' > "${WORK}/bad-value.toml"
refuses "an unknown compositor is refused with the real list" "$H_BAD" "hyprland" \
        blueprint show --file "${WORK}/bad-value.toml"

printf '[agent]\nsandbox = "sandboxed"\n' > "${WORK}/bad-sandbox.toml"
refuses "an unknown sandbox policy is refused with the real list" "$H_BAD" "unrestricted" \
        blueprint show --file "${WORK}/bad-sandbox.toml"

# An app name is argv for a package engine that runs as root, and it can arrive
# over `rime sync import` from another machine.
printf '[apps]\ninstall = ["-rf"]\n' > "${WORK}/bad-flag.toml"
refuses "an app name that would be read as a flag is refused" "$H_BAD" "command-line flag" \
        blueprint show --file "${WORK}/bad-flag.toml"

printf '[apps]\ninstall = ["usr/bin/foo"]\n' > "${WORK}/bad-path.toml"
refuses "an app name containing a path separator is refused" "$H_BAD" "only letters" \
        blueprint show --file "${WORK}/bad-path.toml"

printf '[apps]\ninstall = ["../../etc/passwd"]\n' > "${WORK}/bad-dots.toml"
refuses "a traversing app name is refused" "$H_BAD" "starts with '.'" \
        blueprint show --file "${WORK}/bad-dots.toml"

printf '[apps]\ninstall = ["vendor-driver.rpm"]\n' > "${WORK}/bad-rpm.toml"
refuses "a local .rpm is refused: a blueprint must reproduce elsewhere" "$H_BAD" \
        "reproduce on another machine" blueprint show --file "${WORK}/bad-rpm.toml"

refuses "a --file that does not exist is an error, not an empty blueprint" "$H_BAD" \
        "no such file" blueprint show --file "${WORK}/nope.toml"

# §10's own example, with the one value Rime Shell cannot honour corrected.
cat > "${WORK}/roadmap.toml" <<'BP'
[desktop]
compositor = "labwc"
theme = "content"

[apps]
install = ["firefox", "obsidian", "steam"]

[development]
languages = ["python", "rust", "typescript"]

[agent]
default = "claude"
sandbox = "project"

[gaming]
enabled = true
BP
exits "the roadmap's own example parses" 0 "$H_BAD" blueprint show --file "${WORK}/roadmap.toml"

# ═════════════════════════════════════════════════════════════════════════════
section "show and init"

H_NEW="$(newhome new)"
out="$(rime_in "$H_NEW" blueprint show 2>&1)"
if grep -q "manages nothing" <<<"$out"; then ok "a machine with no blueprint says so"
else bad "a machine with no blueprint says so" "got: $(head -1 <<<"$out")"; fi
if grep -q "never on this machine" <<<"$out"; then ok "show reports it has never been applied"
else bad "show reports it has never been applied"; fi

exits "init writes a blueprint" 0 "$H_NEW" blueprint init
if [ -f "${H_NEW}/.config/rime/blueprint.toml" ]; then ok "init created the file"
else bad "init created the file"; fi
# A starter file that changed the machine would be a trap.
out="$(rime_in "$H_NEW" blueprint show 2>&1)"
if grep -q "manages nothing" <<<"$out"; then ok "the starter blueprint manages nothing"
else bad "the starter blueprint manages nothing" "got: $(tail -3 <<<"$out" | tr '\n' ' ')"; fi
refuses "init refuses to clobber an existing blueprint" "$H_NEW" "already exists" blueprint init
exits "init --force overwrites" 0 "$H_NEW" blueprint init --force

# The generated record and the hand-edited file must never share a directory:
# that is how someone ends up editing the wrong one.
json="$(rime_in "$H_NEW" blueprint show --json 2>&1)"
if python3 -c "
import json,sys,os
d=json.loads(sys.argv[1])
u=os.path.dirname(d['paths']['user']); s=os.path.dirname(d['paths']['applied_state'])
sys.exit(0 if u != s else 1)
" "$json" 2>/dev/null; then
    ok "the blueprint and the generated record live in different directories"
else
    bad "the blueprint and the generated record live in different directories"
fi
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
assert d['schema'] == 1, d['schema']
assert d['applied'] is None
assert 'digest' in d
" "$json" 2>/dev/null; then
    ok "show --json is valid JSON with the documented shape"
else
    bad "show --json is valid JSON with the documented shape" "$(head -c 200 <<<"$json")"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "diff measures the machine"

H_DIFF="$(newhome diff)"
writes_blueprint "$H_DIFF" <<'BP'
[desktop]
theme = "monochrome"

[agent]
default = "codex"
sandbox = "strict"
BP

exits "diff exits 1 when there is drift apply could close" 1 "$H_DIFF" blueprint diff
out="$(rime_in "$H_DIFF" blueprint diff 2>&1)"
for want in "[desktop] theme" "[agent] default" "[agent] sandbox" "THIS USER"; do
    if grep -qF -- "$want" <<<"$out"; then ok "diff reports: $want"
    else bad "diff reports: $want"; fi
done

# Blocked changes are real drift the user should see, but they are not drift
# any number of `apply` runs could close, so they must not set the exit code —
# a permanently non-zero exit makes the signal useless in a script.
H_BLOCK="$(newhome blocked)"
writes_blueprint "$H_BLOCK" <<'BP'
[gaming]
enabled = true
BP
out="$(rime_in "$H_BLOCK" blueprint diff 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a blocked-only diff still exits 0"
else bad "a blocked-only diff still exits 0" "exit $rc"; fi
if grep -q "CANNOT CONVERGE" <<<"$out"; then ok "a blocked change is reported anyway"
else bad "a blocked change is reported anyway"; fi
# The refusal must name the command that works. It used to assert the phrase
# "Gaming edition image", which was correct only while there were editions;
# with one image that reason is retired and the remedy is a package install.
if grep -q "sudo rime install steam gamescope" <<<"$out"; then ok "the gaming refusal names the command that fixes it"
else bad "the gaming refusal names the command that fixes it" "$out"; fi
# The retired advice must never come back: rebasing onto gaming-mesa or
# gaming-nvidia is a no-op now, since they resolve to the image already running.
if grep -qE "Gaming edition|rebase onto|gaming-mesa|gaming-nvidia" <<<"$out"; then
  bad "the gaming refusal must not mention a retired edition" "$out"
else ok "the gaming refusal mentions no retired edition"; fi
# …and no step exists for it. Installing a gaming package set onto Daily is the
# edition leakage the root AGENTS.md forbids.
if rime_in "$H_BLOCK" blueprint diff --json 2>&1 | python3 -c "
import json,sys
d=json.load(sys.stdin)
g=[c for c in d['changes'] if c['what'].startswith('[gaming]')]
sys.exit(0 if g and g[0]['step'] is None else 1)
"; then ok "[gaming] produces no step, only a reason"
else bad "[gaming] produces no step, only a reason"; fi

# ═════════════════════════════════════════════════════════════════════════════
section "[development] languages converge through a capsule"

# ── why PATH is reduced to the fake directory for this section ───────────────
# `Host::languages()` probes PATH for gcc, python3, cargo, bash and the rest,
# and both this developer's machine and the CI runner have most of them. A
# planning assertion made against the ambient PATH would therefore pass or fail
# depending on whose machine it ran on. With PATH holding only the fakes, none
# of the probe programs resolve, so "no language is on the host" is a fact
# rather than a hope — and the two ways a language can be satisfied can be
# exercised one at a time.
H_LANG="$(newhome languages)"
writes_blueprint "$H_LANG" <<'BP'
[development]
languages = ["rust", "typescript"]
BP

lang_diff() { rime_env "$H_LANG" PATH="$BIN" -- "$@" 2>&1 </dev/null; }

out="$(lang_diff blueprint diff)"
for want in "[development] rust" "[development] typescript" \
            "provision a capsule for rust" "THIS USER"; do
    if grep -qF -- "$want" <<<"$out"; then ok "diff reports: $want"
    else bad "diff reports: $want" "$(head -6 <<<"$out" | tr '\n' ' ')"; fi
done
if grep -q "CANNOT CONVERGE" <<<"$out"; then
    bad "[development] no longer reports itself blocked" \
        "still printing a CANNOT CONVERGE section"
else
    ok "[development] no longer reports itself blocked"
fi

# A BEHAVIOUR CHANGE on a shipped command, asserted deliberately. Phase 7
# shipped this section with `step: None`, so a machine missing a language
# reported CANNOT CONVERGE and `diff` exited 0 forever. Now there is a step, so
# it exits 1 like every other closeable drift. That looks identical to "the
# feature working" in a self-check, which is why it has its own assertion.
rime_env "$H_LANG" PATH="$BIN" -- blueprint diff >/dev/null 2>&1 </dev/null; rc=$?
if [ "$rc" = 1 ]; then ok "diff now exits 1 for a missing language (was 0)"
else bad "diff now exits 1 for a missing language (was 0)" "exit $rc"; fi

# A step per language, never one for the list: each language maps to its own
# capsule and a partial failure has to leave the others converged.
# Captured first rather than piped: `diff --json` exits 1 when there is drift,
# and under `pipefail` that would fail the pipeline before python3 said anything.
lang_json="$(lang_diff blueprint diff --json)"
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
dev=[c for c in d['changes'] if c['what'].startswith('[development]')]
sys.exit(0 if len(dev) == 2
         and all(c['step'] and c['domain'] == 'user' and c['blocked'] is None
                 for c in dev)
         else 1)
" "$lang_json"; then ok "one user-domain step per language, none blocked"
else bad "one user-domain step per language, none blocked" "$lang_json"; fi

# Capsules are ROOTLESS. `rime-env` refuses to run as root and says so, and a
# root-domain step here would need an authentication prompt to enter the
# capsule afterwards — which is the thing this whole command promises not to
# do. So `sudo rime apply` must have nothing to do for this section.
if python3 -c "
import json,sys
d=json.loads(sys.argv[1])
sys.exit(0 if not [c for c in d['changes']
                   if c['what'].startswith('[development]')
                   and c['domain'] == 'root'] else 1)
" "$lang_json"; then ok "no language step is ever root-domain"
else bad "no language step is ever root-domain" \
        "a rootless capsule must never be provisioned by a root apply"; fi

# ── satisfied way 1: a capsule records that it provides the language ─────────
# This is the mechanism §8 asks for and the reason phase 7 deferred the
# section. The record is the same file `rime env provision` writes, in the same
# directory the engine resolves — if the writer and the observer disagreed
# about that path, `apply` would provision a capsule the next `diff` could not
# see.
mkdir -p "${H_LANG}/.local/share/rime/env"
printf '{"name":"rust","languages":["rust"],"exports":[]}\n' \
    > "${H_LANG}/.local/share/rime/env/rust.json"
out="$(lang_diff blueprint diff)"
if grep -qF "[development] rust" <<<"$out"; then
    bad "a capsule providing a language satisfies it" "rust is still reported as drift"
else
    ok "a capsule providing a language satisfies it"
fi
if grep -qF "[development] typescript" <<<"$out"; then
    ok "…and the other language still drifts"
else
    bad "…and the other language still drifts" "both went quiet, which is a false converge"
fi

# A record that does not parse must contribute nothing and INVENT nothing: one
# hand-edited capsule record must neither take the whole observation down nor
# silently satisfy a language.
printf '{not json\n' > "${H_LANG}/.local/share/rime/env/broken.json"
out="$(lang_diff blueprint diff)"
if grep -qF "[development] typescript" <<<"$out"; then
    ok "a broken capsule record does not break the observation"
else
    bad "a broken capsule record does not break the observation" \
        "$(head -4 <<<"$out" | tr '\n' ' ')"
fi
rm -f "${H_LANG}/.local/share/rime/env/broken.json"

# ── satisfied way 2: the toolchain is already on the host's PATH ─────────────
# The Rime images ship gcc, python3, node and bash, and a developer's own
# toolchains (rustup, a Go tarball, an SDK in ~/.local) are on PATH too, so
# this is the COMMON case. Reading it as drift would provision gigabytes of
# capsule to duplicate software the machine already has, which is the
# "reformats a machine the first time it runs" failure the blueprint type is
# written to avoid. Proved with a fake `tsc` rather than by
# hoping the runner has one.
cat > "${BIN}/tsc" <<'FAKE'
#!/bin/sh
exit 0
FAKE
chmod +x "${BIN}/tsc"
out="$(lang_diff blueprint diff)"
if grep -qF "[development] typescript" <<<"$out"; then
    bad "a toolchain on PATH satisfies the language" "typescript still reported as drift"
else
    ok "a toolchain on PATH satisfies the language"
fi
exits "…and the whole section is then converged" 0 "$H_LANG" \
      blueprint diff 2>/dev/null || true
rm -f "${BIN}/tsc"

# ── the live convergence path ───────────────────────────────────────────────
# The step reaches `/usr/libexec/rime-env` by ABSOLUTE path, so no amount of
# PATH faking intercepts it — the same hole the root-domain steps have, except
# a user-domain step has no domain filtering to fall back on. RIME_ENV_ENGINE
# is what makes the live path testable at all; see its doc comment for why it
# is safe to be overridable when rime-pkg's ENV_ENGINE deliberately is not.
H_PROV="$(newhome provision)"
writes_blueprint "$H_PROV" <<'BP'
[development]
languages = ["go"]
BP
STUB="${WORK}/env-engine-stub"
# The stub writes the capsule record a real `rime env provision` writes, and
# that is the point of it rather than a convenience: `apply` re-measures after
# converging, so this assertion only passes if the ENGINE's record directory
# and the PLANNER's are the same directory. The engine resolves
# ${RIME_ENV_HOME:-${XDG_DATA_HOME:-$HOME/.local/share}/rime/env}; if the two
# halves disagreed, `apply` would report residual drift for a capsule it had
# just created, and the symptom would look like a broken test.
#
# Shell BUILTINS only. PATH here holds nothing but the fakes, so `mkdir` does
# not resolve — a stub that called it would exit 0 having written nothing, and
# then the assertion below would fail for a reason that has nothing to do with
# the code under test. The suite creates the directory instead, and the stub
# fails loudly if its one write does not land.
cat > "$STUB" <<'FAKE'
#!/bin/sh
echo "env-engine $*" >> "$RIME_TEST_CALLS"
[ "${1:-}" = provision ] || exit 1
printf '{"name":"%s","languages":["%s"],"exports":[]}\n' "$2" "$2" \
    > "${HOME}/.local/share/rime/env/$2.json" || exit 1
exit 0
FAKE
chmod +x "$STUB"
mkdir -p "${H_PROV}/.local/share/rime/env"

out="$(rime_env "$H_PROV" PATH="$BIN" RIME_ENV_ENGINE="$STUB" -- apply 2>&1 </dev/null)"; rc=$?
if [ "$rc" = 0 ]; then ok "a live apply converges a language"
else bad "a live apply converges a language" "exit $rc: $(tail -3 <<<"$out" | tr '\n' ' ')"; fi
if grep -q '^env-engine provision go$' "$CALLS"; then
    ok "…by handing the ENGINE the language and nothing else"
else
    bad "…by handing the ENGINE the language and nothing else" \
        "calls: $(grep env-engine "$CALLS" | tr '\n' ' ')"
fi
# The capsule a language lives in is the engine's decision. A capsule name in
# the argv would be a second answer to the same question, which is exactly what
# phase 7 deferred this section to avoid.
if grep -qE '^env-engine provision (cdev|go|node|python|rust|shell) ' "$CALLS"; then
    bad "the CLI never names a capsule" "the argv carries more than the language"
else
    ok "the CLI never names a capsule"
fi
if grep -q '^FORBIDDEN' "$CALLS"; then
    bad "provisioning escalates nothing" "$(grep '^FORBIDDEN' "$CALLS" | head -2 | tr '\n' ' ')"
else
    ok "provisioning escalates nothing"
fi
# The assertion the stub exists for: `apply` re-measured afterwards and found
# the language provided. That can only be true if the engine wrote the record
# where the planner looks for it.
if grep -q "Still not converged" <<<"$out"; then
    bad "the engine's record directory is the one the planner reads" \
        "apply provisioned a capsule and could not then see it"
else
    ok "the engine's record directory is the one the planner reads"
fi
exits "…so a second diff is converged" 0 "$H_PROV" blueprint diff

# A dry run must reach the engine not at all. The steps are printed; nothing is
# spawned, because a dry run never constructs a converger.
: > "$CALLS"
H_PDRY="$(newhome provision-dry)"
# The record directory exists here too, so "the engine never ran" cannot pass
# because a stub that DID run would have failed to write anyway.
mkdir -p "${H_PDRY}/.local/share/rime/env"
writes_blueprint "$H_PDRY" <<'BP'
[development]
languages = ["go"]
BP
out="$(rime_env "$H_PDRY" PATH="$BIN" RIME_ENV_ENGINE="$STUB" -- apply --dry-run 2>&1 </dev/null)"
if grep -q "provision a capsule for go" <<<"$out"; then ok "a dry run prints the step"
else bad "a dry run prints the step" "$(head -6 <<<"$out" | tr '\n' ' ')"; fi
if grep -q env-engine "$CALLS"; then
    bad "…and never runs the engine" "$(grep env-engine "$CALLS" | tr '\n' ' ')"
else
    ok "…and never runs the engine"
fi

# An engine that refuses must be reported as a failure, with the re-measurement
# still showing the drift. Reporting success on the strength of an exit code is
# how a converger comes to believe in a machine that does not exist.
: > "$CALLS"
cat > "$STUB" <<'FAKE'
#!/bin/sh
echo "env-engine $*" >> "$RIME_TEST_CALLS"
echo "rime-env: error: no capsule engine here" >&2
exit 1
FAKE
chmod +x "$STUB"
H_PFAIL="$(newhome provision-failing)"
writes_blueprint "$H_PFAIL" <<'BP'
[development]
languages = ["go"]
BP
out="$(rime_env "$H_PFAIL" PATH="$BIN" RIME_ENV_ENGINE="$STUB" -- apply 2>&1 </dev/null)"; rc=$?
if [ "$rc" = 1 ]; then ok "an engine that refuses makes apply exit non-zero"
else bad "an engine that refuses makes apply exit non-zero" "exit $rc"; fi
if grep -q "FAILED provision a capsule for go" <<<"$out"; then
    ok "the failure is reported as a failure"
else
    bad "the failure is reported as a failure" "$(tail -4 <<<"$out" | tr '\n' ' ')"
fi
if grep -q "re-measured, not assumed" <<<"$out"; then
    ok "apply re-measures and still reports the language missing"
else
    bad "apply re-measures and still reports the language missing"
fi
: > "$CALLS"

# ═════════════════════════════════════════════════════════════════════════════
section "the blueprint classifies apps exactly as the shipped engine does"

# The planner has to classify independently — it compares against `flatpak
# list` and against the engine's requested list, which are different sources —
# but it must not classify DIFFERENTLY, or a blueprint reports an app as
# missing forever while the engine keeps installing it from the other source.
ENGINE="${ROOT}/files/system/libexec/rime-pkg"
if [ ! -f "$ENGINE" ]; then
    bad "the shipped package engine is present" "$ENGINE is missing"
else
    H_CLASS="$(newhome classify)"
    NAMES=(firefox org.gimp.GIMP com.valvesoftware.Steam python3-pip
           md.obsidian.Obsidian gcc-c++ NetworkManager-tui io.github.foo.Bar)
    printf '[apps]\ninstall = [' > "${WORK}/classify.toml"
    for n in "${NAMES[@]}"; do printf '"%s", ' "$n" >> "${WORK}/classify.toml"; done
    printf ']\n' >> "${WORK}/classify.toml"

    plan="$(rime_in "$H_CLASS" blueprint diff --json --file "${WORK}/classify.toml" 2>/dev/null)"
    for n in "${NAMES[@]}"; do
        # The engine's own function, sourced from the shipped script. Sourcing
        # runs its main() with no arguments, which prints usage and returns 0,
        # leaving every function defined exactly as the image has them.
        if bash -c 'e=$1; n=$2; set --; source "$e" >/dev/null 2>&1; is_flatpak_id "$n"' \
               _ "$ENGINE" "$n"; then engine_says=flatpak; else engine_says=package; fi
        blueprint_says="$(python3 -c "
import json,sys
d=json.loads(sys.argv[1]); n=sys.argv[2]
for c in d['changes']:
    if n in c['desired'].split():
        print('flatpak' if 'flatpaks' in c['what'] else 'package'); break
else:
    print('absent')
" "$plan" "$n")"
        if [ "$engine_says" = "$blueprint_says" ]; then
            ok "classification agrees with rime-pkg: $n ($engine_says)"
        else
            bad "classification agrees with rime-pkg: $n" \
                "engine says $engine_says, blueprint says $blueprint_says"
        fi
    done
fi

# ═════════════════════════════════════════════════════════════════════════════
section "the dry run is a report, not a rehearsal"

H_DRY="$(newhome dry)"
writes_blueprint "$H_DRY" <<'BP'
[desktop]
theme = "monochrome"

[agent]
default = "codex"
sandbox = "strict"
BP

dry="$(rime_in "$H_DRY" apply --dry-run 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a dry run exits 0"
else bad "a dry run exits 0" "exit $rc"; fi
if grep -q "nothing was changed" <<<"$dry"; then ok "a dry run says it changed nothing"
else bad "a dry run says it changed nothing"; fi

# Dry-run must WRITE nothing. Not the generated record, not the user's files.
if [ ! -e "${H_DRY}/.local/state/rime/blueprint-state.toml" ]; then
    ok "a dry run writes no generated state file"
else
    bad "a dry run writes no generated state file"
fi
if [ ! -e "${H_DRY}/.config/rime/agent.json" ]; then
    ok "a dry run does not write the agent configuration"
else
    bad "a dry run does not write the agent configuration"
fi
if [ ! -e "${H_DRY}/.config/rime-shell/src/user_data/wallpaper.json" ]; then
    ok "a dry run does not write the shell's wallpaper.json"
else
    bad "a dry run does not write the shell's wallpaper.json"
fi

# The discriminating assertion, and the reason this suite runs a live apply at
# all: "the dry run prints the same steps" is only meaningful if it is compared
# against what a real run actually does. Two identical printouts from the same
# unused code path would prove nothing.
#
# Safe because `apply` never escalates: as an ordinary user, the only steps in
# its domain are files under this throwaway HOME.
dry_steps="$(sed -n 's/^ *set /set /p' <<<"$dry" | sort)"
live="$(rime_in "$H_DRY" apply 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a live apply exits 0 when every step succeeds"
else bad "a live apply exits 0 when every step succeeds" "exit $rc: $(tail -3 <<<"$live" | tr '\n' ' ')"; fi
live_steps="$(sed -n 's/^rime apply: \(set .*\)$/\1/p' <<<"$live" | sort)"
if [ -n "$dry_steps" ] && [ "$dry_steps" = "$live_steps" ]; then
    ok "the live run performs exactly the steps the dry run printed"
else
    bad "the live run performs exactly the steps the dry run printed" \
        "dry=[$(tr '\n' '|' <<<"$dry_steps")] live=[$(tr '\n' '|' <<<"$live_steps")]"
fi

# …and the machine really did move.
if grep -q monochrome "${H_DRY}/.config/rime-shell/src/user_data/wallpaper.json" 2>/dev/null; then
    ok "the colour scheme was actually written"
else
    bad "the colour scheme was actually written"
fi
if grep -q '"default_agent": *"codex"' "${H_DRY}/.config/rime/agent.json" 2>/dev/null; then
    ok "the agent default was actually written"
else
    bad "the agent default was actually written"
fi

exits "diff is converged after applying" 0 "$H_DRY" blueprint diff

# ═════════════════════════════════════════════════════════════════════════════
section "apply is idempotent"

before="$(cat "${H_DRY}/.config/rime-shell/src/user_data/wallpaper.json" 2>/dev/null)"
second="$(rime_in "$H_DRY" apply 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a second apply exits 0"
else bad "a second apply exits 0" "exit $rc"; fi
if grep -q "Nothing to do" <<<"$second"; then ok "a second apply reports nothing to do"
else bad "a second apply reports nothing to do" "$(tail -2 <<<"$second" | tr '\n' ' ')"; fi
if [ "$(sed -n 's/^rime apply: \(set .*\)$/\1/p' <<<"$second" | wc -l)" = 0 ]; then
    ok "a second apply performs no steps"
else
    bad "a second apply performs no steps"
fi
after="$(cat "${H_DRY}/.config/rime-shell/src/user_data/wallpaper.json" 2>/dev/null)"
if [ "$before" = "$after" ]; then ok "a second apply leaves the files byte-identical"
else bad "a second apply leaves the files byte-identical"; fi

# ═════════════════════════════════════════════════════════════════════════════
section "a step that cannot complete is reported, not swallowed"

# The one branch of `apply` the success path never reaches. A user step can
# legitimately fail — `wallpaper.json` is the user's file and can be anything —
# and the failure must be visible in three ways: a non-zero exit, a message
# naming the file, and the re-measurement still reporting the drift. The last
# is the one that matters: reporting success on the strength of an exit code
# is how a converger comes to believe in a machine that does not exist.
H_FAIL="$(newhome failing)"
mkdir -p "${H_FAIL}/.config/rime-shell/src/user_data"
printf '{not json' > "${H_FAIL}/.config/rime-shell/src/user_data/wallpaper.json"
writes_blueprint "$H_FAIL" <<'BP'
[desktop]
theme = "neutral"
BP
out="$(rime_in "$H_FAIL" apply 2>&1)"; rc=$?
if [ "$rc" = 1 ]; then ok "a failed step makes apply exit non-zero"
else bad "a failed step makes apply exit non-zero" "exit $rc"; fi
if grep -q "FAILED set colour scheme" <<<"$out"; then ok "the failure is reported as a failure"
else bad "the failure is reported as a failure" "$(tail -3 <<<"$out" | tr '\n' ' ')"; fi
if grep -q "wallpaper.json is not valid JSON" <<<"$out"; then ok "the message names the file and why"
else bad "the message names the file and why"; fi
if grep -q "Applied 0 of 1 step" <<<"$out"; then ok "the count reflects what actually happened"
else bad "the count reflects what actually happened"; fi
if grep -q "re-measured, not assumed" <<<"$out"; then ok "apply re-measures and still reports the drift"
else bad "apply re-measures and still reports the drift"; fi
# The user's unreadable file must be left exactly as it was. Overwriting it to
# make a step succeed would destroy whatever the user was in the middle of.
if [ "$(cat "${H_FAIL}/.config/rime-shell/src/user_data/wallpaper.json")" = '{not json' ]; then
    ok "a file apply could not parse is left untouched"
else
    bad "a file apply could not parse is left untouched"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "user-owned and generated state stay separate"

# §10's own bullet. `apply` writes a record; it must never write the blueprint.
bp_before="$(cat "${H_DRY}/.config/rime/blueprint.toml")"
rime_in "$H_DRY" apply >/dev/null 2>&1
bp_after="$(cat "${H_DRY}/.config/rime/blueprint.toml")"
if [ "$bp_before" = "$bp_after" ]; then ok "apply never rewrites the blueprint"
else bad "apply never rewrites the blueprint"; fi

STATE="${H_DRY}/.local/state/rime/blueprint-state.toml"
if [ -f "$STATE" ]; then ok "apply wrote its generated record"
else bad "apply wrote its generated record"; fi
if head -1 "$STATE" 2>/dev/null | pipe_has "GENERATED"; then
    ok "the generated record says it is generated, on line 1"
else
    bad "the generated record says it is generated, on line 1"
fi
if grep -q 'domain = "user"' "$STATE" 2>/dev/null; then
    ok "the record names the privilege domain it converged"
else
    bad "the record names the privilege domain it converged"
fi
# The record is not an input. Corrupting it must not change what diff reports.
diff_before="$(rime_in "$H_DRY" blueprint diff 2>&1)"
printf 'this is not toml at all\n' > "$STATE"
diff_after="$(rime_in "$H_DRY" blueprint diff 2>&1)"
if [ "$diff_before" = "$diff_after" ]; then
    ok "diff re-measures the machine and never reads the generated record"
else
    bad "diff re-measures the machine and never reads the generated record"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "the environment guard"

H_GUARD="$(newhome guard)"
writes_blueprint "$H_GUARD" <<'BP'
[desktop]
theme = "neutral"
BP

out="$(rime_env "$H_GUARD" RIME_BLUEPRINT_NO_APPLY=1 -- apply 2>&1)"; rc=$?
if [ "$rc" = 2 ]; then ok "a guarded live apply exits 2"
else bad "a guarded live apply exits 2" "exit $rc"; fi
if grep -q "RIME_BLUEPRINT_NO_APPLY" <<<"$out"; then ok "the refusal names the variable"
else bad "the refusal names the variable"; fi
if [ ! -e "${H_GUARD}/.config/rime-shell/src/user_data/wallpaper.json" ]; then
    ok "a guarded apply changed nothing"
else
    bad "a guarded apply changed nothing"
fi

# "0" is the value someone would expect to turn a guard OFF. It must not.
rc=$(rime_env "$H_GUARD" RIME_BLUEPRINT_NO_APPLY=0 -- apply >/dev/null 2>&1; echo $?)
if [ "$rc" = 2 ]; then ok "RIME_BLUEPRINT_NO_APPLY=0 still refuses"
else bad "RIME_BLUEPRINT_NO_APPLY=0 still refuses" "exit $rc"; fi

# The documented divergence from rime-display-apply's RIME_DISPLAY_NO_LIVE:
# this guard blocks only the LIVE path, so a dry run still works with it set.
# That is what lets CI export it for a whole job as a blanket net while every
# planning assertion still runs — assert it, or someone will "fix" it into
# matching the display guard and silently disable half of this file.
out="$(rime_env "$H_GUARD" RIME_BLUEPRINT_NO_APPLY=1 -- apply --dry-run 2>&1)"; rc=$?
if [ "$rc" = 0 ]; then ok "a dry run still works with the guard set"
else bad "a dry run still works with the guard set" "exit $rc"; fi
if grep -q "set colour scheme to neutral" <<<"$out"; then
    ok "the guarded dry run still reports the plan"
else
    bad "the guarded dry run still reports the plan"
fi

# Empty is the off switch, matching rime-display-apply's Python truthiness
# check. If it were not, setting the variable in a shell profile would disable
# `rime apply` permanently.
rc=$(rime_env "$H_GUARD" RIME_BLUEPRINT_NO_APPLY= -- apply >/dev/null 2>&1; echo $?)
if [ "$rc" = 0 ]; then ok "an empty guard value does not block a real apply"
else bad "an empty guard value does not block a real apply" "exit $rc"; fi

# ═════════════════════════════════════════════════════════════════════════════
section "root-domain work is reported, never attempted"

# The structural reason this suite is safe: `apply` converges only the domain
# it is already in. As an ordinary user it must report the system changes and
# perform none of them — no sudo, no package engine, no session change.
H_ROOT="$(newhome rootdomain)"
writes_blueprint "$H_ROOT" <<'BP'
[desktop]
compositor = "niri"

[apps]
install = ["htop", "org.gimp.GIMP"]
BP
out="$(rime_in "$H_ROOT" apply 2>&1)"; rc=$?
if grep -q "sudo rime apply" <<<"$out"; then ok "system changes are reported with what to run"
else bad "system changes are reported with what to run" "$(tail -3 <<<"$out" | tr '\n' ' ')"; fi
if grep -q "Nothing to do as user" <<<"$out"; then ok "and none of them is attempted as the user"
else bad "and none of them is attempted as the user" "$(tail -3 <<<"$out" | tr '\n' ' ')"; fi
if [ "$rc" = 0 ]; then ok "reporting the other domain is not an error"
else bad "reporting the other domain is not an error" "exit $rc"; fi

# ═════════════════════════════════════════════════════════════════════════════
section "sync carries settings and no secrets"

H_A="$(newhome sync-a)"
H_B="$(newhome sync-b)"
writes_blueprint "$H_A" <<'BP'
[desktop]
compositor = "niri"
theme = "monochrome"

[apps]
install = ["htop", "md.obsidian.Obsidian"]

[development]
languages = ["rust"]

[agent]
default = "codex"
BP

# Plant a sentinel where the runtime keeps credentials. A bundle is a file
# people put in a git repository; §4's whole claim is that agents use
# credentials without the credential travelling, and a sync bundle leaking one
# would undo that at a stroke.
SENTINEL="rime-sentinel-do-not-export-9f3a1c"
mkdir -p "${H_A}/.local/state/rime/secrets"
printf '{"token": "%s"}\n' "$SENTINEL" > "${H_A}/.local/state/rime/secrets/github.json"
printf '{"grant": "%s"}\n' "$SENTINEL" > "${H_A}/.local/state/rime/secret-grants.json"
printf '{"grant": "%s"}\n' "$SENTINEL" > "${H_A}/.local/state/rime/grants.json"

rime_in "$H_A" sync export --output "${WORK}/bundle.toml" >/dev/null 2>&1
if [ -f "${WORK}/bundle.toml" ]; then ok "export wrote a bundle"
else bad "export wrote a bundle"; fi
if grep -q "$SENTINEL" "${WORK}/bundle.toml" 2>/dev/null; then
    bad "a bundle carries no credentials" "the sentinel token is in the bundle"
else
    ok "a bundle carries no credentials"
fi

# The headline flow on a machine that has only run `blueprint init`: every
# section is commented out, so the blueprint is EMPTY. Each section table is
# elided on the way out, and a bundle whose `[blueprint]` table lost its header
# too would fail on the receiving machine with "missing field `blueprint`" —
# the far end, hours later, with no way to tell which side was wrong.
H_EMPTY="$(newhome sync-empty)"
rime_in "$H_EMPTY" blueprint init >/dev/null 2>&1
exits "an empty blueprint exports" 0 "$H_EMPTY" \
      sync export --output "${WORK}/empty.toml" --no-projects
exits "…and the bundle reads back on the other machine" 0 "$H_B" \
      sync show "${WORK}/empty.toml"

exits "sync show reads a bundle without importing it" 0 "$H_B" sync show "${WORK}/bundle.toml"
if [ ! -e "${H_B}/.config/rime/blueprint.toml" ]; then ok "sync show writes nothing"
else bad "sync show writes nothing"; fi

exits "sync import installs the blueprint" 0 "$H_B" sync import "${WORK}/bundle.toml"
a_json="$(rime_in "$H_A" blueprint show --json 2>/dev/null)"
b_json="$(rime_in "$H_B" blueprint show --json 2>/dev/null)"
if python3 -c "
import json,sys
a=json.loads(sys.argv[1]); b=json.loads(sys.argv[2])
sys.exit(0 if a['blueprint'] == b['blueprint'] and a['digest'] == b['digest'] else 1)
" "$a_json" "$b_json" 2>/dev/null; then
    ok "the second machine's blueprint is identical to the first's"
else
    bad "the second machine's blueprint is identical to the first's"
fi

# import must converge nothing: reproducing a machine and changing this one are
# two decisions.
if [ ! -e "${H_B}/.local/state/rime/blueprint-state.toml" ]; then
    ok "import converges nothing"
else
    bad "import converges nothing" "it wrote an applied-state record"
fi
if [ ! -e "${H_B}/.config/rime-shell/src/user_data/wallpaper.json" ]; then
    ok "import did not touch the shell's configuration"
else
    bad "import did not touch the shell's configuration"
fi

# An existing blueprint is the user's own work.
printf '[desktop]\ntheme = "neutral"\n' > "${H_B}/.config/rime/blueprint.toml"
refuses "import refuses to clobber a different blueprint" "$H_B" "--force" \
        sync import "${WORK}/bundle.toml"
exits "import --force replaces it" 0 "$H_B" sync import "${WORK}/bundle.toml" --force
if [ -f "${H_B}/.config/rime/blueprint.toml.previous" ]; then
    ok "--force keeps the previous blueprint"
else
    bad "--force keeps the previous blueprint"
fi

# ═════════════════════════════════════════════════════════════════════════════
section "a bundle is hostile input"

mk_bundle() { printf '[bundle]\nschema = %s\ncreated = 1760000000\n\n[blueprint]\n%s\n' "$1" "$2"; }

mk_bundle 2 "" > "${WORK}/future.toml"
refuses "a bundle from a newer Rime is refused, not half-read" "$H_B" "schema 2" \
        sync import "${WORK}/future.toml"

mk_bundle 1 "[blueprint.apps]
install = [\"-rf\"]" > "${WORK}/evil-app.toml"
refuses "a bundle whose app name is a flag is refused" "$H_B" "command-line flag" \
        sync import "${WORK}/evil-app.toml"

mk_bundle 1 "
[[projects]]
slug = \"x\"
path = \"/usr/share/rime-shell\"" > "${WORK}/evil-usr.toml"
refuses "a bundle project under /usr is refused" "$H_B" "not a project location" \
        sync import "${WORK}/evil-usr.toml"

mk_bundle 1 "
[[projects]]
slug = \"x\"
path = \"/home/someone/../../etc\"" > "${WORK}/evil-dots.toml"
refuses "a bundle project path containing .. is refused" "$H_B" "'..'" \
        sync import "${WORK}/evil-dots.toml"

mk_bundle 1 "
[[projects]]
slug = \"../escape\"
path = \"/home/someone/p\"" > "${WORK}/evil-slug.toml"
refuses "a bundle project slug that escapes its directory is refused" "$H_B" "slug must be" \
        sync import "${WORK}/evil-slug.toml"

# A legitimate project that is simply not here must be REPORTED, never created.
mk_bundle 1 "
[[projects]]
slug = \"rime-os\"
path = \"${WORK}/not-cloned-here\"
remote = \"git@github.com:AndreNijman/rime-os\"" > "${WORK}/absent.toml"
out="$(rime_in "$H_B" sync import "${WORK}/absent.toml" --force 2>&1)"
if grep -q "not on this machine" <<<"$out"; then ok "an absent project is reported"
else bad "an absent project is reported" "$(tail -3 <<<"$out" | tr '\n' ' ')"; fi
if grep -q "AndreNijman/rime-os" <<<"$out"; then ok "…with the remote to clone"
else bad "…with the remote to clone"; fi
if [ ! -e "${WORK}/not-cloned-here" ]; then ok "import created no directory"
else bad "import created no directory" "it made the path up"; fi

# ═════════════════════════════════════════════════════════════════════════════
section "blueprint set — the write path §10's GUI editor needs"

# Without a write verb the shell would have to author TOML itself, which means a
# second implementation of the schema. It drifts the first time a field is added
# and the round-trip stops being lossless — the property the whole design rests
# on. So `set` reuses the same normalise + validate + to_toml + atomic write a
# hand-edited file goes through.

h="$(newhome set-roundtrip)"
printf '[desktop]\ncompositor = "labwc"\n\n[apps]\ninstall = ["firefox", "obsidian", "firefox"]\n' \
    > "${h}/.config/rime/blueprint.toml"

json="$(rime_in "$h" blueprint show --json 2>/dev/null \
        | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["blueprint"]))' 2>/dev/null)"
if [ -n "$json" ]; then
    ok "show --json exposes the blueprint for an editor to read"
else
    bad "show --json exposes the blueprint for an editor to read"
fi

printf '%s' "$json" | rime_in "$h" blueprint set --json - >/dev/null 2>&1
rc=$?
[ "$rc" = 0 ] && ok "a blueprint round-trips back through set" \
              || bad "a blueprint round-trips back through set" "exit $rc"

grep -q 'compositor = "labwc"' "${h}/.config/rime/blueprint.toml" \
    && ok "the round-trip preserves what was declared" \
    || bad "the round-trip preserves what was declared"

# normalise() runs on the way in, exactly as it does for a hand-edited file.
[ "$(grep -c '"firefox"' "${h}/.config/rime/blueprint.toml")" = 1 ] \
    && ok "set dedupes like a hand-edited file, because it uses the same parser" \
    || bad "set dedupes like a hand-edited file"

# Writing desired state and changing the machine are separate verbs. If `set`
# ever created the generated record, `diff` would start agreeing with `apply` by
# construction instead of by measurement.
[ ! -e "${h}/.local/state/rime/blueprint-state.toml" ] \
    && ok "set converges nothing and writes no generated state" \
    || bad "set converges nothing and writes no generated state"

# ── refusals ────────────────────────────────────────────────────────────────
# Each of these must leave the previous good blueprint intact. A write verb that
# truncates on bad input is worse than no write verb: the editor is the only
# thing that calls it, and a bad round-trip would silently unmanage everything.
h2="$(newhome set-refusals)"
printf '[desktop]\ncompositor = "labwc"\n' > "${h2}/.config/rime/blueprint.toml"

printf '' | rime_in "$h2" blueprint set --json - >/dev/null 2>&1
[ $? -ne 0 ] && ok "empty stdin is refused" || bad "empty stdin is refused"

printf 'not json at all' | rime_in "$h2" blueprint set --json - >/dev/null 2>&1
[ $? -ne 0 ] && ok "unparseable JSON is refused" || bad "unparseable JSON is refused"

printf '{"desktop":{"compositor":"nonesuch"}}' | rime_in "$h2" blueprint set --json - >/dev/null 2>&1
[ $? -ne 0 ] && ok "a value validate() rejects is refused" \
             || bad "a value validate() rejects is refused"

rime_in "$h2" blueprint set </dev/null >/dev/null 2>&1
[ $? -ne 0 ] && ok "set without --json - is refused" || bad "set without --json - is refused"

grep -q 'compositor = "labwc"' "${h2}/.config/rime/blueprint.toml" \
    && ok "every refusal left the existing blueprint untouched" \
    || bad "every refusal left the existing blueprint untouched"

# ═════════════════════════════════════════════════════════════════════════════
section "nothing in this suite could prompt for a password"

# The assertion the whole design exists for. `apply` never runs sudo, so no
# polkit dialog and no keyring unlock is reachable from here — and this is what
# turns that from a claim into a checked fact.
forbidden="$(grep '^FORBIDDEN' "$CALLS" 2>/dev/null)"
if [ -z "$forbidden" ]; then
    ok "no privilege-escalating or session-changing tool was invoked"
else
    bad "no privilege-escalating or session-changing tool was invoked" \
        "$(head -3 <<<"$forbidden" | tr '\n' ' ')"
fi
# And the only external tool that ran at all was a read.
unexpected="$(grep -v '^flatpak \(list\|remotes\|info\)' "$CALLS" 2>/dev/null | grep -v '^git ')"
if [ -z "$unexpected" ]; then
    ok "the only tool calls made were read-only"
else
    bad "the only tool calls made were read-only" "$(head -3 <<<"$unexpected" | tr '\n' ' ')"
fi

echo
printf 'rime-blueprint: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
