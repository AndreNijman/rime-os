#!/usr/bin/env python3
"""Regenerate every Rime brand export (the Origin mark) from the masters in src/.

(The directory is still called ridge/: it held the layered ridge, Origin's predecessor, and every package,
module and test reads it by that path.)

Inputs (src/):
  rime-origin-symbol.svg        geometry master: five <circle>s c1..c5, all tangent at ONE point on c1 (the origin)
  rime-origin-symbol-small.svg  the same construction with wider gaps, for 24 px and below
  rime-wordmark.svg             "rime" as outlined paths (data-baseline, data-xheight)
  palette.json                  the brand colours and the six named accents

The circles are filled even-odd: c1 ink, c2 open, c3 ink, c4 open, c5 ink, which leaves three bodies:
coat (c1 - c2), ring (c3 - c4) and core (c5). Exports draw each body as its own <path>, so the boot splash can
fade them in one by one.

Outputs (under OUT, default: this directory): svg/ templates/ png/ icons/ favicon/ accents/.
Those six directories are deleted and rebuilt, so nothing stale survives.

Needs python3 (standard library only) and resvg on PATH. make-ridge-assets.sh documents the pinned
nix shell command; checks.x86_64-linux.branding reruns this and compares every file.
"""
import colorsys
import json
import os
import re
import shutil
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "src")
OUT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else HERE
GENERATED = ["svg", "templates", "png", "icons", "favicon", "accents"]

ICON_SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
SMALL_MAX = 24  # sizes at or below this use the small-size master
ACCENT_SIZES = [256, 512]
FAVICON_SIZES = [16, 32, 48]
# Lockup heights at 1x, in px (2x doubles them). Clear-space exports keep the same scale.
LOCKUP_1X = {"lockup-horizontal": 96, "lockup-stacked": 192, "wordmark": 96}

# Layout ratios (docs/branding.md, "Lockups"). The wordmark is Golos Text, 1000 units per em, x-height 530.
EM = 1000.0
XH_PER_SYMBOL = 0.53 / 0.92  # horizontal: the symbol is 0.92 em tall, so x-height / symbol height = 0.576
H_GAP_EM = 0.36              # horizontal: symbol to wordmark gap, in em
S_SYMBOL_PER_WORD = 0.62     # stacked: symbol width / wordmark width
S_GAP_PER_XH = 0.60          # stacked: symbol bottom to tittle top / x-height
CLEAR_PER_SYMBOL = 15 / 84   # clear space = the core's radius (15 of the 84-unit disc)
TILE_RADIUS = 0.17           # app icon corner radius / side
TILE_MARK_WIDTH = 0.62       # app icon mark diameter / side
TILE_MARK_CY = 0.5           # app icon mark centre / side
SQUARE_MARK_WIDTH = 15 / 16  # transparent square symbol: mark width / side
BODIES = (("coat", 0, 1), ("ring", 2, 3), ("core", 4, None))  # (id, filled circle, open circle)

PAL = json.load(open(os.path.join(SRC, "palette.json")))


def on_light(hexv):
    """The accent as drawn on a light ground: the same hue and saturation, lightness capped at Rime Blue Deep's,
    so a pale accent never washes out on white and a dark one is left alone."""
    r, g, b = (int(hexv[i:i + 2], 16) / 255 for i in (1, 3, 5))
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    dr, dg, db = (int(PAL["rime_blue_deep"][i:i + 2], 16) / 255 for i in (1, 3, 5))
    cap = colorsys.rgb_to_hls(dr, dg, db)[1]
    if l <= cap:
        return hexv.upper()
    rr, gg, bb = colorsys.hls_to_rgb(h, cap, s)
    return "#%02X%02X%02X" % tuple(round(v * 255) for v in (rr, gg, bb))


VARIANTS = {  # name: (ink, mark, wordmark)
    "light": (PAL["charcoal"], PAL["rime_blue_deep"], PAL["charcoal"]),
    "dark": (PAL["off_white"], PAL["rime_blue"], PAL["off_white"]),
    "mono-black": (PAL["mono_black"], PAL["mono_black"], PAL["mono_black"]),
    "mono-white": (PAL["mono_white"], PAL["mono_white"], PAL["mono_white"]),
}
TEMPLATE = ("{{fg}}", "{{accent}}", "{{fg}}")


def fmt(v):
    v = round(v, 3)
    return ("%.3f" % v).rstrip("0").rstrip(".") if v != int(v) else str(int(v))


def read(name):
    with open(os.path.join(SRC, name)) as f:
        return f.read()


