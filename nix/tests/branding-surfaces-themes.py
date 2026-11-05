#!/usr/bin/env python3
"""The boot splash themes keep the brand rules (checks.x86_64-linux.branding-surfaces-static).

    branding-surfaces-themes.py <themes dir> <accents.json> <ridge script source>

* 25 themes: rime-os and rime-os-accent-00..23, each a script theme naming its images under /etc/plymouth/themes/<name>.
* Only plane 3 takes a colour: rime-os's lockup-base has no saturated pixel, an accent theme's neutral images are
  links into rime-os (so no theme can recolour plane 1, 2, 4 or the wordmark), every lockup-accent has exactly the
  same coverage (plane 3's shape) and is painted in that theme's colour, and plane 3 does not sit on another plane.
* The dots and bullets are the theme's colour; every PNG carries transparency.
* Every theme runs the same script; only the HI_ line differs. The refresh function does no Image work (the engine
  rule from bootc's script: scale once, then move opacity), there is no SetUpdateStatusFunction, refresh is 60 Hz.
"""
import json
import os
import re
import sys

from PIL import Image

THEMES, ACCENTS, SCRIPT = sys.argv[1:4]
fails = []


def fail(msg):
    fails.append(msg)
    print("FAIL:", msg)


def rgb(hexc):
    return tuple(int(hexc[i:i + 2], 16) for i in (1, 3, 5))


acc = json.load(open(ACCENTS))
colours = {"rime-os": acc["base"]}
colours.update({f"rime-os-accent-{nn}": h for nn, h in acc["slots"].items()})
assert len(colours) == 25, len(colours)

names = sorted(os.listdir(THEMES))
if names != sorted(colours):
    fail(f"themes are {names}, expected rime-os + rime-os-accent-00..23")

NEUTRAL = ["lockup-base", "dot-off", "prompt"]
COLOURED = ["lockup-accent", "dot-on", "bullet"]
SUFFIXES = ["", "-hd"]

src = open(SCRIPT).read()
referenced = sorted(set(re.findall(r'Image\("([^"]+)"\)', src)))
expected_imgs = sorted(f"{i}{s}.png" for i in NEUTRAL + COLOURED for s in SUFFIXES)
if referenced != expected_imgs:
    fail(f"the script loads {referenced}, the generator makes {expected_imgs}")


def load(path):
    im = Image.open(path)
    if im.mode != "RGBA":
        fail(f"{path} is {im.mode}, not RGBA")
        im = im.convert("RGBA")
    return im


def alpha_mask(im, cut=128):
    return [a >= cut for a in im.getchannel("A").getdata()]


base_dir = os.path.join(THEMES, "rime-os")
ref_cov = {}
for s in SUFFIXES:
    ref_cov[s] = alpha_mask(load(os.path.join(base_dir, f"lockup-accent{s}.png")))
    base = load(os.path.join(base_dir, f"lockup-base{s}.png"))
    # plane 3 is left out of the base: no saturated ink anywhere in it (off-white #FAFAFA only)
    bad = sum(1 for (r, g, b, a) in base.getdata() if a >= 64 and (max(r, g, b) - min(r, g, b) > 12 or min(r, g, b) < 200))
    if bad:
        fail(f"rime-os/lockup-base{s}.png has {bad} coloured or dark pixels: a plane other than 3 took a colour")
    # plane 3 is not painted over another plane: the two images barely touch (antialiased edges only)
    bmask = alpha_mask(base, 200)
    overlap = sum(1 for x, y in zip(bmask, alpha_mask(load(os.path.join(base_dir, f"lockup-accent{s}.png")), 200)) if x and y)
    if overlap:
        fail(f"rime-os lockup{s}: plane 3 covers {overlap} pixels of the other planes")
    if base.size != Image.open(os.path.join(base_dir, f"lockup-accent{s}.png")).size:
        fail(f"rime-os lockup{s}: base and accent are not one canvas")
    if sum(ref_cov[s]) < 200:
        fail(f"rime-os/lockup-accent{s}.png is nearly empty ({sum(ref_cov[s])} px): plane 3 is missing")

