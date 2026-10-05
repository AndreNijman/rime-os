#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
#  Assertions for /usr/libexec/rime-labwc-keybinds — §17's fourth generator.
#
#  The shell has written Hyprland .conf/.lua and niri .kdl on every keybind edit
#  for a long time. labwc got nothing, because it has no IPC to push bindings
#  over and no include mechanism to append a generated file to. So on labwc the
#  Keybinds page was fully interactive and completely inert: rebind the
#  launcher, watch the UI confirm it, press the key, nothing happens.
#
#  Generation is a pure function of the model, so it is tested directly. The
#  splice is tested against fixture rc.xml files rather than a live one — this
#  suite must never touch ~/.config/labwc/rc.xml, and the helper's `apply`
#  default would do exactly that if it were called without --rc.
#
#      ./tests/test-labwc-keybinds.sh
# ─────────────────────────────────────────────────────────────────────────────
set -uo pipefail
# `set +e` is deliberate. This suite counts failures rather than aborting, and
# several assertions run commands that exit non-zero on purpose. CI invokes a
# script as `bash -e {0}`, under which a `x="$(cmd)"` assignment whose command
# fails kills the whole run part-way through.
set +e

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GEN="${ROOT}/files/system/libexec/rime-labwc-keybinds"
CHECK="${ROOT}/files/scripts/check-labwc-keybinds"
RC="${ROOT}/files/desktop/labwc/rc.xml"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0; fail=0
ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skp() { printf 'SKIP  %s\n' "$1"; }
section() { printf '\n── %s ──\n' "$1"; }

[ -f "$GEN" ] || { printf 'missing %s\n' "$GEN" >&2; exit 1; }

# The model lives in the shell, so this suite needs a shell tree. The checkout
# first: the installed copy is whatever the last image shipped, which lags the
# tree under test, and checking a change against a stale source of truth fails
# for reasons unrelated to the change.
# RIME_SHELL_TREE first, because the two repositories are not always siblings:
# roadmap work happens in paired worktrees under /var/tmp/rime-work, where
# `../rime-shell` resolves to nothing and the fallback silently measures the
# INSTALLED shell — whatever the last image shipped. That is the worst of the
# three outcomes: it runs, it is green, and it says nothing about the change.
SHELL_TREE=""
for cand in "${RIME_SHELL_TREE:-}" "${ROOT}/../rime-shell" /usr/share/rime-shell; do
    [ -n "$cand" ] || continue
    [ -f "${cand}/src/services/config_tab/KeybindService.qml" ] && { SHELL_TREE="$cand"; break; }
done

if [ -z "$SHELL_TREE" ]; then
    # A suite that skips proves nothing, and this one skipped its ENTIRE self on
    # CI the first time it ran there: no rime-shell checkout next to the repo, no
    # /usr/share/rime-shell in the runner, `passed=0 failed=0`, green tick. That
    # is indistinguishable from working.
    #
    # So CI sets RIME_REQUIRE_SHELL_TREE=1 and the skip becomes a failure. A
    # developer running this locally without a shell checkout still gets a skip,
    # because there the absence is obvious and the alternative is a suite nobody
    # can run.
    if [ "${RIME_REQUIRE_SHELL_TREE:-0}" = "1" ]; then
        printf 'FAIL  a rime-shell tree is required here and none was found\n' >&2
        printf '      looked in: $RIME_SHELL_TREE, %s/../rime-shell and /usr/share/rime-shell\n' "$ROOT" >&2
        printf '\npassed=0 failed=1\n'
        exit 1
    fi
    printf 'no rime-shell tree available; nothing here can run\n' >&2
    printf '\npassed=0 failed=0 (skipped: no shell tree)\n'
    exit 0
fi

