#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  mutate-platform-theme.sh — prove test-rime-platform-theme.sh can go red,
#  AND prove it does not go red at the three decoys it was written to ignore.
#
#  A guard on a value is the easy kind to write vacuously. `grep -q qt6ct` over
#  this repository passes on a COPY path in Containerfile.base and on the
#  comment in files/system/qt6ct/qt6ct.conf that explains the palette — two
#  places that set nothing — so a suite can look thorough, go green, and be
#  satisfied entirely by prose. That is the dominant CI defect family in this
#  repository ("a gate that runs and inspects nothing"), and the second is a
#  gate that reads state belonging to whatever environment it happens to run
#  in. This harness is aimed at both.
#
#  So there are two kinds of mutant here and both are scored:
#
#    RED mutants (M*) break a link in the chain; a NAMED assertion must fail.
#    GREEN mutants (G*) add a decoy that sets nothing; the suite must STAY
#      green. A guard that fires on a comment is as useless as one that never
#      fires, and it is worse to live with, because the first person to hit it
#      deletes the assertion rather than the comment.
#
#  Verdicts are THREE-way, not two. This unit scored a mutant the suite had
#  CAUGHT as SURVIVED six rounds running, because "the suite went red somewhere
#  else" and "the suite stayed green" look identical if you only grep for one
#  sentence. classify() distinguishes them and is self-tested below in all
#  three states before a single mutant is applied.
#
#  Restores are `git checkout --`, never `cp`: authoritative about content, and
#  a fresh mtime. A `cp` restore in this unit once left a file holding another
#  file's contents and it was caught by an editor notice rather than by the
#  harness. The file set is compared against HEAD after every mutate AND every
#  restore.
#
#  Run from anywhere: ./tests/mutate-platform-theme.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }
cd "$(dirname "$0")/.." || exit 2

CF="Containerfile.core"
LABWC="files/desktop/labwc/environment"
SUITE_F="tests/test-rime-platform-theme.sh"
# Containerfile.base is in the set because G1 is not the only reason a run can
# touch it: the COPY assertion resolves across every Containerfile, and a mutant
# applied to a file outside $FILES is never put back, which makes every verdict
# after it a verdict about a tree nobody is watching.
CFB="Containerfile.base"
FILES="$CF $CFB $LABWC $SUITE_F"
SUITE="./tests/test-rime-platform-theme.sh"

applied=0; noapply=0; caught=0; survived=0; misscored=0; held=0; falsered=0

tree_clean() { [ -z "$(git diff --name-only -- $FILES 2>/dev/null)" ]; }

restore() {
    git checkout -- $FILES 2>/dev/null
    if ! tree_clean; then
        echo "ABORT: tree still dirty after restore; verdicts would be meaningless" >&2
        git diff --stat -- $FILES >&2
        exit 3
    fi
}

# `env -i` deliberately. This suite reads files rather than the environment, but
# the sister suite in rime-shell was green for a whole round only because it
# inherited QT_QPA_PLATFORMTHEME from the operator's shell, and the harness that
# found that is this one's shape. A verdict must not be about whose terminal it
# ran in.
run_suite() {
    env -i HOME="$HOME" PATH="$PATH" USER="${USER:-$(id -un)}" \
        TMPDIR="${TMPDIR:-/tmp}" "$SUITE" 2>&1
}

# How many assertions the suite reported FAILED, or 0 if it printed no totals
# line at all — a crash must never read as a clean green run.
suite_failures() {
    printf '%s\n' "$1" \
        | sed -n 's/^rime-platform-theme: [0-9]* passed, \([0-9]*\) failed.*/\1/p' \
        | head -1 | grep -E '^[0-9]+$' || echo 0
}

# classify <suite output> <expected FAIL substring> -> CAUGHT | MISSCORED | SURVIVED
classify() {
    local out="$1" want="$2"
    if printf '%s' "$out" | pipe_has "^FAIL  .*$want"; then
        echo CAUGHT
    elif [ "$(suite_failures "$out")" -gt 0 ]; then
        echo MISSCORED
    else
        echo SURVIVED
    fi
}

