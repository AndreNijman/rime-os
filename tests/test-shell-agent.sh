#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  The agent shell integration, in the shells that are not POSIX.
#
#  bash and zsh share files/desktop/shell/agent.sh. fish and nushell cannot:
#  neither can source a POSIX script, so each has its own file, and the only way
#  to know those files behave the same is to RUN each shell and compare.
#
#  So every assertion below launches a real `fish` or `nu`. Nothing here greps
#  the source for a function name and calls that a passing test — the repo has
#  been bitten by exactly that shape before.
#
#  Nothing touches the developer's own configuration. Each shell is pointed at a
#  fixture XDG_DATA_HOME / XDG_CONFIG_HOME / XDG_STATE_HOME under $WORK, and
#  `rime` on PATH is a stub that records what it was asked. The real daemon is
#  never contacted and no session is ever started.
#
#      ./tests/test-shell-agent.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` is deliberate. This suite COUNTS failures rather than aborting, and
# several assertions run commands that exit non-zero on purpose — a guard
# firing, a usage error, a completion with the runtime down. GitHub Actions
# invokes a script as `bash -e {0}`, and under `-e` an assignment whose command
# exits non-zero kills the whole script part-way through, reporting every
# remaining assertion as a failure.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0; skip=0; sections_run=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
# A skip is LOUD and counted. A silent skip is how a suite reports a green tick
# over nothing asserted.
skipped() { printf 'SKIP  %s — %s\n' "$1" "$2"; skip=$((skip + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

# shellcheck disable=SC2043  # one hard requirement today, written as the same
# tool-check list every other suite here uses.
for tool in python3; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

FISH_CONF="${ROOT}/files/desktop/fish/rime-agent.fish"
FISH_COMP="${ROOT}/files/desktop/fish/completions"
NU_FILE="${ROOT}/files/desktop/nushell/rime.nu"

# ── the fixture ──────────────────────────────────────────────────────────────
#
# A stub `rime` that answers the four queries the integrations make, and — the
# point of it — APPENDS EVERY INVOCATION to a log. That log is what makes the
# "the prompt indicator forks nothing" assertion real rather than a claim about
# the source.
BIN="${WORK}/bin"; mkdir -p "$BIN"
CALLS="${WORK}/rime-calls.log"
: > "$CALLS"
cat > "${BIN}/rime" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >> "${CALLS}"
case "\$*" in
    "agent list --json")       printf '[\n  {\n    "id": 4\n  }\n]\n' ;;
    "agent list --all --json") printf '[\n  {\n    "id": 4\n  },\n  {\n    "id": 7\n  }\n]\n' ;;
    "agent adapters")          printf 'NAME     STATUS\nclaude*  installed\ncodex    installed\n' ;;
    "secret list --json")      printf '[\n  {\n    "service": "github"\n  },\n  {\n    "service": "openai"\n  }\n]\n' ;;
    "secret capabilities")     printf 'capabilities:\n  repo.read\n  repo.write\n' ;;
    "request verbs")           printf 'verbs:\n  pkg.install\n  service.restart\n' ;;
    "request list --all --json") printf '[\n  {\n    "id": 1\n  }\n]\n' ;;
    "project layout templates") printf 'NAME       ARRANGEMENT      OPENS\ndev        main-vertical    editor, agent, terminal\nagents     tiled            several agents\n' ;;
    *) printf 'STUB %s\n' "\$*" ;;
esac
EOF
chmod +x "${BIN}/rime"

# A second stub for the case that matters most in practice: the runtime is not
# running, so every query fails. Completion must stay silent.
DOWN="${WORK}/down"; mkdir -p "$DOWN"
cat > "${DOWN}/rime" <<'EOF'
#!/bin/sh
echo "rime: the agent runtime is not running" >&2
exit 1
EOF
chmod +x "${DOWN}/rime"

# The pre-rename CLI name, stubbed on BOTH fixture PATHs, and it only records.
# On a machine still running APEX, /usr/share/fish/vendor_conf.d/apex-agent.fish
# (rime-rename: keep — the host's file name) is the host's copy of the
# pre-rename integration, and it defines `a` as `apex agent run`. Its name no
# longer matches the file under test, so fish sourced both, the host's `a` won,
# and `a --agent claude "fix the tests"` reached the real /usr/bin/apex: a live
# agent session on the developer's machine, measured. The fixture's vendor dir
# now shadows that name (below); this stub is the second wall, and the fish
# section fails if anything ever reaches it.
for d in "$BIN" "$DOWN"; do
    cat > "${d}/apex" <<EOF
#!/bin/sh
printf 'APEX-STUB %s\n' "\$*" >> "${WORK}/apex-stub.log"
exit 1
EOF
    chmod +x "${d}/apex"
done

# A PATH with no `rime` at all — a partial image, a container. Symlinks rather
# than the real /usr/bin, because that is where `rime` lives on a developer
# machine and including it would test nothing.
NORIME="${WORK}/norime"; mkdir -p "$NORIME"
for b in fish nu bash sh sed cat tail printf date env; do
    p="$(command -v "$b" 2>/dev/null)" && ln -sf "$p" "${NORIME}/${b}"
done

