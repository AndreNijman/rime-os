#!/usr/bin/env bash
# The compositor's motion classes, and Rime Shell's surfaces left to the shell.
#
#     tests/test-rime-hypr-motion.sh
#
# files/desktop/hypr/rime/appearance.lua gives Hyprland one motion class per
# kind of change (Rime Shell UI/UX roadmap v3 Phase 20): windows in, windows
# out, moves, fades, layers, workspaces, borders — on the shell's own curves, so
# a window opening and a panel opening read as one system. And it exempts the
# shell's layer surfaces from Hyprland's layer animations, because the shell
# draws every one of their motions itself.
#
# That exemption is the part worth measuring, and it was measured before it was
# written: in a nested Hyprland 0.56.2 on the stock tree, the network panel
# poured out of the bar TRANSLUCENT — 66-90 % of its footprint a blend of panel
# and wallpaper from 240 to 345 ms into the open, snapping opaque at 400 ms,
# which is fadeLayersIn (inherited from `fade`, 400 ms) running on top of the
# shell's own pour. With the rule, the only blend left is the shell's content
# fade at its edges (at most 7 %). That probe needs a GPU, a nested compositor
# and the shell, so it is not this suite; this suite pins what it found.
#
# Two halves:
#
#   STATIC (always, python3 only) — the curves are the shell's tokens, the
#     springs are the shell's kind of spring (a whisper of overshoot at most)
#     and every one is shared with the shell through RIME_SPRINGS, every class
#     exists, closing is shorter than opening and stays on a curve, a window's
#     fade is quicker than its growth, and the exemption names the shell's
#     namespace with no_anim.
#   VERIFY (where Hyprland is installed) — `Hyprland --verify-config` accepts
#     the file. It rejects an unknown leaf, an unknown style, an unknown
#     layer-rule key and a spring spelt with the wiki's `damping` (this Hyprland
#     wants `dampening`), so "config ok" means the names are real in THIS
#     Hyprland.
#
# Each half is mutated to prove it can fail. The verify half skips with status 0
# where Hyprland is missing (CI); the static half runs everywhere.
set -uo pipefail
# grep -q exits at its first match; under pipefail a producer that is still
# writing then fails (EPIPE, or 141 from SIGPIPE) and so does the pipeline,
# at random. pipe_has reads its input to the end.
pipe_has() { grep "$@" >/dev/null; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
F="$root/files/desktop/hypr/rime/appearance.lua"

pass=0; fail=0; skipped=0
ok()   { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }
skip() { printf 'SKIP  %s\n' "$1"; skipped=$((skipped + 1)); }
sec()  { printf '\n── %s ──\n' "$1"; }
finish() {
    printf '\nrime hypr-motion: %d passed, %d failed, %d skipped\n' "$pass" "$fail" "$skipped"
    [ "$fail" -eq 0 ]
}

[ -f "$F" ] || { bad "appearance.lua is present"; finish; exit $?; }

# ── STATIC ───────────────────────────────────────────────────────────────────
# One verdict line per rule: "<RULE> PASS|FAIL <detail>".
static_verdicts() {
    python3 - "$1" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
code = "\n".join(l.split("--", 1)[0] for l in src.split("\n"))

def verdict(rule, good, detail=""):
    print(rule, "PASS" if good else "FAIL", detail)

# The shell's tokens (rime-shell src/theme/motion.js CURVES), as control points,
# and the one curve that is Material 3's own rather than the shell's.
TOKENS = {
    "rimeSpring":      [(0.25, 0.2), (0.15, 1.0)],   # spring (critically damped, fitted)
    "rimeAccel":       [(0.4, 0.0), (0.65, 1.0)],    # standardAccel
    "rimeStandard":    [(0.25, 0.1), (0.25, 1.0)],   # standard
    "rimeEffects":     [(0.25, 0.1), (0.25, 1.0)],   # effects
    "emphasizedAccel": [(0.4, 0.0), (0.75, 0.9)],    # emphasizedAccel
    "exitFade":        [(0.3, 0.0), (0.8, 0.15)],    # Material 3 emphasized-accelerate
}
curves = {}
for m in re.finditer(r'hl\.curve\(\s*"(\w+)"\s*,\s*\{[^}]*points\s*=\s*\{\s*\{\s*([-\d.]+)\s*,\s*([-\d.]+)\s*\}\s*,\s*\{\s*([-\d.]+)\s*,\s*([-\d.]+)\s*\}', code):
    curves[m.group(1)] = [(float(m.group(2)), float(m.group(3))), (float(m.group(4)), float(m.group(5)))]
off = [n for n, p in TOKENS.items() if curves.get(n) != p]
verdict("CURVES", not off, "drifted or missing: " + ",".join(off))

# The springs, as the file writes them: spring("name", response, damping).
# motion.js SPRINGS' rule is "a whisper of overshoot at most" (damping 0.8-1)
# and nothing past the hero beat (640 ms). Two are the shell's own roles.
springs = {m.group(1): (float(m.group(2)), float(m.group(3)))
           for m in re.finditer(r'^\s*spring\(\s*"(\w+)"\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*\)', code, re.M)}
SHELL = {"rimeGlide": ("selection", 0.40, 0.86), "rimePage": ("page", 0.38, None)}
bad_s = [n for n in ("rimeArrive", "rimeGlide", "rimePage") if n not in springs]
bad_s += [f"{n}={r}/{z}" for n, (r, z) in springs.items() if not (0.8 <= z <= 1.0 and 0 < r <= 0.64)]
for n, (role, r, z) in SHELL.items():
    if n in springs and (abs(springs[n][0] - r) > 1e-9 or (z is not None and abs(springs[n][1] - z) > 1e-9)):
        bad_s.append(f"{n} is not the shell's {role}")
verdict("SPRINGS", not bad_s, "; ".join(bad_s))

# Shared with the shell: the helper records each spring in the global
# RIME_SPRINGS with exactly the numbers it declares, and no spring is declared
# around it. The shell scales a spring from that table; one missing from it
# would stay at its balanced speed while everything else followed the setting.
helper = re.search(r'local function spring\(\s*name\s*,\s*response\s*,\s*damping\s*\)(.*?)\nend', code, re.S)
body = helper.group(1) if helper else ""
direct = re.findall(r'hl\.curve\([^)]*type\s*=\s*"spring"', code.replace(body, "") if body else code)
shared = bool(re.search(r'^RIME_SPRINGS\s*=\s*\{\s*\}', code, re.M)
              and "RIME_SPRINGS[name] = s" in body
              and re.search(r'hl\.curve\(\s*name\s*,\s*\{\s*type\s*=\s*"spring"\s*,\s*mass\s*=\s*s\.mass\s*,\s*stiffness\s*=\s*s\.stiffness\s*,\s*dampening\s*=\s*s\.dampening\s*\}\s*\)', body)
              and re.search(r'stiffness\s*=\s*\(\s*2\s*\*\s*math\.pi\s*/\s*response\s*\)\s*\^\s*2', body)
              and re.search(r'dampening\s*=\s*2\s*\*\s*damping\s*\*\s*math\.sqrt\(\s*stiffness\s*\)', body))
verdict("SHARED", shared and not direct,
        ("the helper does not record and declare the same numbers" if not shared else "")
        + (f" {len(direct)} spring(s) declared outside the helper" if direct else ""))

anims = {}
for m in re.finditer(r'hl\.animation\(\s*\{([^}]*)\}\s*\)', code):
    body_a = m.group(1)
    leaf = re.search(r'leaf\s*=\s*"(\w+)"', body_a)
    if not leaf: continue
    speed = re.search(r'speed\s*=\s*([\d.]+)', body_a)
    bez = re.search(r'bezier\s*=\s*"(\w+)"', body_a)
    spr = re.search(r'spring\s*=\s*"(\w+)"', body_a)
    on = re.search(r'enabled\s*=\s*true', body_a)
    anims[leaf.group(1)] = dict(speed=float(speed.group(1)) if speed else None,
                                curve=(bez or spr).group(1) if (bez or spr) else None,
                                kind="bezier" if bez else "spring" if spr else None, on=bool(on))
need = ["windowsIn", "windowsOut", "windowsMove", "fadeIn", "fadeOut",
        "workspaces", "layersIn", "layersOut", "border"]
missing = [l for l in need if l not in anims or not anims[l]["on"] or anims[l]["speed"] is None
           or anims[l]["curve"] is None]
dangling = [f"{l}->{a['curve']}" for l, a in anims.items()
            if a["kind"] == "spring" and a["curve"] not in springs]
verdict("CLASSES", not missing and not dangling,
        "missing or disabled: " + ",".join(missing) + (" unknown spring: " + ",".join(dangling) if dangling else ""))

def spd(l): return anims.get(l, {}).get("speed") or 0
verdict("CLOSE_SHORTER",
        0 < spd("windowsOut") < spd("windowsIn") and 0 < spd("layersOut") < spd("layersIn"),
        f"windows {spd('windowsIn')}/{spd('windowsOut')}, layers {spd('layersIn')}/{spd('layersOut')}")

# Arriving, moving and a switch are springs; leaving stays on curves, because a
# spring ends only at rest and a closing window lives until its animation ends.
a = lambda l: (anims.get(l, {}).get("kind"), anims.get(l, {}).get("curve"))
verdict("CHARACTER",
        a("windowsIn") == ("spring", "rimeArrive") and a("windowsMove") == ("spring", "rimeGlide")
        and a("workspaces") == ("spring", "rimePage")
        and a("windowsOut") == ("bezier", "emphasizedAccel") and a("fadeOut") == ("bezier", "exitFade"),
        f"in={a('windowsIn')} move={a('windowsMove')} ws={a('workspaces')} out={a('windowsOut')} fadeOut={a('fadeOut')}")

# A window is opaque well before it has grown, so the growth is what is seen.
# (A spring leaf's speed is its response, so the two compare in one unit.)
verdict("SIZE_LEADS", 0 < spd("fadeIn") <= spd("windowsIn") / 2,
        f"fadeIn {spd('fadeIn')} vs windowsIn {spd('windowsIn')}")

rules = [m.group(1) for m in re.finditer(r'hl\.layer_rule\(\s*\{(.*?)\}\s*\)', code, re.S)]
exempt = [r for r in rules
          if re.search(r'namespace\s*=\s*"\^quickshell\$"', r) and re.search(r'no_anim\s*=\s*true', r)]
verdict("EXEMPT", len(exempt) == 1, f"{len(exempt)} rule(s) exempt ^quickshell$")
PY
}

label() {
    case "$1" in
        CURVES)        echo "the curves are Rime Shell's tokens (exitFade: Material 3's emphasized-accelerate)" ;;
        SPRINGS)       echo "the springs are the shell's kind (damping 0.8-1, response <= 640 ms); glide and page are its selection and page" ;;
        SHARED)        echo "every spring goes through the helper that shares it with the shell (RIME_SPRINGS, same numbers)" ;;
        CLASSES)       echo "every motion class is declared and enabled, and every spring it names exists" ;;
        CLOSE_SHORTER) echo "closing is shorter than opening, for windows and for layers" ;;
        CHARACTER)     echo "opening, moving and a workspace switch are springs; closing stays on its curves" ;;
        SIZE_LEADS)    echo "a window's fade-in is at most half its growth, so the growth is what is seen" ;;
        EXEMPT)        echo "exactly one layer rule leaves Rime Shell's surfaces (^quickshell\$) unanimated" ;;
    esac
}

