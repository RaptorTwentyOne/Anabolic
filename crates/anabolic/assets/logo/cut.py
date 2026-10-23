# Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
# Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
# Reading this to reuse it? You may: keep this notice with what you take,
# and say it comes from Anabolic. That is all the license asks.

"""Cuts the rendered logo sheet into anabolic.ico and raw RGBA images.

Usage: cut.py <sheet.png> <sheet.json> <out dir>
"""

import json
import sys
from pathlib import Path

from PIL import Image

sheet = Image.open(sys.argv[1]).convert("RGBA")
layout = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))["layout"]
out = Path(sys.argv[3])

images = {}
for cell in layout:
    box = (cell["left"], 0, cell["left"] + cell["size"], cell["size"])
    images[cell["name"]] = sheet.crop(box)

# The exe icon: every size Windows asks for, each one rendered at that size.
icon_sizes = [256, 128, 96, 64, 48, 40, 32, 24, 20, 16]
frames = [images[f"icon-{s}"] for s in icon_sizes]
frames[0].save(out / "anabolic.ico", format="ICO", sizes=[(s, s) for s in icon_sizes], append_images=frames[1:])

# Raw RGBA for the window icon and the rail mark: no decoder needed at runtime.
for name in ("icon-64", "mark-64"):
    (out / f"{name}.rgba").write_bytes(images[name].tobytes())
    images[name].save(out / f"{name}.png")

print("wrote anabolic.ico, icon-64.rgba, mark-64.rgba")
