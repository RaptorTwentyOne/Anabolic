# Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
# Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
# Reading this to reuse it? You may: keep this notice with what you take,
# and say it comes from Anabolic. That is all the license asks.

"""Writes the ANABOLIC wordmark (wordmark.svg) and the logo lockup, mark
beside wordmark (lockup.svg), as outlines: no font needed to show them.

The letters are Aldrich (../fonts), emboldened for the logo alone: every
outline is offset outward with sharp miter joins, then squeezed back to the
original cap height, so the stems grow by half while the corners and the
height stay Aldrich's. The app's own text uses Aldrich as drawn.

Run after make.mjs (the lockup embeds mark.svg):

    node make.mjs && python wordmark.py && powershell -File rasterize.ps1

Needs fontTools and skia-pathops (pip install fonttools skia-pathops).
"""

from pathlib import Path

import pathops
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

HERE = Path(__file__).parent
TEXT = "ANABOLIC"
ORANGE = "#FF6A3D"
# Stroke added around every outline, as a fraction of the em: stems go from
# 0.10 em to 0.15 em.
WEIGHT = 0.05
# Space between letters, as a fraction of the em.
TRACKING = 0.08

font = TTFont(HERE.parent / "fonts" / "Aldrich-Regular.ttf")
upem = font["head"].unitsPerEm
cmap = font.getBestCmap()
glyphs = font.getGlyphSet()
w = WEIGHT * upem
cap = font["glyf"][cmap[ord("H")]].yMax


def bold(name):
    """The glyph's outline, emboldened, in font units (y up)."""
    path = pathops.Path()
    glyphs[name].draw(path.getPen())
    path.simplify(fix_winding=True)
    stroke = pathops.Path()
    path.draw(stroke.getPen())
    stroke.stroke(w, pathops.LineCap.BUTT_CAP, pathops.LineJoin.MITER_JOIN, 4)
    return pathops.op(path, stroke, pathops.PathOp.UNION, fix_winding=True)


# Lay the letters out in font units: the emboldened ink grows by w/2 on each
# side, so each advance grows by w and the gaps stay Aldrich's plus tracking.
# y flips for SVG, and the squeeze keeps the caps at Aldrich's height.
k = cap / (cap + w)
ink = pathops.Path()
x = 0.0
for ch in TEXT:
    name = cmap[ord(ch)]
    bold(name).draw(TransformPen(ink.getPen(), (1, 0, 0, -k, x + w / 2, cap - k * w / 2)))
    x += glyphs[name].width + w + TRACKING * upem
# The box is the ink's: no side bearings at either end.
left, _, right, _ = ink.bounds
width = right - left
pen = SVGPathPen(None, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
ink.draw(pen)
d = pen.getCommands()


def wordmark_svg():
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width / upem * 40:.1f}" height="{cap / upem * 40:.1f}" '
        f'viewBox="{left:.1f} 0 {width:.1f} {cap}"><path fill="{ORANGE}" d="{d}"/></svg>\n'
    )


def lockup_svg():
    # The mark's 64 box beside caps 0.43 of its height, centred on it: the
    # proportions of the lockup the brand was chosen from.
    mark = (HERE / "mark.svg").read_text(encoding="utf-8")
    inner = mark[mark.index(">") + 1 : mark.rindex("</svg>")]
    caps = 64 * 0.43
    scale = caps / cap
    gap = 15
    total = 64 + gap + width * scale
    ty = (64 - caps) / 2
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{total:.1f}" height="64" viewBox="0 0 {total:.1f} 64">'
        f"{inner}"
        f'<path fill="{ORANGE}" transform="translate({64 + gap - left * scale:.2f} {ty:.2f}) scale({scale:.6f})" d="{d}"/>'
        f"</svg>\n"
    )


(HERE / "wordmark.svg").write_text(wordmark_svg(), encoding="utf-8")
(HERE / "lockup.svg").write_text(lockup_svg(), encoding="utf-8")
print(f"wrote wordmark.svg, lockup.svg ({width:.0f} x {cap} units)")
