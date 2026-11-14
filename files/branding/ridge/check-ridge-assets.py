#!/usr/bin/env python3
"""Check the committed brand exports (the Origin mark) against a fresh regeneration and the brand rules.

  check-ridge-assets.py <committed ridge dir> <regenerated dir>

Needs python3 with Pillow. Run by checks.x86_64-linux.branding (nix/tests/branding.nix).
"""
import colorsys
import json
import math
import os
import re
import struct
import sys

from PIL import Image

COMMITTED, FRESH = sys.argv[1], sys.argv[2]
GENERATED = ["svg", "templates", "png", "icons", "favicon", "accents"]
ICON_SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
VARIANTS = ["light", "dark", "mono-black", "mono-white"]
fails = []


def fail(msg):
    fails.append(msg)


def tree(root):
    out = set()
    for d in GENERATED:
        for base, _, files in os.walk(os.path.join(root, d)):
            out.update(os.path.relpath(os.path.join(base, f), root) for f in files)
    return out


def pixels(path):
    im = Image.open(path)
    return im.mode, im.size, im.tobytes()


# 1. The exports are exactly what the masters produce.
a, b = tree(COMMITTED), tree(FRESH)
for rel in sorted(a - b):
    fail(f"stale export (the generator no longer makes it): {rel}")
for rel in sorted(b - a):
    fail(f"missing export (run make-ridge-assets.sh and commit): {rel}")
for rel in sorted(a & b):
    p, q = os.path.join(COMMITTED, rel), os.path.join(FRESH, rel)
    same = pixels(p) == pixels(q) if rel.endswith(".png") else open(p, "rb").read() == open(q, "rb").read()
    if not same:
        fail(f"out of date (differs from a regeneration): {rel}")

# 2. The masters: five circles c1..c5, radii falling, every one tangent to c1 at the SAME point (the origin).
BODIES = ("coat", "ring", "core")
for m in ["rime-origin-symbol.svg", "rime-origin-symbol-small.svg"]:
    text = open(os.path.join(COMMITTED, "src", m)).read()
    found = re.findall(r'<circle id="c(\d)" cx="([^"]+)" cy="([^"]+)" r="([^"]+)"', text)
    if [n for n, *_ in found] != ["1", "2", "3", "4", "5"] or text.count("<circle") != 5 or "<path" in text:
        fail(f"src/{m}: must be exactly five <circle>s, c1..c5 in order (got {[n for n, *_ in found]})")
        continue
    if 'fill-rule="evenodd"' not in text:
        fail(f"src/{m}: the circles must be filled even-odd")
    cs = [tuple(float(v) for v in c) for _, *c in found]
    (x1, y1, r1), origins = cs[0], []
    for i, (x, y, r) in enumerate(cs[1:], 2):
        if not cs[i - 2][2] - r >= 4:
            fail(f"src/{m}: c{i} must be at least 4 units smaller than c{i - 1} (an open gap or body)")
        d = math.hypot(x - x1, y - y1)
        if abs(d - (r1 - r)) > 0.01:
            fail(f"src/{m}: c{i} is not tangent to c1 (centre distance {d:.3f}, radii differ by {r1 - r})")
        if d:
            origins.append((x1 + r1 * (x - x1) / d, y1 + r1 * (y - y1) / d))
    if origins and max(math.hypot(ox - origins[0][0], oy - origins[0][1]) for ox, oy in origins) > 0.02:
        fail(f"src/{m}: the circles touch c1 at different points, not one origin: {origins}")

# 3. Adaptive templates: each symbol is three bodies (coat, ring, core) in {{accent}}, the wordmark is {{fg}},
#    and no literal colour.
for t in sorted(os.listdir(os.path.join(COMMITTED, "templates"))):
    text = open(os.path.join(COMMITTED, "templates", t)).read()
    fills = re.findall(r'<path id="(coat|ring|core)" fill="([^"]+)"', text)
    symbols = text.count('id="core"')
    if "wordmark" not in t and symbols != 1:
        fail(f"templates/{t}: expected one symbol, found {symbols}")
    if [n for n, _ in fills] != list(BODIES) * symbols:
        fail(f"templates/{t}: the bodies must be coat, ring, core in that order (got {[n for n, _ in fills]})")
    for n, f in fills:
        if f != "{{accent}}":
            fail(f"templates/{t}: {n} fill is {f}, must be {{{{accent}}}}")
    if text.count("{{accent}}") != 3 * symbols:
        fail(f"templates/{t}: {{{{accent}}}} appears {text.count('{{accent}}')} times, expected {3 * symbols}")
    words = 1 if re.search(r"<path d=", text) else 0
    if text.count("{{fg}}") != words:
        fail(f"templates/{t}: {{{{fg}}}} appears {text.count('{{fg}}')} times, expected {words}")
    if re.search(r"#[0-9A-Fa-f]{6}", text):
        fail(f"templates/{t}: contains a literal colour")


# 4. Accent exports change the mark's colour only; on a light ground a pale accent is darkened to Rime Blue
#    Deep's lightness, keeping its hue.
def strip_mark(text):
    return re.sub(r'(<path id="(?:coat|ring|core)" fill=")[^"]+"', r'\1X"', re.sub(r"<title>.*?</title>", "", text))