# Which tree was picked, said out loud. Falling back to the installed shell is
# legitimate on a machine with no checkout, but it means testing against
# whatever the LAST IMAGE shipped — and when that lags the tree under test, the
# failures look like real regressions in the code you just wrote. That happened
# while integrating the P1 branches: three assertions failed in a git worktree
# purely because the worktree has no sibling rime-shell, and the fallback was
# silent about it.
#
# And the NOTE was not enough. It is printed once, at the top, and the FAIL
# lines are forty lines below it; on 2026-09-13 a reader who had just fixed the
# CI half of this same problem ran the suite in a worktree with no sibling
# checkout, read three failures, and reported a screen-reader hole in the
# product. The three failures were real about /usr/share/rime-shell and false
# about both repositories at roadmap/v2.2, where the same suite is 39/0.
#
# So: under RIME_REQUIRE_SHELL_TREE=1 the installed shell is REFUSED rather than
# quietly measured. That flag means "measure the tree under test", and the
# installed shell is by definition not it — CI vendors a checkout to a sibling
# path, so nothing that sets the flag is relying on this fallback. Locally it
# turns three failures that read as a product defect into one line naming the
# variable to set.
if [ "$SHELL_TREE" = /usr/share/rime-shell ] \
   && [ "${RIME_REQUIRE_SHELL_TREE:-0}" = "1" ]; then
    printf 'FAIL  RIME_REQUIRE_SHELL_TREE=1 and the only shell tree found is the INSTALLED\n' >&2
    printf '      one at /usr/share/rime-shell — whatever the LAST IMAGE shipped, which on a\n' >&2
    printf '      roadmap machine lags both repositories by weeks. Measuring it would produce\n' >&2
    printf '      failures about staleness that read exactly like regressions.\n' >&2
    printf '      Set RIME_SHELL_TREE=<the rime-shell checkout under test>.\n' >&2
    printf '\npassed=0 failed=1\n'
    exit 1
fi
case "$SHELL_TREE" in
    /usr/share/rime-shell)
        printf 'NOTE  no rime-shell checkout beside this repo; testing against the
'
        printf '      INSTALLED shell at %s, which is whatever the last image
' "$SHELL_TREE"
        printf '      shipped. Failures here may be staleness, not regressions.

'
        ;;
    *)  printf 'using rime-shell tree: %s\n\n' "$SHELL_TREE" ;;
esac

# The provenance of the thing being measured, on the line people quote. The
# summary is what gets pasted into a handoff, and `passed=36 failed=3` carries
# no hint that the three failures are about a shell tree from three weeks ago.
SHELL_TREE_ID="$SHELL_TREE"
if [ -d "$SHELL_TREE/.git" ] || git -C "$SHELL_TREE" rev-parse --git-dir >/dev/null 2>&1; then
    SHELL_TREE_ID="$SHELL_TREE @ $(git -C "$SHELL_TREE" log -1 --format=%h 2>/dev/null || echo unknown)"
elif [ "$SHELL_TREE" = /usr/share/rime-shell ]; then
    SHELL_TREE_ID="$SHELL_TREE (INSTALLED — whatever the last image shipped)"
fi

# What root._shellDir resolves to on a booted system. Fixed even though the
# model is read from a checkout — the seeded rc.xml is written for the installed
# location, not for whatever path this test happens to run from.
INSTALLED=/usr/share/rime-shell
g() { python3 "$GEN" "$@" --shell-dir "$SHELL_TREE" --shell-path "$INSTALLED"; }

section "the generator"

python3 -c "import ast; ast.parse(open('$GEN').read())" \
    && ok "rime-labwc-keybinds is valid Python" || bad "rime-labwc-keybinds is valid Python"

block="$(g print 2>/dev/null)"
[ -n "$block" ] && ok "it generates a non-empty block" || bad "it generates a non-empty block"

n="$(printf '%s\n' "$block" | grep -c '<keybind key=')"

