#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-skill.sh — executable assertions for P1-025/026/027/028: the
#  agent's skills, where its plugins came from, and which trust plane a
#  connector sits on.
#
#  Two modes, the split `test-rime-trust.sh` and `test-boot-v2.sh` already use:
#
#    (no argument)     Structural checks needing no toolchain — that the verbs
#                      are wired into the CLI, that no reader in these modules
#                      turns a refused read into a fact, and that the dead-code
#                      the branch could not land with was fixed by giving it
#                      callers rather than by silencing the compiler.
#
#    --with-binary     Drives the built `rime` against fixture HOMEs. It DIES
#                      if the binary is absent rather than skipping: a skipped
#                      check counts as a success, which is the failure this
#                      repository has now recorded three times.
#
#  ── What this is guarding ───────────────────────────────────────────────────
#  Four claims, each of which is easy to make falsely:
#
#    1. an inventory of skills — and "0 skills" from a directory nobody could
#       read is the single most damaging sentence this code can print, because
#       "nothing unexpected is installed" is exactly what somebody would rely
#       on it for. `profile.rs`'s own skill counter still has that defect
#       (`read_dir(...).flatten()` at profile.rs:1789); `rime skill list`
#       refuses to.
#
#    2. provenance — a recorded origin that nothing re-checks against the bytes
#       on disk is a label. The registry holds a marketplace, a version and an
#       install path, and no hash at all; two of this machine's recorded
#       versions are the literal string "unknown".
#
#    3. isolation — the word "sandbox" may only be spent where bubblewrap
#       actually runs. This repository has had to correct that word twice.
#
#    4. trust planes — a connector that is a local program and one that is a
#       cloud address are different exposures, and neither `rime mcp list` nor
#       the JSON named the difference before.
#
#  Every fixture HOME gets its own $HOME *and* its own $XDG_STATE_HOME, because
#  `rime provenance record` writes a baseline store and `store_path` prefers
#  XDG_STATE_HOME over the home it is handed. A suite that set only HOME would
#  write over the real store of whoever ran it.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WITH_BINARY=0
[[ "${1:-}" == "--with-binary" ]] && WITH_BINARY=1

PASS=0 FAIL=0 NOTRUN=0
ok()  { PASS=$((PASS + 1)); printf '  ok   %s\n' "$*"; }
bad() { FAIL=$((FAIL + 1)); printf '  FAIL %s\n' "$*"; }
# A third answer, and it is the point of this suite rather than a convenience.
# "Could not run" is not "passed" and is not "failed", and the one thing it
# must never do is stay silent: a check that quietly did not happen is how a
# suite reports 82/0 about something it never measured. Counted, printed in
# the summary, and deliberately NOT added to PASS.
skipped() { NOTRUN=$((NOTRUN + 1)); printf '  ----  %s\n' "$*"; }
sec() { printf '\n== %s ==\n' "$*"; }
has() { # has <needle> <haystack-file> <label>
    if grep -qF -- "$1" "$2"; then ok "$3"; else
        bad "$3 — no '$1' in:"; sed 's/^/       /' "$2" >&2
    fi
}
hasnt() {
    if grep -qF -- "$1" "$2"; then
        bad "$3 — found '$1' in:"; sed 's/^/       /' "$2" >&2
    else ok "$3"; fi
}

SKILLRS="$REPO/rimed/rime/src/skill.rs"
PROVRS="$REPO/rimed/rime/src/provenance.rs"
DIGESTRS="$REPO/rimed/rime/src/digest.rs"
CONNRS="$REPO/rimed/rime/src/connector.rs"
MAINRS="$REPO/rimed/rime/src/main.rs"
AGENTRS="$REPO/rimed/rime/src/agent.rs"
for f in "$SKILLRS" "$PROVRS" "$DIGESTRS" "$CONNRS" "$MAINRS" "$AGENTRS"; do
    [[ -f "$f" ]] || { echo "FATAL: missing $f" >&2; exit 1; }
done

TMP="$(mktemp -d)"
trap 'chmod -R u+rwX "$TMP" 2>/dev/null; rm -rf "$TMP"' EXIT

