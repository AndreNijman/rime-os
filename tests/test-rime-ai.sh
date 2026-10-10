#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-ai.sh — assertions against the REAL `rime` binary for §14's
#  local inference service.
#
#  Nothing here re-implements the CLI. Every case runs the real built binary,
#  and the Rust unit tests in `rimed-core::ai` and `rime::ai` are a different
#  thing from this: they check the planners, this checks what a person or a
#  script actually gets back.
#
#  ── EVERY invocation redirects stdin from /dev/null, and that is load-bearing
#  `rime ai run` accepts a piped prompt (`git diff | rime ai run "review
#  this"`), so with no arguments and an open stdin it waits for EOF — correctly,
#  the same way `cat` does. Measured here: an invocation without the
#  redirection hung for two minutes before being killed. A suite that forgets
#  it does not fail, it hangs, and a hung CI job is worse than a red one
#  because nobody can tell it from a slow one.
#
#  ── What this suite is mostly about: refusals that protect something ────────
#  §14's sharpest decisions are all refusals, and each one guards a specific
#  bad outcome:
#
#    * `--listen` on a TCP port, because a TCP connection carries no peer
#      credential, so a listener on 127.0.0.1 is open to every account on the
#      machine and to anything that can make an HTTP request from a page.
#    * a `--url` pull with no `--digest`, because verifying a download against
#      a digest the same server handed you proves only that it sent the same
#      bytes twice.
#    * a model id that is a path, because ids become directory names in a
#      shared root-owned store.
#
#  ── What it deliberately does NOT do ───────────────────────────────────────
#  No network, no root, no daemon, and nothing that writes to /var/lib/rime/ai
#  — there is an explicit assertion that the shared store is untouched, because
#  it is root-owned and a test that could write there would be a test that
#  could corrupt a real machine's weights. It also never starts `rime-aid`.
#
#  PASS = every verb answers, every refusal refuses with a non-zero exit and a
#         reason, and no TCP listener is ever offered.
#
#  Run from anywhere: ./tests/test-rime-ai.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e`: this suite COUNTS failures, and most cases run commands that exit
# non-zero on purpose. Under `bash -e {0}`, which is how GitHub Actions invokes
# a script, the first of those would end the run and report nothing.
set +e
cd "$(dirname "$0")" || exit 2
REPO=$(cd .. && pwd)

pass=0; fail=0
ok()  { printf 'PASS  %-62s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-62s %s\n' "$1" "$2"; fail=$((fail+1)); }
section() { printf '\n── %s ──\n' "$1"; }

# Compare two files without an external binary.
#
# This used `diff -q`, and `diff` is diffutils — which is NOT installed in the
# container CI builds in. There, `diff -q` exited 127, the test read that as
# "the files differ", and the suite reported that the developer's own state had
# been modified when nothing had touched it.
#
# A false alarm this time. The same shape produces a false PASS the moment the
# comparison is written the other way round, and nothing asserted the binary was
# there — which is the rule these suites already state for prerequisites and did
# not follow for this one.
#
# Command substitution strips trailing newlines from both sides equally, so
# equality is unaffected. These are small text files by construction.
same_file() {
    [ "$(cat "$1" 2>/dev/null)" = "$(cat "$2" 2>/dev/null)" ]
}

command -v python3 >/dev/null 2>&1 || {
    echo "FATAL: python3 is required to validate JSON output" >&2; exit 2
}

RIME_BIN=${RIME_BIN:-$REPO/rimed/target/debug/rime}
if [ ! -x "$RIME_BIN" ]; then
    echo "building the rime binary (not found at $RIME_BIN)…"
    ( cd "$REPO/rimed" && cargo build --locked --bin rime ) || {
        echo "FATAL: could not build the rime binary" >&2; exit 2
    }
fi
[ -x "$RIME_BIN" ] || { echo "FATAL: no rime binary at $RIME_BIN" >&2; exit 2; }