# EVERY id in the model is either generated or explicitly skipped. Nothing may
# fall out in between.
#
# This replaces a `-ge 30` threshold that carried this exact assertion's name
# and could not do its job: the generator was silently dropping 20 of the 68
# bindings — every `workspace-N` and `move-workspace-N`, because the id regex
# was `[A-Za-z-]+` and those ids contain digits — and 44 >= 30, so it passed.
# They were not reported as skipped either, since they never reached the code
# that reports that, so the build check printed "48 defaults" and agreed.
#
# A count compared against a number derived from the model cannot drift the way
# a hand-picked floor can.
accounted="$(python3 - "$SHELL_TREE" "$INSTALLED" "$ROOT" <<'PYEOF'
import importlib.util
import sys
from importlib.machinery import SourceFileLoader
# $ROOT, not a relative path: run from tests/ and a relative load fails, the
# three assertions built on it print blank counts ("sees  of 68 ids"), and the
# suite reads like a broken product rather than a broken invocation.
# spec + exec_module: Python 3.15 removed SourceFileLoader.load_module().
_l = SourceFileLoader("k", sys.argv[3] + "/files/system/libexec/rime-labwc-keybinds")
k = importlib.util.module_from_spec(importlib.util.spec_from_loader("k", _l))
sys.modules["k"] = k
_l.exec_module(k)
defaults = k.shell_defaults(sys.argv[1], sys.argv[2])
block, skipped = k.generate(defaults)
generated = block.count("<keybind key=")
print(f"{len(defaults)} {generated} {len(skipped)}")
PYEOF
)"
d_total="$(echo "$accounted" | cut -d' ' -f1)"
d_gen="$(echo "$accounted" | cut -d' ' -f2)"
d_skip="$(echo "$accounted" | cut -d' ' -f3)"

[ $((d_gen + d_skip)) -eq "$d_total" ] \
    && ok "every default is either generated or skipped ($d_gen + $d_skip = $d_total)" \
    || bad "bindings vanished between the model and the output: $d_gen generated + $d_skip skipped != $d_total defaults"

[ "$n" = "$d_gen" ] \
    && ok "it generates every binding it can ($n)" \
    || bad "it generates every binding it can (printed $n, accounted $d_gen)"

# The model is read out of QML by regex, so a parse that silently matches almost
# nothing is a live risk. Compare against the ids actually present.
present="$(python3 - "$SHELL_TREE" <<'PYEOF'
import re, sys
src = open(sys.argv[1] + "/src/services/config_tab/KeybindService.qml", encoding="utf-8").read()
start = src.index("_defaults")
end = src.find("\n    })", start)
print(len(re.findall(r'"([A-Za-z0-9-]+)":\s*\{', src[start:end])))
PYEOF
)"
[ "$d_total" = "$present" ] \
    && ok "the model parser sees every id in _defaults ($present)" \
    || bad "the model parser sees $d_total of $present ids in _defaults"

# ── the screen reader's way in (roadmap P2-003) ─────────────────────────────
# The image ships orca and deliberately autostarts nothing, so this one binding
# is the whole of "a blind user can turn the reader on". It is asserted HERE
# rather than only in rime-shell because labwc is the third of the three
# sessions and the one that gets its bindings by a different mechanism from the
# other two: no IPC, no include, an allowlist plus an exec arm that DROPS any
# command still starting with `$` after substitution. A binding written the
# natural way — `type: "exec", command: "$qsIpc …"` — reaches Hyprland and niri
# and is silently skipped here, which satisfies two thirds of the criterion
# while reading as though it satisfied all of it. That is exactly the trap
# voice-ptt's comment in KeybindService.qml records, and the only thing that
# catches it is running this generator.
if printf '%s\n' "$block" | grep -q 'rime-screen-reader'; then
    ok "the screen-reader binding survives the labwc generator"
else
    # The provenance goes IN the failure, not only in the header. This is the
    # one assertion in the suite that reads as an accessibility hole in the
    # product, so it must never be quotable without saying which shell tree
    # produced it.
    bad "the screen-reader binding survives the labwc generator — on Floating there is no way to start a reader (model read from $SHELL_TREE_ID)"
