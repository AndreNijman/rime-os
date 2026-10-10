#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  End-to-end assertions for `rime project layout` (roadmap §6).
#
#  The unit tests in rime-agent-core cover the capture rule, the descendant cwd
#  walk, the terminal-flag table and the store. What they cannot cover is the
#  command: that `save` refuses to overwrite a good layout with an empty one,
#  that `restore` never involves a shell, and that a window belonging to a
#  DIFFERENT project is not captured.
#
#  The compositor adapter is FAKED, through RIME_WINDOW_ADAPTER. That is the
#  whole reason the adapter is a separate program: the window list is the only
#  compositor-specific input, so replacing it makes every assertion below
#  deterministic and runnable with no compositor at all.
#
#  Nothing here starts a real window. `restore` is only ever called with
#  --dry-run, so the suite cannot open anything on the developer's desktop.
#
#      ./tests/test-project-layout.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
# `set +e` is deliberate and load-bearing. This suite COUNTS failures rather
# than aborting on them, and several assertions run commands that exit non-zero
# on purpose — a refusal, a guard firing, a bad argument. GitHub Actions invokes
# a script as `bash -e {0}`, and under `-e` a `x="$(cmd)"` assignment whose
# command exits non-zero terminates the whole script. That is exactly what
# happened: the suite passed locally, and on CI it died part-way through with
# the remaining assertions reported as failures.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
section() { printf '\n── %s ──\n' "$1"; }

# ── prerequisites ────────────────────────────────────────────────────────────
#
# A missing prerequisite is a FAILURE, never a skip. This suite used to
# whole-suite-skip on a missing `cargo`, print "0 passed, 0 failed (skipped)"
# and exit 0 — a green tick over nothing asserted, which is the shape
# docs/p1-progress.md already records this repository being bitten by three
# times, most recently when the labwc keybind suite reported passed=0 failed=0
# on its first CI run.
#
# `pgrep` is on the list for a sharper reason than "the script calls it". The
# "a dry run starts nothing" assertion counts `pgrep -c -x sleep` before and
# after, with `|| echo 0` on both sides: with no pgrep both sides are "0", the
# comparison holds, and the assertion PASSES having measured nothing. Same
# shape as the `diff -q` that exited 127 in the CI container and was read as a
# verdict.
for tool in cargo git python3 pgrep; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "FATAL: $tool is required; this suite cannot test anything without it" >&2
        exit 2
    }
done

section "the binary"
if ! cargo build --manifest-path "${ROOT}/rimed/Cargo.toml" --bin rime >/dev/null 2>&1; then
    bad "rime builds"
    printf '\nproject-layout: %d passed, %d failed\n' "$pass" "$fail"
    exit 1
fi
ok "rime builds"
# $CARGO_TARGET_DIR, when set, moves the whole target directory — so the
# hardcoded path was a "No such file or directory" for every assertion, and the
# suite reported 19 failures that had nothing to do with layouts.
Rime="${CARGO_TARGET_DIR:-${ROOT}/rimed/target}/debug/rime"

# Isolated state, so the developer's own saved layouts are never touched.
export XDG_STATE_HOME="${WORK}/state"
mkdir -p "$XDG_STATE_HOME"

# Two projects, so "belongs to this project" can actually be tested.
PROJ="${WORK}/mine"
OTHER="${WORK}/theirs"
for d in "$PROJ" "$OTHER"; do
    mkdir -p "$d"
    git -C "$d" init -q 2>/dev/null
    git -C "$d" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init 2>/dev/null
done

# ── the fake adapter ─────────────────────────────────────────────────────────
# Emits whatever ${WORK}/windows.json holds. The pids in it are REAL processes
# this script starts, because the capture rule reads /proc — a fabricated pid
# would be skipped as dead and every assertion would pass for the wrong reason.
FAKE="${WORK}/fake-adapter"
cat > "$FAKE" <<EOF
#!/bin/sh
case "\$1" in
    list)       cat "${WORK}/windows.json" ;;
    compositor) echo fake ;;
    *)          echo "fake adapter: \$1 unsupported" >&2; exit 1 ;;
esac
EOF
chmod +x "$FAKE"
export RIME_WINDOW_ADAPTER="$FAKE"