def circles(name):
    text = read(name)
    found = re.findall(r'<circle id="c(\d)" cx="([^"]+)" cy="([^"]+)" r="([^"]+)"', text)
    if [n for n, *_ in found] != ["1", "2", "3", "4", "5"] or text.count("<circle") != 5:
        sys.exit(f"{name}: expected exactly five circles c1..c5 in order")
    return [tuple(float(v) for v in c) for _, *c in found]


def viewbox(text):
    return [float(v) for v in re.search(r'viewBox="([^"]+)"', text).group(1).split()]


def wordmark(name, metric):
    text = read(name)
    vb = viewbox(text)
    attr = lambda a: float(re.search(rf'data-{a}="([^"]+)"', text).group(1))  # noqa: E731
    return {"w": vb[2], "h": vb[3], "baseline": attr("baseline"), "unit": attr(metric),
            "paths": re.findall(r'<path d="([^"]+)"', text)}


SYMBOL = circles("rime-origin-symbol.svg")
SYMBOL_SMALL = circles("rime-origin-symbol-small.svg")
SW, SH = viewbox(read("rime-origin-symbol.svg"))[2:]
WORD = wordmark("rime-wordmark.svg", "xheight")


def circle_d(c):
    x, y, r = c
    return f"M{fmt(x - r)} {fmt(y)}a{fmt(r)} {fmt(r)} 0 1 0 {fmt(2 * r)} 0a{fmt(r)} {fmt(r)} 0 1 0 {fmt(-2 * r)} 0Z"


def body_d(cs, filled, hole):
    return circle_d(cs[filled]) + (circle_d(cs[hole]) if hole is not None else "")


def g(tx, ty, s, inner, fill=None):
    f = f' fill="{fill}"' if fill else ""
    return f'<g transform="translate({fmt(tx)} {fmt(ty)}) scale({fmt(s)})"{f}>{inner}</g>'


def symbol_svg(colours, small=False, tx=0.0, ty=0.0, s=1.0, only=None):
    """The three bodies, each its own even-odd path in the mark colour. `only` keeps one body (boot splash)."""
    cs = SYMBOL_SMALL if small else SYMBOL
    mark = colours[1]
    out = [f'<path id="{n}" fill="{mark}" fill-rule="evenodd" d="{body_d(cs, a, b)}"/>'
           for n, a, b in BODIES if only in (None, n)]
    return g(tx, ty, s, "".join(out))


def word_svg(word, colours, tx, ty, s):
    return g(tx, ty, s, "".join(f'<path d="{d}"/>' for d in word["paths"]), colours[2])


class Layout:
    """A composition in symbol units: a box (x0, y0, x1, y1), parts and the clear-space unit."""

    def __init__(self, parts, box, unit):
        self.parts, self.box, self.unit = parts, box, unit

    def svg(self, colours, title, small=False, pad=0.0, only=None):
        x0, y0, x1, y1 = self.box
        x0, y0, x1, y1 = x0 - pad, y0 - pad, x1 + pad, y1 + pad
        body = "".join(p(colours, small, only) for p in self.parts)
        w, h = x1 - x0, y1 - y0
        return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{fmt(x0)} {fmt(y0)} {fmt(w)} {fmt(h)}" '
                f'width="{fmt(w)}" height="{fmt(h)}">\n  <title>{title}</title>\n  {body}\n</svg>\n')

    def size(self, pad=0.0):
        x0, y0, x1, y1 = self.box
        return x1 - x0 + 2 * pad, y1 - y0 + 2 * pad


def sym_part(tx=0.0, ty=0.0, s=1.0):
    return lambda c, small, only: "" if only == "word" else symbol_svg(c, small, tx, ty, s, only)


def word_part(word, tx, ty, s):
    return lambda c, small, only: word_svg(word, c, tx, ty, s) if only in (None, "word") else ""


def layouts():
    clear = CLEAR_PER_SYMBOL * SH
    out = {"symbol": Layout([sym_part()], (0, 0, SW, SH), clear)}
    # horizontal: x-height = 0.576 symbol height (symbol = 0.92 em); tittle-top..baseline centred on the symbol
    k = XH_PER_SYMBOL * SH / WORD["unit"]
    tx, ty = SW + H_GAP_EM * EM * k, SH / 2 - WORD["baseline"] * k / 2
    out["lockup-horizontal"] = Layout([sym_part(), word_part(WORD, tx, ty, k)],
                                      (0, min(0, ty), tx + WORD["w"] * k, max(SH, ty + WORD["h"] * k)), clear)
    # stacked: symbol width = 0.62 wordmark width, centred over it
    k = SW / (S_SYMBOL_PER_WORD * WORD["w"])
    xh = WORD["unit"] * k
    tx, ty = (SW - WORD["w"] * k) / 2, SH + S_GAP_PER_XH * xh
    out["lockup-stacked"] = Layout([sym_part(), word_part(WORD, tx, ty, k)],
                                   (tx, 0, tx + WORD["w"] * k, ty + WORD["h"] * k), clear)
    # wordmark only, at the horizontal lockup's scale
    k = XH_PER_SYMBOL * SH / WORD["unit"]
    out["wordmark"] = Layout([word_part(WORD, 0, 0, k)], (0, 0, WORD["w"] * k, WORD["h"] * k), WORD["unit"] * k)
    return out


