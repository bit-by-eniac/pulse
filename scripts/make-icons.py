#!/usr/bin/env python3
"""Draws the app icon and writes every size/format the packages need.

    pip install pillow && python3 scripts/make-icons.py

Outputs go to assets/ (committed, so builds never need Pillow):
  icon-{16..1024}.png, icon.ico, icon.icns (macOS only: needs `iconutil`).
"""
import math
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw

OUT = Path(__file__).resolve().parent.parent / "assets"
S = 4096  # draw big, then downsample for smooth edges


def squircle_mask(size, n=5.0, inset=0.0):
    """Superellipse mask, close to the macOS app icon shape."""
    img = Image.new("L", (size, size), 0)
    px = img.load()
    r = size / 2 - inset
    c = size / 2
    for y in range(size):
        dy = abs(y - c) / r
        if dy > 1:
            continue
        # solve |x|^n = 1 - |y|^n
        dx = (1 - dy**n) ** (1 / n)
        x0, x1 = int(c - dx * r), int(c + dx * r)
        for x in range(max(0, x0), min(size, x1 + 1)):
            px[x, y] = 255
    return img


def draw_icon():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    inset = S * 0.055  # macOS icons leave a margin
    mask = squircle_mask(S // 4, inset=inset / 4).resize((S, S), Image.LANCZOS)

    # graphite body with a faint vertical gradient
    body = Image.new("RGBA", (S, S))
    d = ImageDraw.Draw(body)
    top, bottom = (0x1d, 0x1e, 0x24), (0x0b, 0x0c, 0x0f)
    for y in range(S):
        t = y / S
        d.line([(0, y), (S, y)], fill=tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,))
    img.paste(body, (0, 0), mask)

    # hairline border
    edge = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ed = ImageDraw.Draw(edge)
    border = mask.copy()
    inner = squircle_mask(S // 4, inset=inset / 4 + 6).resize((S, S), Image.LANCZOS)
    ring = Image.composite(Image.new("L", (S, S), 0), border, inner)
    edge.paste(Image.new("RGBA", (S, S), (0x3a, 0x3d, 0x4a, 255)), (0, 0), ring)
    img = Image.alpha_composite(img, edge)

    d = ImageDraw.Draw(img)
    c = S / 2
    # progress ring: a partial arc, like a scrubber
    r = S * 0.335
    w = S * 0.024
    box = [c - r, c - r, c + r, c + r]
    d.arc(box, 0, 360, fill=(0x34, 0x37, 0x42, 255), width=int(w))
    d.arc(box, -90, 150, fill=(0xf4, 0xf4, 0xf6, 255), width=int(w))
    # round caps
    for ang in (-90, 150):
        a = math.radians(ang)
        rc = r - w / 2  # PIL strokes arcs inward from the box
        x, y = c + rc * math.cos(a), c + rc * math.sin(a)
        d.ellipse([x - w / 2, y - w / 2, x + w / 2, y + w / 2], fill=(0xf4, 0xf4, 0xf6, 255))
    # the white disc and the play triangle
    rd = S * 0.205
    d.ellipse([c - rd, c - rd, c + rd, c + rd], fill=(0xf4, 0xf4, 0xf6, 255))
    tri = [(c - rd * 0.30, c - rd * 0.52), (c + rd * 0.58, c), (c - rd * 0.30, c + rd * 0.52)]
    d.polygon(tri, fill=(0x0d, 0x0e, 0x11, 255))
    return img


def main():
    OUT.mkdir(exist_ok=True)
    big = draw_icon()
    sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
    for s in sizes:
        big.resize((s, s), Image.LANCZOS).save(OUT / f"icon-{s}.png")
    big.resize((256, 256), Image.LANCZOS).save(
        OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    )
    if shutil.which("iconutil"):
        iconset = OUT / "icon.iconset"
        iconset.mkdir(exist_ok=True)
        for s in (16, 32, 128, 256, 512):
            big.resize((s, s), Image.LANCZOS).save(iconset / f"icon_{s}x{s}.png")
            big.resize((s * 2, s * 2), Image.LANCZOS).save(iconset / f"icon_{s}x{s}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(OUT / "icon.icns")], check=True)
        shutil.rmtree(iconset)
    else:
        print("iconutil not found: keeping the committed icon.icns", file=sys.stderr)
    print("icons written to", OUT)


if __name__ == "__main__":
    main()