def hls(hexv):
    return colorsys.rgb_to_hls(*(int(hexv[i:i + 2], 16) / 255 for i in (1, 3, 5)))


acc = json.load(open(os.path.join(COMMITTED, "accents", "accents.json")))
cap = hls(acc["base_on_light"])[1]
if len(acc["slots"]) != 24 or set(acc["slots_on_light"]) != set(acc["slots"]):
    fail("accents/accents.json: expected 24 slots, each with an on-light colour")
for mode in ("light", "dark"):
    ref = strip_mark(open(os.path.join(COMMITTED, f"svg/square/rime-symbol-square-{mode}.svg")).read())
    for kind, table in (("named", acc["named"]), ("slots", acc["slots"])):
        for key, hexv in table.items():
            stem = f"rime-symbol-{key}" if kind == "named" else f"rime-symbol-slot-{key}"
            text = open(os.path.join(COMMITTED, f"accents/{kind}/{stem}-{mode}.svg")).read()
            if strip_mark(text) != ref:
                fail(f"accents/{kind}/{stem}-{mode}.svg: differs from the {mode} symbol outside the mark's colour")
            marks = set(re.findall(r'<path id="(?:coat|ring|core)" fill="([^"]+)"', text))
            want = hexv
            if mode == "light":
                # slots: accents.json names the on-light colour; named accents: whatever single colour was drawn
                want = acc["slots_on_light"][key] if kind == "slots" else (next(iter(marks)) if len(marks) == 1 else "")
                h0, l0, s0 = hls(hexv)
                h1, l1, s1 = hls(want) if want else (0, 1, 0)
                if l1 > cap + 0.005 or (l0 <= cap and want.upper() != hexv.upper()) or \
                        (s0 > 0.05 and min(abs(h1 - h0), 1 - abs(h1 - h0)) * 360 > 1.5):
                    fail(f"accents/{kind}/{stem}-light.svg: {want} is not {hexv} at a lightness of at most {cap:.3f}")
            if marks != {want}:
                fail(f"accents/{kind}/{stem}-{mode}.svg: the mark is {sorted(marks)}, not {want}")


# 5. Sizes, transparency and tiles.
def corners_clear(im, limit=0):
    w, h = im.size
    return all(im.getpixel(p)[3] <= limit for p in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)])


def mostly_clear(im):
    """A tight crop may put ink on a corner pixel; its background must still be transparent."""
    hist = im.getchannel("A").histogram()
    return hist[0] >= 0.2 * im.size[0] * im.size[1]


def has_ink(im):
    return im.getchannel("A").getextrema()[1] == 255


for var in VARIANTS:
    for n in ICON_SIZES:
        rel = f"png/symbol/rime-symbol-{var}-{n}.png"
        im = Image.open(os.path.join(COMMITTED, rel)).convert("RGBA")
        if im.size != (n, n) or not corners_clear(im) or not has_ink(im):
            fail(f"{rel}: must be {n}x{n}, transparent at the corners, with opaque ink")
for kind in ("dark", "light"):
    for n in ICON_SIZES:
        rel = f"icons/app/rime-app-icon-{kind}-{n}.png"
        im = Image.open(os.path.join(COMMITTED, rel)).convert("RGBA")
        # the rounded corner is antialiased at 16 px (alpha 16 there), so allow a trace of coverage
        if im.size != (n, n) or not corners_clear(im, 32) or im.getpixel((n // 2, n // 10))[3] != 255:
            fail(f"{rel}: must be {n}x{n}, an opaque tile with transparent rounded corners")
for rel in sorted(a):
    if rel.endswith(".png") and not rel.startswith("icons/"):
        im = Image.open(os.path.join(COMMITTED, rel)).convert("RGBA")
        tight = rel.startswith("png/lockups/") and "/clearspace/" not in rel
        if not (mostly_clear(im) if tight else corners_clear(im)) or not has_ink(im):
            fail(f"{rel}: must have a transparent background and carry opaque ink")
for rel in sorted(r for r in a if r.endswith("@1x.png")):
    h1 = Image.open(os.path.join(COMMITTED, rel)).size[1]
    h2 = Image.open(os.path.join(COMMITTED, rel.replace("@1x", "@2x"))).size[1]
    if abs(h2 - 2 * h1) > 1:
        fail(f"{rel}: the @2x export is {h2} px high, not 2 x {h1}")

# 6. The .ico holds 16, 32 and 48 px images.
data = open(os.path.join(COMMITTED, "favicon/favicon.ico"), "rb").read()
_, kind, count = struct.unpack("<HHH", data[:6])
dims = sorted(data[6 + 16 * i] for i in range(count))
if kind != 1 or dims != [16, 32, 48]:
    fail(f"favicon/favicon.ico: expected icon entries 16/32/48, got {dims}")

for f in fails:
    print("FAIL:", f)
print(f"brand assets: {len(a)} exports checked, {len(fails)} failures")
sys.exit(1 if fails else 0)
