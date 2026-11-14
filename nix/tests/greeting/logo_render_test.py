"""The recoloured logo, end to end: wallpaper -> Rime Shell's matugen config -> rime-logo-render -> PNGs.

    python3 logo_render_test.py MATUGEN LOGO_RENDER RESVG TEMPLATE SHELL_DIR [--expect-fail]

SHELL_DIR is the BUILT rime-shell tree (share/rime-shell), so the config is the one the Shell renders at runtime,
after rime-shell.nix pointed its post_hook at the profile (/run/current-system/sw/libexec/rime-logo-render). That
one path is swapped for LOGO_RENDER, which is the only difference from a login. matugen runs with the Shell's own
arguments (WallpaperService.qml), twice, on two different wallpapers.

The colour is checked against a mask: TEMPLATE rendered with pure red for {{accent}} and pure blue for {{fg}}.
The Origin mark is one colour, so the mask must be all red (no ink), and every opaque red pixel must be the
wallpaper's accent in the recoloured PNG; so a swapped colour or a stale PNG fails, whatever the mark's geometry.
"""
import os
import re
import shutil
import subprocess
import sys
import tempfile

from PIL import Image

args = [a for a in sys.argv[1:] if not a.startswith("--")]
MATUGEN, RENDER, RESVG, TEMPLATE, SHELL_DIR = args
EXPECT_FAIL = "--expect-fail" in sys.argv[1:]
PROFILE_HOOK = "/run/current-system/sw/libexec/rime-logo-render"
RIME_BLUE = (0x91, 0xC2, 0xDC)

failures = []
count = 0


def expect(name, cond, detail=""):
    global count
    count += 1
    print(f"{'ok  ' if cond else 'FAIL'} {name}" + ("" if cond else f" -- {detail}"))
    if not cond:
        failures.append(name)
    return cond


def hexrgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


def wallpaper(path, a, b):
    img = Image.new("RGB", (192, 128))
    px = img.load()
    for y in range(128):
        for x in range(192):
            t = x / 191
            px[x, y] = tuple(int(a[i] + (b[i] - a[i]) * t) for i in range(3))
    img.save(path)


def read_colors(path):
    out = {}
    for line in open(path):
        k, _, v = line.strip().partition("=")
        out[k] = v
    return out


def mark_check(label, png, mask, accent, fg):
    img = Image.open(png).convert("RGBA")
    if not expect(f"{label}: 512 px wide, same size as the mask", img.size == mask.size, (img.size, mask.size)):
        return
    acc, fgc = hexrgb(accent), hexrgb(fg)
    bad_a = bad_f = bad_t = n_a = n_f = 0
    p, m = img.load(), mask.load()
    for y in range(mask.size[1]):
        for x in range(mask.size[0]):
            mr, mg, mb, ma = m[x, y]
            r, g, b, a = p[x, y]
            if ma == 0:
                bad_t += a != 0
            elif ma == 255 and (mr, mg, mb) == (255, 0, 0):
                n_a += 1
                bad_a += a != 255 or max(abs(r - acc[0]), abs(g - acc[1]), abs(b - acc[2])) > 1
            elif ma == 255 and (mr, mg, mb) == (0, 0, 255):
                n_f += 1
                bad_f += a != 255 or max(abs(r - fgc[0]), abs(g - fgc[1]), abs(b - fgc[2])) > 1
    expect(f"{label}: the mark ({n_a} px) is the accent {accent}", n_a > 5000 and bad_a == 0, f"{bad_a} differ")
    expect(f"{label}: one colour, no ink in the mark ({n_f} px of {fg})", n_f == 0, f"{n_f} px take the ink")
    expect(f"{label}: transparent where the symbol is not", bad_t == 0, f"{bad_t} opaque")


