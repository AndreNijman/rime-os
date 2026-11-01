# Wordmark typefaces

The wordmarks in this directory are OUTLINES: no font is needed to use, render or regenerate any ridge asset.
`outline-wordmarks.py` drew them once from these fonts; run it again only to change a typeface or the tracking.

| Wordmark | Typeface | Instance | Tracking | Licence |
|---|---|---|---|---|
| `rime` (`rime-wordmark.svg`) | Golos Text (`GolosText[wght].ttf`) | wght 600 | -0.025 em | SIL OFL 1.1, `OFL-GolosText.txt` |
| `RIME OS` (`rime-os-wordmark.svg`) | Archivo (`Archivo[wdth,wght].ttf`) | wdth 125, wght 700 | +0.045 em, word space -0.16 em | SIL OFL 1.1, `OFL-Archivo.txt` |

Source of both files: the google/fonts tree packaged by nixpkgs as `google-fonts` (0-unstable-2026-03-13, the
flake's nixpkgs), directories `ofl/golostext/` and `ofl/archivo/`. Build them with
`google-fonts.override { fonts = [ "GolosText" "Archivo" ]; }` and pass its `share/fonts/truetype` to
`outline-wordmarks.py`.

Why these: the brand pack is raster only and names no typeface. Each wordmark was matched by rendering candidate
OFL families at many weights/widths/trackings and scoring the overlap (IoU) with the page crops:

- `rime`: Golos Text 600 0.894, Onest 650 0.893, Hanken Grotesk 700 0.910. Hanken was rejected because its i and
  the page's i disagree on the dot (square vs round), which a whole-word score does not see.
- `RIME OS`: Archivo wdth 125 wght 700, per-glyph IoU 0.866 mean, glyph widths within 1-7 % of the page.

The OFL allows embedding the outlined glyphs in a logo; the licence texts sit next to the outlines so the
attribution travels with them. The fonts were not renamed or modified, and are not redistributed here.
