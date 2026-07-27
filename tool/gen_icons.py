#!/usr/bin/env python3
"""Regenerate the launcher icons and the splash asset from the source logo.

    python tool/gen_icons.py path/to/VON_LOGO.png

Committed rather than run once by hand so the icons can be rebuilt from a new
source file without anyone re-deriving the sizing rules below.

The sizing rule that matters: an adaptive icon is 108dp, but only the central
72dp is *guaranteed* visible — a launcher may mask to a circle of that size.
The V's tips sit at the corners of its bounding box, so the mark is scaled by
its true radial extent from its own centre, not by its width or bbox diagonal.
Get this wrong and the tips get shaved off on round-icon launchers.
"""

from __future__ import annotations

import math
import pathlib
import sys

from PIL import Image

# Fraction of the guaranteed-visible radius the mark should occupy. Below ~0.9
# the icon looks timid; above ~0.97 the anti-aliased edge touches the mask.
FILL = 0.94
SAFE_FRACTION = 72 / 108

# 108dp expressed in pixels at each density bucket.
BUCKETS = {
    "mdpi": 108,
    "hdpi": 162,
    "xhdpi": 216,
    "xxhdpi": 324,
    "xxxhdpi": 432,
}

ROOT = pathlib.Path(__file__).resolve().parent.parent
RES = ROOT / "android/app/src/main/res"
ASSETS = ROOT / "assets"


def main(source: pathlib.Path) -> None:
    src = Image.open(source).convert("RGBA")
    width, height = src.size

    # Ignore the soft outer glow when locating the mark: it reaches the canvas
    # edge and would swamp the measurement.
    solid = src.getchannel("A").point(lambda v: 255 if v > 60 else 0)
    box = solid.getbbox()
    if box is None:
        raise SystemExit(f"{source} appears to be fully transparent")
    bx0, by0, bx1, by1 = box
    cx, cy = (bx0 + bx1) / 2, (by0 + by1) / 2

    pixels = solid.load()
    extent = max(
        math.hypot(x - cx, y - cy)
        for y in range(by0, by1)
        for x in range(bx0, bx1)
        if pixels[x, y]
    )

    safe_radius = width * SAFE_FRACTION / 2
    scale = (safe_radius * FILL) / extent
    print(f"mark centre ({cx:.0f},{cy:.0f}), radial extent {extent:.0f}px")
    print(f"scale {scale:.3f}")

    def foreground(size: int) -> Image.Image:
        canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        k = (size / width) * scale
        resized = src.resize(
            (max(1, round(width * k)), max(1, round(height * k))), Image.LANCZOS
        )
        # Align the mark's centre with the canvas centre. The image centre is a
        # different point, because the glow is brighter on one side.
        canvas.paste(
            resized, (round(size / 2 - cx * k), round(size / 2 - cy * k)), resized
        )
        return canvas

    def monochrome(size: int) -> Image.Image:
        # Launchers tint this, so a soft-alpha glow would smear into a blob.
        alpha = foreground(size).getchannel("A").point(lambda v: 255 if v > 140 else 0)
        white = Image.new("L", (size, size), 255)
        return Image.merge("RGBA", (white, white, white, alpha))

    for bucket, size in BUCKETS.items():
        out = RES / f"mipmap-{bucket}"
        out.mkdir(parents=True, exist_ok=True)
        foreground(size).save(out / "ic_launcher_foreground.png")
        monochrome(size).save(out / "ic_launcher_monochrome.png")
        print(f"  mipmap-{bucket}  {size}x{size}")

    # The in-app splash shows the whole logo including the glow, with no
    # safe-zone crop — nothing masks it there.
    ASSETS.mkdir(exist_ok=True)
    src.resize((512, 512), Image.LANCZOS).save(ASSETS / "von_logo.png")
    print("  assets/von_logo.png  512x512")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    main(pathlib.Path(sys.argv[1]))