# ═════════════════════════════════════════════════════════════════════════════
sec "the verbs are wired into the CLI, not merely written"
# A module nobody dispatches to satisfies a grep and not a user. Each of these
# is the dispatch arm, not the enum variant, because an arm is what a refactor
# actually drops.
for pair in \
    'skill::list|rime skill list' \
    'skill::audit|rime skill audit' \
    'provenance::show|rime provenance show' \
    'provenance::record|rime provenance record' \
    'connector::planes_main|rime mcp planes' \
    'connector::memory_main|rime mcp memory'
do
    # `|` and not `:`, because every symbol here contains `::` and `%%:*` would
    # have cut each one down to its module name — six assertions that passed on
    # a grep for the wrong string.
    sym="${pair%%|*}"; verb="${pair#*|}"
    if grep -q "${sym}(" "$MAINRS" "$REPO/rimed/rime/src/mcp.rs"; then
        ok "$verb reaches $sym"
    else
        bad "$verb is declared but nothing calls $sym"
    fi
done

# ═════════════════════════════════════════════════════════════════════════════
sec "the dead code was given callers, not silenced"
# This branch was merged, compiled and then ABORTED before commit because its
# own tip did not survive `cargo clippy -- -D warnings`: digest.rs's helpers
# were unreachable until provenance.rs existed to call them. The fix was to
# finish the caller. `#[allow(dead_code)]` would have made the same red go
# green while leaving an unreachable safety helper in a security module — and
# the next reader would have no way to tell which of the two had happened.
for f in "$SKILLRS" "$PROVRS" "$DIGESTRS" "$CONNRS"; do
    if grep -q 'allow(dead_code)' "$f"; then
        bad "$(basename "$f") silences dead_code instead of calling the code"
    else
        ok "$(basename "$f") has no allow(dead_code)"
    fi
done

# ═════════════════════════════════════════════════════════════════════════════
sec "a refused read is never an absence"
# The defect shape this repository has now found in about fifteen places, and
# these are the two modules whose whole subject is telling a measurement apart
# from a guess. `Path::exists()` returns false for a directory that is there
# and refused; so does `read_dir(..).ok()`, and so does `.flatten()` on the
# iterator, which drops the per-entry Err.
# Comment lines are stripped first. These modules document the defect at
# length — `profile.rs`'s `read_dir(..).flatten()` is quoted in skill.rs's own
# header — and a checker that read the description of a bug as the bug would
# go red for the prose explaining why it is red. It would then be silenced by
# deleting the explanation, which is the worst available outcome.
code() { grep -vE '^[[:space:]]*(//|\*)' "$1"; }
for f in "$SKILLRS" "$PROVRS"; do
    n="$(code "$f" | grep -cE '\.exists\(\)')"
    if [[ "$n" -eq 0 ]]; then
        ok "$(basename "$f") has no .exists() reader"
    else
        bad "$(basename "$f") has $n .exists() call(s) in code; each reads EACCES as absence"
        code "$f" | grep -nE '\.exists\(\)' | sed 's/^/       /' >&2
    fi
done
for f in "$SKILLRS" "$PROVRS" "$DIGESTRS"; do
    if code "$f" | pipe_has -E 'read_dir\([^)]*\)[[:space:]]*\.ok\(\)|\.flatten\(\)'; then
        bad "$(basename "$f") drops a directory-read refusal (read_dir().ok() or .flatten())"
    else
        ok "$(basename "$f") keeps every directory-read refusal"
    fi
done
# The tri-state itself: a comparison that cannot answer must not be allowed to
# look like one that answered "no".
if grep -q 'pub fn same_as(&self, other: &Digest) -> Option<bool>' "$DIGESTRS"; then
    ok "Digest::same_as returns Option, so an unmeasurable pair is not a mismatch"
else
    bad "Digest::same_as no longer returns Option — an unmeasured tree can now read as changed"
fi

# ═════════════════════════════════════════════════════════════════════════════
sec "the word sandbox is spent only where bubblewrap runs, and never without a when"
# P1-026's second criterion cannot be delivered whole, and the shortfall MOVED
# when the session launcher started wrapping third-party definitions at launch.
# There are now two ways an MCP server comes to be confined and they do not hold
# in the same places, so the report must never print "sandboxed" without the
# clause saying where it stops. The failure modes are symmetrical: a report that
# says "sandboxed" of a hand-run session, and one that says "NOT sandboxed" of
# a server every Rime-started session confines.
has 'bubblewrap confines it' "$PROVRS" "a wrapped MCP server is the one thing called sandboxed"
has 'sandboxed in a session `rime agent` starts' "$PROVRS" \
    "a server the LAUNCH FILE wraps is named as that, not as unconfined"
