#!/usr/bin/env python3
"""Build the layered-ridge boot splash themes from the adaptive ridge templates.

    make-ridge-themes.py <templates dir> <accents.json> <rime-os.script> <font file> <out dir> [image dir prefix]

Writes <out dir>/rime-os (the default, plane 3 in Rime Blue) and <out dir>/rime-os-accent-00 .. -23 (plane 3 in the
24 hue slots of accents.json: slot n = hue n*15 deg at Rime Blue's lightness and saturation, the buckets
rime-plymouth-accent picks from the owner's matugen accent).

Only plane 3 changes colour (brand pack page 4), and that is structural here: the lockup is rendered as two images
on one canvas, lockup-base (planes 1, 2, 4 and the wordmark in off-white, plane 3 left out) and lockup-accent (plane 3
alone). An accent theme owns only what carries the accent (lockup-accent, dot-on, bullet, its script's HI_ line and
its .plymouth); every neutral image is a relative symlink into rime-os, so 24 accents cost the initrd a few KB each.

Needs resvg on PATH and the Python standard library. Used by nix/pkgs/rime-plymouth.nix (pinned resvg).
"""
import json
import os
import re
import subprocess
import sys
import tempfile

TEMPLATES, ACCENTS, SCRIPT, FONT, OUT = (os.path.abspath(a) for a in sys.argv[1:6])
PREFIX = sys.argv[6] if len(sys.argv) > 6 else "/etc/plymouth/themes"

FG = "#FAFAFA"               # off-white: planes 1/2/4 and the wordmark on charcoal (palette.json off_white)
NEUTRAL_DOT = "#FAFAFA"      # the unlit dots: off-white at DOT_OFF_ALPHA over charcoal
DOT_OFF_ALPHA = 0.22
# Sizes at the 1200-line reference screen; -hd is drawn for a 2400-line screen (the script scales once).
LOCKUP_H = 112               # px: the symbol's height (the lockup is exactly as tall as the symbol)
DOT = 10                     # px: progress dot diameter
BULLET = 12                  # px: passphrase bullet diameter
PROMPT_PT = 17               # px: fallback prompt text size
PROMPT = "Enter passphrase to unlock"

NEUTRAL = ["lockup-base", "dot-off", "prompt"]
COLOURED = ["lockup-accent", "dot-on", "bullet"]


def resvg(svg_text, png, height=None):
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as f:
        f.write(svg_text)
        src = f.name
    cmd = ["resvg", "--skip-system-fonts", "--use-font-file", FONT]
    if height:
        cmd += ["--height", str(height)]
    subprocess.run(cmd + [src, png], check=True)
    os.unlink(src)


def lockup(fg, accent):
    t = open(os.path.join(TEMPLATES, "rime-lockup-horizontal.svg.in")).read()
    assert t.count("{{accent}}") == 1, "the template must give exactly one plane the accent"
    return t.replace("{{fg}}", fg).replace("{{accent}}", accent)


def dot(colour, size, alpha=1.0):
    r = size / 2
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 {size} {size}">'
            f'<circle cx="{r}" cy="{r}" r="{r - 0.5}" fill="{colour}" fill-opacity="{alpha}"/></svg>')


def prompt(size):
    # Width generous enough for the text at this size; resvg trims nothing, the script centres the image.
    w, hgt = int(size * 16), int(size * 1.9)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{hgt}" viewBox="0 0 {w} {hgt}">'
            f'<text x="{w / 2}" y="{size * 1.35}" text-anchor="middle" font-family="DejaVu Sans" '
            f'font-size="{size}" fill="#FAFAFA">{PROMPT}</text></svg>')


def hi_line(hexc, label):
    r, g, b = (int(hexc[i:i + 2], 16) / 255 for i in (1, 3, 5))
    return f"HI_R = {r:.3f}; HI_G = {g:.3f}; HI_B = {b:.3f};   # {hexc.upper()} ({label}, plane 3)"


def theme_file(name, title, desc):
    return (f"[Plymouth Theme]\nName={title}\nDescription={desc}\nModuleName=script\n\n[script]\n"
            f"ImageDir={PREFIX}/{name}\nScriptFile={PREFIX}/{name}/rime-os.script\n")


def coloured(d, accent):
    for suffix, k in (("", 1), ("-hd", 2)):
        resvg(lockup("none", accent), os.path.join(d, f"lockup-accent{suffix}.png"), LOCKUP_H * k)
        resvg(dot(accent, DOT * k), os.path.join(d, f"dot-on{suffix}.png"))
        resvg(dot(accent, BULLET * k), os.path.join(d, f"bullet{suffix}.png"))


def script_for(hexc, label):
    text = open(SCRIPT).read()
    new, n = re.subn(r"^HI_R = .*$", hi_line(hexc, label), text, count=1, flags=re.M)
    assert n == 1, "the script must have one HI_R line"
    return new


def main():
    acc = json.load(open(ACCENTS))
    base = acc["base"]
    os.makedirs(OUT, exist_ok=True)

    d = os.path.join(OUT, "rime-os")
    os.makedirs(d)
    for suffix, k in (("", 1), ("-hd", 2)):
        resvg(lockup(FG, "none"), os.path.join(d, f"lockup-base{suffix}.png"), LOCKUP_H * k)
        resvg(dot(NEUTRAL_DOT, DOT * k, DOT_OFF_ALPHA), os.path.join(d, f"dot-off{suffix}.png"))
        resvg(prompt(PROMPT_PT * k), os.path.join(d, f"prompt{suffix}.png"))
    coloured(d, base)
    open(os.path.join(d, "rime-os.script"), "w").write(script_for(base, "Rime Blue"))
    open(os.path.join(d, "rime-os.plymouth"), "w").write(
        theme_file("rime-os", "Rime OS", "Rime OS boot splash: the layered ridge, plane 3 in Rime Blue"))

    for nn, hexc in sorted(acc["slots"].items()):
        name = f"rime-os-accent-{nn}"
        d = os.path.join(OUT, name)
        os.makedirs(d)
        for img in NEUTRAL:
            for suffix in ("", "-hd"):
                os.symlink(f"../rime-os/{img}{suffix}.png", os.path.join(d, f"{img}{suffix}.png"))
        coloured(d, hexc)
        open(os.path.join(d, "rime-os.script"), "w").write(script_for(hexc, f"slot {nn}, hue {int(nn) * 15} deg"))
        open(os.path.join(d, f"{name}.plymouth"), "w").write(theme_file(
            name, f"Rime OS accent {nn}",
            f"Rime OS boot splash, plane 3 at hue {int(nn) * 15} deg (follows the owner's matugen accent)"))


if __name__ == "__main__":
    main()