fi
# The <action> sits on the line AFTER its <keybind>, so the key is read by
# matching the whole element rather than by assuming the two share a line.
printf '%s\n' "$block" > "$WORK/block.txt"
sr_key="$(python3 - "$WORK/block.txt" <<'PYEOF'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
key = ""
for m in re.finditer(r'<keybind key="([^"]+)">(.*?)</keybind>', text, re.S):
    if "rime-screen-reader" in m.group(2):
        key = m.group(1); break
print(key)
PYEOF
)"
[ "$sr_key" = "W-A-s" ] \
    && ok "on the combination a screen-reader user already knows (W-A-s = SUPER+ALT+S)" \
    || bad "the screen-reader binding is on [$sr_key], not W-A-s (model read from $SHELL_TREE_ID)"

# A duplicate key is two bindings fighting over one shortcut, and labwc resolves
# that by taking one of them silently.
dupes="$(printf '%s\n' "$block" | grep -oE '<keybind key="[^"]+"' | sort | uniq -d)"
[ -z "$dupes" ] \
    && ok "no shortcut is generated twice" \
    || bad "duplicate shortcuts generated: $dupes"

printf '%s\n' "$block" | grep -q 'RIME-KEYBINDS-BEGIN' \
    && ok "the block carries its begin marker" || bad "the block carries its begin marker"
printf '%s\n' "$block" | grep -q 'RIME-KEYBINDS-END' \
    && ok "the block carries its end marker" || bad "the block carries its end marker"

# Deterministic: the splice compares against generated output, so an unstable
# ordering would make `check` fail at random.
b2="$(g print 2>/dev/null)"
[ "$block" = "$b2" ] && ok "generation is deterministic" || bad "generation is deterministic"

# The block has to be well-formed on its own, or the splice's post-validation is
# the only thing standing between a typo and an unparseable rc.xml.
printf '<r>%s</r>' "$block" > "$WORK/frag.xml"
python3 -c "import xml.etree.ElementTree as ET,sys; ET.parse(sys.argv[1])" "$WORK/frag.xml" \
    && ok "the generated block is well-formed XML" || bad "the generated block is well-formed XML"

section "what does not translate"

# labwc is a floating compositor: some tiling dispatchers have no equivalent.
# They must be REPORTED, not mapped onto something approximate — a shortcut
# doing the wrong thing is worse than one doing nothing.
skips="$(g print 2>/dev/null | grep -c 'no labwc equivalent')"
[ "$skips" -gt 0 ] \
    && ok "untranslatable bindings are reported ($skips)" \
    || bad "untranslatable bindings are reported"

g print 2>/dev/null | grep -q 'no labwc equivalent: window-pseudo' \
    && ok "pseudo-tiling is reported as unsupported" \
    || bad "pseudo-tiling is reported as unsupported"

# Nothing may be emitted with an unresolved Hyprland variable in it: labwc runs
# the command through execvp, so `$terminal` would be a literal argument.
printf '%s\n' "$block" | grep -q 'command="\$' \
    && bad "no unresolved config variable reaches a command" \
    || ok "no unresolved config variable reaches a command"

# The browser bind must name NO browser. It opens whatever the user has set as
# default, so a hardcoded `firefox` or `zen` here would make the shortcut
# contradict the user's own setting — which is exactly what it used to do.
printf '%s\n' "$block" | grep -q 'command="/usr/libexec/rime-open-browser"' \
    && ok "the browser bind opens the default browser, not a named one" \
    || bad "the browser bind opens the default browser, not a named one"

printf '%s\n' "$block" | grep -qE 'command="(firefox|zen|zen-browser|chromium|google-chrome)"' \
    && bad "no generated bind hardcodes a browser" \
    || ok "no generated bind hardcodes a browser"

printf '%s\n' "$block" | grep -q 'command="alacritty"' \
    && ok "\$terminal is resolved to the installed terminal" \
    || bad "\$terminal is resolved to the installed terminal"

# The screenshot bindings embed a path. It has to be the INSTALLED one, not
# whatever tree the generator read the model out of.
printf '%s\n' "$block" | grep -q "command=\"bash ${INSTALLED}/src/scripts/screenshot.sh" \
    && ok "generated paths name the installed shell, not the build tree" \
    || bad "generated paths name the installed shell, not the build tree"

section "user overrides"

mkdir -p "$WORK/ov"
printf '{"dashboard-launcher": {"mods": "SUPER + SHIFT", "key": "P"}}' > "$WORK/ov/keybinds.json"
ovblock="$(g print --overrides "$WORK/ov/keybinds.json" 2>/dev/null)"

printf '%s\n' "$ovblock" | grep -A1 'key="W-S-p"' | grep -q 'rime shell launcher' \
    && ok "a rebind reaches the generated config" \
    || bad "a rebind reaches the generated config"

printf '%s\n' "$ovblock" | grep -q 'key="A-space"' \
    && bad "the replaced default is gone" \
    || ok "the replaced default is gone"

# A corrupt overrides file must fall back to the defaults, not produce an empty
# config — losing every shortcut is a much worse failure than ignoring an edit.
printf 'not json at all' > "$WORK/ov/bad.json"
badov="$(g print --overrides "$WORK/ov/bad.json" 2>/dev/null | grep -c '<keybind key=')"
[ "$badov" = "$n" ] \
    && ok "an unreadable overrides file falls back to the defaults" \
    || bad "an unreadable overrides file falls back to the defaults (got $badov, want $n)"

section "splicing into rc.xml"

# No markers yet: the block goes in before </keyboard>, where labwc expects
# keybinds, and not at the end of the file.
cat > "$WORK/fresh.xml" <<'XML'
<?xml version="1.0"?>
<!-- a header comment that must survive -->
<labwc_config>
  <keyboard>
    <keybind key="A-Tab"><action name="NextWindow"/></keybind>
  </keyboard>
</labwc_config>
XML
g apply --rc "$WORK/fresh.xml" --no-reload >/dev/null 2>&1
grep -q 'RIME-KEYBINDS-BEGIN' "$WORK/fresh.xml" \
    && ok "a file with no markers gets the block inserted" \
    || bad "a file with no markers gets the block inserted"
grep -q 'a header comment that must survive' "$WORK/fresh.xml" \
    && ok "the header comment survives the splice" \
    || bad "the header comment survives the splice"
grep -q 'A-Tab' "$WORK/fresh.xml" \
    && ok "bindings the generator does not own survive" \
    || bad "bindings the generator does not own survive"
python3 -c "import xml.etree.ElementTree as ET,sys; ET.parse(sys.argv[1])" "$WORK/fresh.xml" \
    && ok "the spliced file parses" || bad "the spliced file parses"

# Well-formed is not the same as correct. A block spliced in as a SIBLING of
# <keyboard> parses fine and does nothing — labwc reads keybinds only from
# inside that element. The anchor used to be rfind("</keyboard>"), which
# happily matched the phrase inside a comment.
cat > "$WORK/trap.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <keyboard>
    <keybind key="A-Tab"><action name="NextWindow"/></keybind>
  </keyboard>
  <!-- old notes: the block used to live before </keyboard> here -->
</labwc_config>
XML
g apply --rc "$WORK/trap.xml" --no-reload >/dev/null 2>&1
placed="$(python3 - "$WORK/trap.xml" <<'PYEOF'
import sys, xml.etree.ElementTree as ET
t = ET.parse(sys.argv[1]).getroot()
inside = sum(len(kb.findall("keybind")) for kb in t.iter("keyboard"))
print(f"{inside} {len(list(t.iter('keybind')))}")
PYEOF
)"
# Both halves matter. Equality alone passes vacuously when the splice is
# REFUSED — the file keeps its single pre-existing bind, 1 == 1, green tick.
# So the block must also actually be there.
p_in="$(echo "$placed" | cut -d' ' -f1)"
p_all="$(echo "$placed" | cut -d' ' -f2)"
if [ "$p_in" = "$p_all" ] && [ "$p_in" -gt "$n" ]; then
    ok "bindings land inside <keyboard> even when a comment mentions the closing tag"