has 'Start the agent yourself' "$PROVRS" \
    "and the at-launch sentence carries the clause saying where it stops"
# Hooks and scripts get no sandbox FROM Rime — which is not the same as running
# unconfined, because they are inside whatever the session itself is confined
# to. The old wording here said "nothing in Rime confines them" and understated
# it in one direction while a reader could take it the other way.
has 'nothing here adds a sandbox of its own' "$PROVRS" \
    "hooks are stated as getting no sandbox from Rime"
has 'under `--sandbox unrestricted` is nothing' "$PROVRS" \
    "and the case where the session has none either is named"
has 'hasExecutableContent' "$PROVRS" \
    "the JSON pairs everythingExecutableIsSandboxed with whether there is anything to confine"
# A shortfall reported with no remedy is the same defect as a control reported
# as missing, which round 2 of this unit had to go back and delete five of.
has 'removableByPluginPolicy' "$PROVRS" \
    "and says, beside it, that dimension 8 can start the session without the plugin"
has '--plugins none' "$PROVRS" \
    "the hooks line names the flag that removes them, not just the sandbox that does not reach them"
has 'Nothing here' "$SKILLRS" "skill.rs says nothing confines a skill's scripts"
# `trustOnFirstUse` in the data, not only in a paragraph: a match here means the
# tree has not changed SINCE Rime first saw it, which is not verification.
has 'trustOnFirstUse' "$PROVRS" "the JSON declares the baseline is trust-on-first-use"
has '"signed": false' "$PROVRS" "the JSON declares that nothing here is signed"

sec "dimension 8: the plugin content no MCP confinement reaches"
# P1-026 criterion 2's second half. Dimension 7 confines what a plugin
# DECLARES in its .mcp.json; a plugin's hooks are spawned by the agent itself
# and no MCP configuration of any kind is involved. So the dimension removes
# where the other one confines, and these check that the removal is wired all
# the way from the flag to the document — not that a constant exists.
has '--plugins' "$AGENTRS" "rime agent run takes --plugins"
has 'PLUGIN_POLICY_VERSION' "$AGENTRS" \
    "and refuses to send it to a daemon that would drop it, at its OWN revision"
# The fail-open this dimension shipped with for one revision: `plugins` was in
# dimensions() before it was in the version table, so every unrecognised name
# fell through to POLICY_DIMENSIONS_VERSION — revision 2 — and a protocol-2
# daemon was told it understood a key it drops.
has '"sandbox" | "connectors" | "plugins"' "$AGENTRS" \
    "no late dimension falls through to the revision the first six shipped in"
has '"--plugins", self.plugins' "$AGENTRS" \
    "a remote run carries the flag, or the far end loads every plugin it has"

LIVERS="$REPO/rimed/rime/tests/plugin_removal_live.rs"
if [[ -f "$LIVERS" ]]; then
    ok "the live removal test exists"
    # The three properties that separate this from a grep for a JSON key.
    has 'pluginconf::curate' "$LIVERS" "it curates through the real function"
    has 'hook::settings_json' "$LIVERS" \
        "and starts the agent with the document rime-agentd actually writes"
    has 'hook_settings_args' "$LIVERS" \
        "passed with the adapter's own argv, not a hand-rolled --settings"
    has 'run_session(&root, &home, &sentinel, None)' "$LIVERS" \
        "the control run comes first: the hook has to RUN before a removal means anything"
    has 'known_marketplaces.json' "$LIVERS" \
        "the fixture registers its marketplace — an unregistered plugin is silently inert"
    has '#[ignore' "$LIVERS" "it is opt-in, because CI has no claude to drive"
else
    bad "rimed/rime/tests/plugin_removal_live.rs is missing: criterion 2's second half has no live proof"
fi