# Long-lived processes whose cwd is each project. `sh -c 'cd X; sleep'` is not
# enough: the capture reads /proc/<pid>/cwd, so the process must genuinely BE
# in that directory.
( cd "$PROJ"  && exec sleep 600 ) & MINE_PID=$!
( cd "$OTHER" && exec sleep 600 ) & THEIRS_PID=$!
trap 'kill "$MINE_PID" "$THEIRS_PID" 2>/dev/null; rm -rf "$WORK"' EXIT
sleep 0.3

windows() { printf '%s' "$1" > "${WORK}/windows.json"; }

# ── nothing to save ──────────────────────────────────────────────────────────
section "an empty capture does not destroy a good layout"
windows '[]'
out="$(cd "$PROJ" && "$Rime" project layout save 2>&1)"
printf '%s' "$out" | pipe_has "nothing saved" \
    && ok "an empty window list saves nothing and says so" \
    || { bad "an empty window list saves nothing and says so"; printf '      %s\n' "$out"; }

# ── capture ──────────────────────────────────────────────────────────────────
section "only windows working inside the project are captured"
windows "$(cat <<EOF
[
  { "handle": "0xdeadbeef", "pid": ${MINE_PID},   "app_id": "Alacritty", "title": "mine",   "workspace": "2", "floating": false },
  { "handle": "0xcafebabe", "pid": ${THEIRS_PID}, "app_id": "Alacritty", "title": "theirs", "workspace": "5", "floating": false },
  { "handle": null,         "pid": null,          "app_id": "labwcwin",  "title": "no pid", "workspace": "",  "floating": null }
]
EOF
)"
out="$(cd "$PROJ" && "$Rime" project layout save 2>&1)"
printf '%s' "$out" | pipe_has "saved 1 window" \
    && ok "exactly one window is captured" \
    || { bad "exactly one window is captured"; printf '      %s\n' "$out"; }
printf '%s' "$out" | pipe_has "workspace(s) 2" \
    && ok "the workspace is recorded" || bad "the workspace is recorded"

json="$(cd "$PROJ" && "$Rime" project layout show --json 2>/dev/null)"
printf '%s' "$json" | python3 -c "
import json,sys
l = json.load(sys.stdin)
e = l['entries']
assert len(e) == 1, e
assert e[0]['cwd'] == '${PROJ}', e
assert e[0]['workspace'] == '2', e
assert e[0]['terminal'] is True, e
assert l['saved'] > 0, l
" 2>"${WORK}/cap.err" \
    && ok "the entry records the project's own cwd" \
    || { bad "the entry records the project's own cwd"; sed 's/^/      /' "${WORK}/cap.err"; }

# The property that makes a layout restorable at all.
printf '%s' "$json" | pipe_has "0xdeadbeef" \
    && bad "no compositor window handle is stored" \
    || ok "no compositor window handle is stored"

section "the other project's layout is its own"
out="$(cd "$OTHER" && "$Rime" project layout save 2>&1)"
printf '%s' "$out" | pipe_has "saved 1 window" \
    && ok "the other project captures its own window" || bad "the other project captures its own window"
mine="$(cd "$PROJ" && "$Rime" project layout show --json 2>/dev/null | python3 -c 'import json,sys; print(json.load(sys.stdin)["entries"][0]["cwd"])')"
[ "$mine" = "$PROJ" ] \
    && ok "saving one project's layout did not overwrite the other's" \
    || bad "saving one project's layout did not overwrite the other's (got ${mine})"

# ── restore ──────────────────────────────────────────────────────────────────
section "restore rebuilds a terminal with its directory"
# The stored argv for the fixture is `sleep 600`, not a terminal — but it was
# classified as one from its app_id, which is exactly the case that matters:
# a terminal's stored argv does NOT carry its working directory, so replaying
# it verbatim opens a terminal in the wrong place.
out="$(cd "$PROJ" && TERMINAL=foot "$Rime" project layout restore --dry-run 2>&1)"
printf '%s\n' "$out" | sed 's/^/      /'

