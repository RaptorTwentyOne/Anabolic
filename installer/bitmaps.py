"""Draws the installer's two pictures from the logo: the strip at the top of
its pages (banner.bmp) and the panel beside its first and last page
(dialog.bmp). Windows Installer draws its own black text over the white
parts, so only the left of the panel and the right of the strip are ours.

Usage: python bitmaps.py   (writes beside this file)
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

here = Path(__file__).parent
assets = here.parent / "crates" / "anabolic" / "assets"
BG = (0x0C, 0x0E, 0x12)
ACCENT = (0xFF, 0x6A, 0x3D)
WHITE = (0xFF, 0xFF, 0xFF)


def icon(size):
    """The icon as the .ico holds it at that size: drawn for it, not scaled."""
    ico = Image.open(assets / "logo" / "anabolic.ico")
    ico.size = (size, size)
    return ico.convert("RGBA")


# WixUI's sizes: 493 x 312 for the panel, of which the left 164 show beside
# the text; 493 x 58 for the strip.
panel = Image.new("RGB", (493, 312), WHITE)
draw = ImageDraw.Draw(panel)
draw.rectangle((0, 0, 163, 311), fill=BG)
draw.rectangle((160, 0, 163, 311), fill=ACCENT)
mark = icon(96)
panel.paste(mark, ((160 - 96) // 2, 72), mark)
font = ImageFont.truetype(str(assets / "fonts" / "Aldrich-Regular.ttf"), 24)
width = draw.textlength("Anabolic", font=font)
draw.text(((160 - width) / 2, 184), "Anabolic", font=font, fill=WHITE)
panel.save(here / "dialog.bmp")

strip = Image.new("RGB", (493, 58), WHITE)
mark = icon(48)
strip.paste(mark, (493 - 48 - 8, 5), mark)
strip.save(here / "banner.bmp")