# ═════════════════════════════════════════════════════════════════════════════
if [[ "$WITH_BINARY" -eq 0 ]]; then
    printf '\n%s\n' "── binary checks skipped (pass --with-binary) ──"
    printf '\n%d passed, %d failed' "$PASS" "$FAIL"
    if [[ "$NOTRUN" -gt 0 ]]; then printf ', %d could not run' "$NOTRUN"; fi
    printf '\n'
    [[ "$FAIL" -eq 0 ]] || exit 1
    exit 0
fi

Rime="${Rime:-$REPO/rimed/target/debug/rime}"
[[ -x "$Rime" ]] || Rime="${CARGO_TARGET_DIR:-}/debug/rime"
if [[ ! -x "$Rime" ]]; then
    echo "FATAL: no rime binary. Build it, or set Rime=/path/to/rime." >&2
    echo "       A skipped assertion reports as a pass, which is the bug this refuses." >&2
    exit 1
fi

# Run the CLI against a fixture HOME. Both HOME and XDG_STATE_HOME, always:
# `provenance record` writes a baseline store and `store_path` prefers
# XDG_STATE_HOME, so setting only HOME would write over the real one.
# XDG_CONFIG_HOME too, so `rime mcp memory` cannot read the caller's
# declaration. cwd is the fixture as well, so no `.mcp.json` in the checkout
# leaks into `servers::discover`.
run() { # run <home> <args…>  → prints exit code, output in $TMP/out|err
    local h="$1"; shift
    ( cd "$h" && HOME="$h" XDG_STATE_HOME="$h/.state" XDG_CONFIG_HOME="$h/.config" \
        "$Rime" "$@" > "$TMP/out" 2> "$TMP/err" )
    echo $?
}
# cwd IS the fixture home on purpose: it is the case that found the
# double-counting defect, and a suite that quietly ran from somewhere else
# would have kept missing it.

# A fixture home with one skill directory.
skillhome() { # skillhome <name> [<skill> …]
    local h="$TMP/$1"; shift
    mkdir -p "$h/.claude/skills"
    for s in "$@"; do
        mkdir -p "$h/.claude/skills/$s"
        printf -- '---\nname: %s\n---\n\nA skill.\n' "$s" \
            > "$h/.claude/skills/$s/SKILL.md"
    done
    printf '%s' "$h"
}

sec "rime skill list counts what it read, and says so"
H="$(skillhome skills alpha beta)"
rc="$(run "$H" skill list)"
[[ "$rc" == 0 ]] && ok "rime skill list exits 0 on a readable profile" \
    || { bad "rime skill list exited $rc"; sed 's/^/       /' "$TMP/err" >&2; }
has '2 skills' "$TMP/out" "both skills are counted"
has 'alpha' "$TMP/out" "the first skill is named"
has 'documentation only' "$TMP/out" "a skill with no program is documentation only"

sec "one directory reached by two routes is one skill, not two"
# Found by this suite's own first fixture. Run from your own home, `$HOME` is
# also the cwd, so `~/.claude/skills` is reached once as the user profile and
# once as the project checkout: every skill was listed twice under two origins
# and the count doubled. 34 skills became 68 by changing directory.
H="$(skillhome dup alpha beta)"
run "$H" skill list >/dev/null
has '2 skills' "$TMP/out" "running from \$HOME itself still counts each skill once"
n="$(grep -c '^alpha ' "$TMP/out")"
[[ "$n" -eq 1 ]] && ok "alpha appears on exactly one row" \
    || bad "alpha appears on $n rows — one directory was counted as several skills"
# And the dedup must not be able to swallow a genuinely separate project root.
P="$TMP/proj"; mkdir -p "$P/.claude/skills/gamma"
printf -- '---\nname: gamma\n---\n' > "$P/.claude/skills/gamma/SKILL.md"
( cd "$P" && HOME="$H" XDG_STATE_HOME="$H/.state" XDG_CONFIG_HOME="$H/.config" \
    "$Rime" skill list > "$TMP/out" 2>"$TMP/err" )
has 'gamma' "$TMP/out" "a real project root is still collected"
has '3 skills' "$TMP/out" "and it adds to the profile's skills rather than replacing them"

sec "a skill that ships a program is told apart from one that does not"
H="$(skillhome exec plain runner)"
printf '#!/bin/sh\necho hi\n' > "$H/.claude/skills/runner/go.sh"
chmod +x "$H/.claude/skills/runner/go.sh"
run "$H" skill list >/dev/null
has '1 ship a program' "$TMP/out" "the execute bit is what makes a skill executable"
run "$H" skill audit >/dev/null
has 'go.sh' "$TMP/out" "the audit names the program"
has 'your own profile' "$TMP/out" "the audit says where a program-shipping skill came from"