else
    bad "the block did not land inside <keyboard> (inside=$p_in total=$p_all, expected > $n)"
fi

# Idempotent. A second apply must replace the region, not stack another copy.
before="$(grep -c '<keybind key=' "$WORK/fresh.xml")"
g apply --rc "$WORK/fresh.xml" --no-reload >/dev/null 2>&1
after="$(grep -c '<keybind key=' "$WORK/fresh.xml")"
[ "$before" = "$after" ] \
    && ok "applying twice replaces rather than duplicates" \
    || bad "applying twice replaces rather than duplicates ($before then $after)"

# An override applied over an existing region replaces it.
g apply --rc "$WORK/fresh.xml" --overrides "$WORK/ov/keybinds.json" --no-reload >/dev/null 2>&1
grep -q 'key="W-S-p"' "$WORK/fresh.xml" && ! grep -q 'key="A-space"' "$WORK/fresh.xml" \
    && ok "re-applying with an override replaces the old shortcut" \
    || bad "re-applying with an override replaces the old shortcut"

# An rc.xml upgraded from APEX carries the region under the APEX markers
# (rime-rename: keep — the spelling on disk). `apply` must replace that region,
# not insert a second one beside it: the stale one's binds would all stay live,
# each calling a CLI name the image no longer ships.
cat > "$WORK/apex-era.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config>
  <keyboard>
    <keybind key="A-Tab"><action name="NextWindow"/></keybind>
    <!-- APEX-KEYBINDS-BEGIN — generated by apex-labwc-keybinds. Do not edit. -->
    <keybind key="W-F9"><action name="Execute" command="apex shell stale-verb"/></keybind>
    <!-- APEX-KEYBINDS-END -->
  </keyboard>
