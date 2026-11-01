#!/usr/bin/env python3
"""Check the committed ridge exports against a fresh regeneration and the brand rules.

  check-ridge-assets.py <committed ridge dir> <regenerated dir>

Needs python3 with Pillow. Run by checks.x86_64-linux.branding (nix/tests/branding.nix).
"""
import json
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

# 2. The masters: exactly four planes, in order, each a polygon of at least four points.
for m in ["rime-ridge-symbol.svg", "rime-ridge-symbol-small.svg"]:
    text = open(os.path.join(COMMITTED, "src", m)).read()
    ids = re.findall(r'<polygon id="plane-(\d)"', text)
    if ids != ["1", "2", "3", "4"] or text.count("<polygon") != 4 or "<path" in text:
        fail(f"src/{m}: must be exactly four <polygon> planes, plane-1..plane-4 in order (got {ids})")
    for n, pts in re.findall(r'<polygon id="plane-(\d)"[^>]*\bpoints="([^"]+)"', text):
        if len(pts.split()) < 4:
            fail(f"src/{m}: plane-{n} has fewer than four points")

# 3. Adaptive templates: plane 3 (and only plane 3) takes {{accent}}; every other ink is {{fg}}; no literal colour.
for t in sorted(os.listdir(os.path.join(COMMITTED, "templates"))):
    text = open(os.path.join(COMMITTED, "templates", t)).read()
    fills = re.findall(r'<polygon id="plane-(\d)" fill="([^"]+)"', text)
    symbols = len(fills) // 4
    if "wordmark" not in t and symbols != 1:
        fail(f"templates/{t}: expected one symbol, found {len(fills)} planes")
    for n, f in fills:
        want = "{{accent}}" if n == "3" else "{{fg}}"
        if f != want:
            fail(f"templates/{t}: plane-{n} fill is {f}, must be {want}")
    if text.count("{{accent}}") != symbols:
        fail(f"templates/{t}: {{{{accent}}}} appears {text.count('{{accent}}')} times, expected {symbols}")
    words = 1 if re.search(r"<path ", text) else 0
    if text.count("{{fg}}") != 3 * symbols + words:
        fail(f"templates/{t}: {{{{fg}}}} appears {text.count('{{fg}}')} times, expected {3 * symbols + words}")
    if re.search(r"#[0-9A-Fa-f]{6}", text):
        fail(f"templates/{t}: contains a literal colour")


# 4. Accent exports change plane 3 only.
def strip3(text):
    return re.sub(r'(<polygon id="plane-3" fill=")[^"]+"', r'\1X"', re.sub(r"<title>.*?</title>", "", text))


acc = json.load(open(os.path.join(COMMITTED, "accents", "accents.json")))
if len(acc["slots"]) != 24:
    fail("accents/accents.json: expected 24 slots")
for mode in ("light", "dark"):
    ref = strip3(open(os.path.join(COMMITTED, f"svg/square/rime-symbol-square-{mode}.svg")).read())
    for kind, table in (("named", acc["named"]), ("slots", acc["slots"])):
        for key, hexv in table.items():
            stem = f"rime-symbol-{key}" if kind == "named" else f"rime-symbol-slot-{key}"
            text = open(os.path.join(COMMITTED, f"accents/{kind}/{stem}-{mode}.svg")).read()
            if strip3(text) != ref:
                fail(f"accents/{kind}/{stem}-{mode}.svg: differs from the {mode} symbol outside plane 3")
            if f'<polygon id="plane-3" fill="{hexv}"' not in text:
                fail(f"accents/{kind}/{stem}-{mode}.svg: plane 3 is not {hexv}")


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
print(f"ridge assets: {len(a)} exports checked, {len(fails)} failures")
sys.exit(1 if fails else 0)