sec "a skills directory that cannot be read is NEVER reported as zero skills"
# The assertion this whole unit exists for. `profile.rs`'s doctor still answers
# "0 skills" here; that is the sentence somebody would rely on to conclude
# nothing unexpected is installed, and it would be a refusal wearing an
# absence's clothes.
H="$(skillhome denied one two)"
chmod 0000 "$H/.claude/skills"
if [[ -r "$H/.claude/skills" ]]; then
    printf '  skip  this user reads a 0000 directory (root or CAP_DAC_OVERRIDE)\n'
else
    rc="$(run "$H" skill list)"
    [[ "$rc" != 0 ]] && ok "an unreadable skills directory exits non-zero ($rc)" \
        || bad "an unreadable skills directory exited 0 — an incomplete inventory read as a success"
    has 'no skills were READ' "$TMP/out" "it says nothing was READ rather than that there are none"
    hasnt 'no skills on this machine' "$TMP/out" \
        "it never says there are no skills on a machine it could not look at"
    rc="$(run "$H" skill list --json)"
    if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if d.get("complete") is False and d.get("count")==0 else 1)' "$TMP/out"; then
        ok "the JSON pairs count 0 with complete:false, so a consumer cannot read it as empty"
    else
        bad "the JSON reported count 0 without complete:false"
        sed 's/^/       /' "$TMP/out" >&2
    fi
fi
chmod 0755 "$H/.claude/skills"

# ─────────────────────────────────────────────────────────────────────────────
# A fixture home with one marketplace and one installed plugin.
plughome() { # plughome <name>
    local h="$TMP/$1"
    mkdir -p "$h/.claude/plugins/marketplaces/mk" "$h/plug" "$h/.state" "$h/.config"
    printf '{}\n' > "$h/plug/plugin.json"
    printf '#!/bin/sh\necho one\n' > "$h/plug/run.sh"
    chmod +x "$h/plug/run.sh"
    cat > "$h/.claude/plugins/known_marketplaces.json" <<JSON
{"mk": {"source": {"source": "github", "repo": "o/r"},
        "installLocation": "$h/.claude/plugins/marketplaces/mk",
        "lastUpdated": "2026-01-01T00:00:00.000Z"}}
JSON
    cat > "$h/.claude/plugins/installed_plugins.json" <<JSON
{"version": 2, "plugins": {"p@mk": [
  {"scope": "user", "installPath": "$h/plug", "version": "1.0.0",
   "installedAt": "2026-01-01T00:00:00.000Z"}]}}
JSON
    printf '{"enabledPlugins": {"p@mk": true}}\n' > "$h/.claude/settings.json"
    printf '%s' "$h"
}

sec "provenance: a first look is not a pass"
H="$(plughome prov)"
rc="$(run "$H" provenance show)"
has 'no digest was ever recorded' "$TMP/out" "an unrecorded plugin says nothing was recorded"
# NOT `hasnt 'unchanged'`: the closing paragraph uses the word to explain what a
# match does not prove, and an assertion that forbade it would be silenced by
# deleting the paragraph. The VERDICT line is what must be absent.
hasnt 'hash to what was recorded' "$TMP/out" "a first look never claims the tree is unchanged"
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
a=d["plugins"][0]["attest"]
sys.exit(0 if a["state"]=="first-sight" and a["unchanged"] is None else 1)' \
    <(run "$H" provenance show --json >/dev/null; cat "$TMP/out") 2>/dev/null; then
    ok "the JSON verdict is first-sight with unchanged:null, not a pass"
else
    run "$H" provenance show --json >/dev/null
    if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
a=d["plugins"][0]["attest"]
sys.exit(0 if a["state"]=="first-sight" and a["unchanged"] is None else 1)' "$TMP/out"; then
        ok "the JSON verdict is first-sight with unchanged:null, not a pass"
    else
        bad "a first look did not report first-sight/null"
        sed 's/^/       /' "$TMP/out" >&2
    fi
fi