</labwc_config>
XML
g apply --rc "$WORK/apex-era.xml" --no-reload >/dev/null 2>&1
if grep -q 'RIME-KEYBINDS-BEGIN' "$WORK/apex-era.xml" \
   && [ "$(grep -c 'KEYBINDS-BEGIN' "$WORK/apex-era.xml")" = 1 ] \
   && ! grep -q 'stale-verb' "$WORK/apex-era.xml" && grep -q 'A-Tab' "$WORK/apex-era.xml"; then
    ok "an APEX-era region is replaced by one Rime region, the user's binds kept"
else
    bad "an APEX-era region is replaced by one Rime region, the user's binds kept"
fi
cat > "$WORK/apex-era-check.xml" <<'XML'
<?xml version="1.0"?>
<labwc_config><keyboard>
    <!-- APEX-KEYBINDS-BEGIN — generated by apex-labwc-keybinds. Do not edit. -->
    <!-- APEX-KEYBINDS-END -->
</keyboard></labwc_config>
XML
g check --rc "$WORK/apex-era-check.xml" > "$WORK/apex-era-check.out" 2>&1
grep -q 'has no Rime keybind region' "$WORK/apex-era-check.out" \
    && bad "check reads an APEX-era region as a region" \
    || ok "check reads an APEX-era region as a region"

section "refusals"

# A clean refusal and an unhandled traceback both exit non-zero, so exit status
# alone cannot tell them apart — and `tempfile.mkstemp` sits outside its try, so
# the traceback path is real rather than hypothetical. Every refusal below is
# checked for the absence of a traceback as well as a non-zero status.
refused() {  # refused <desc> <stderr-file> <status>
    if [ "$3" -ne 0 ] && ! grep -q "Traceback" "$2"; then
        ok "$1"
    elif grep -q "Traceback" "$2"; then
        bad "$1 (crashed instead of refusing)"
        head -3 "$2" | sed 's/^/        /'
    else
        bad "$1 (did not refuse)"
    fi
}