# Where `fish` and `nu` actually are. Every scrubbed PATH below is a fixture
# directory plus `/usr/bin:/bin` — the fixture is what makes "rime is absent"
# and "the runtime is down" mean something, and `/usr/bin:/bin` was standing in
# for "and the interpreter, obviously". It is not obvious: it is an assumption
# about the machine. On this image both shells are in /usr/bin; nushell is in
# no distribution archive, so CI installs it to /usr/local/bin, which no
# scrubbed PATH here contains. Measured on run 34802332142 — twenty-three
# nushell assertions read `env: 'nu': No such file or directory` while
# `command -v nu` in the guard above them answered `/usr/local/bin/nu`. The
# guard and the invocations were asking different questions, so the section
# neither ran nor said it had not.
#
# Symlinks, the same idiom as $NORIME, rather than adding /usr/local/bin: this
# directory can contain nothing but the two interpreters, so it cannot leak a
# stray binary into a fixture that is testing an absence. It is appended LAST
# everywhere for the same reason — it can never shadow $BIN's or $DOWN's stub.
SHELLS="${WORK}/shells"; mkdir -p "$SHELLS"
for b in fish nu; do
    p="$(command -v "$b" 2>/dev/null)" && ln -sf "$p" "${SHELLS}/${b}"
done

PROJ="${WORK}/proj"; mkdir -p "$PROJ"
OUTSIDE="${WORK}/outside"; mkdir -p "$OUTSIDE"

# Session records in the daemon's own on-disk shape: pretty-printed JSON, two
# space indent. The prompt parsers read these directly, so a fixture in any
# other shape would test a format nothing writes.
STATE="${WORK}/state"
SESS="${STATE}/rime/agent/sessions"
mkdir -p "$SESS"
record() { # id project state exit_code
    cat > "${SESS}/$1.json" <<EOF
{
  "id": $1,
  "agent": "claude",
  "cwd": "$2",
  "project": "$2",
  "project_name": "proj",
  "state": "$3",
  "exit_code": $4,
  "attached": 0
}
EOF
}
record 4 "$PROJ"    working          null
record 5 "$PROJ"    permission_request null
record 6 "$PROJ"    waiting_for_user null
record 7 "$OUTSIDE" working          null
record 8 "$PROJ"    working          0

# An empty state directory, for "nothing running".
EMPTY="${WORK}/state-empty"; mkdir -p "${EMPTY}/rime/agent/sessions"

# ── what bash says, to compare against ───────────────────────────────────────
# The parity target is not a description of the prompt format, it is the bytes
# `agent.sh` produces for the same records. Captured once, here.
bash_prompt() { # cwd state_home
    (cd "$1" && env -i PATH="${BIN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
        XDG_STATE_HOME="$2" bash --noprofile --norc -c \
        ". '${ROOT}/files/desktop/shell/agent.sh'; rime_agent_prompt" 2>/dev/null)
}

# ── the bash/zsh opt-out, under both of its names ────────────────────────────
# APEX_NO_AGENT_ALIASES is what a user set before the rename (rime-rename: keep),
# in a ~/.zshrc.local or ~/.bashrc that no image rewrites. Honouring only the new
# name would hand them back shortcuts that shadow their own `a`.
section "bash opt-out"
sh_has_a() { # extra-env…
    env -i PATH="${BIN}:/usr/bin:/bin" HOME="${WORK}/home" "$@" bash --noprofile --norc -c \
        ". '${ROOT}/files/desktop/shell/agent.sh'; type a >/dev/null 2>&1 && echo has-a || echo no-a" 2>/dev/null
}
[ "$(sh_has_a)" = has-a ] \
    && ok "bash gets the shortcuts by default" || bad "bash gets the shortcuts by default"
for v in RIME_NO_AGENT_ALIASES APEX_NO_AGENT_ALIASES; do
    [ "$(sh_has_a "$v=1")" = no-a ] \
        && ok "$v=1 drops the bash shortcuts" || bad "$v=1 drops the bash shortcuts"
done