def square_symbol(colours, small, title, style=""):
    side = SW / SQUARE_MARK_WIDTH
    body = symbol_svg(colours, small, (side - SW) / 2, (side - SH) / 2)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {fmt(side)} {fmt(side)}" '
            f'width="{fmt(side)}" height="{fmt(side)}">\n  <title>{title}</title>\n{style}  {body}\n</svg>\n')


def app_icon(kind, small):
    side = 1024.0
    s = TILE_MARK_WIDTH * side / SW
    tx, ty = (side - SW * s) / 2, TILE_MARK_CY * side - SH * s / 2
    if kind == "dark":
        tile = f'<rect width="1024" height="1024" rx="{fmt(TILE_RADIUS * side)}" fill="{PAL["tile_dark"]}"/>'
        colours = VARIANTS["dark"]
    else:
        b = 4.0  # border drawn inside the tile, 8 units wide
        tile = (f'<rect x="{fmt(b)}" y="{fmt(b)}" width="{fmt(side - 2 * b)}" height="{fmt(side - 2 * b)}" '
                f'rx="{fmt(TILE_RADIUS * side - b)}" fill="{PAL["tile_light"]}" '
                f'stroke="{PAL["tile_light_border"]}" stroke-width="{fmt(2 * b)}"/>')
        colours = VARIANTS["light"]
    title = f"Rime app icon ({kind} tile{', small-size master' if small else ''})"
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">\n'
            f'  <title>{title}</title>\n  {tile}\n  {symbol_svg(colours, small, tx, ty, s)}\n</svg>\n')


def write(rel, text):
    path = os.path.join(OUT, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)
    return path


def render(svg_path, png_rel, width=None, height=None):
    png = os.path.join(OUT, png_rel)
    os.makedirs(os.path.dirname(png), exist_ok=True)
    size = ["-w", str(width)] if width else ["-h", str(height)]
    subprocess.run(["resvg", *size, svg_path, png], check=True)


def ico(pngs, dest):
    """A Windows .ico whose entries are PNG images (Vista and later, every browser)."""
    blobs = [open(p, "rb").read() for _, p in pngs]
    out = struct.pack("<HHH", 0, 1, len(pngs))
    offset = 6 + 16 * len(pngs)
    for (size, _), blob in zip(pngs, blobs):
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
    with open(dest, "wb") as f:
        f.write(out + b"".join(blobs))


def slot_colours():
    """The 24 per-owner accent slots (hue n*15 deg) at Rime Blue's lightness and saturation."""
    r, gg, b = (int(PAL["rime_blue"][i:i + 2], 16) / 255 for i in (1, 3, 5))
    h, l, s = colorsys.rgb_to_hls(r, gg, b)
    out = []
    for n in range(24):
        rr, g2, bb = colorsys.hls_to_rgb(n * 15 / 360, l, s)
        out.append("#%02X%02X%02X" % tuple(round(v * 255) for v in (rr, g2, bb)))
    return out, h * 360