# Three cases, all real, and the third is the one only CI reaches — a runner
# has no terminal emulator installed at all. Skipping it would leave the
# documented fallback untested everywhere.
have_term=no
for t in foot alacritty ghostty kitty wezterm xterm; do
    command -v "$t" >/dev/null 2>&1 && { have_term=yes; break; }
done

if [ "$have_term" = yes ]; then
    # Whichever emulator is present, the working directory must be passed —
    # a terminal's stored argv does not carry it, so replaying it verbatim
    # opens a terminal in the wrong place.
    printf '%s' "$out" | pipe_has -F "${PROJ}" \
        && ok "a terminal is restored with the project directory" \
        || bad "a terminal is restored with the project directory"
    if command -v foot >/dev/null 2>&1; then
        printf '%s' "$out" | pipe_has -- "--working-directory ${PROJ}" \
            && ok "\$TERMINAL wins and gets its own flag" \
            || bad "\$TERMINAL wins and gets its own flag"
    fi
else
    # No emulator: the documented fallback is to replay the stored argv and
    # SAY so, rather than inventing a command that cannot run.
    printf '%s' "$out" | pipe_has "no terminal emulator found" \
        && ok "no emulator is reported rather than guessed at" \
        || { bad "no emulator is reported rather than guessed at"; }
    printf '%s' "$out" | pipe_has "would run (ws 2): sleep 600" \
        && ok "the stored argv is replayed when there is no emulator" \
        || bad "the stored argv is replayed when there is no emulator"
fi
printf '%s' "$out" | pipe_has "1 window(s) would be restored" \
    && ok "the dry run reports what it would do" || bad "the dry run reports what it would do"

section "a dry run starts nothing"
# Counted by WORKING DIRECTORY, not by program name. `pgrep -c -x sleep` counts
# every `sleep` on the machine, so anything else on it starting or finishing in
# the half second below flipped the comparison — this suite failed on a
# developer machine that was simply busy, which is a test that reports on the
# machine rather than on the code. `restore` spawns with `current_dir(&e.cwd)`,
# so a process it started is one whose cwd is the project, and nothing outside
# this suite has that.
count_in_project() {
    local n=0 d
    for d in /proc/[0-9]*; do
        [ "$(readlink -f "$d/cwd" 2>/dev/null)" = "$PROJ" ] && n=$((n + 1))
    done
    printf '%s' "$n"
}
before="$(count_in_project)"
(cd "$PROJ" && "$Rime" project layout restore --dry-run >/dev/null 2>&1)
sleep 0.5
after="$(count_in_project)"
# The fixture process itself lives in the project, so the count is never zero —
# which is the property that makes this a real measurement rather than "0 = 0".
[ "$before" -ge 1 ] \
    && ok "the fixture process is visible, so the count means something" \
    || bad "the fixture process is visible, so the count means something"
[ "$before" = "$after" ] \
    && ok "no process was started by a dry run" \
    || bad "a dry run started something (${before} -> ${after})"

section "an application is restored verbatim"
# A non-terminal app_id means the stored argv IS the answer — it carries its
# own arguments, and substituting a terminal command would lose them.
windows "$(cat <<EOF
[ { "handle": null, "pid": ${MINE_PID}, "app_id": "firefox", "title": "app", "workspace": "3", "floating": false } ]
EOF
)"
(cd "$PROJ" && "$Rime" project layout save >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" project layout restore --dry-run 2>&1)"
printf '%s' "$out" | pipe_has "would run (ws 3): sleep 600" \
    && ok "an application keeps its own argv" \
    || { bad "an application keeps its own argv"; printf '      %s\n' "$out"; }

section "switch by project, not by workspace number"
# §6's "allow switching by project, not only by numeric workspace". The fake
# adapter records what it was asked to do, so the assertion is on the ACTION and
# not on a real compositor moving.
SWITCHLOG="${WORK}/switched"
cat > "$FAKE" <<EOF
#!/bin/sh
case "\$1" in
    list)       cat "${WORK}/windows.json" ;;
    workspace)  echo "\$2" >> "${SWITCHLOG}" ;;
    compositor) echo fake ;;
    *)          echo "fake adapter: \$1 unsupported" >&2; exit 1 ;;
