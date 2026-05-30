#!/usr/bin/env python3
"""Generate assets/mighty-ide.ico — the Mighty brand "M" mark.

Vivid-Modern look: an ember (#F4A259) chevron-M stroked on a near-black rounded
tile, rendered at 16/32/48/256 and assembled into a single multi-resolution .ico.

The glyph mirrors the IDE's in-app `icons::LANG_M` path `M4 18 V8 l5 4 l5 -4 v10`
(a 24-unit viewBox), so the desktop/Explorer icon matches the Welcome-screen mark.

Run: python tools/make-icon.py  (writes assets/mighty-ide.ico)
"""
import os
from PIL import Image, ImageDraw

# Brand palette (Vivid-Modern).
EMBER = (244, 162, 89, 255)        # #F4A259 accent
EMBER_HI = (255, 196, 130, 255)    # lighter top of the gradient
NEAR_BLACK = (18, 18, 22, 255)     # tile fill
TILE_HI = (34, 34, 42, 255)        # subtle top sheen

# The LANG_M polyline on a 24-unit viewBox: (4,18)->(4,8)->(9,12)->(14,8)->(14,18).
GLYPH = [(4, 18), (4, 8), (9, 12), (14, 8), (14, 18)]
# Center the 4..14 (x) / 8..18 (y) extents in the 24 box: shift +3 x, -1 y so the
# mark sits optically centered.
GLYPH = [(x + 3, y - 1) for (x, y) in GLYPH]

SS = 8  # supersample factor for crisp antialiasing, then downscale.


def render(size: int) -> Image.Image:
    """Render one square icon image at `size` px."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Rounded tile with a faint vertical sheen (two bands).
    radius = int(s * 0.22)
    inset = max(1, int(s * 0.02))
    d.rounded_rectangle([inset, inset, s - inset, s - inset], radius=radius, fill=NEAR_BLACK)
    # Top sheen band (slightly lighter) clipped to the upper third.
    sheen = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    sd = ImageDraw.Draw(sheen)
    sd.rounded_rectangle([inset, inset, s - inset, s - inset], radius=radius, fill=TILE_HI)
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rectangle([0, 0, s, int(s * 0.45)], fill=110)
    img.paste(sheen, (0, 0), mask)

    # Ember "M" stroke. Scale the 24-unit glyph into the tile's safe area.
    pad = s * 0.26
    span = s - 2 * pad
    pts = [(pad + (x / 24.0) * span, pad + (y / 24.0) * span) for (x, y) in GLYPH]
    width = max(SS, int(s * 0.085))

    # Soft ember glow under the stroke (wider, translucent).
    glow_w = int(width * 2.0)
    d.line(pts, fill=(244, 162, 89, 70), width=glow_w, joint="curve")
    for px, py in pts:
        r = glow_w / 2
        d.ellipse([px - r, py - r, px + r, py + r], fill=(244, 162, 89, 50))

    # Main stroke (ember), rounded joins/caps via overdrawn end dots.
    d.line(pts, fill=EMBER, width=width, joint="curve")
    r = width / 2
    for px, py in pts:
        d.ellipse([px - r, py - r, px + r, py + r], fill=EMBER)
    # Highlight along the two upstrokes' tops for a lit feel.
    d.line([pts[1], pts[2]], fill=EMBER_HI, width=max(SS, int(width * 0.5)), joint="curve")
    d.line([pts[3], pts[2]], fill=EMBER_HI, width=max(SS, int(width * 0.5)), joint="curve")

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.normpath(os.path.join(here, "..", "assets", "mighty-ide.ico"))
    sizes = [16, 32, 48, 256]
    base = render(256)
    # Pillow writes a multi-resolution .ico from one image + a `sizes` list,
    # rendering each from the largest; supply our own per-size renders for
    # crispness at small sizes by passing them via append_images-equivalent.
    imgs = {sz: render(sz) for sz in sizes}
    base = imgs[256]
    base.save(
        out,
        format="ICO",
        sizes=[(sz, sz) for sz in sizes],
        append_images=[imgs[sz] for sz in sizes if sz != 256],
    )
    print(f"wrote {out} ({os.path.getsize(out)} bytes; sizes={sizes})")


if __name__ == "__main__":
    main()