WORK=$(mktemp -d /tmp/rime-ai-test.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

export XDG_CONFIG_HOME=$WORK/config
export XDG_STATE_HOME=$WORK/state
export XDG_RUNTIME_DIR=$WORK/run
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

# The catalogue is image content, installed to /usr/share/rimeos/ai by the
# build. On a developer machine running the previous image it does not exist
# yet, so the suite points at the repository copy — the same file the image
# will carry. Without this, `--available` fails for a reason that has nothing
# to do with the change being tested, which is the coupling
# tests/test-labwc-keybinds.sh already documents for the shell tree.
#
# Asserted rather than assumed present: if the repo copy ever moves, this must
# fail loudly here rather than silently fall back to the installed path.
# Overridable from outside so the non-empty assertion below can be negative
# controlled against a deliberately empty catalogue. Defaults to the repository
# copy, which is what CI uses.
export RIME_AI_CATALOGUE=${RIME_AI_CATALOGUE:-$REPO/config/ai/catalogue.toml}
[ -f "$RIME_AI_CATALOGUE" ] || {
    echo "FATAL: no catalogue at $RIME_AI_CATALOGUE — has config/ai/ moved?" >&2
    exit 2
}

# The shared store is root-owned on a real machine. Recorded before anything
# runs so the last case can prove nothing here wrote to it.
STORE=/var/lib/rime/ai
STORE_BEFORE=$WORK/store-before
if [ -d "$STORE" ]; then
    find "$STORE" -printf '%p %s\n' 2>/dev/null | sort > "$STORE_BEFORE"
else
    : > "$STORE_BEFORE"
fi

# ── the only way this suite invokes the binary ──────────────────────────────
# stdin from /dev/null, always. See the header: without it `rime ai run` waits
# for a piped prompt and the suite hangs rather than failing.
ai()   { "$RIME_BIN" ai "$@" </dev/null 2>&1; }
rc_of() { "$RIME_BIN" ai "$@" >/dev/null 2>&1 </dev/null; printf '%s' "$?"; }

# ── the harness is honest about what it is testing ──────────────────────────
section "the harness itself"
if [ "$("$RIME_BIN" --version | head -1 | grep -c rime)" = "1" ]; then
    ok "the binary under test answers --version"
else
    bad "the binary under test answers --version" "$("$RIME_BIN" --version 2>&1 | head -1)"
fi
# A verb that does not exist must not silently pass, or every case below could
# be asserting against clap's usage text rather than against the feature.
#
# Captured to a variable and then grepped, never piped. `set -o pipefail` is on
# for this file, so `deliberately-failing-command | grep` reports the COMMAND's
# non-zero status even when grep matched — which made this case fail while both
# halves of it were true.
out=$(ai nosuchverb)
if [ "$(rc_of nosuchverb)" != "0" ] && printf '%s' "$out" | pipe_has -iE "unrecognized|error"; then
    ok "a verb that does not exist is an error (so the cases below mean something)"
else
    bad "a nonexistent verb errors" "$out"
fi

# ── the TCP refusal, which is the security decision of §14 ──────────────────
section "the endpoint is a Unix socket, and only that"
out=$(ai serve --listen 127.0.0.1:11434)
if [ "$(rc_of serve --listen 127.0.0.1:11434)" != "0" ]; then
    ok "--listen exits non-zero, so a script cannot miss the refusal"
else
    bad "--listen exits non-zero" "exit 0"
fi
if printf '%s' "$out" | pipe_has "peer credential"; then
    ok "the refusal gives the reason: a TCP connection carries no peer credential"
else
    bad "the refusal names peer credentials" "$out"
fi
if printf '%s' "$out" | pipe_has "SO_PEERCRED"; then
    ok "the refusal names the mechanism that does work"
else
    bad "the refusal names SO_PEERCRED" "$out"
fi
if printf '%s' "$out" | pipe_has "rime host"; then
    ok "the refusal points at the transport that does cross machines"
else
    bad "the refusal points at rime host" "$out"
fi
# The absence assertion: no verb may advertise a TCP endpoint anywhere.
if ai serve | pipe_has -E "127\.0\.0\.1:[0-9]+|0\.0\.0\.0:[0-9]+|localhost:[0-9]+"; then
    bad "no verb advertises a TCP endpoint" "$(ai serve | head -3)"
else
    ok "no verb advertises a TCP endpoint"
fi
if ai status | pipe_has "\.sock"; then
    ok "status names a socket path as the endpoint"
else
    bad "status names a socket path" "$(ai status | head -4)"
fi
# It must be inside this user's runtime directory, not a shared location.
ep=$(ai status | sed -n 's/^endpoint: *//p' | tr -d ' ')
case "$ep" in
  "$XDG_RUNTIME_DIR"/*) ok "the endpoint is inside this user's \$XDG_RUNTIME_DIR" ;;
  *) bad "the endpoint is inside \$XDG_RUNTIME_DIR" "got [$ep]" ;;
esac

# ── pull, and what a digest actually proves ─────────────────────────────────
section "pulling a model"
out=$(ai pull https://evil.example/model.gguf)
if [ "$(rc_of pull https://evil.example/model.gguf)" != "0" ]; then
    ok "a bare URL is refused with a non-zero exit"
else
    bad "a bare URL is refused" "exit 0"
fi
if printf '%s' "$out" | pipe_has "same bytes twice"; then
    ok "the refusal explains what a self-served digest would prove"
else
    bad "the refusal explains the digest argument" "$out"
fi
if printf '%s' "$out" | pipe_has "catalogue"; then
    ok "the refusal points at the catalogue shipped in the signed image"
else
    bad "the refusal points at the catalogue" "$out"
fi

# Ids become directory names in a shared, root-owned store.
#
# Each case requires the MALFORMED-id message, not merely a non-zero exit. An
# earlier version accepted any failure, and a mutant that disabled id
# validation entirely still passed five of the six — because an id that is not
# in the catalogue is refused for that reason instead, which is a different
# check reached later. "It refused" was true; "it refused because the id is
# malformed" was what needed asserting.
# Each malformed id gets its OWN reason — "it contains '/', and the id is a
# path component under models/manifests", "it is empty", a length message —
# which is better than one generic refusal and is why this asserts on the two
# things they all share: the "is not usable" prefix, which distinguishes a
# malformed id from one that is merely absent from the catalogue, and the rule
# sentence.
RULE="is not usable"
RULE2="Ids are 1-96 characters"
for badid in "../../etc/passwd" "a/b" ".hidden" "" "$(printf 'x%.0s' $(seq 1 200))"; do
    got=$(ai pull "$badid")
    if [ "$(rc_of pull "$badid")" != "0" ] \
       && printf '%s' "$got" | pipe_has -F "$RULE" \
       && printf '%s' "$got" | pipe_has -F "$RULE2"; then
        ok "model id $(printf '%.20s' "${badid:-(empty)}") is refused AS MALFORMED"
    else
        bad "model id ${badid:-(empty)} is refused as malformed" "$(printf '%.90s' "$got")"
    fi
done
# `-dash` is refused by clap as an unknown option before the id check ever
# runs, which is a different and equally acceptable refusal — asserted
# separately so it is not mistaken for id validation.
if [ "$(rc_of pull -dash)" != "0" ]; then
    ok "an id starting with a dash is refused (by argument parsing, not the id check)"
else
    bad "an id starting with a dash is refused" "accepted"
fi
out=$(ai pull "../../etc/passwd")
if printf '%s' "$out" | pipe_has "Ids are 1-96 characters"; then
    ok "the id refusal states the rule rather than just saying invalid"
else
    bad "the id refusal states the rule" "$out"
fi

# An unknown-but-legal id is a different refusal from an illegal one.
out=$(ai pull totally-unknown-model)
if [ "$(rc_of pull totally-unknown-model)" != "0" ]; then
    ok "a legal id that is not in the catalogue is refused"
else
    bad "an unknown catalogue id is refused" "exit 0"
fi
if ! printf '%s' "$out" | pipe_has "must start with a letter"; then
    ok "an unknown id does not reuse the malformed-id message"
else
    bad "unknown and malformed ids give different messages" "$out"
fi

# --dry-run must not need root and must not write.
if [ "$(rc_of pull totally-unknown-model --dry-run)" != "0" ]; then
    ok "--dry-run on an unknown model still refuses"
else
    bad "--dry-run on an unknown model refuses" "exit 0"
fi

# ── models ──────────────────────────────────────────────────────────────────
section "listing models"
out=$(ai models)
if [ "$(rc_of models)" = "0" ]; then
    ok "listing an empty store is not an error"
else
    bad "listing an empty store is not an error" "exit $(rc_of models)"
fi
if printf '%s' "$out" | pipe_has "rime ai models --available" && printf '%s' "$out" | pipe_has "pull"; then
    ok "an empty store names the two commands that change that"
else
    bad "an empty store names the next commands" "$out"
fi
if ai models --json | python3 -c "import json,sys; json.load(sys.stdin)" 2>/dev/null; then
    ok "models --json is valid JSON"
else
    bad "models --json is valid JSON" "$(ai models --json | head -3)"
fi
avail=$(ai models --available)
if printf '%s' "$avail" | pipe_has -E "[a-z0-9]"; then
    ok "--available reports what the image's catalogue offers"
else
    bad "--available reports the catalogue" "$avail"
fi
# Every id the catalogue offers must be one `pull` would accept — otherwise the
# catalogue could advertise a model no verb can install.
# The catalogue must not be empty, because the parity assertion below is
# vacuously true when it is — "every id it advertises is pullable" passes
# trivially over zero ids, which is exactly how it passed before there were any
# entries. This makes an accidental emptying fail here.
adverts=$(printf '%s' "$avail" | grep -cE "^ +[a-z0-9][a-z0-9._+-]* +[0-9]+ MiB")
if [ "$adverts" -ge 1 ]; then
    ok "the catalogue advertises at least one model ($adverts)"
else
    bad "the catalogue advertises at least one model" "it is empty, so the next assertion proves nothing"
fi

badids=0
while read -r id; do
    [ -n "$id" ] || continue
    [ "$(rc_of pull "$id" --dry-run)" = "2" ] && badids=$((badids+1))
done <<< "$(printf '%s' "$avail" \
              | sed -n 's/^ \+\([a-z0-9][a-z0-9._+-]*\) \+[0-9]\+ MiB.*/\1/p')"
if [ "$badids" = "0" ]; then
    ok "every id the catalogue advertises is one pull will accept"
else
    bad "every advertised id is pullable" "$badids were refused as malformed"
fi

# ── status ──────────────────────────────────────────────────────────────────
section "status"
out=$(ai status)
if [ "$(rc_of status)" = "0" ]; then
    ok "status with no service running is not an error"
else
    bad "status with no service is not an error" "exit $(rc_of status)"
fi
if printf '%s' "$out" | pipe_has -i "not running"; then
    ok "status says the service is not running rather than implying it is"
else
    bad "status reports the service state" "$out"
fi
# The install hint must name a real Rime mechanism, not invent a third one.
if printf '%s' "$out" | pipe_has -E "rime install|rime env"; then
    ok "the runtime hint uses rime install or rime env, not a new mechanism"
else
    bad "the runtime hint uses an existing mechanism" "$out"
fi
if printf '%s' "$out" | pipe_has -E "cpu"; then
    ok "status reports at least the cpu backend as available"
else
    bad "status reports available backends" "$out"
fi
if ai status --json | python3 -c "import json,sys; json.load(sys.stdin)" 2>/dev/null; then
    ok "status --json is valid JSON"
else
    bad "status --json is valid JSON" "$(ai status --json | head -3)"
fi
# Nothing may claim a backend it cannot demonstrate. cuda on a machine with no
# nvidia-smi would be a claim.
if command -v nvidia-smi >/dev/null 2>&1; then
    ok "this machine has nvidia-smi, so cuda may legitimately be listed"
elif ai status | pipe_has "available.*cuda"; then
    bad "cuda is not claimed without nvidia-smi" "$(ai status | grep available)"
else
    ok "cuda is not claimed on a machine without nvidia-smi"
fi

# ── run ─────────────────────────────────────────────────────────────────────
section "run"
out=$(ai run)
if [ "$(rc_of run)" != "0" ]; then
    ok "no prompt at all is refused with a non-zero exit"
else
    bad "no prompt is refused" "exit 0"
fi
if printf '%s' "$out" | pipe_has "pipe it in" || printf '%s' "$out" | pipe_has "|"; then
    ok "the refusal names both ways to give a prompt"
else
    bad "the refusal names both ways to give a prompt" "$out"
fi

# --explain must plan and generate nothing. With no runtime installed it must
# still say something useful rather than failing obscurely.
out=$(ai run --explain "hello")
if printf '%s' "$out" | pipe_has -E "endpoint|Request"; then
    ok "--explain prints a plan"
else
    bad "--explain prints a plan" "$out"
fi
if printf '%s' "$out" | pipe_has "\.sock"; then
    ok "the plan names the socket it would use"
else
    bad "the plan names the endpoint" "$out"
fi
# A plan is not a generation: nothing may be produced.
if printf '%s' "$out" | pipe_has -iE "^(assistant|answer):"; then
    bad "--explain generates nothing" "it produced output that looks like a reply"
else
    ok "--explain generates nothing"
fi

# A piped prompt is accepted — the reason stdin must be redirected everywhere
# else in this file.
out=$(printf 'from a pipe\n' | "$RIME_BIN" ai run --explain 2>&1)
if printf '%s' "$out" | pipe_has -E "endpoint|Request"; then
    ok "a piped prompt is accepted"
else
    bad "a piped prompt is accepted" "$out"
fi

# ── the shared store is never touched by an unprivileged run ────────────────
section "the shared store"
STORE_AFTER=$WORK/store-after
if [ -d "$STORE" ]; then
    find "$STORE" -printf '%p %s\n' 2>/dev/null | sort > "$STORE_AFTER"
else
    : > "$STORE_AFTER"
fi
if same_file "$STORE_BEFORE" "$STORE_AFTER"; then
    ok "nothing in this suite wrote to the root-owned shared store"
else
    bad "the shared store was not touched" "IT CHANGED — see $STORE"
fi
# And the store path must be under /var, never /usr, which is read-only.
if ai status | pipe_has "/var/"; then
    ok "the store lives under /var, not the read-only /usr"
else
    bad "the store lives under /var" "$(ai status | grep -i store)"
fi

# ── no daemon was started ──────────────────────────────────────────────────
section "no side effects"
# `pgrep -x`, not `pgrep -f`: -f matches this script's own command line, which
# produced five false positives in this repository's history.
if pgrep -x rime-aid >/dev/null 2>&1; then
    bad "no rime-aid daemon was started by this suite" "one is running"
else
    ok "no rime-aid daemon was started by this suite"
fi
# No socket may have been created in the fixture runtime directory either.
# Materialised, not piped: `find … | grep -q .` makes grep exit on the first
# line, find dies of SIGPIPE, and under `pipefail` the pipeline is 141 — so the
# `if` took the else branch and this said "no socket was created" exactly WHEN
# one had been. Another assertion that could only fail open.
find "$XDG_RUNTIME_DIR" -type s > "${WORK}/stray-sockets.txt" 2>/dev/null
if [ -s "${WORK}/stray-sockets.txt" ]; then
    bad "no socket was created" "$(cat "${WORK}/stray-sockets.txt")"
else
    ok "no socket was created"
fi

printf '\nrime ai: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