def main():
    tmp = tempfile.mkdtemp()
    try:
        return run(tmp)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def run(tmp):
    home = f"{tmp}/home"
    os.makedirs(home)
    logo = f"{home}/.cache/rime/logo"

    # The mask: the same template, sentinel colours, the same renderer and width as rime-logo-render.
    svg = open(TEMPLATE).read().replace("{{accent}}", "#ff0000").replace("{{fg}}", "#0000ff")
    open(f"{tmp}/mask.svg", "w").write(svg)
    subprocess.run([RESVG, "-w", "512", f"{tmp}/mask.svg", f"{tmp}/mask.png"], check=True)
    mask = Image.open(f"{tmp}/mask.png").convert("RGBA")

    # The Shell's config, exactly as WallpaperService renders it, hook path swapped for the build under test.
    tmpl = open(f"{SHELL_DIR}/src/config/matugen.toml.in").read()
    expect("the built Shell's hook names the profile's rime-logo-render", PROFILE_HOOK in tmpl,
           "rime-shell.nix should have rewritten /usr/libexec/")
    cfg = tmpl.replace("@SRCDIR@", SHELL_DIR).replace("@HOME@", home).replace(PROFILE_HOOK, RENDER)
    os.makedirs(f"{home}/.config/rime-shell")
    cfg_path = f"{home}/.config/rime-shell/matugen.toml"
    open(cfg_path, "w").write(cfg)
    outputs = re.findall(r"^output_path *= *'(.*)'", cfg, re.M)
    expect("rethemeIfStale's output list includes the logo colours (so existing users get the logo)",
           f"{logo}/colors" in outputs, outputs)
    expect("no output under the read-only Shell tree", not any(o.startswith(SHELL_DIR) for o in outputs), outputs)

    env = {"HOME": home, "PATH": "/usr/bin:/bin:" + os.environ.get("PATH", "")}

    def matugen(wall, mode):
        return subprocess.run([MATUGEN, "image", wall, "-c", cfg_path, "--source-color-index", "0",
                               "--type", "scheme-tonal-spot", "-m", mode], env=env, cwd=tmp,
                              capture_output=True, text=True, timeout=120)

    seen = {}
    for name, a, b, mode in (("warm", (205, 60, 30), (240, 140, 50), "dark"),
                             ("green", (20, 120, 60), (90, 190, 110), "light")):
        wall = f"{tmp}/{name}.png"
        wallpaper(wall, a, b)
        r = matugen(wall, mode)
        if not expect(f"[{name}] matugen with the Shell's config exits 0", r.returncode == 0,
                      r.stdout[-800:] + r.stderr[-800:]):
            continue
        if not expect(f"[{name}] colours written", os.path.exists(f"{logo}/colors")):
            continue
        c = read_colors(f"{logo}/colors")
        expect(f"[{name}] four #RRGGBB colours", sorted(c) == ["accent_dark", "accent_light", "fg_dark", "fg_light"]
               and all(re.fullmatch(r"#[0-9a-fA-F]{6}", v) for v in c.values()), c)
        expect(f"[{name}] accent is the wallpaper's, not Rime Blue",
               max(abs(x - y) for x, y in zip(hexrgb(c["accent_dark"]), RIME_BLUE)) > 8, c)
        for variant in ("dark", "light"):
            png = f"{logo}/rime-mark-{variant}.png"
            if expect(f"[{name}] rime-mark-{variant}.png rendered by the hook", os.path.exists(png)):
                mark_check(f"[{name}] {variant}", png, mask, c[f"accent_{variant}"], c[f"fg_{variant}"])
            expect(f"[{name}] rime-mark-{variant}.svg beside it",
                   "{{" not in open(f"{logo}/rime-mark-{variant}.svg").read()
                   if os.path.exists(f"{logo}/rime-mark-{variant}.svg") else False)
        leftovers = [f for f in os.listdir(logo) if f.startswith(".")]
        expect(f"[{name}] no temporary files left", not leftovers, leftovers)
        seen[name] = c["accent_dark"]
    expect("a new wallpaper recolours the logo", len(seen) == 2 and seen["warm"] != seen["green"], seen)

    # rime-logo-render refuses a bad colours file and leaves the last good logo in place.
    before = open(f"{logo}/rime-mark-dark.png", "rb").read() if os.path.exists(f"{logo}/rime-mark-dark.png") \
        else None
    for why, text in (("a sed expression", "accent_dark=#00ff00/g;s/x\nfg_dark=#000000\naccent_light=#000000\n"
                       "fg_light=#000000\n"),
                      ("a missing key", "accent_dark=#00ff00\nfg_dark=#000000\n"),
                      ("a truncated value", "accent_dark=#00ff0\nfg_dark=#000000\naccent_light=#000000\n"
                       "fg_light=#000000\n")):
        open(f"{logo}/colors", "w").write(text)
        r = subprocess.run([RENDER, logo], env=env, capture_output=True, text=True)
        after = open(f"{logo}/rime-mark-dark.png", "rb").read() if before is not None else None
        expect(f"rime-logo-render refuses {why}, old logo kept", r.returncode != 0 and after == before,
               (r.returncode, r.stderr))
    leftovers = [f for f in os.listdir(logo) if f.startswith(".")]
    expect("no temporary files left after a refusal", not leftovers, leftovers)

    print(f"{count} checks, {len(failures)} failed" + (f": {failures}" if failures else ""))
    return 1 if failures else 0


if __name__ == "__main__":
    rc = main()
    if EXPECT_FAIL:
        print("mutant " + ("caught" if rc else "NOT caught"))
        sys.exit(0 if rc else 1)
    sys.exit(rc)
