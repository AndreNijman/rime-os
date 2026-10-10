#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-a11y-stack.sh — prove test-rime-a11y-stack.sh can go red.
#
#  The suite makes two claims that are easy to assert vacuously: a package is
#  installed, and a package is NOT started. The first is the M7 trap — the word
#  appears in the stanza's own prose, so a grep stays green with the package
#  deleted. The second is the N8 trap — "not found" is what a broken search
#  returns as well as a clean one.
#
#  Restores are `git checkout --`, never `cp -p` and never `mv`. The file set is
#  compared against HEAD after every mutate AND every restore.
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

CORE="Containerfile.core"
AUTO="files/desktop/labwc/autostart"
READER="files/system/libexec/rime-screen-reader"
SUITE_F="tests/test-rime-a11y-stack.sh"
FILES="$CORE $AUTO $READER $SUITE_F"
SUITE="./tests/test-rime-a11y-stack.sh"

applied=0; noapply=0; caught=0; survived=0

tree_clean() { [ -z "$(git diff --name-only -- $FILES 2>/dev/null)" ]; }

restore() {
    git checkout -- $FILES 2>/dev/null
    if ! tree_clean; then
        echo "ABORT: tree still dirty after restore; verdicts would be meaningless" >&2
        git diff --stat -- $FILES >&2
        exit 3
    fi
}

run_suite() { "$SUITE" 2>&1; }

mutate() {   # mutate <id> <file> <from> <to> <assertion substring that must go red>
    local id="$1" file="$2" from="$3" to="$4" want="$5"

    tree_clean || { echo "ABORT: tree dirty BEFORE $id" >&2; exit 3; }
    if ! grep -qF -- "$from" "$file"; then
        printf '%-5s NO-APPLY  anchor absent in %s — this mutant proves nothing\n' "$id" "$file"
        noapply=$((noapply + 1)); return
    fi
    python3 - "$file" "$from" "$to" <<'EDIT'
import sys
p, a, b = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p).read()
assert s.count(a) >= 1
open(p, 'w').write(s.replace(a, b, 1))
EDIT
    if tree_clean; then
        printf '%-5s NO-APPLY  the edit changed nothing\n' "$id"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))

    local out; out="$(run_suite)"
    if printf '%s' "$out" | pipe_has "^FAIL  .*$want"; then
        printf '%-5s CAUGHT    %s\n' "$id" "$want"
        caught=$((caught + 1))
    else
        printf '%-5s SURVIVED  %s\n' "$id" "$want"
        printf '%s\n' "$out" | grep -E '^(FAIL|SKIP|rime-a11y-stack)' | sed 's/^/      /'
        survived=$((survived + 1))
    fi
    restore
}

echo "── baseline: green, or nothing below means anything ──"
base="$(run_suite)"
printf '%s\n' "$base" | grep -E '^rime-a11y-stack'
if ! printf '%s' "$base" | pipe_has -E '^rime-a11y-stack: [0-9]+ passed, 0 failed'; then
    echo "ABORT: the suite is not green to begin with" >&2
    printf '%s\n' "$base" | grep -E '^(FAIL|SKIP)' >&2
    exit 3
fi

echo
echo "── the mutants ──"

# D1 — the package is removed and the paragraph explaining it is left behind.
#      This is the whole reason the suite parses instead of grepping: the word
#      `orca` is still in the file half a dozen times after this edit.
mutate D1 "$CORE" \
    '    dnf5 -y install orca; \' \
    '    dnf5 -y install; \' \
    "Containerfile.core installs orca"

# D2 — somebody switches the reader on for everybody. A screen reader that
#      starts unbidden talks over a sighted user's first boot, and it is the
#      kind of "helpful" change that would never be questioned.
mutate D2 "$AUTO" \
    'command -v fcitx5 >/dev/null && fcitx5 -d -r &' \
    'command -v fcitx5 >/dev/null && fcitx5 -d -r &