sec "static: the motion classes"
verdicts="$(static_verdicts "$F")"
while read -r rule verdict detail; do
    [ -n "$rule" ] || continue
    if [ "$verdict" = PASS ]; then ok "$(label "$rule")"; else bad "$(label "$rule") — $detail"; fi
done <<<"$verdicts"
if [ "$(grep -c . <<<"$verdicts")" -eq 8 ]; then ok "all eight static rules were evaluated"
else bad "expected five static verdicts, got: $verdicts"; fi

MW="$(mktemp -d)"; trap 'rm -rf "$MW"' EXIT INT TERM
mutant() {   # mutant <label> <python old> <new> <rule that must FAIL>
    cp "$F" "$MW/a.lua"
    if ! python3 - "$MW/a.lua" "$2" "$3" <<'PY'
import sys
p, old, new = sys.argv[1:4]
s = open(p).read()
if old not in s: sys.exit(3)
open(p, "w").write(s.replace(old, new, 1))
PY
    then bad "self-test $1: the mutation did not apply"; return; fi
    if static_verdicts "$MW/a.lua" | pipe_has "^$4 FAIL"; then ok "self-test $1: caught"
    else bad "self-test $1: SURVIVED"; fi
}
mutant "a curve drifting from its token" '{ { 0.25, 0.2 }, { 0.15, 1.0 } }' '{ { 0.25, 0.2 }, { 0.2, 1.0 } }' CURVES
mutant "a spring that bounces" 'spring("rimeArrive", 0.48, 0.80)' 'spring("rimeArrive", 0.48, 0.55)' SPRINGS
mutant "the glide no longer the shell's selection" 'spring("rimeGlide",  0.40, 0.86)' 'spring("rimeGlide",  0.44, 0.86)' SPRINGS
mutant "a spring the shell cannot read" '    RIME_SPRINGS[name] = s' '    local _ = s' SHARED
mutant "a spring declared around the helper" 'spring("rimePage",   0.38, 0.92)' 'spring("rimePage",   0.38, 0.92)
hl.curve("rimePage", { type = "spring", mass = 1, stiffness = 273, dampening = 30 })' SHARED
mutant "the move class dropped" 'hl.animation({ leaf = "windowsMove"' 'hl.animation({ leaf = "windowsMoveX"' CLASSES
mutant "a leaf on a spring nobody declared" 'spring = "rimeGlide" }' 'spring = "rimeGlyde" }' CLASSES
mutant "a close as long as the open" 'leaf = "windowsOut",  enabled = true, speed = 2.0' 'leaf = "windowsOut",  enabled = true, speed = 4.8' CLOSE_SHORTER
mutant "a close on a spring" 'speed = 2.0, bezier = "emphasizedAccel", style' 'speed = 2.0, spring = "rimeArrive", style' CHARACTER
mutant "an open back on a curve" 'speed = 4.8, spring = "rimeArrive",      style' 'speed = 4.8, bezier = "rimeSpring",      style' CHARACTER
mutant "a fade as long as the growth" 'leaf = "fadeIn",     enabled = true, speed = 1.8' 'leaf = "fadeIn",     enabled = true, speed = 4.8' SIZE_LEADS
mutant "the exemption animating again" 'no_anim = true' 'no_anim = false' EXEMPT