# ── bash/zsh `aw`: the worktree names the session (§1.4) ─────────────────────
# `aw <worktree>` passes the worktree as `--name` unless the command line or
# $RIME_AGENT_NAME already names it — twice would be refused by the CLI — and
# leaves a name past the 64-character limit off rather than passing it to be
# refused. Asserted in bash and zsh against the stub's own log.
section "bash/zsh aw"
sh_aw() { # shell extra-env… -- aw-args…
    local shell="$1"; shift
    local -a extra=()
    while [ "$1" != "--" ]; do extra+=("$1"); shift; done
    shift
    local q; q="$(printf '%q ' "$@")"
    : > "$CALLS"
    case "$shell" in
        bash) env -i PATH="${BIN}:/usr/bin:/bin" HOME="${WORK}/home" "${extra[@]}" bash --noprofile --norc -c \
                  ". '${ROOT}/files/desktop/shell/agent.sh'; aw ${q}" >/dev/null 2>&1 ;;
        zsh)  env -i PATH="${BIN}:/usr/bin:/bin" HOME="${WORK}/home" "${extra[@]}" zsh -f -c \
                  ". '${ROOT}/files/desktop/shell/agent.sh'; aw ${q}" >/dev/null 2>&1 ;;
    esac
    cat "$CALLS"
}
for sh in bash zsh; do
    if ! command -v "$sh" >/dev/null 2>&1; then
        skipped "$sh aw" "$sh is not installed on this machine"
        continue
    fi
    sections_run=$((sections_run + 1))
    got="$(sh_aw "$sh" -- feature-x "do the thing")"
    [ "$got" = "agent run --worktree feature-x --name feature-x do the thing" ] \
        && ok "$sh \`aw\` names the session after the worktree" \
        || bad "$sh \`aw\` names the session after the worktree (got '$got')"
    got="$(sh_aw "$sh" -- feature-x --name "auth refactor" "do it")"
    [ "$got" = "agent run --worktree feature-x --name auth refactor do it" ] \
        && ok "$sh \`aw\` leaves a --name that was given alone" \
        || bad "$sh \`aw\` leaves a --name that was given alone (got '$got')"
    got="$(sh_aw "$sh" -- feature-x -n mine)"
    [ "$got" = "agent run --worktree feature-x -n mine" ] \
        && ok "$sh \`aw\` leaves a -n that was given alone" \
        || bad "$sh \`aw\` leaves a -n that was given alone (got '$got')"
    got="$(sh_aw "$sh" RIME_AGENT_NAME=from-env -- feature-x)"
    [ "$got" = "agent run --worktree feature-x" ] \
        && ok "$sh \`aw\` leaves \$RIME_AGENT_NAME to the CLI" \
        || bad "$sh \`aw\` leaves \$RIME_AGENT_NAME to the CLI (got '$got')"
    # A `--name` after `--` is the agent's argument, not rime's.
    got="$(sh_aw "$sh" -- feature-x -- --name x)"
    [ "$got" = "agent run --worktree feature-x --name feature-x -- --name x" ] \
        && ok "$sh \`aw\` does not read the agent's own arguments as a name" \
        || bad "$sh \`aw\` does not read the agent's own arguments as a name (got '$got')"
    long="$(printf 'w%.0s' $(seq 1 65))"
    got="$(sh_aw "$sh" -- "$long")"
    [ "$got" = "agent run --worktree $long" ] \
        && ok "$sh \`aw\` leaves a worktree name past the limit off" \
        || bad "$sh \`aw\` leaves a worktree name past the limit off (got '$got')"
done

# ─────────────────────────────────────────────────────────────────────────────
#  fish
# ─────────────────────────────────────────────────────────────────────────────
section "fish"
# `[ -x "${SHELLS}/fish" ]`, not `command -v fish`. The guard has to ask the
# same question the assertions will: the symlink exists exactly when the shell
# was found, and it is what the scrubbed PATH resolves. Asking the ambient PATH
# instead is how the nushell section below spent a run reporting twenty-three
# behaviour failures that were one missing directory.
if [ ! -x "${SHELLS}/fish" ]; then
    skipped "fish integration" "fish is not installed on this machine"
elif [ ! -r "$FISH_CONF" ]; then
    bad "fish: ${FISH_CONF} exists"
