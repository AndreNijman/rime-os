#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  check-doc-verbs.sh — every `rime …` command a doc shows, asked of the CLI
#  that ships.
#
#  ── Why ─────────────────────────────────────────────────────────────────────
#  `docs/agent-runtime.md` told the reader to run `rime request install clang`
#  and `rime project restore`. Neither parses: the verbs are
#  `rime request ask install` and `rime project layout restore`. Both had been
#  wrong long enough to be quoted into other work. A doc command that does not
#  parse is a user following instructions into an error, and prose review does
#  not catch it because the sentence around it reads correctly.
#
#  So this asks clap instead of trusting the prose. `--help` on a real
#  subcommand exits 0; on an unknown one it exits non-zero. Nothing is run.
#
#  ── The allow file, and why it is necessary rather than a cop-out ───────────
#  Good documentation names commands that do NOT exist, on purpose:
#
#    docs/boot-v2.md   "There is deliberately no `rime boot ack` verb"
#    docs/recovery.md  "Adding an `rime recover previous` verb would have been…"
#    docs/rollback.md  "An earlier version claimed `rime reset --keep-home`
#                       shipped in M3. It never did"
#
#  All three are the doc being careful, and a checker that flagged them would
#  train people to delete the sentence that explains the absence. This does not
#  parse English negation; it takes a list of deliberate mentions instead, one
#  per line, with a written reason. `#` starts a comment.
#
#  Historical records under docs/m*-notes.md, docs/m0-results.md and
#  docs/p*-progress.md are NOT checked: they record what was true on a date,
#  and rewriting them to match today's CLI would be falsifying a log rather
#  than fixing a doc. `canonical_docs` below is that set, written down instead
#  of implied, because the reverse pass is meaningless over a subset — hand it
#  three files and every verb in the binary looks undocumented.
#
#  ── The reverse direction, and why the forward one could never find this ────
#  Everything above walks documented verb → real command. `rime remote pair`,
#  `devices`, `revoke`, `status` and `enable` shipped, and were named in no
#  document at all. This checker ran over every doc in the tree and said
#  nothing, because a verb that appears in no doc produces no line to check:
#  the whole class is invisible from this direction, by construction.
#
#  So the second pass asks the opposite question. Every `rime <verb>` and
#  `rime <verb> <sub>` the built binary offers must be named by some doc, or
#  be listed in `tests/doc-verbs-undocumented` — which is a DEBT list, not an
#  allow list, and the difference is the ratchet: an entry that has since been
#  documented is itself a failure, so the file can only shrink. Adding to it
#  is a line in the diff of the PR that adds the verb, which is where somebody
#  can argue about it.
#
#  The binary, not the installed `rime`, for this direction: the question is
#  whether THIS tree's command surface is documented, and asking a copy of
#  Rime from before the branch would pass a verb the branch just added.
#
#  Usage:  tests/check-doc-verbs.sh                       both, canonical docs
#          tests/check-doc-verbs.sh docs/agent-runtime.md forward, those files
#          tests/check-doc-verbs.sh --reverse             reverse, canonical docs
#          Rime=/path/to/rime tests/check-doc-verbs.sh docs/*.md
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

# THIS TREE's binary by default, falling back to whatever `rime` is on PATH.
#
# It used to be the installed one, on the argument that a shipped doc should
# work on the machine the reader is typing on. That argument is good and it is
# about a released image; it is the wrong question for a gate on a branch, in
# two ways that both showed up the moment this file was pointed at every doc.
# A branch that adds a verb documents it and the older installed binary calls
# the doc wrong — 40 false BADs on `roadmap/v2.2` today, none of them a defect.
# And on a CI runner there is no installed `rime` at all, so the check SKIPs
# and proves nothing, which is the same defect wearing a green tick.
#
# The doc and the CLI move together in this repository, so the question the
# gate should ask is whether THIS tree's docs match THIS tree's binary. To ask
# the other question — does a released doc work on a machine — name the binary:
#
#     Rime=rime tests/check-doc-verbs.sh docs/*.md
#
# A false BAD from a verb this branch introduces is the tool working; a false
# PASS from silently accepting one would not be.
RIME_BUILT=${RIME_BUILT:-rimed/target/debug/rime}
ALLOW=${ALLOW:-tests/doc-verbs-allow}
DEBT=${DEBT:-tests/doc-verbs-undocumented}

ensure_built() {
    # ALWAYS build, never just check for the file. `[ -x "$RIME_BUILT" ] &&
    # return 0` asked the wrong question: it proves a binary exists, not that
    # it matches the source. `cargo test --bins --no-run` builds
    # deps/rime-<hash> and never refreshes target/debug/rime, so this gate
    # would answer about a binary from an earlier commit and say nothing about
    # it. Measured 2026-09-12: a first run reported 166 documented verbs, a
    # rebuild gave 171 -- the `rime user` verbs P2-016 had just landed. The
    # verdict happened not to change; a gate that reads a stale binary is one
    # that can report a clean surface for code nobody compiled.
    #
    # cargo is incremental, so this is a no-op when it is already current.
    ( cd rimed && cargo build --locked --bin rime ) || return 1
    [ -x "$RIME_BUILT" ]
}

if [ -z "${Rime:-}" ]; then
    if ensure_built; then
        Rime=$RIME_BUILT
    else
        echo "WARN  could not build $RIME_BUILT; asking \`rime\` on PATH instead" >&2
        Rime=rime
    fi
fi

# ── what the two passes are asked to look at ────────────────────────────────
docs=()
mode=both
for a in "$@"; do
    case "$a" in
        --reverse) mode=reverse ;;
        --forward) mode=forward ;;
        -*) echo "unknown option: $a" >&2; exit 2 ;;
        *) docs+=("$a") ;;
    esac
