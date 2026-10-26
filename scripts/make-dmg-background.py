#!/usr/bin/env python3
"""Makes the background of the macOS disk image window: the glow of the
generated art in light greys, with an arrow from Pulse.app to Applications.

    pip install pillow && python3 scripts/make-dmg-background.py

Reads packaging/macos/dmg-background-source.png (generated stand-in art, may be
replaced) and writes dmg-background.jpg next to it (committed, so builds never
need Pillow). The window size and icon positions must match dmg-settings.py.

Light on purpose: Finder draws the icon labels in black over a background
picture, even in dark mode, and has no setting for their colour.
One 2x JPEG marked 144 dpi: Finder shows it at 640x400 points, sharp on Retina,
for a fraction of the size of the PNG + @2x TIFF pair.
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageOps

DIR = Path(__file__).resolve().parent.parent / "packaging" / "macos"
W, H = 640, 400  # window content, in points
SCALE = 2
APP_X, APPS_X, ICON_Y = 170, 470, 185  # icon centres (dmg-settings.py)
EDGE = (0xD3, 0xD5, 0xDB)  # where the art is darkest
CENTER = (0xF4, 0xF4, 0xF6)  # the app's chalk white, where it glows
INK = (0x0D, 0x0E, 0x11)  # the app's background colour, for the arrow


def art():
    src = Image.open(DIR / "dmg-background-source.png").convert("L")
    # Centre crop to the window's 16:10.
    w = min(src.width, round(src.height * W / H))
    h = round(w * H / W)
    x, y = (src.width - w) // 2, (src.height - h) // 2
    # Blurred: the art's fine grain would only cost bytes.
    glow = src.crop((x, y, x + w, y + h)).filter(ImageFilter.GaussianBlur(6))
    glow = ImageOps.autocontrast(glow.resize((W * SCALE, H * SCALE), Image.LANCZOS))
    return ImageOps.colorize(glow, EDGE, CENTER)


def arrow():
    """A hairline arrow between the icons, like the app's own icons."""
    ss = 4  # supersampling for smooth edges
    k = SCALE * ss
    layer = Image.new("L", (W * k, H * k), 0)
    d = ImageDraw.Draw(layer)
    width = round(2 * k)
    x0, x1, y = (APP_X + 82) * k, (APPS_X - 82) * k, ICON_Y * k
    head = 9 * k
    d.line([(x0, y), (x1, y)], fill=255, width=width)
    d.line([(x1 - head, y - head), (x1, y), (x1 - head, y + head)], fill=255, width=width, joint="curve")
    for px, py in [(x0, y), (x1 - head, y - head), (x1 - head, y + head), (x1, y)]:
        r = width / 2
        d.ellipse([px - r, py - r, px + r, py + r], fill=255)  # round caps
    return layer.resize((W * SCALE, H * SCALE), Image.LANCZOS)


def main():
    img = art()
    img.paste(Image.new("RGB", img.size, INK), (0, 0), arrow().point(lambda v: v * 0.45))
    out = DIR / "dmg-background.jpg"
    img.save(out, quality=94, dpi=(72 * SCALE, 72 * SCALE), optimize=True)
    print("->", out)


if __name__ == "__main__":
    main()