# ── VERIFY ───────────────────────────────────────────────────────────────────
sec "verify: Hyprland accepts it"
if ! command -v Hyprland >/dev/null 2>&1; then
    skip "Hyprland is not installed; the names cannot be checked against a real parser here"
    finish; exit $?
fi

verify() {   # verify <appearance.lua> — prints Hyprland's parsing result
    local H RT
    H="$(mktemp -d)"; RT="$(mktemp -d)"; chmod 0700 "$RT"
    mkdir -p "$H/.config/hypr/rime"
    cp "$1" "$H/.config/hypr/rime/appearance.lua"
    printf 'require("rime.appearance")\n' > "$H/.config/hypr/hyprland.lua"
    env -i HOME="$H" PATH=/usr/bin:/bin XDG_RUNTIME_DIR="$RT" \
        timeout 60 Hyprland --verify-config -c "$H/.config/hypr/hyprland.lua" 2>&1 \
        | sed -n '/Config parsing result/,$p'
    rm -rf "$H" "$RT"
}

got="$(verify "$F")"
if grep -qx 'config ok' <<<"$got"; then
    ok "Hyprland --verify-config: config ok"
else
    bad "Hyprland --verify-config: config ok — got: $(grep -v '^$\|====' <<<"$got" | head -3)"
fi

vmutant() {   # vmutant <label> <old> <new> — verify must REJECT it
    cp "$F" "$MW/v.lua"
    python3 - "$MW/v.lua" "$2" "$3" <<'PY' || { bad "self-test $1: the mutation did not apply"; return; }
import sys
p, old, new = sys.argv[1:4]
s = open(p).read()
if old not in s: sys.exit(3)
open(p, "w").write(s.replace(old, new, 1))
PY
    if grep -qx 'config ok' <<<"$(verify "$MW/v.lua")"; then bad "self-test $1: ACCEPTED"
    else ok "self-test $1: rejected"; fi
}
vmutant "an animation leaf this Hyprland does not have" 'leaf = "windowsIn"' 'leaf = "windowsInn"'
vmutant "a style this Hyprland does not have" 'style = "popin 80%"' 'style = "wobble"'
vmutant "a layer-rule key this Hyprland does not have" 'namespace = "^quickshell$"' 'namespacex = "^quickshell$"'
vmutant "a spring spelt the wiki's way (damping)" 'dampening = s.dampening })' 'damping = s.dampening })'

finish
