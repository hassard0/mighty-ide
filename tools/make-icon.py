#!/usr/bin/env python3
"""Generate assets/mighty-ide.ico — the Mighty brand mark.

The taskbar icon has to work at 16px, so this is deliberately simpler than the
large Welcome art: a high-contrast Mighty "M" stroke on a saturated tile,
rendered separately at 16/32/48/256 and assembled into one multi-resolution ico.

The glyph mirrors the IDE's in-app `icons::LANG_M` path `M5 18 V7 l7 6 l7 -6 v11`
(a 24-unit viewBox), so the desktop/Explorer icon matches the UI mark.

Run: python tools/make-icon.py  (writes assets/mighty-ide.ico)
"""
import os
from PIL import Image, ImageDraw

# Brand palette (Vivid-Modern, but with enough contrast for the taskbar).
ACCENT_TOP = (157, 121, 255, 255)      # bright indigo
ACCENT_BOTTOM = (86, 61, 181, 255)     # deep violet
ACCENT_EDGE = (196, 176, 255, 255)
INK = (255, 255, 255, 255)
INK_SHADOW = (25, 19, 56, 110)

# The centered LANG_M polyline on a 24-unit viewBox:
# (5,18)->(5,7)->(12,13)->(19,7)->(19,18).
GLYPH = [(5, 18), (5, 7), (12, 13), (19, 7), (19, 18)]

SS = 8  # supersample factor for crisp antialiasing, then downscale.


def render(size: int) -> Image.Image:
    """Render one square icon image at `size` px."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Rounded violet tile. At 16px the tile itself must carry the silhouette,
    # so use a strong fill and only subtle depth.
    radius = int(s * 0.22)
    inset = max(1, int(s * 0.02))
    for y in range(inset, s - inset + 1):
        t = (y - inset) / max(1, (s - 2 * inset))
        col = tuple(int(ACCENT_TOP[i] * (1 - t) + ACCENT_BOTTOM[i] * t) for i in range(4))
        d.line([(inset, y), (s - inset, y)], fill=col, width=1)
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle([inset, inset, s - inset, s - inset], radius=radius, fill=255)
    tile = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    tile.paste(img, (0, 0), mask)
    img = tile
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([inset, inset, s - inset, s - inset], radius=radius, outline=ACCENT_EDGE, width=max(1, SS))

    # White Mighty "M" stroke. Scale the 24-unit glyph into the tile's safe area.
    pad = s * 0.18
    span = s - 2 * pad
    pts = [(pad + (x / 24.0) * span, pad + (y / 24.0) * span) for (x, y) in GLYPH]
    width = max(SS * 2, int(s * 0.12))

    # Shadow then main stroke, with rounded joins/caps via overdrawn dots.
    shadow_pts = [(x + max(1, s * 0.012), y + max(1, s * 0.018)) for x, y in pts]
    d.line(shadow_pts, fill=INK_SHADOW, width=int(width * 1.15), joint="curve")
    d.line(pts, fill=INK, width=width, joint="curve")
    r = width / 2
    for px, py in pts:
        d.ellipse([px - r, py - r, px + r, py + r], fill=INK)

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.normpath(os.path.join(here, "..", "assets", "mighty-ide.ico"))
    preview = os.path.normpath(os.path.join(here, "..", "dist", "icon-preview.png"))
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
    os.makedirs(os.path.dirname(preview), exist_ok=True)
    base.save(preview, format="PNG")
    print(f"wrote {out} ({os.path.getsize(out)} bytes; sizes={sizes})")


if __name__ == "__main__":
    main()
