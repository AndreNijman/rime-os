# Wordmark typeface

The wordmark in this directory is an OUTLINE: no font is needed to use, render or regenerate any brand asset.
`outline-wordmarks.py` drew them once from these fonts; run it again only to change a typeface or the tracking.

| Wordmark | Typeface | Instance | Tracking | Licence |
|---|---|---|---|---|
| `rime` (`rime-wordmark.svg`) | Golos Text (`GolosText[wght].ttf`) | wght 600 | -0.025 em | SIL OFL 1.1, `OFL-GolosText.txt` |

Source of the font: the google/fonts tree packaged by nixpkgs as `google-fonts` (0-unstable-2026-03-13, the
flake's nixpkgs), directory `ofl/golostext/`. Build them with
`google-fonts.override { fonts = [ "GolosText" ]; }` and pass its `share/fonts/truetype` to
`outline-wordmarks.py`.

Why these: the brand pack is raster only and names no typeface. The wordmark was matched by rendering candidate
OFL families at many weights/widths/trackings and scoring the overlap (IoU) with the page crops:

- `rime`: Golos Text 600 0.894, Onest 650 0.893, Hanken Grotesk 700 0.910. Hanken was rejected because its i and
  the page's i disagree on the dot (square vs round), which a whole-word score does not see.

The OFL allows embedding the outlined glyphs in a logo; the licence text sits next to the outline so the
attribution travels with them. The fonts were not renamed or modified, and are not redistributed here.

The wide-caps `RIME OS` wordmark (Archivo) went with the layered ridge: the Origin brand has one wordmark, `rime`.