ref_script_lines = None
for name, hexc in sorted(colours.items()):
    d = os.path.join(THEMES, name)
    want = rgb(hexc)

    pl = open(os.path.join(d, f"{name}.plymouth")).read()
    for line in ("ModuleName=script", f"ImageDir=/etc/plymouth/themes/{name}",
                 f"ScriptFile=/etc/plymouth/themes/{name}/rime-os.script"):
        if line not in pl.splitlines():
            fail(f"{name}.plymouth lacks {line}")

    for img in expected_imgs:
        p = os.path.join(d, img)
        if not os.path.isfile(p):
            fail(f"{name}/{img} does not exist (or is a dangling link)")
            continue
        im = load(p)
        if min(im.getchannel("A").getdata()) == 255:
            fail(f"{name}/{img} has no transparent pixel")

    if name != "rime-os":
        for i in NEUTRAL:
            for s in SUFFIXES:
                p = os.path.join(d, f"{i}{s}.png")
                if not os.path.islink(p) or os.readlink(p) != f"../rime-os/{i}{s}.png":
                    fail(f"{name}/{i}{s}.png must be a link to ../rime-os/{i}{s}.png (only plane 3 may change colour)")

    for s in SUFFIXES:
        acc_img = load(os.path.join(d, f"lockup-accent{s}.png"))
        if alpha_mask(acc_img) != ref_cov[s]:
            fail(f"{name}/lockup-accent{s}.png does not cover exactly plane 3 (coverage differs from rime-os)")
        for img in (f"lockup-accent{s}.png", f"dot-on{s}.png", f"bullet{s}.png"):
            px = [(r, g, b) for (r, g, b, a) in load(os.path.join(d, img)).getdata() if a == 255]
            off = [p for p in px if max(abs(p[k] - want[k]) for k in range(3)) > 2]
            if not px or off:
                fail(f"{name}/{img}: {len(off)} of {len(px)} opaque pixels are not {hexc}")

    lines = open(os.path.join(d, "rime-os.script")).read().splitlines()
    hi = [l for l in lines if l.startswith("HI_R = ")]
    r, g, b = (v / 255 for v in want)
    if len(hi) != 1 or not hi[0].startswith(f"HI_R = {r:.3f}; HI_G = {g:.3f}; HI_B = {b:.3f};"):
        fail(f"{name}/rime-os.script: HI_ line {hi} is not {hexc}")
    rest = [l for l in lines if not l.startswith("HI_R = ")]
    if ref_script_lines is None:
        ref_script_lines = rest
    elif rest != ref_script_lines:
        fail(f"{name}/rime-os.script differs from rime-os's in more than the HI_ line")

# The engine rules, on the source script.
if "SetUpdateStatusFunction" in src:
    fail("the script registers SetUpdateStatusFunction (bootc's script dropped it: status text tears the splash)")
if not re.search(r"^RATE = 60;", src, re.M) or "Plymouth.SetRefreshRate(RATE);" not in src:
    fail("the splash does not refresh at 60 Hz")
m = re.search(r"^fun refresh\s*\(\)\s*\{", src, re.M)
if not m:
    fail("no refresh function")
else:
    depth, i = 1, m.end()
    while depth and i < len(src):
        depth += {"{": 1, "}": -1}.get(src[i], 0)
        i += 1
    body = src[m.end():i]
    for bad in ("Image(", ".Scale(", ".Rotate(", "Image.Text("):
        if bad in body:
            fail(f"the refresh function calls {bad}: image work belongs in setup, not in every frame")

if fails:
    sys.exit(f"{len(fails)} failure(s)")
print(f"branding-surfaces-themes: {len(colours)} themes, {len(expected_imgs)} images each, rules hold")