def main():
    for d in GENERATED:
        shutil.rmtree(os.path.join(OUT, d), ignore_errors=True)
    L = layouts()
    names = {"symbol": "symbol", "lockup-horizontal": "horizontal lockup", "lockup-stacked": "stacked lockup",
             "wordmark": "wordmark"}

    # vector variants, clear-space variants and the adaptive templates
    for key, lay in L.items():
        for var, colours in VARIANTS.items():
            write(f"svg/rime-{key}-{var}.svg", lay.svg(colours, f"Rime {names[key]} ({var})"))
            write(f"svg/clearspace/rime-{key}-{var}-clearspace.svg",
                  lay.svg(colours, f"Rime {names[key]} ({var}, with clear-space)", pad=lay.unit))
        write(f"templates/rime-{key}.svg.in",
              lay.svg(TEMPLATE, f"Rime {names[key]} (adaptive template: the mark takes the accent token)"))
    for var, colours in VARIANTS.items():
        write(f"svg/rime-symbol-small-{var}.svg",
              L["symbol"].svg(colours, f"Rime symbol, small-size master ({var})", small=True))
        for small in (False, True):
            sfx = "-small" if small else ""
            write(f"svg/square/rime-symbol-square{sfx}-{var}.svg",
                  square_symbol(colours, small, f"Rime symbol, square canvas ({var})"))
    write("templates/rime-symbol-small.svg.in",
          L["symbol"].svg(TEMPLATE, "Rime symbol, small-size master (adaptive template)", small=True))
    write("templates/rime-symbol-square.svg.in",
          square_symbol(TEMPLATE, False, "Rime symbol, square canvas (adaptive template)"))
    for kind in ("dark", "light"):
        write(f"svg/rime-app-icon-{kind}.svg", app_icon(kind, False))
        write(f"svg/rime-app-icon-{kind}-small.svg", app_icon(kind, True))

    # PNG ladders: transparent square symbols and the app icon tiles
    for var in VARIANTS:
        for n in ICON_SIZES:
            sfx = "-small" if n <= SMALL_MAX else ""
            render(os.path.join(OUT, f"svg/square/rime-symbol-square{sfx}-{var}.svg"),
                   f"png/symbol/rime-symbol-{var}-{n}.png", width=n)
    for kind in ("dark", "light"):
        for n in ICON_SIZES:
            sfx = "-small" if n <= SMALL_MAX else ""
            render(os.path.join(OUT, f"svg/rime-app-icon-{kind}{sfx}.svg"),
                   f"icons/app/rime-app-icon-{kind}-{n}.png", width=n)

    # lockups at 1x and 2x, with and without clear-space (same scale)
    for key, h1 in LOCKUP_1X.items():
        lay = L[key]
        _, h = lay.size()
        _, hp = lay.size(lay.unit)
        for var in VARIANTS:
            for scale in (1, 2):
                render(os.path.join(OUT, f"svg/rime-{key}-{var}.svg"),
                       f"png/lockups/rime-{key}-{var}@{scale}x.png", height=h1 * scale)
                render(os.path.join(OUT, f"svg/clearspace/rime-{key}-{var}-clearspace.svg"),
                       f"png/lockups/clearspace/rime-{key}-{var}-clearspace@{scale}x.png",
                       height=round(h1 * scale * hp / h))

    # favicons: the light symbol, transparent; the SVG follows the browser's colour scheme
    fav = []
    for n in FAVICON_SIZES:
        render(os.path.join(OUT, "svg/square/rime-symbol-square-small-light.svg" if n <= SMALL_MAX
                            else "svg/square/rime-symbol-square-light.svg"), f"favicon/favicon-{n}.png", width=n)
        fav.append((n, os.path.join(OUT, f"favicon/favicon-{n}.png")))
    ico(fav, os.path.join(OUT, "favicon/favicon.ico"))
    style = ("  <style>path{fill:%s}@media (prefers-color-scheme: dark){path{fill:%s}}</style>\n"
             % (PAL["rime_blue_deep"], PAL["rime_blue"]))
    write("favicon/favicon.svg", square_symbol(VARIANTS["light"], True, "Rime", style))

    # accents: the six named ones and the 24 per-owner slots. Only the mark changes colour; on a light ground
    # it is drawn at on_light() so a pale accent keeps its contrast.
    slots, base_hue = slot_colours()
    named = PAL["accents"]
    jobs = [(f"accents/named/rime-symbol-{a}", hexv) for a, hexv in named.items()]
    jobs += [(f"accents/slots/rime-symbol-slot-{n:02d}", hexv) for n, hexv in enumerate(slots)]
    for stem, hexv in jobs:
        for mode in ("light", "dark"):
            mark = on_light(hexv) if mode == "light" else hexv
            colours = (VARIANTS[mode][0], mark, VARIANTS[mode][2])
            path = write(f"{stem}-{mode}.svg", square_symbol(colours, False, f"Rime symbol, accent {mark} ({mode})"))
            for n in ACCENT_SIZES:
                render(path, f"{stem}-{mode}-{n}.png", width=n)
    write("accents/accents.json", json.dumps({
        "_comment": "Mark colours. Slot n = hue n*15 deg at Rime Blue's HLS lightness and saturation; "
                    "the same 24 buckets rime-plymouth-accent picks from the owner's matugen accent. "
                    "slots_on_light: the same slot drawn on a light ground (lightness capped at Rime Blue Deep's).",
        "base": PAL["rime_blue"], "base_on_light": PAL["rime_blue_deep"],
        "base_hue_deg": round(base_hue, 2), "base_slot": round(base_hue / 15) % 24,
        "named": named, "slots": {f"{n:02d}": hexv for n, hexv in enumerate(slots)},
        "slots_on_light": {f"{n:02d}": on_light(hexv) for n, hexv in enumerate(slots)},
    }, indent=2) + "\n")


if __name__ == "__main__":
    main()