done
# Naming files means "check these", which only the forward pass can honour.
[ ${#docs[@]} -gt 0 ] && [ "$mode" = both ] && mode=forward

# Every doc a reader is meant to follow. Historical records are excluded here
# rather than by whoever calls this, so the reverse pass cannot be handed a
# subset and made to look bad — or, worse, handed docs/ twice and made to look
# good.
canonical_docs() {
    local f
    for f in docs/*.md README.md; do
        [ -f "$f" ] || continue
        case "$f" in
            docs/m[0-9]*-notes.md|docs/m0-results.md|docs/p[0-9]-progress.md) continue ;;
        esac
        echo "$f"
    done
}

fail=0
MENTIONS=$(mktemp) || { echo "FATAL: no temp file" >&2; exit 2; }
trap 'rm -f "$MENTIONS"' EXIT

# ── forward: every command a doc shows, asked of the CLI that ships ─────────
forward() {
    local pass=0 skip=0 bad=0 doc c
    command -v "$Rime" >/dev/null 2>&1 || {
        echo "SKIP  no \`$Rime\` on PATH; the forward pass asks the shipped CLI"
        return 0
    }
    for doc in "$@"; do
        [ -f "$doc" ] || { echo "no such file: $doc"; bad=$((bad+1)); continue; }
        mapfile -t cmds < <(mentions "$doc")
        for c in "${cmds[@]}"; do
            if $Rime ${c#rime } --help >/dev/null 2>&1; then
                pass=$((pass+1))
            elif allowed "$c"; then
                printf 'ALLOW %-34s %s\n' "$c" "$(basename "$doc")"
                skip=$((skip+1))
            else
                printf 'BAD   %-34s %s\n' "$c" "$(basename "$doc")"
                bad=$((bad+1))
            fi
        done
    done
    printf '\ndoc verbs: %d valid, %d deliberate, %d not a command\n' "$pass" "$skip" "$bad"
    fail=$((fail + bad))
}

allowed() {
    [ -f "$ALLOW" ] || return 1
    grep -v '^[[:space:]]*#' "$ALLOW" | pipe_has -xF "$1"
}

# Every `rime …` a file names. One extraction, used by both directions, so
# "documented" means the same thing whichever way the question is asked.
mentions() {
    grep -ohE '(^|[`[:space:]])(sudo )?rime [a-z][a-z-]*( [a-z][a-z-]*)?' "$@" \
        | sed -E 's/^[`[:space:]]+//; s/^sudo //' \
        | sort -u
}

# ── reverse: every command the binary offers, asked of the docs ─────────────
built_commands() {
    local v s subs
    for v in $("$RIME_BUILT" --help 2>&1 \
        | sed -n '/^Commands:/,/^Options:/p' | grep -oE '^  [a-z][a-z-]*' | tr -d ' '); do
        # clap's own, and it documents itself.
        [ "$v" = help ] && continue
        echo "rime $v"
        subs=$("$RIME_BUILT" "$v" --help 2>&1 \
            | sed -n '/^Commands:/,/^Options:/p' | grep -oE '^  [a-z][a-z-]*' | tr -d ' ')
        for s in $subs; do
            [ "$s" = help ] && continue
            echo "rime $v $s"
        done
    done
}

# A subverb has to be named exactly. A top-level verb counts as named when any
# mention starts with it, because a doc that only ever writes
# `rime remote pair` has still told the reader that `rime remote` exists.
is_documented() {
    local c=$1
    grep -qxF "$c" "$MENTIONS" && return 0
    case "$c" in
        "rime "*" "*) return 1 ;;
        *) grep -qE "^$c( |\$)" "$MENTIONS" ;;
    esac
}

in_debt_list() {
    [ -f "$DEBT" ] || return 1
    grep -v '^[[:space:]]*#' "$DEBT" | pipe_has -xF "$1"
}

reverse() {
    # Not a SKIP. The reverse pass asks what the binary contains, so without a
    # binary there is no question — and a checker that reports "fine" when it
    # could not look is the defect this pass exists to close, in green.
    ensure_built || {
        echo "FATAL: could not build $RIME_BUILT; the reverse pass cannot run" >&2
        fail=$((fail+1))
        return
    }
    mentions "$@" > "$MENTIONS"

    local documented=0 debt=0 undeclared=0 stale=0 c
    while read -r c; do
        if is_documented "$c"; then
            documented=$((documented+1))
            # The ratchet. A command that is documented AND still declared as
            # debt would let the list outlive the gap it records, and a debt
            # list nobody prunes is an allow list with a sad name.
            if in_debt_list "$c"; then
                printf 'STALE %-34s documented now; drop it from %s\n' "$c" "$DEBT"
                stale=$((stale+1))
            fi
        elif in_debt_list "$c"; then
            debt=$((debt+1))
        else
            printf 'BAD   %-34s is in the binary and in no document\n' "$c"
            undeclared=$((undeclared+1))
        fi
    done < <(built_commands)

    printf '\ncommand surface: %d documented, %d declared undocumented, %d undocumented and undeclared, %d stale\n' \
        "$documented" "$debt" "$undeclared" "$stale"
    fail=$((fail + undeclared + stale))
}

case "$mode" in
    forward) forward "${docs[@]}" ;;
    reverse) mapfile -t docs < <(canonical_docs); reverse "${docs[@]}" ;;
    both)
        mapfile -t docs < <(canonical_docs)
        forward "${docs[@]}"
        echo
        reverse "${docs[@]}"
        ;;
esac

[ "$fail" -eq 0 ]
