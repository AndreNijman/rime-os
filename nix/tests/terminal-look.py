"""Measure two terminal screenshots of the same pattern (nix/tests/terminal-look.nix): Alacritty and Ghostty.

Usage: terminal-look.py ALACRITTY.png GHOSTTY.png OUTDIR

The pattern (terminal-look.nix) prints text, then one row of 16 background blocks of 4 cells each
(palette 0..15), on row PALETTE_ROW. The same colours also colour text above it, so a block is found as the
pixel rows holding a long horizontal run (MIN_RUN pixels: longer than any glyph stroke) of exactly its colour.
That bounding box gives the cell grid: width / 4 = cell width, height = cell height, and the block's left
and top edges minus whole cells = the padding. Writes OUTDIR/report.json, OUTDIR/diff.png (per-pixel difference,
amplified) and OUTDIR/side-by-side.png. Asserts nothing: the test decides what must match.
"""

import json
import os
import re
import sys

from PIL import Image, ImageChops

PALETTE_ROW = 5  # 0-based terminal row of the block line (see the pattern)
BLOCK_CELLS = 4
MIN_RUN = 20
# Alacritty 0.17's built-in colours (alacritty/src/config/color.rs): what Rime's terminal looked like.
ALACRITTY = {
    "background": "#181818", "foreground": "#d8d8d8",
    0: "#181818", 1: "#ac4242", 2: "#90a959", 3: "#f4bf75", 4: "#6a9fb5", 5: "#aa759f", 6: "#75b5aa", 7: "#d8d8d8",
    8: "#6b6b6b", 9: "#c55555", 10: "#aac474", 11: "#feca88", 12: "#82b8c8", 13: "#c28cb8", 14: "#93d3c3",
    15: "#f8f8f8",
}
UNIQUE = [1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15]


def rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


def hexof(c):
    return "#%02x%02x%02x" % c[:3]


def bbox_of(img, colour):
    """The bounding box of the long horizontal runs of exactly this colour (a block, not glyph strokes)."""
    mask = ImageChops.difference(img, Image.new("RGB", img.size, colour)).convert("L").point(
        lambda v: 255 if v == 0 else 0)
    rough = mask.getbbox()
    if not rough:
        return None
    w = img.size[0]
    box = None
    for y in range(rough[1], rough[3]):
        row = mask.crop((0, y, w, y + 1)).tobytes()
        for m in re.finditer(rb"\xff{%d,}" % MIN_RUN, row):
            l, r = m.start(), m.end()
            box = (l, y, r, y + 1) if box is None else (min(box[0], l), min(box[1], y), max(box[2], r), y + 1)
    return box


def changed(a, b):
    """Pixels where two same-size images differ at all."""
    return ImageChops.difference(a, b).convert("L").point(lambda v: 255 if v else 0).histogram()[255]


def measure(path):
    img = Image.open(path).convert("RGB")
    out = {"size": img.size}
    out["dominant"] = hexof(max(img.getcolors(maxcolors=1 << 24))[1])
    blocks = {}
    for i in UNIQUE:
        b = bbox_of(img, rgb(ALACRITTY[i]))
        blocks[i] = b
    out["blocks"] = {str(i): b for i, b in blocks.items()}
    out["palette_found"] = sorted(i for i, b in blocks.items() if b)
    out["palette_missing"] = sorted(i for i, b in blocks.items() if not b)
    b1 = blocks.get(1)
    if b1:
        cw = (b1[2] - b1[0]) / BLOCK_CELLS
        ch = b1[3] - b1[1]
        out["cell_width"] = cw
        out["cell_height"] = ch
        out["padding_left"] = b1[0] - BLOCK_CELLS * cw  # block 1 starts after block 0 (4 cells)
        out["padding_top"] = b1[1] - PALETTE_ROW * ch
        # every block where the grid says it should be
        out["grid_errors"] = []
        for i in UNIQUE:
            b = blocks[i]
            if not b:
                continue
            want = (out["padding_left"] + i * BLOCK_CELLS * cw, b1[1],
                    out["padding_left"] + (i + 1) * BLOCK_CELLS * cw, b1[3])
            if any(abs(a - w) > 0.5 for a, w in zip(b, want)):
                out["grid_errors"].append({"index": i, "found": b, "want": want})
        # the text rows above the blocks: the first non-background pixel = the first glyph's ink
        bg = rgb(ALACRITTY["background"])
        top = img.crop((0, 0, img.size[0], b1[1]))
        ink = ImageChops.difference(top, Image.new("RGB", top.size, bg)).convert("L").point(
            lambda v: 255 if v > 24 else 0).getbbox()
        out["text_ink_bbox"] = ink
        # the background straight after the block line, far right: the window's empty background
        out["background_sample"] = hexof(img.getpixel((img.size[0] - 3, img.size[1] - 3)))
        out["background_sample_mid"] = hexof(img.getpixel((img.size[0] // 2, b1[3] + 3 * ch)))
    return img, out


def main():
    a_path, g_path, outdir = sys.argv[1:4]
    os.makedirs(outdir, exist_ok=True)
    a_img, a = measure(a_path)
    g_img, g = measure(g_path)
    report = {"alacritty": a, "ghostty": g}
    if a_img.size == g_img.size:
        diff = ImageChops.difference(a_img, g_img)
        report["pixels_differing"] = changed(a_img, g_img)
        # more than antialiasing: a pixel whose brightness differs by more than 16 of 255
        report["pixels_differing_over_16"] = ImageChops.difference(a_img, g_img).convert("L").point(
            lambda v: 255 if v > 16 else 0).histogram()[255]
        report["pixels_total"] = a_img.size[0] * a_img.size[1]
        Image.eval(diff, lambda v: min(255, v * 4)).save(os.path.join(outdir, "diff.png"))
    side = Image.new("RGB", (a_img.size[0] + g_img.size[0] + 8, max(a_img.size[1], g_img.size[1])), (255, 0, 255))
    side.paste(a_img, (0, 0))
    side.paste(g_img, (a_img.size[0] + 8, 0))
    side.save(os.path.join(outdir, "side-by-side.png"))
    with open(os.path.join(outdir, "report.json"), "w") as f:
        json.dump(report, f, indent=2, default=list)
    print(json.dumps(report, indent=2, default=list))


if __name__ == "__main__":
    main()