# An unparseable rc.xml is never rewritten. labwc already falls back to defaults
# silently on a broken config; overwriting it would destroy the user's own
# bindings along with whatever the real problem was.
printf '<labwc_config><keyboard></labwc_config>' > "$WORK/broken.xml"
cp "$WORK/broken.xml" "$WORK/broken.orig"
g apply --rc "$WORK/broken.xml" --no-reload >/dev/null 2>"$WORK/e1"
refused "an unparseable rc.xml is refused" "$WORK/e1" $?
cmp -s "$WORK/broken.xml" "$WORK/broken.orig" \
    && ok "the unparseable file is left untouched" \
    || bad "the unparseable file is left untouched"

# No </keyboard> means there is nowhere correct to put the block.
printf '<labwc_config><theme/></labwc_config>' > "$WORK/nokbd.xml"
cp "$WORK/nokbd.xml" "$WORK/nokbd.orig"
g apply --rc "$WORK/nokbd.xml" --no-reload >/dev/null 2>"$WORK/e2"
refused "a file with no <keyboard> is refused" "$WORK/e2" $?
cmp -s "$WORK/nokbd.xml" "$WORK/nokbd.orig" \
    && ok "that file is left untouched too" || bad "that file is left untouched too"

# A missing file is a skip, not a crash: a user who has never launched labwc has
# no rc.xml, and saving a keybind must not fail because of it.
g apply --rc "$WORK/does-not-exist.xml" --no-reload >/dev/null 2>"$WORK/e3"
refused "a missing rc.xml reports rather than crashing" "$WORK/e3" $?

section "the build-time check"

python3 "$CHECK" "$SHELL_TREE" "$RC" >/dev/null 2>&1 \
    && ok "the seeded rc.xml is exactly what the generator produces" \
    || { python3 "$CHECK" "$SHELL_TREE" "$RC" 2>&1 | head -20
         bad "the seeded rc.xml is exactly what the generator produces"; }

# The negative control. Without it, a check that silently passed on everything
# would look identical to a check that works.
cp "$RC" "$WORK/mutated.xml"
sed -i 's|<keybind key="A-space">|<keybind key="W-space">|' "$WORK/mutated.xml"
python3 "$CHECK" "$SHELL_TREE" "$WORK/mutated.xml" >/dev/null 2>&1 \
    && bad "the check fails on a drifted rc.xml" \
    || ok "the check fails on a drifted rc.xml"

# A file with no Rime region at all must fail rather than pass vacuously.
sed '/RIME-KEYBINDS-BEGIN/,/RIME-KEYBINDS-END/d' "$RC" > "$WORK/noregion.xml"
python3 "$CHECK" "$SHELL_TREE" "$WORK/noregion.xml" >/dev/null 2>&1 \
    && bad "the check fails when the region is missing entirely" \
    || ok "the check fails when the region is missing entirely"

section "the shell asks for it"

KS="${SHELL_TREE}/src/services/config_tab/KeybindService.qml"
if [ -f "$KS" ]; then
    grep -q 'rime-labwc-keybinds' "$KS" \
        && ok "KeybindService invokes the generator" \
        || bad "KeybindService invokes the generator"
    # The shell is a \$HOME checkout that updates independently of the image the
    # helper ships in, so a missing helper must not log a failed spawn on every
    # save.
    grep -q 'test -x /usr/libexec/rime-labwc-keybinds' "$KS" \
        && ok "it checks the helper exists before spawning it" \
        || bad "it checks the helper exists before spawning it"
else
    skp "no KeybindService.qml to check"
fi

printf '\npassed=%d failed=%d  (rime-shell model read from %s)\n' \
    "$pass" "$fail" "$SHELL_TREE_ID"
[ "$fail" -eq 0 ]