sec "provenance: record, then an untouched tree is unchanged and an edited one is a finding"
rc="$(run "$H" provenance record)"
[[ "$rc" == 0 ]] && ok "rime provenance record exits 0" \
    || { bad "rime provenance record exited $rc"; sed 's/^/       /' "$TMP/err" >&2; }
[[ -f "$H/.state/rime/plugin-provenance.json" ]] \
    && ok "the baseline store landed under XDG_STATE_HOME, not the caller's real one" \
    || bad "no store at $H/.state/rime/plugin-provenance.json — record wrote somewhere else"
run "$H" provenance show >/dev/null
has 'hash to what was recorded' "$TMP/out" "an untouched tree reads as unchanged"

# The finding the whole module exists to produce.
printf '#!/bin/sh\necho TWO\n' > "$H/plug/run.sh"
rc="$(run "$H" provenance show)"
[[ "$rc" != 0 ]] && ok "an edited plugin exits non-zero ($rc)" \
    || bad "an edited plugin exited 0 — the finding would not fail a script"
has 'CHANGED' "$TMP/out" "the edit is reported as CHANGED"
run "$H" provenance show --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
a=d["plugins"][0]["attest"]
sys.exit(0 if a["state"]=="changed" and a["unchanged"] is False
         and a["recorded"] != a["found"] else 1)' "$TMP/out"; then
    ok "the JSON carries both hashes, so a reader can see which pair disagreed"
else
    bad "the changed verdict did not carry recorded and found"
fi

# A content-identical file that gained +x must still be a change: provenance is
# the hash of what RUNS, and a chmod is the cheapest way to make a tree
# executable without editing a byte of it.
H="$(plughome chmodonly)"
run "$H" provenance record >/dev/null
chmod -x "$H/plug/run.sh"
rc="$(run "$H" provenance show)"
has 'CHANGED' "$TMP/out" "a changed execute bit changes the digest, with no byte edited"

sec "provenance: runtime state inside the tree is not a change"
# Claude Code writes .in_use/<pid> INSIDE the installed plugin. A digest that
# covered it would report a change every time the plugin was used, and a check
# that cries wolf on every run trains its reader to ignore the one time it
# means something.
H="$(plughome volatile)"
run "$H" provenance record >/dev/null
mkdir -p "$H/plug/.in_use" && : > "$H/plug/.in_use/4242"
rc="$(run "$H" provenance show)"
hasnt 'CHANGED' "$TMP/out" "a live-process marker is not a change"
has 'excluded' "$TMP/out" "and the exclusion is reported, so the digest is never quietly partial"
# But an exclusion must not hide a program. A script under a same-named
# directory deeper in the tree is still measured.
mkdir -p "$H/plug/tools/.in_use"
printf '#!/bin/sh\necho hidden\n' > "$H/plug/tools/.in_use/go.sh"
chmod +x "$H/plug/tools/.in_use/go.sh"
rc="$(run "$H" provenance show)"
has 'CHANGED' "$TMP/out" \
    "a file under a deeper .in_use IS measured — an exclusion that hid a program would be a hole"

sec "provenance: a store that cannot be read is not a machine with nothing recorded"
# The most damaging collapse available here. An unreadable store makes every
# plugin look unrecorded, which is exactly what a deleted store looks like —
# and a report that said "not recorded" would be reporting a clean bill of
# health for a machine whose records had just been destroyed.
H="$(plughome badstore)"
mkdir -p "$H/.state/rime"
printf '{ this is not json' > "$H/.state/rime/plugin-provenance.json"
rc="$(run "$H" provenance show)"
[[ "$rc" != 0 ]] && ok "an unusable store exits non-zero ($rc)" \
    || bad "an unusable store exited 0"
has 'clean bill of health' "$TMP/out" "the report warns against reading it as healthy"
run "$H" provenance show --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
a=d["plugins"][0]["attest"]
sys.exit(0 if a["state"]=="could-not-run" and a["unchanged"] is None
         and d["complete"] is False else 1)' "$TMP/out"; then
    ok "the verdict is could-not-run (about this machine), not first-sight (about the records)"
else
    bad "an unreadable store made a plugin read as first-sight"
    sed 's/^/       /' "$TMP/out" >&2