else
    sections_run=$((sections_run + 1))
    FD="${WORK}/fishdata"
    mkdir -p "${FD}/fish/vendor_conf.d" "${FD}/fish/vendor_completions.d"
    cp "$FISH_CONF" "${FD}/fish/vendor_conf.d/"
    # Shadows a host's pre-rename copy: fish sources the first file of each
    # name across its vendor directories, and this directory comes first.
    : > "${FD}/fish/vendor_conf.d/apex-agent.fish"  # rime-rename: keep — the host file it shadows
    cp "${FISH_COMP}"/*.fish "${FD}/fish/vendor_completions.d/"

    # Every shipped file must parse. A syntax error in a vendor_conf.d file is a
    # broken shell for every fish user on the image.
    parse_bad=0
    for f in "$FISH_CONF" "${FISH_COMP}"/*.fish; do
        fish -n "$f" </dev/null >/dev/null 2>&1 || { parse_bad=1; echo "      $f"; }
    done
    [ "$parse_bad" -eq 0 ] && ok "every shipped fish file parses" \
                           || bad "every shipped fish file parses"

    fishrun() { # cwd extra-env… -- code
        local cwd="$1"; shift
        local -a extra=()
        while [ "$1" != "--" ]; do extra+=("$1"); shift; done
        shift
        (cd "$cwd" && env -i PATH="${BIN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
            XDG_DATA_HOME="$FD" XDG_CONFIG_HOME="${WORK}/fishcfg" \
            "${extra[@]}" fish -c "$1" 2>&1)
    }

    # ── the shortcuts exist, and are the same six ────────────────────────────
    out="$(fishrun "$PROJ" -- 'for f in a aa al ad aw ap
    functions -q $f; and echo "have $f"; or echo "MISSING $f"
end')"
    if ! printf '%s' "$out" | pipe_has MISSING; then
        ok "fish defines all six shortcuts"
    else
        bad "fish defines all six shortcuts"; printf '      %s\n' "$out"
    fi

    # ── they call the right thing ────────────────────────────────────────────
    : > "$CALLS"
    fishrun "$PROJ" -- 'a --agent claude "fix the tests"' >/dev/null
    grep -qx 'agent run --agent claude fix the tests' "$CALLS" \
        && ok "fish \`a\` runs \`rime agent run\` with its arguments" \
        || { bad "fish \`a\` runs \`rime agent run\` with its arguments"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    fishrun "$PROJ" -- 'ap layout show' >/dev/null
    grep -qx 'project layout show' "$CALLS" \
        && ok "fish \`ap\` forwards to \`rime project\`" || bad "fish \`ap\` forwards to \`rime project\`"

    : > "$CALLS"
    out="$(fishrun "$PROJ" -- 'aw')"
    printf '%s' "$out" | pipe_has 'usage: aw <worktree-name>' \
        && ok "fish \`aw\` with no worktree explains itself" \
        || { bad "fish \`aw\` with no worktree explains itself"; printf '      %s\n' "$out"; }

    : > "$CALLS"
    fishrun "$PROJ" -- 'aw feature-x "do the thing"' >/dev/null
    grep -qx 'agent run --worktree feature-x --name feature-x do the thing' "$CALLS" \
        && ok "fish \`aw\` puts the worktree on the command line, and names the session after it" \
        || { bad "fish \`aw\` puts the worktree on the command line, and names the session after it"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    fishrun "$PROJ" -- 'aw feature-x --name "auth refactor"' >/dev/null
    grep -qx 'agent run --worktree feature-x --name auth refactor' "$CALLS" \
        && ok "fish \`aw\` leaves a --name that was given alone" \
        || { bad "fish \`aw\` leaves a --name that was given alone"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    fishrun "$PROJ" RIME_AGENT_NAME=from-env -- 'aw feature-x' >/dev/null
    grep -qx 'agent run --worktree feature-x' "$CALLS" \
        && ok "fish \`aw\` leaves \$RIME_AGENT_NAME to the CLI" \
        || { bad "fish \`aw\` leaves \$RIME_AGENT_NAME to the CLI"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    fishrun "$PROJ" -- 'aw feature-x -- --name x' >/dev/null
    grep -qx 'agent run --worktree feature-x --name feature-x -- --name x' "$CALLS" \
        && ok "fish \`aw\` does not read the agent's own arguments as a name" \
        || { bad "fish \`aw\` does not read the agent's own arguments as a name"; sed 's/^/      /' "$CALLS"; }

    # `aa` with no id: exactly one running session, so no id is needed. The stub
    # reports one, which is the case the shortcut exists for.
    : > "$CALLS"
    fishrun "$PROJ" -- 'aa' >/dev/null
    grep -qx 'agent attach 4' "$CALLS" \
        && ok "fish \`aa\` attaches to the only running session" \
        || { bad "fish \`aa\` attaches to the only running session"; sed 's/^/      /' "$CALLS"; }

    # ── the opt-out ──────────────────────────────────────────────────────────
    out="$(fishrun "$PROJ" RIME_NO_AGENT_ALIASES=1 -- 'functions -q a; and echo BAD; or echo gone
functions -q rime_agent_prompt; and echo prompt-kept; or echo BAD-PROMPT')"
    printf '%s' "$out" | pipe_has '^gone$' && printf '%s' "$out" | pipe_has 'prompt-kept' \
        && ok "RIME_NO_AGENT_ALIASES drops the shortcuts and keeps the prompt" \
        || { bad "RIME_NO_AGENT_ALIASES drops the shortcuts and keeps the prompt"; printf '      %s\n' "$out"; }

    # …and under its pre-rename name (rime-rename: keep — what users already set).
    out="$(fishrun "$PROJ" APEX_NO_AGENT_ALIASES=1 -- 'functions -q a; and echo BAD; or echo gone')"
    printf '%s' "$out" | pipe_has '^gone$' \
        && ok "APEX_NO_AGENT_ALIASES, the pre-rename name, drops them too" \
        || { bad "APEX_NO_AGENT_ALIASES, the pre-rename name, drops them too"; printf '      %s\n' "$out"; }

    out="$(fishrun "$PROJ" RIME_NO_AGENT_ALIASES=1 -- 'complete -C "rime agent attach "')"
    printf '%s' "$out" | pipe_has '^4' \
        && ok "completion survives the opt-out" || bad "completion survives the opt-out"

    # ── no rime installed ────────────────────────────────────────────────────
    out="$( (cd "$PROJ" && env -i PATH="$NORIME" HOME="${WORK}/home" XDG_DATA_HOME="$FD" \
        XDG_CONFIG_HOME="${WORK}/fishcfg" "${NORIME}/fish" -c \
        'functions -q a; and echo BAD; or echo none; echo alive' 2>&1) )"
    printf '%s' "$out" | pipe_has '^none$' && printf '%s' "$out" | pipe_has '^alive$' \
        && ok "a machine with no rime gets no shortcuts and a working shell" \
        || { bad "a machine with no rime gets no shortcuts and a working shell"; printf '      %s\n' "$out"; }

    # ── the double-source guard ──────────────────────────────────────────────
    # Both fish itself and a user who copied the file into ~/.config/fish/conf.d
    # can source it. The second must be a no-op, not a redefinition.
    out="$(fishrun "$PROJ" -- "source '${FISH_CONF}'; echo sourced-twice-ok; functions -q a; and echo still-have-a")"
    printf '%s' "$out" | pipe_has 'sourced-twice-ok' && printf '%s' "$out" | pipe_has 'still-have-a' \
        && ok "sourcing the fish file twice is harmless" \
        || { bad "sourcing the fish file twice is harmless"; printf '      %s\n' "$out"; }

    # ── the prompt indicator ─────────────────────────────────────────────────
    got="$(fishrun "$PROJ" XDG_STATE_HOME="$STATE" -- 'rime_agent_prompt')"
    want="$(bash_prompt "$PROJ" "$STATE")"
    [ -n "$want" ] && [ "$got" = "$want" ] \
        && ok "the fish prompt is byte-identical to the bash one" \
        || { bad "the fish prompt is byte-identical to the bash one"
             printf '      fish: %s\n      bash: %s\n' "$got" "$want"; }

    # $OUTSIDE has exactly one session of its own, and $PROJ has three. Neither
    # may see the other's — which is the whole reason the prompt matches on the
    # recorded project root instead of just counting session files.
    got="$(fishrun "$OUTSIDE" XDG_STATE_HOME="$STATE" -- 'rime_agent_prompt')"
    want="$(bash_prompt "$OUTSIDE" "$STATE")"
    [ -n "$want" ] && [ "$got" = "$want" ] \
        && ok "another project's prompt counts only its own sessions" \
        || { bad "another project's prompt counts only its own sessions"
             printf '      fish: %s\n      bash: %s\n' "$got" "$want"; }

    got="$(fishrun "${WORK}" XDG_STATE_HOME="$STATE" -- 'rime_agent_prompt')"
    [ -z "$got" ] && ok "a directory no session is working in shows nothing" \
                  || bad "a directory no session is working in shows nothing (got '${got}')"

    got="$(fishrun "$PROJ" XDG_STATE_HOME="$EMPTY" -- 'rime_agent_prompt')"
    [ -z "$got" ] && ok "an empty session directory prints nothing" \
                  || bad "an empty session directory prints nothing (got '${got}')"

    got="$(fishrun "$PROJ" XDG_STATE_HOME="${WORK}/no-such-state" -- 'rime_agent_prompt; echo rc=$status')"
    [ "$got" = "rc=0" ] && ok "no state directory at all is not an error" \
                        || bad "no state directory at all is not an error (got '${got}')"

    # The property the prompt lives or dies by: it runs before every command, so
    # it must not fork. Asserted against the stub's own call log, not against
    # the source.
    : > "$CALLS"
    fishrun "$PROJ" XDG_STATE_HOME="$STATE" -- 'rime_agent_prompt' >/dev/null
    [ ! -s "$CALLS" ] && ok "the fish prompt never runs rime" \
                      || { bad "the fish prompt never runs rime"; sed 's/^/      /' "$CALLS"; }

    # ── completion ───────────────────────────────────────────────────────────
    comp() { fishrun "$PROJ" -- "complete -C \"$1\""; }

    printf '%s' "$(comp 'rime ')" | pipe_has '^agent' \
        && ok "completion offers the top-level verbs" || bad "completion offers the top-level verbs"
    printf '%s' "$(comp 'rime agent ')" | pipe_has '^attach' \
        && ok "completion offers the agent verbs" || bad "completion offers the agent verbs"
    out="$(comp 'rime agent attach ')"
    printf '%s' "$out" | pipe_has '^4' && printf '%s' "$out" | pipe_has '^7' \
        && ok "completion offers session ids, exited ones included" \
        || { bad "completion offers session ids, exited ones included"; printf '      %s\n' "$out"; }
    printf '%s' "$(comp 'rime agent default ')" | pipe_has '^claude' \
        && ok "completion offers agent names with the default marker stripped" \
        || bad "completion offers agent names with the default marker stripped"
    printf '%s' "$(comp 'rime request ask ')" | pipe_has 'pkg.install' \
        && ok "completion asks the CLI for the requestable verbs" \
        || bad "completion asks the CLI for the requestable verbs"
    printf '%s' "$(comp 'rime secret grant ')" | pipe_has '^github' \
        && ok "completion asks the CLI for the stored services" \
        || bad "completion asks the CLI for the stored services"
    printf '%s' "$(comp 'rime secret grant github ')" | pipe_has 'repo.read' \
        && ok "completion asks the CLI for the capability vocabulary" \
        || bad "completion asks the CLI for the capability vocabulary"
    printf '%s' "$(comp 'rime project layout ')" | pipe_has '^restore' \
        && ok "completion offers the layout verbs" || bad "completion offers the layout verbs"
    printf '%s' "$(comp 'rime project layout ')" | pipe_has '^templates' \
        && ok "completion offers the template verbs" || bad "completion offers the template verbs"
    printf '%s' "$(comp 'rime project layout open ')" | pipe_has '^dev' \
        && ok "completion asks the CLI for the layout templates" \
        || bad "completion asks the CLI for the layout templates"
    printf '%s' "$(comp 'aa ')" | pipe_has '^4' \
        && ok "the aa shortcut completes session ids" || bad "the aa shortcut completes session ids"
    printf '%s' "$(comp 'ad ')" | pipe_has '^4' \
        && ok "the ad shortcut completes session ids" || bad "the ad shortcut completes session ids"
    printf '%s' "$(comp 'a --agent ')" | pipe_has '^claude' \
        && ok "the a shortcut completes agent names" || bad "the a shortcut completes agent names"
    printf '%s' "$(comp 'a -')" | pipe_has -- '^--name' \
        && ok "the a shortcut offers --name" || bad "the a shortcut offers --name"
    printf '%s' "$(comp 'rime agent ')" | pipe_has '^rename' \
        && ok "completion offers the rename verb" || bad "completion offers the rename verb"
    printf '%s' "$(comp 'rime agent rename ')" | pipe_has '^4' \
        && ok "rename completes session ids" || bad "rename completes session ids"
    printf '%s' "$(comp 'rime agent rename 4 --')" | pipe_has -- '^--clear' \
        && ok "rename offers --clear" || bad "rename offers --clear"
    printf '%s' "$(comp 'ap layout ')" | pipe_has '^restore' \
        && ok "the ap shortcut completes layout verbs" || bad "the ap shortcut completes layout verbs"

    # `list` is a verb under agent, project, request AND secret. A completion
    # that matched on "the word list was typed" would fire in all four.
    out="$(comp 'rime project ')"
    printf '%s' "$out" | pipe_has '^worktrees' && ! printf '%s' "$out" | pipe_has '^adapters' \
        && ok "the project verbs do not leak the agent verbs" \
        || { bad "the project verbs do not leak the agent verbs"; printf '      %s\n' "$out"; }

    # ── the runtime is down ──────────────────────────────────────────────────
    out="$( (cd "$PROJ" && env -i PATH="${DOWN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
        XDG_DATA_HOME="$FD" XDG_CONFIG_HOME="${WORK}/fishcfg" fish -c \
        'complete -C "rime agent attach "' 2>&1) )"
    [ -z "$out" ] && ok "completion with the runtime down prints nothing at all" \
                  || { bad "completion with the runtime down prints nothing at all"; printf '      %s\n' "$out"; }
fi

# Nothing in any section may have reached the pre-rename CLI (see the stub).
if [ -s "${WORK}/apex-stub.log" ]; then
    bad "no shortcut reached the pre-rename apex CLI"; sed 's/^/      /' "${WORK}/apex-stub.log"
else
    ok "no shortcut reached the pre-rename apex CLI"
fi

# ─────────────────────────────────────────────────────────────────────────────
#  nushell
# ─────────────────────────────────────────────────────────────────────────────
section "nushell"
if [ ! -x "${SHELLS}/nu" ]; then
    skipped "nushell integration" "nu is not installed on this machine"
elif [ ! -r "$NU_FILE" ]; then
    bad "nushell: ${NU_FILE} exists"
else
    sections_run=$((sections_run + 1))

    # nushell reads vendor autoload files in the REPL only — `nu -c` and
    # `nu script.nu` do not see them. So the behaviour assertions `source` the
    # file explicitly, and the install PATH is asserted separately, by asking
    # nushell itself rather than by trusting a hardcoded directory.
    nurun() { # cwd extra-env… -- code
        local cwd="$1"; shift
        local -a extra=()
        while [ "$1" != "--" ]; do extra+=("$1"); shift; done
        shift
        (cd "$cwd" && env -i PATH="${BIN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
            "${extra[@]}" nu -n -c "source ${NU_FILE}
$1" 2>&1)
    }

    out="$(nurun "$PROJ" -- 'print SOURCED')"
    [ "$out" = "SOURCED" ] && ok "the nushell file loads with no error" \
        || { bad "the nushell file loads with no error"; printf '      %s\n' "$out"; }

    # The install directory, asked of nushell. A file in the wrong place breaks
    # nothing visibly — it is simply never read — so "it looks right" is exactly
    # the check that would pass on the day nushell changes it.
    out="$(env -i PATH="/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" nu -n -c \
        '$nu.vendor-autoload-dirs | to text' 2>&1)"
    printf '%s' "$out" | pipe_has -x '/usr/share/nushell/vendor/autoload' \
        && ok "nushell reads the directory the image installs into" \
        || { bad "nushell reads the directory the image installs into"; printf '      %s\n' "$out"; }

    # ── the shortcuts ────────────────────────────────────────────────────────
    : > "$CALLS"
    nurun "$PROJ" -- 'a --agent claude "fix the tests"' >/dev/null
    grep -qx 'agent run --agent claude fix the tests' "$CALLS" \
        && ok "nushell \`a\` runs \`rime agent run\` with its arguments" \
        || { bad "nushell \`a\` runs \`rime agent run\` with its arguments"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'al --all' >/dev/null
    grep -qx 'agent list --all' "$CALLS" \
        && ok "nushell \`al\` forwards flags to \`rime agent list\`" \
        || bad "nushell \`al\` forwards flags to \`rime agent list\`"

    : > "$CALLS"
    nurun "$PROJ" -- 'ap layout show' >/dev/null
    grep -qx 'project layout show' "$CALLS" \
        && ok "nushell \`ap\` forwards to \`rime project\`" || bad "nushell \`ap\` forwards to \`rime project\`"

    : > "$CALLS"
    nurun "$PROJ" -- 'aa' >/dev/null
    grep -qx 'agent attach 4' "$CALLS" \
        && ok "nushell \`aa\` attaches to the only running session" \
        || { bad "nushell \`aa\` attaches to the only running session"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'aa 9 --replay' >/dev/null
    grep -qx 'agent attach 9 --replay' "$CALLS" \
        && ok "nushell \`aa\` forwards an id and its flags verbatim" \
        || { bad "nushell \`aa\` forwards an id and its flags verbatim"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'aw feature-x "do the thing"' >/dev/null
    grep -qx 'agent run --worktree feature-x --name feature-x do the thing' "$CALLS" \
        && ok "nushell \`aw\` puts the worktree on the command line, and names the session after it" \
        || { bad "nushell \`aw\` puts the worktree on the command line, and names the session after it"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'aw feature-x -n mine' >/dev/null
    grep -qx 'agent run --worktree feature-x -n mine' "$CALLS" \
        && ok "nushell \`aw\` leaves a -n that was given alone" \
        || { bad "nushell \`aw\` leaves a -n that was given alone"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" RIME_AGENT_NAME=from-env -- 'aw feature-x' >/dev/null
    grep -qx 'agent run --worktree feature-x' "$CALLS" \
        && ok "nushell \`aw\` leaves \$RIME_AGENT_NAME to the CLI" \
        || { bad "nushell \`aw\` leaves \$RIME_AGENT_NAME to the CLI"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'rime agent rename 4 auth refactor' >/dev/null
    grep -qx 'agent rename 4 auth refactor' "$CALLS" \
        && ok "nushell's rename extern passes the name through" \
        || { bad "nushell's rename extern passes the name through"; sed 's/^/      /' "$CALLS"; }

    out="$(nurun "$PROJ" -- 'aw')"
    printf '%s' "$out" | pipe_has 'usage: aw <worktree-name>' \
        && ok "nushell \`aw\` with no worktree explains itself" \
        || { bad "nushell \`aw\` with no worktree explains itself"; printf '      %s\n' "$out"; }

    # ── externs are a signature, not a gate ──────────────────────────────────
    # The property that makes shipping them safe. An `extern` that refused an
    # argument the CLI accepts would be strictly worse than no completion: it
    # would make a working command look unsupported.
    : > "$CALLS"
    nurun "$PROJ" -- 'rime agent attach 3 --json --not-a-real-flag extra' >/dev/null
    grep -qx 'agent attach 3 --json --not-a-real-flag extra' "$CALLS" \
        && ok "an extern passes unknown flags straight through to rime" \
        || { bad "an extern passes unknown flags straight through to rime"; sed 's/^/      /' "$CALLS"; }

    : > "$CALLS"
    nurun "$PROJ" -- 'rime doctor --deep' >/dev/null
    grep -qx 'doctor --deep' "$CALLS" \
        && ok "a subcommand with no extern is untouched" || bad "a subcommand with no extern is untouched"

    # ── the prompt indicator ─────────────────────────────────────────────────
    got="$(nurun "$PROJ" XDG_STATE_HOME="$STATE" -- 'print -n (rime-agent-prompt)')"
    want="$(bash_prompt "$PROJ" "$STATE")"
    [ -n "$want" ] && [ "$got" = "$want" ] \
        && ok "the nushell prompt is byte-identical to the bash one" \
        || { bad "the nushell prompt is byte-identical to the bash one"
             printf '      nu:   %s\n      bash: %s\n' "$got" "$want"; }

    got="$(nurun "$OUTSIDE" XDG_STATE_HOME="$STATE" -- 'print -n (rime-agent-prompt)')"
    want="$(bash_prompt "$OUTSIDE" "$STATE")"
    [ -n "$want" ] && [ "$got" = "$want" ] \
        && ok "another project's prompt counts only its own sessions (nushell)" \
        || { bad "another project's prompt counts only its own sessions (nushell)"
             printf '      nu:   %s\n      bash: %s\n' "$got" "$want"; }

    got="$(nurun "${WORK}" XDG_STATE_HOME="$STATE" -- 'print -n (rime-agent-prompt)')"
    [ -z "$got" ] && ok "a directory no session is working in shows nothing (nushell)" \
                  || bad "a directory no session is working in shows nothing (nushell) (got '${got}')"

    got="$(nurun "$PROJ" XDG_STATE_HOME="${WORK}/no-such-state" -- 'print -n (rime-agent-prompt)')"
    [ -z "$got" ] && ok "no state directory at all is not an error (nushell)" \
                  || bad "no state directory at all is not an error (nushell) (got '${got}')"

    : > "$CALLS"
    nurun "$PROJ" XDG_STATE_HOME="$STATE" -- 'rime-agent-prompt | ignore' >/dev/null
    [ ! -s "$CALLS" ] && ok "the nushell prompt never runs rime" \
                      || { bad "the nushell prompt never runs rime"; sed 's/^/      /' "$CALLS"; }

    # ── completion sources ───────────────────────────────────────────────────
    nucomp() { nurun "$PROJ" -- "print (($1) | str join ' ')"; }
    [ "$(nucomp 'nu-complete rime sessions')" = "4 7" ] \
        && ok "nushell completes session ids, exited ones included" \
        || bad "nushell completes session ids, exited ones included (got '$(nucomp 'nu-complete rime sessions')')"
    [ "$(nucomp 'nu-complete rime agents')" = "claude codex" ] \
        && ok "nushell completes agent names with the default marker stripped" \
        || bad "nushell completes agent names with the default marker stripped"
    [ "$(nucomp 'nu-complete rime services')" = "github openai" ] \
        && ok "nushell asks the CLI for the stored services" \
        || bad "nushell asks the CLI for the stored services"
    [ "$(nucomp 'nu-complete rime capabilities')" = "repo.read repo.write" ] \
        && ok "nushell asks the CLI for the capability vocabulary" \
        || bad "nushell asks the CLI for the capability vocabulary"
    [ "$(nucomp 'nu-complete rime operations')" = "pkg.install service.restart" ] \
        && ok "nushell asks the CLI for the requestable verbs" \
        || bad "nushell asks the CLI for the requestable verbs"
    [ "$(nucomp 'nu-complete rime templates')" = "dev agents" ] \
        && ok "nushell asks the CLI for the layout templates" \
        || bad "nushell asks the CLI for the layout templates (got '$(nucomp 'nu-complete rime templates')')"
    printf '%s' "$(nucomp 'nu-complete rime layout')" | pipe_has 'templates' \
        && ok "nushell offers the new layout verbs" || bad "nushell offers the new layout verbs"

    # ── the runtime is down ──────────────────────────────────────────────────
    out="$( (cd "$PROJ" && env -i PATH="${DOWN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
        nu -n -c "source ${NU_FILE}
print -n ((nu-complete rime sessions) | str join ' ')" 2>&1) )"
    [ -z "$out" ] && ok "nushell completion with the runtime down is silent and empty" \
                  || { bad "nushell completion with the runtime down is silent and empty"; printf '      %s\n' "$out"; }

    # ── the autoload path, end to end ────────────────────────────────────────
    # Everything above sourced the file by hand, which proves the code but not
    # the install. This drives a real nushell REPL over a pty with the file in a
    # fixture vendor-autoload directory, because the REPL is the only mode that
    # reads one. Answering the cursor-position query is required: reedline asks
    # for it and waits.
    ND="${WORK}/nudata"
    mkdir -p "${ND}/nushell/vendor/autoload" "${WORK}/nucfg/nushell" "${WORK}/nucache"
    cp "$NU_FILE" "${ND}/nushell/vendor/autoload/"
    # Empty config and env files, present so nushell does not open its
    # "create one with defaults (Y/n)" prompt — which would otherwise eat the
    # scripted keystrokes and make this look like an autoload failure.
    : > "${WORK}/nucfg/nushell/config.nu"
    : > "${WORK}/nucfg/nushell/env.nu"
    cat > "${WORK}/replrun.py" <<'PY'
import os, pty, select, sys, time
cmd = sys.argv[1:]
pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)
script = [b'print $"AUTOLOAD-(rime-agent-prompt | describe)"\r', b"exit\r"]
# Sends are keyed on the shell's own OSC 133 markers, never on a timer. A timed
# send races reedline: the second line arrives before the first was submitted,
# the two concatenate, and the failure then looks like a missing command when it
# is really a missing keystroke. 133;B ends a prompt, 133;D ends a command.
# Enter is CR, not LF — a terminal in raw mode gets what the key sends.
out, sent, ready, deadline = b"", 0, False, time.time() + 30
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.3)
    if r:
        try:
            d = os.read(fd, 65536)
        except OSError:
            break            # the pty closed: `exit` worked
        if not d:
            break
        out += d
        if b"\x1b[6n" in d:  # reedline asks where the cursor is and WAITS
            os.write(fd, b"\x1b[1;1R")
        if sent == 0:
            ready = ready or b"133;B" in d
        elif b"133;D" in d:
            ready = True
    if ready and sent < len(script):
        os.write(fd, script[sent]); sent += 1; ready = False
sys.stdout.write(out.decode("utf-8", "replace"))
PY
    repl="$( (cd "$PROJ" && env -i PATH="${BIN}:/usr/bin:/bin:${SHELLS}" HOME="${WORK}/home" \
        XDG_DATA_HOME="$ND" XDG_CONFIG_HOME="${WORK}/nucfg" XDG_CACHE_HOME="${WORK}/nucache" \
        XDG_STATE_HOME="$STATE" TERM=xterm \
        python3 "${WORK}/replrun.py" nu --no-history 2>&1 | tr -d '\r') )"
    if printf '%s' "$repl" | pipe_has 'AUTOLOAD-string'; then
        ok "a real nushell REPL autoloads the file from the vendor directory"
    elif printf '%s' "$repl" | pipe_has -i 'not found'; then
        bad "a real nushell REPL autoloads the file from the vendor directory"
        printf '%s' "$repl" | tail -5 | sed 's/^/      /'
    else
        skipped "the nushell REPL autoload check" \
                "could not drive a reedline REPL on this machine"
    fi
fi

# ─────────────────────────────────────────────────────────────────────────────
printf '\nshell-agent: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skip"
if [ "$sections_run" -eq 0 ]; then
    echo "FATAL: every section skipped — this run asserted nothing" >&2
    exit 2
fi
[ "$fail" -eq 0 ]