# ── self-test: the scoring above, in all three states and both directions ────
selftest() {
    local red green fails=0
    red="FAIL    ...and the theme it names is qt6ct, the plugin that installs a QTranslator
rime-platform-theme: 15 passed, 3 failed, 0 skipped"
    green="rime-platform-theme: 18 passed, 0 failed, 0 skipped"

    chk() {  # chk <label> <want> <got>
        if [ "$2" = "$3" ]; then printf '  ok   %s\n' "$1"
        else printf '  FAIL %s — want %s got %s\n' "$1" "$2" "$3"; fails=$((fails + 1)); fi
    }
    chk "a red suite naming the expectation is CAUGHT" \
        CAUGHT    "$(classify "$red"   "and the theme it names is qt6ct")"
    chk "a red suite NOT naming it is MISSCORED, not a survival" \
        MISSCORED "$(classify "$red"   "a sentence this suite never prints")"
    chk "a green suite is the only thing that is a SURVIVAL" \
        SURVIVED  "$(classify "$green" "a sentence this suite never prints")"
    chk "counting failures off a red totals line"   3 "$(suite_failures "$red")"
    chk "counting failures off a green totals line" 0 "$(suite_failures "$green")"
    chk "output with no totals line at all counts 0 and cannot read as green" \
        0 "$(suite_failures "crashed before printing anything")"
    [ "$fails" -eq 0 ] || { echo "ABORT: the harness cannot score itself" >&2; exit 3; }
}

echo "── self-test: this harness can tell its three verdicts apart ──"
selftest

# apply_edit <file> <from> <to> -> 0 applied, 1 anchor absent
apply_edit() {
    local file="$1" from="$2" to="$3"
    grep -qF -- "$from" "$file" || return 1
    python3 - "$file" "$from" "$to" <<'EDIT'
import sys
p, a, b = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(p, encoding="utf-8").read()
assert s.count(a) >= 1
open(p, 'w', encoding="utf-8").write(s.replace(a, b, 1))
EDIT
}

score_red() {   # score_red <id> <expected FAIL substring>
    local id="$1" want="$2" out verdict
    out="$(run_suite)"; verdict="$(classify "$out" "$want")"
    if [ "$verdict" = CAUGHT ]; then
        printf '%-5s CAUGHT    %s\n' "$id" "$want"
        caught=$((caught + 1))
    elif [ "$verdict" = MISSCORED ]; then
        # NOT a survival: the suite went red, just not on the assertion this
        # mutant NAMES. Fix the expectation, not the code.
        printf '%-5s MISSCORED the suite went red (%s failed) but not on the named assertion\n' \
               "$id" "$(suite_failures "$out")"
        printf '      expected a FAIL line containing: %s\n' "$want"
        printf '      ── what actually went red ──\n'
        printf '%s\n' "$out" | grep -E '^FAIL' | sed 's/^/      /'
        misscored=$((misscored + 1))
    else
        printf '%-5s SURVIVED  %s\n' "$id" "$want"
        printf '      ── the suite stayed GREEN with this mutant applied ──\n'
        printf '%s\n' "$out" | grep -E '^(FAIL|SKIP|rime-platform-theme)' | sed 's/^/      /'
        survived=$((survived + 1))
    fi
}

# mutate <id> <file> <from> <to> <assertion substring that must go red>
mutate() {
    local id="$1" file="$2" from="$3" to="$4" want="$5"
    tree_clean || { echo "ABORT: tree dirty BEFORE $id" >&2; exit 3; }
    if ! apply_edit "$file" "$from" "$to"; then
        printf '%-5s NO-APPLY  anchor absent in %s — this mutant proves nothing\n' "$id" "$file"
        noapply=$((noapply + 1)); return
    fi
    if tree_clean; then
        printf '%-5s NO-APPLY  the edit changed nothing\n' "$id"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))
    score_red "$id" "$want"
    restore
}

# mutate_both <id> <from1> <to1> <from2> <to2> <expected FAIL substring>
# The coordinated rename: BOTH files changed consistently, so every "these two
# agree" assertion is satisfied. Something else has to catch it.
mutate_both() {
    local id="$1" f1="$2" t1="$3" f2="$4" t2="$5" want="$6"
    tree_clean || { echo "ABORT: tree dirty BEFORE $id" >&2; exit 3; }
    if ! apply_edit "$CF" "$f1" "$t1" || ! apply_edit "$LABWC" "$f2" "$t2"; then
        printf '%-5s NO-APPLY  one of the two anchors is absent\n' "$id"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))
    score_red "$id" "$want"
    restore
}