esac
EOF
chmod +x "$FAKE"

# Two windows on ws 7 and one on ws 3, so "most populated wins" is testable
# rather than coincidental.
windows "$(cat <<EOF
[
  { "handle": null, "pid": ${MINE_PID}, "app_id": "Alacritty", "title": "a", "workspace": "7", "floating": false },
  { "handle": null, "pid": ${MINE_PID}, "app_id": "Alacritty", "title": "b", "workspace": "7", "floating": false },
  { "handle": null, "pid": ${MINE_PID}, "app_id": "firefox",   "title": "c", "workspace": "3", "floating": false }
]
EOF
)"
(cd "$PROJ" && "$Rime" project layout save >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" project switch 2>&1)"
printf '%s' "$out" | pipe_has "workspace 7" \
    && ok "switch picks the workspace with the most windows" \
    || { bad "switch picks the workspace with the most windows"; printf '      %s\n' "$out"; }
[ "$(tail -1 "$SWITCHLOG" 2>/dev/null)" = "7" ] \
    && ok "the compositor was actually asked to switch" \
    || bad "the compositor was actually asked to switch"

# By name, from outside the project — the point of switching BY PROJECT.
out="$(cd "${WORK}" && "$Rime" project switch mine 2>&1)"
printf '%s' "$out" | pipe_has "workspace 7" \
    && ok "a project can be switched to by name from anywhere" \
    || { bad "a project can be switched to by name from anywhere"; printf '      %s\n' "$out"; }

out="$(cd "${WORK}" && "$Rime" project switch no-such-project 2>&1)"
printf '%s' "$out" | pipe_has "no known project" \
    && ok "an unknown project name is refused" || bad "an unknown project name is refused"

# No layout means no recorded workspace, and that is said plainly.
(cd "$OTHER" && "$Rime" project layout forget >/dev/null 2>&1)
out="$(cd "$OTHER" && "$Rime" project switch 2>&1)"
printf '%s' "$out" | pipe_has "no layout saved" \
    && ok "switching without a layout explains what is missing" \
    || { bad "switching without a layout explains what is missing"; printf '      %s\n' "$out"; }

section "forget"
# Output is captured and THEN matched, never piped straight into grep. Under
# `set -o pipefail` a command that exits non-zero fails the whole pipeline even
# when grep matched — and every assertion below is about a command that
# correctly exits non-zero, so piping made two of them fail for a reason that
# had nothing to do with the behaviour being tested.
(cd "$PROJ" && "$Rime" project layout forget >/dev/null 2>&1)
out="$(cd "$PROJ" && "$Rime" project layout show 2>&1)"
printf '%s' "$out" | pipe_has "no layout saved" \
    && ok "a forgotten layout is gone" || bad "a forgotten layout is gone"

# NOT --dry-run: with no layout there is nothing to start, and this asserts
# that the real command says so rather than doing something odd.
out="$(cd "$PROJ" && "$Rime" project layout restore 2>&1)"
printf '%s' "$out" | pipe_has "no layout saved" \
    && ok "restoring nothing says so instead of failing oddly" \
    || { bad "restoring nothing says so instead of failing oddly"; printf '      %s\n' "$out"; }

section "outside a repository"
mkdir -p "${WORK}/bare"
out="$(cd "${WORK}/bare" && "$Rime" project layout save 2>&1)"
printf '%s' "$out" | pipe_has "not inside a git repository" \
    && ok "a directory that is not a project is refused clearly" \
    || { bad "a directory that is not a project is refused clearly"; printf '      %s\n' "$out"; }

section "an adapter that cannot enumerate"
BROKEN="${WORK}/broken-adapter"
printf '#!/bin/sh\necho "no window query for labwc" >&2\nexit 1\n' > "$BROKEN"
chmod +x "$BROKEN"
out="$(cd "$PROJ" && RIME_WINDOW_ADAPTER="$BROKEN" "$Rime" project layout save 2>&1)"
printf '%s' "$out" | pipe_has "no window query" \
    && ok "a compositor with no window query is reported, not guessed around" \
    || { bad "a compositor with no window query is reported, not guessed around"; printf '      %s\n' "$out"; }

printf '\nproject-layout: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
