#!/usr/bin/env python3
"""Outline the wordmark into src/*.svg so no font is needed at runtime.

Run once when the typeface or the tracking changes (make-ridge-assets.sh never needs the fonts):

  nix shell --impure --expr 'let p = (builtins.getFlake "github:NixOS/nixpkgs/<rev in flake.lock>")
    .legacyPackages.x86_64-linux; in p.python3.withPackages (q: [ q.fonttools ])' \
    --command python3 outline-wordmarks.py <fonts dir>...

where the <fonts dir>s hold the static or variable TTFs named in WORDMARKS (nixpkgs google-fonts with
fonts = [ ... ], see FONTS.md). Coordinates are font units with the y axis flipped; y = 0 is the top of the ink.
"""
import os
import sys

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = sys.argv[1:] or ["."]

# text, font file, variation axes, tracking (em, between letters), extra word space (em), metric, output
WORDMARKS = [
    ("rime", "GolosText[wght].ttf", {"wght": 600}, -0.025, 0.0, "xheight", "rime-wordmark.svg"),
]


def outline(text, path, axes, track, word, metric, out):
    font = TTFont(next(os.path.join(d, path) for d in FONTS if os.path.exists(os.path.join(d, path))))
    if "fvar" in font:
        font = instancer.instantiateVariableFont(font, axes)
    upm = font["head"].unitsPerEm
    gs, cmap, hmtx = font.getGlyphSet(), font.getBestCmap(), font["hmtx"]
    x, placed = 0.0, []
    for ch in text:
        name = cmap[ord(ch)]
        if ch != " ":
            placed.append((name, x))
        x += hmtx[name][0] + track * upm + (word * upm if ch == " " else 0)
    bp = BoundsPen(gs)
    for name, dx in placed:
        gs[name].draw(TransformPen(bp, (1, 0, 0, 1, dx, 0)))
    x0, y0, x1, y1 = bp.bounds
    paths = []
    for name, dx in placed:
        sp = SVGPathPen(gs, ntos=lambda v: ("%.1f" % v).rstrip("0").rstrip("."))
        gs[name].draw(TransformPen(sp, (1, 0, 0, -1, dx - x0, y1)))
        paths.append(sp.getCommands())
    os2 = font["OS/2"]
    unit = os2.sxHeight if metric == "xheight" else os2.sCapHeight
    family = path.split("[")[0].split(".")[0]
    fmt = lambda v: ("%.1f" % v).rstrip("0").rstrip(".")  # noqa: E731
    body = "\n".join(f'    <path d="{d}"/>' for d in paths)
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {fmt(x1 - x0)} {fmt(y1 - y0)}" '
           f'width="{fmt(x1 - x0)}" height="{fmt(y1 - y0)}" data-baseline="{fmt(y1)}" data-{metric}="{unit}">\n'
           f'  <title>{text}: {family} {axes}, tracking {track} em, outlined (no font needed)</title>\n'
           f'  <g fill="#212224">\n{body}\n  </g>\n</svg>\n')
    with open(os.path.join(HERE, out), "w") as f:
        f.write(svg)
    print(out, family, axes, "upm", upm, metric, unit, "ink", round(x1 - x0), "x", round(y1 - y0))


for spec in WORDMARKS:
    outline(*spec)