# hold <id> <file> <from> <to> <why this must NOT fire>
# The other direction. A decoy that sets nothing is added to a real file and the
# suite has to stay green. Scored separately so a false red is never counted as
# a caught mutant.
hold() {
    local id="$1" file="$2" from="$3" to="$4" why="$5"
    tree_clean || { echo "ABORT: tree dirty BEFORE $id" >&2; exit 3; }
    if ! apply_edit "$file" "$from" "$to"; then
        printf '%-5s NO-APPLY  anchor absent in %s\n' "$id" "$file"
        noapply=$((noapply + 1)); restore; return
    fi
    applied=$((applied + 1))
    local out; out="$(run_suite)"
    if [ "$(suite_failures "$out")" -eq 0 ] \
       && printf '%s' "$out" | pipe_has '^rime-platform-theme: [0-9]* passed, 0 failed'; then
        printf '%-5s HELD      %s\n' "$id" "$why"
        held=$((held + 1))
    else
        printf '%-5s FALSE-RED %s\n' "$id" "$why"
        printf '      ── the suite fired on something that sets nothing ──\n'
        printf '%s\n' "$out" | grep -E '^(FAIL|rime-platform-theme)' | sed 's/^/      /'
        falsered=$((falsered + 1))
    fi
    restore
}

echo
echo "── baseline: green, or nothing below means anything ──"
base="$(run_suite)"
printf '%s\n' "$base" | grep -E '^rime-platform-theme'
if ! printf '%s' "$base" | pipe_has -E '^rime-platform-theme: [0-9]+ passed, 0 failed'; then
    echo "ABORT: the suite is not green to begin with" >&2
    printf '%s\n' "$base" | grep -E '^(FAIL|SKIP)' >&2
    exit 3
fi

echo
echo "── RED mutants: a link in the chain is broken ──"

# M1 — the VALUE in /etc/environment changes. This is the mutant the round
#      exists for: it was caught by nothing anywhere in either repository. The
#      build stays green, the image ships, and every mirrored surface goes
#      quietly left-to-right because qt5ct's plugin is not there to install a
#      QTranslator.
mutate M1 "$CF" \
    "printf 'QT_QPA_PLATFORMTHEME=qt6ct" \
    "printf 'QT_QPA_PLATFORMTHEME=qt5ct" \
    "and the theme it names is qt6ct"

# M2 — the labwc session alone is changed. The worst shape of this bug: two
#      compositors mirror and the third does not, so it reads as a labwc
#      problem rather than as a one-word edit.
mutate M2 "$LABWC" \
    'QT_QPA_PLATFORMTHEME=qt6ct' \
    'QT_QPA_PLATFORMTHEME=qt5ct' \
    "and it AGREES with /etc/environment"

# M3 — the coordinated rename. Both files changed consistently, so every
#      agreement assertion is satisfied and the suite has to fall back on the
#      one thing a rename cannot fake: the theme has to be a package the image
#      really installs.
mutate_both M3 \
    "printf 'QT_QPA_PLATFORMTHEME=qt6ct" "printf 'QT_QPA_PLATFORMTHEME=qt5ct" \
    'QT_QPA_PLATFORMTHEME=qt6ct'         'QT_QPA_PLATFORMTHEME=qt5ct' \
    "the image installs the platform theme plugin the variable names"

# M4 — the whole /etc/environment write is deleted. test-rime-ai-apps.sh also
#      goes red on this one, with a message about Electron; this is the suite
#      that says what it costs.
mutate M4 "$CF" \
    "    printf 'QT_QPA_PLATFORMTHEME=qt6ct\nTERMINAL=alacritty\nELECTRON_OZONE_PLATFORM_HINT=auto\n' >> /etc/environment; \\" \
    "    true; \\" \
    "exactly one Containerfile assignment of QT_QPA_PLATFORMTHEME exists"

# M5 — the redirect is removed and nothing else. The printf still runs, still
#      names the variable, still contains the right value — and writes to the
#      build log instead of the image. Every grep-shaped assertion in the
#      repository stays green through this.
mutate M5 "$CF" \
    "ELECTRON_OZONE_PLATFORM_HINT=auto\n' >> /etc/environment;" \
    "ELECTRON_OZONE_PLATFORM_HINT=auto\n';" \
    "and it is redirected into /etc/environment"

# M6 — the plugin package is dropped. The variable then names a platform theme
#      with nothing behind it, which Qt handles by silently falling back: no
#      warning, no crash, no QTranslator, no mirroring.
mutate M6 "$CF" \
    '        qt6ct qt6-qttranslations qt6-qtwebengine' \
    '        qt6-qttranslations qt6-qtwebengine' \
    "the image installs the platform theme plugin the variable names"