fi
# And record must refuse to clobber it rather than destroying the only evidence
# that something had changed.
rc="$(run "$H" provenance record)"
[[ "$rc" != 0 ]] && ok "record refuses to overwrite a store it could not read ($rc)" \
    || bad "record overwrote an unreadable store, destroying records it cannot reproduce"
has 'destroy records' "$TMP/err" "and it says why it refused"

sec "provenance: a plugin nobody could hash is neither a pass nor a failure"
H="$(plughome missing)"
rm -rf "$H/plug"
rc="$(run "$H" provenance show)"
[[ "$rc" != 0 ]] && ok "an unmeasurable plugin makes the report exit non-zero ($rc)" \
    || bad "a plugin that could not be checked exited 0"
has 'does NOT cover' "$TMP/out" "the report says which plugin it does not cover"
run "$H" provenance show --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if d["plugins"][0]["attest"]["state"]=="could-not-run"
         and d["complete"] is False else 1)' "$TMP/out"; then
    ok "could-not-run, and the report declares itself incomplete"
else
    bad "an unhashable plugin was not could-not-run/incomplete"
fi

sec "provenance: a documentation-only plugin is not reported as sandboxed"
H="$(plughome docsonly)"
rm -f "$H/plug/run.sh"
run "$H" provenance show >/dev/null
has 'no process of its own to confine' "$TMP/out" \
    "a plugin with nothing to confine says so, rather than reporting as sandboxed"
hasnt 'bubblewrap confines it' "$TMP/out" "and the word is not spent on it"
run "$H" provenance show --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
c=d["plugins"][0]["confinement"]
sys.exit(0 if c["everythingExecutableIsSandboxed"] is True
         and c["hasExecutableContent"] is False else 1)' "$TMP/out"; then
    ok "the vacuous true ships beside hasExecutableContent:false, so it cannot be misread"
else
    bad "everythingExecutableIsSandboxed was not paired with hasExecutableContent"
fi

sec "provenance: a git marketplace gets a commit, a non-git one gets a claim"
H="$(plughome rev)"
MK="$H/.claude/plugins/marketplaces/mk"
printf '8f5c9d3f86ccaeedbaefd66b039cfb3743775e0e\n' > "$MK/.gcs-sha"
run "$H" provenance show >/dev/null
has 'claimed by .gcs-sha' "$TMP/out" "a revision file beside a non-git tree is a claim"
has 'nothing here can check the files against it' "$TMP/out" "and the report says it cannot be re-checked"
rm -f "$MK/.gcs-sha"
if git -C "$MK" init -q 2>/dev/null; then
    git -C "$MK" config user.email t@t; git -C "$MK" config user.name t
    printf 'a\n' > "$MK/a"; git -C "$MK" add a; git -C "$MK" commit -qm one
    run "$H" provenance show >/dev/null
    has 'still matches it' "$TMP/out" "a clean checkout reports its working tree still matches"
    printf 'b\n' > "$MK/a"
    rc="$(run "$H" provenance show)"
    has 'MODIFIED' "$TMP/out" \
        "a modified checkout is reported — the one re-checkable origin claim in the feature"
else
    bad "git could not init a repository, so the only re-checkable origin claim went untested"
fi

sec "mcp planes: a local program and a cloud address are different exposures"
H="$TMP/planes"; mkdir -p "$H/.state" "$H/.config"
cat > "$H/.claude.json" <<'JSON'
{"mcpServers": {
  "localone": {"command": "/usr/bin/true", "args": []},
  "cloudone": {"type": "http", "url": "https://example.invalid/mcp"}}}
JSON
rc="$(run "$H" mcp planes)"
[[ "$rc" == 0 ]] && ok "rime mcp planes exits 0" || bad "rime mcp planes exited $rc"
has 'localone' "$TMP/out" "the stdio server is listed"
has 'cloudone' "$TMP/out" "the endpoint server is listed"
run "$H" mcp planes --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
t=json.dumps(d)
sys.exit(0 if "local" in t and "cloud" in t else 1)' "$TMP/out"; then
    ok "both planes are named in the JSON"
else
    bad "the planes JSON does not name both planes"
    sed 's/^/       /' "$TMP/out" >&2
fi
run "$H" mcp list >/dev/null
has 'plane' "$TMP/out" "rime mcp list carries the plane too, not only the new verb"

