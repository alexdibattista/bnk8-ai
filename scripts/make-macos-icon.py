#!/usr/bin/env python3
"""Builds the macOS app icon master from the full-bleed artwork.

    gui/assets/app-icon/icon-full-bleed-1024.png  ->  gui/assets/app-icon/macos-1024.png

macOS does not mask app icons, so the master follows Apple's icon grid: an
824 x 824 rounded square (corner radius 22.37%) centred on a transparent
1024 x 1024 canvas, with a soft drop shadow. scripts/release-macos.sh turns
it into AppIcon.icns. Rerun after changing the artwork; needs Pillow.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent / "gui" / "assets" / "app-icon"
CANVAS = 1024
BODY = 824
RADIUS = round(BODY * 0.2237)
SCALE = 4  # supersampling for a smooth mask edge
SHADOW_OFFSET = 10
SHADOW_BLUR = 18
SHADOW_ALPHA = 0.3


def rounded_mask(size: int, radius: int) -> Image.Image:
    big = Image.new("L", (size * SCALE, size * SCALE), 0)
    ImageDraw.Draw(big).rounded_rectangle(
        (0, 0, size * SCALE - 1, size * SCALE - 1), radius=radius * SCALE, fill=255
    )
    return big.resize((size, size), Image.LANCZOS)


def main() -> None:
    art = Image.open(ROOT / "icon-full-bleed-1024.png").convert("RGBA").resize((BODY, BODY), Image.LANCZOS)
    mask = rounded_mask(BODY, RADIUS)
    art.putalpha(ImageChops.multiply(art.getchannel("A"), mask))

    origin = (CANVAS - BODY) // 2
    shadow_alpha = Image.new("L", (CANVAS, CANVAS), 0)
    shadow_alpha.paste(mask.point(lambda a: round(a * SHADOW_ALPHA)), (origin, origin + SHADOW_OFFSET))
    shadow = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    shadow.putalpha(shadow_alpha.filter(ImageFilter.GaussianBlur(SHADOW_BLUR)))

    icon = Image.alpha_composite(shadow, _placed(art, origin))
    icon.save(ROOT / "macos-1024.png", optimize=True)


def _placed(layer: Image.Image, origin: int) -> Image.Image:
    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    canvas.paste(layer, (origin, origin))
    return canvas


if __name__ == "__main__":
    main()