# M7 — the catalogues are dropped back to being a weak dependency. This is the
#      state the repository was in until this round, and it is the reason the
#      suite asserts an install-list token rather than a file on disk: the file
#      IS on disk here, because dnf installs Recommends by default.
mutate M7 "$CF" \
    'qt6ct qt6-qttranslations qt6-qtwebengine' \
    'qt6ct qt6-qtwebengine' \
    "the image names qt6-qttranslations EXPLICITLY, not as a weak dependency"

# M8 — the labwc assignment is deleted outright rather than changed. Until this
#      round that file was unguarded, so this mutant was invisible.
mutate M8 "$LABWC" \
    'QT_QPA_PLATFORMTHEME=qt6ct' \
    '# QT_QPA_PLATFORMTHEME=qt6ct' \
    "exactly one QT_QPA_PLATFORMTHEME assignment in"

# M9 — `export` is put in front of it. labwc reads this file as KEY=VALUE with
#      no shell syntax at all, so this does not set QT_QPA_PLATFORMTHEME; it
#      sets a variable whose name begins "export ". A silent no-op that looks
#      more correct than the correct version.
mutate M9 "$LABWC" \
    'QT_QPA_PLATFORMTHEME=qt6ct' \
    'export QT_QPA_PLATFORMTHEME=qt6ct' \
    "and contains no shell syntax labwc would take literally"

# M10 — the qt6ct configuration stops being copied in. The palette half, which
#       is what the theme was configured FOR and the reason the whole mechanism
#       reads as optional to anyone looking at it.
mutate M10 "$CFB" \
    'COPY files/system/qt6ct/qt6ct.conf /etc/xdg/qt6ct/qt6ct.conf' \
    'COPY files/system/qt6ct/qt6ct.conf /etc/xdg/qt6ct/qt6ct.conf.disabled' \
    "the qt6ct configuration the theme reads is COPYed into the image"

echo
echo "── GREEN mutants: a decoy that sets nothing must not fire the guard ──"

# G1 — a COMMENT in the Containerfile that reads exactly like the real thing,
#      wrong value and all. This is the M7 trap from the round-20 notes, tried
#      against the real tree instead of against canned input: a bare
#      `grep -q QT_QPA_PLATFORMTHEME` or `grep -q qt6ct` cannot tell this line
#      from the one three thousand lines above it.
hold G1 "$CFB" \
    '# Dark mode (rime-logs 28)' \
    "# printf 'QT_QPA_PLATFORMTHEME=qt5ct\n' >> /etc/environment  <- prose, not a command
# Dark mode (rime-logs 28)" \
    "a Containerfile COMMENT naming a different theme does not fire the guard"

# G2 — the same decoy in the labwc environment file, where a `#` line is a
#      comment to labwc too. The parse is anchored at start-of-line for exactly
#      this reason.
hold G2 "$LABWC" \
    '# Firefox and other Gecko apps on Wayland.' \
    '# QT_QPA_PLATFORMTHEME=qt5ct — the old value, kept for reference
# Firefox and other Gecko apps on Wayland.' \
    "a commented-out assignment in the labwc file does not fire the guard"

# G3 — the theme name appears in a build-time grep, which is a command and not
#      a comment, and still sets nothing. The package-name extraction has to be
#      reading dnf install targets rather than "lines that mention qt6ct".
hold G3 "$CFB" \
    'COPY files/system/qt6ct/qt6ct.conf /etc/xdg/qt6ct/qt6ct.conf' \
    'COPY files/system/qt6ct/qt6ct.conf /etc/xdg/qt6ct/qt6ct.conf
RUN grep -q qt5ct /etc/xdg/qt6ct/qt6ct.conf || true' \
    "a RUN that merely mentions another theme name does not fire the guard"

echo
printf 'mutants applied=%d, failed-to-apply=%d | red: caught=%d SURVIVED=%d MISSCORED=%d | green: held=%d FALSE-RED=%d\n' \
    "$applied" "$noapply" "$caught" "$survived" "$misscored" "$held" "$falsered"
tree_clean || { echo "ABORT: tree dirty at end of run" >&2; exit 3; }
echo "the tree matches HEAD"
[ "$survived" -eq 0 ] && [ "$misscored" -eq 0 ] && [ "$falsered" -eq 0 ] && [ "$noapply" -eq 0 ]
