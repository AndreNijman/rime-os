#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  test-rime-verbs.sh — every verb the CLI is supposed to have is in the binary.
#
#  ── The failure this exists for ─────────────────────────────────────────────
#  Two agents worked on `rimed/rime/src/main.rs` in parallel. One built its
#  commit from a copy of `main.rs` taken before the other's landed, so applying
#  it silently removed `mod task;` and the `Cmd::Task` arm.
#
#  Nothing failed to compile. Removing `mod task;` also stops
#  `rimed/rime/src/task.rs` from being compiled at all, so there was no orphaned
#  reference for rustc to complain about, no dead-code warning, and no test
#  failure — `rime task` simply was not in the binary any more. It was caught by
#  a person re-reading a diff, which is not a mechanism.
#
#  The whole class is invisible to the compiler: a verb's absence looks exactly
#  like a verb that was never written. Only asking the built artifact what it
#  can do will catch it.
#
#  ── Why an exact list and not a count ───────────────────────────────────────
#  A count passes while one verb is swapped for another. This repository has a
#  `-ge 30` assertion in its history that stayed green while 20 of 68 items were
#  silently dropped, so the list is enumerated and every entry checked by name.
#
#  Adding a verb means adding it here. That is the point: the list is the
#  statement of what Rime OS offers, and changing it should be deliberate.
#
#  PASS = every verb below answers `--help` from the built binary.
#
#  Run from anywhere: ./tests/test-rime-verbs.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
set +e
cd "$(dirname "$0")" || exit 2
REPO=$(cd .. && pwd)

pass=0; fail=0
ok()  { printf 'PASS  %-46s\n' "$1"; pass=$((pass+1)); }
bad() { printf 'FAIL  %-46s %s\n' "$1" "$2"; fail=$((fail+1)); }

RIME_BIN=${RIME_BIN:-$REPO/rimed/target/debug/rime}
if [ ! -x "$RIME_BIN" ]; then
    echo "building the rime binary (not found at $RIME_BIN)…"
    ( cd "$REPO/rimed" && cargo build --locked --bin rime ) || {
        echo "FATAL: could not build the rime binary; nothing below can run" >&2
        exit 2
    }
fi
[ -x "$RIME_BIN" ] || { echo "FATAL: no rime binary at $RIME_BIN" >&2; exit 2; }

# ── every verb, by roadmap section ──────────────────────────────────────────
# Ordered by the section that asked for it, so a reader can trace a verb back to
# why it exists. `help` is clap's own and is deliberately not listed.
VERBS="
status tier profile battery fan game mode workload perf gaming
fingerprint pin rollback update live shell metrics doctor changelog
install remove resolve search repo pkg env devices firewall remote
agent project request secret account mcp skill provenance backup
blueprint apply sync plugin cloudflare
ai host build send open
task recover disposable boot
trust storage qualify firmware channel schema
lid permissions user vm browser
"

for v in $VERBS; do
    [ -n "$v" ] || continue
    # `--help` on a subcommand exits 0 and needs no privilege, no D-Bus and no
    # hardware, so this is a pure question about what the binary contains.
    if "$RIME_BIN" "$v" --help >/dev/null 2>&1; then
        ok "rime $v is in the binary"
    else
        bad "rime $v is in the binary" "not a recognised subcommand"
    fi
done

# ── the other direction: a verb the binary has and this list does not ───────
# The forward check above catches a verb that was DROPPED. It cannot catch one
# that was ADDED without being listed, and by 2026-09-11 thirteen had been:
# channel, cloudflare, devices, firewall, firmware, mcp, provenance, qualify,
# remote, schema, skill, storage and trust. For each of those, the guard whose
# entire purpose is to notice a silently vanished verb would not have noticed.
#
# The list stays hand-written — deriving it from the binary would make it agree
# with whatever the binary happens to contain, which is the failure this file
# was written about. This only asserts the two sets match, so drift is a red
# build on the PR that introduces it rather than a gap found years later.
top_verbs="$("$RIME_BIN" --help 2>&1 \
    | sed -n '/^Commands:/,/^Options:/p' \
    | grep -oE '^  [a-z][a-z-]*' | tr -d ' ')"
unlisted=""
for v in $top_verbs; do
    # clap's own, deliberately not listed.
    [ "$v" = "help" ] && continue
    grep -qw -- "$v" <<<"$VERBS" || unlisted="$unlisted $v"
done
if [ -z "$unlisted" ]; then
    ok "every verb in rime --help is in this file's list"
else
    bad "every verb in rime --help is in this file's list" "unlisted:$unlisted"
fi

# ── the guard on the guard ──────────────────────────────────────────────────
# If the binary answered --help for anything at all, this file would pass while
# proving nothing. A verb that certainly does not exist must be rejected.
if "$RIME_BIN" definitely-not-a-verb --help >/dev/null 2>&1; then
    bad "a verb that does not exist is rejected" "the binary accepted a nonsense verb, so every check above is vacuous"
else
    ok "a verb that does not exist is rejected"
fi

# And the top-level help must list them, not merely accept them — a verb hidden
# from help is a verb nobody can find.
top="$("$RIME_BIN" --help 2>&1)"
missing_from_help=""
for v in $VERBS; do
    [ -n "$v" ] || continue
    grep -qE "^[[:space:]]+$v([[:space:]]|$)" <<<"$top" || missing_from_help="$missing_from_help $v"
done
if [ -z "$missing_from_help" ]; then
    ok "every verb is listed in rime --help"
else
    bad "every verb is listed in rime --help" "absent:$missing_from_help"
fi

printf '\nrime verbs: %d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