# P1-028's second criterion: the per-connector switch, and the table that is a
# measurement rather than a description. `connector.rs` once said in five places
# that this did not exist, on a tip that had built it.
run "$H" mcp planes >/dev/null
has 'WHAT `--connectors` DOES' "$TMP/out" "the connector-policy table is printed"
has 'local_only' "$TMP/out" "and names the value that removes the cloud plane without the network"
# The qualifier. Every number in that table is about a session `rime agent`
# started, and a readout that omitted this would be describing the machine.
has 'started through `rime agent`' "$TMP/out" "the readout says where the selection applies"
has 'Start the agent yourself' "$TMP/out" "and where it stops"
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
ps={p["policy"] for p in d.get("connectorPolicies", [])}
sys.exit(0 if d.get("perConnectorSwitch") is True
         and d.get("appliesOnlyToSessionsStartedBy") == "rime agent"
         and {"as_configured","local_only","curated","none"} <= ps else 1)' \
    <(run "$H" mcp planes --json >/dev/null; cat "$TMP/out"); then
    ok "the JSON carries the per-connector switch, its scope, and a row per policy"
else
    bad "the planes JSON does not report the per-connector switch and its scope"
    sed 's/^/       /' "$TMP/out" >&2
fi
# `curated` with an empty connector_allow is REFUSED, not run: "nobody filled
# this in" and "reach nothing" are different statements. A row of zeroes would
# describe a session that never starts as one that starts empty.
run "$H" mcp planes >/dev/null
has 'refused, not run' "$TMP/out" "an empty connector_allow is a refusal, not a row of zeroes"

sec "mcp memory: a view, never a store, and never a guess presented as a fact"
rc="$(run "$H" mcp memory)"
[[ "$rc" == 0 ]] && ok "rime mcp memory exits 0" || bad "rime mcp memory exited $rc"
run "$H" mcp memory --json >/dev/null
if python3 -c '
import json,sys
d=json.load(open(sys.argv[1]))
sys.exit(0 if d.get("rimeOwnedStore") is False
         and d.get("migrationRequired") is False else 1)' "$TMP/out"; then
    ok "the JSON declares Rime owns no store and requires no migration"
else
    bad "the memory JSON does not declare rimeOwnedStore:false / migrationRequired:false"
    sed 's/^/       /' "$TMP/out" >&2
fi
# No --probe anywhere in this suite: probing opens a connection, and a test
# that dialled somebody's memory server would be measuring their NAS.
hasnt '--probe' "$TMP/err" "nothing here probed a live server"

sec "dimension 8, live: a removed plugin's hook is observed not to run"
# The only assertion in this file that watches a process. Everything above is
# a claim ABOUT code; this starts a real agent against a fixture home holding a
# real enabled plugin, and looks for the mark its SessionStart hook leaves.
#
# `claude` is not on the CI runners, so this is the one check here that has a
# third answer. It is never silently skipped: a could-not-run line is printed
# and counted, so a run that measured nothing cannot read as a run that held.
if ! command -v claude >/dev/null 2>&1; then
    skipped "no \`claude\` on PATH — the live plugin-removal test did NOT run here"
elif ! command -v cargo >/dev/null 2>&1; then
    skipped "no \`cargo\` on PATH — the live plugin-removal test did NOT run here"
else
    # Nothing leaves the machine and nothing touches the real home: the test
    # sets its own HOME, its own XDG_RUNTIME_DIR, and an ANTHROPIC_BASE_URL on
    # a dead local port. Slow on purpose — the two removal runs have to wait
    # out a window the control runs proved is long enough.
    if (cd "$REPO/rimed" && cargo test --locked -p rime --test plugin_removal_live \
            -- --ignored --exact a_removed_plugins_hook_is_observed_not_to_run) \
            >"$TMP/live" 2>&1; then
        ok "a plugin removed by dimension 8 did not run its SessionStart hook, and the kept runs did"
    else
        bad "the live plugin-removal test failed"
        tail -40 "$TMP/live" | sed 's/^/       /' >&2
    fi
fi

printf '\n%d passed, %d failed' "$PASS" "$FAIL"
if [[ "$NOTRUN" -gt 0 ]]; then
    printf ', %d could not run' "$NOTRUN"
fi
printf '\n'
[[ "$FAIL" -eq 0 ]] || exit 1