command -v orca >/dev/null && orca --replace &' \
    "nothing in the image autostarts orca"

# D3 — the vacuity floor. An extractor that finds nothing makes every claim
#      below it a claim about an empty set.
mutate D3 "$SUITE_F" \
    "        m = re.search(r'\\bdnf5?\\b.*?\\binstall\\b(.*)\$', c)" \
    "        m = re.search(r'\\bNOTHING-MATCHES-THIS\\b(.*)\$', c)" \
    "the package list was extracted from Containerfile.core"

# D4 — the extractor stops discarding comment lines, which is precisely the
#      failure it exists to avoid: a commented-out install line would then be
#      read as a live one. The fixture carries exactly that shape, which is why
#      the negative control is a fixture and not a word from the real file.
mutate D4 "$SUITE_F" \
    "    if line.startswith('#'):
        continue" \
    "    if False:
        continue" \
    "a package named only in a comment is not extracted"

# ── the switch ──────────────────────────────────────────────────────────────
# "Not autostarted" is only a defensible decision while the person who needs the
# reader can switch it on, so the switch's assertions need the same treatment as
# the package's. D5 and D6 are the two mistakes that were actually available
# when this script was written, and both would have looked correct in review.

# D5 — the toggle tries to stop the reader with a verb orca does not have.
#      orca 49's whole option list is -h -v -r -s -l -e -d -p -u --speech-system
#      --debug-file --debug: there is no --quit. The result is a switch that can
#      turn the reader ON and never off, and the stub in the suite is as strict
#      about its command line as the real argparse, so it refuses it exactly as
#      production would.
mutate D5 "$READER" \
    '    [ -n "$pids" ] && kill -TERM $pids 2>/dev/null' \
    '    [ -n "$pids" ] && orca --quit 2>/dev/null' \
    "a second press stops it"

# D6 — the probe stops recognising the reader the switch itself started. Every
#      press then starts another one and none of them can ever be stopped, which
#      is what a running-reader probe gets wrong in practice whichever spelling
#      it uses.
#
#      This mutant replaced one that asserted something FALSE. It used to turn
#      the probe into `pgrep -x orca` on the stated grounds that comm is
#      `python3` for a `#!/usr/bin/python3` script and `-x` therefore matches
#      nothing. It SURVIVED, and the survival was right: Linux takes comm from
#      the SCRIPT's basename, so comm is `orca` and `-x` works. The claim was
#      corrected in the switch, the Containerfile and the suite rather than the
#      mutant being softened.
mutate D6 "$READER" \
    "(^|/)orca([[:space:]]|\$)" \
    "(^|/)orca-no-such-reader([[:space:]]|\$)" \
    "a second press stops it"

# D7 — the reader stops being installed into the image at all, while the script
#      stays in the repository. Every behavioural assertion above still passes,
#      because they run the script from the checkout; only the question "does
#      anything put this in the image" can see it. K12/K13 in miniature.
mutate D7 "$CORE" \
    'COPY --chmod=0755 files/system/libexec/rime-screen-reader /usr/libexec/rime-screen-reader' \
    '# (the switch is no longer installed)' \
    "Containerfile.core installs it to /usr/libexec/rime-screen-reader"

# D8 — the systemd path is dropped and the switch always execs orca directly.
#      A reader started outside the unit has no Restart=always, and a blind user
#      cannot see that it stopped.
mutate D8 "$READER" \
    '    if use_systemd; then
        systemctl --user start orca.service && return 0' \
    '    if false; then
        systemctl --user start orca.service && return 0' \
    "the switch starts the UNIT"

echo
printf 'mutants applied=%d, failed-to-apply=%d, caught=%d, SURVIVED=%d\n' \
    "$applied" "$noapply" "$caught" "$survived"
tree_clean || { echo "ABORT: tree dirty at end of run" >&2; exit 3; }
echo "the tree matches HEAD"
[ "$survived" -eq 0 ] && [ "$noapply" -eq 0 ]
