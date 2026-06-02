#!/usr/bin/env python3
"""Generate assets/mighty-ide.ico — the Mighty brand mark.

The taskbar icon has to work at 16px, so this is deliberately simpler than the
large Welcome art: a high-contrast Mighty "M" on a quiet editor tile, rendered
at the common Windows shell sizes and assembled into one multi-resolution ico.

The glyph mirrors the IDE's in-app `icons::LANG_M_FILL` path, so the
desktop/Explorer icon matches the UI mark.

Run: python tools/make-icon.py  (writes assets/mighty-ide.ico)
"""
import os
import struct
from PIL import Image, ImageDraw

# Brand palette. The dark tile reads like an IDE in the taskbar; cyan/violet are
# accents instead of large shapes so the small icon stays balanced.
TILE_TOP = (18, 25, 39, 255)
TILE_BOTTOM = (5, 8, 18, 255)
ACCENT_TEAL = (76, 229, 218, 255)
ACCENT_VIOLET = (126, 95, 255, 255)
ACCENT_EDGE = (110, 241, 233, 235)
INK = (165, 255, 248, 255)
INK_HIGHLIGHT = (255, 255, 255, 245)
INK_SHADOW = (43, 24, 109, 170)

# Filled Mighty monogram on a 24-unit viewBox.
GLYPH = [(4.5, 18.5), (4.5, 5.5), (8.3, 5.5), (12, 11.1), (15.7, 5.5), (19.5, 5.5), (19.5, 18.5), (15.8, 18.5), (15.8, 11.6), (12, 17), (8.2, 11.6), (8.2, 18.5)]

SS = 8  # supersample factor for crisp antialiasing, then downscale.


def render(size: int) -> Image.Image:
    """Render one square icon image at `size` px."""
    s = size * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Rounded brand tile. At 16px the tile itself must carry the silhouette, so
    # use a strong fill, modest radius, and no glossy stripe that can alias into
    # taskbar noise.
    compact = size <= 32
    radius = int(s * (0.09 if compact else 0.11))
    inset = max(1, int(s * 0.02))
    for y in range(inset, s - inset + 1):
        t = (y - inset) / max(1, (s - 2 * inset))
        col = tuple(int(TILE_TOP[i] * (1 - t) + TILE_BOTTOM[i] * t) for i in range(4))
        d.line([(inset, y), (s - inset, y)], fill=col, width=1)
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle([inset, inset, s - inset, s - inset], radius=radius, fill=255)
    tile = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    tile.paste(img, (0, 0), mask)
    img = tile
    d = ImageDraw.Draw(img)

    # Accent frame: at taskbar sizes use one crisp outline and skip the nested
    # frame, because two thin rings collapse into fuzzy corners at 16/32px.
    edge_w = max(SS, int(s * (0.022 if compact else 0.018)))
    d.rounded_rectangle(
        [inset, inset, s - inset, s - inset],
        radius=radius,
        outline=ACCENT_EDGE,
        width=edge_w,
    )
    if not compact:
        d.rounded_rectangle(
            [inset + edge_w * 2, inset + edge_w * 2, s - inset - edge_w * 2, s - inset - edge_w * 2],
            radius=max(1, radius - edge_w * 2),
            outline=ACCENT_VIOLET,
            width=max(edge_w, int(s * 0.014)),
        )

    # Mighty monogram. At 16/20/24px the mark needs more optical weight than the
    # large Welcome tile; the shell will shrink it again in the taskbar.
    if size <= 24:
        pad = s * 0.035
    elif compact:
        pad = s * 0.065
    else:
        pad = s * 0.13
    span = s - 2 * pad
    pts = [(pad + (x / 24.0) * span, pad + (y / 24.0) * span) for (x, y) in GLYPH]
    shadow_pts = [(x + max(1, s * 0.012), y + max(1, s * 0.018)) for x, y in pts]
    d.polygon(shadow_pts, fill=INK_SHADOW)
    d.polygon(pts, fill=INK)
    shine = [(x, y - max(1, s * 0.006)) for x, y in pts]
    d.line(shine[:4], fill=INK_HIGHLIGHT, width=max(1, int(s * 0.01)))

    return img.resize((size, size), Image.LANCZOS)


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    out = os.path.normpath(os.path.join(here, "..", "assets", "mighty-ide.ico"))
    preview = os.path.normpath(os.path.join(here, "..", "dist", "icon-preview.png"))
    strip_preview = os.path.normpath(os.path.join(here, "..", "dist", "icon-sizes-preview.png"))
    # Include the common Windows shell/taskbar sizes, not just the classic ICO
    # quartet, so Explorer and the taskbar do not have to resample a near miss.
    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    imgs = [render(sz) for sz in sizes]
    # Write classic BGRA DIB icon entries rather than PNG-compressed entries.
    # Windows accepts PNG ICOs, but some shell/taskbar/System.Drawing paths render
    # them as noise. DIB entries are larger but boring and reliable.
    entries = [_ico_dib(img) for img in imgs]
    header_size = 6 + 16 * len(entries)
    offset = header_size
    with open(out, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(entries)))
        for img, data in zip(imgs, entries):
            w, h = img.size
            f.write(struct.pack("<BBBBHHII", 0 if w == 256 else w, 0 if h == 256 else h, 0, 0, 1, 32, len(data), offset))
            offset += len(data)
        for data in entries:
            f.write(data)
    os.makedirs(os.path.dirname(preview), exist_ok=True)
    imgs[-1].save(preview, format="PNG")
    _save_size_strip(imgs, sizes, strip_preview)
    print(f"wrote {out} ({os.path.getsize(out)} bytes; sizes={sizes})")


def _save_size_strip(imgs, sizes, path: str) -> None:
    """Save a human-checkable preview of every generated icon size."""
    cell = 72
    label_h = 18
    preview = Image.new("RGBA", (cell * len(imgs), cell + label_h), (7, 10, 21, 255))
    d = ImageDraw.Draw(preview)
    for i, (img, size) in enumerate(zip(imgs, sizes)):
        shown = img
        if size > 56:
            shown = img.resize((56, 56), Image.LANCZOS)
        shown_w, shown_h = shown.size
        x = i * cell + (cell - shown_w) // 2
        y = (cell - shown_h) // 2
        preview.alpha_composite(shown, (x, y))
        d.text((i * cell + 8, cell + 2), f"{size}px", fill=(185, 190, 210, 255))
    preview.save(path, format="PNG")


def _ico_dib(img: Image.Image) -> bytes:
    img = img.convert("RGBA")
    w, h = img.size
    pixels = img.load()
    xor = bytearray()
    for y in range(h - 1, -1, -1):
        for x in range(w):
            r, g, b, a = pixels[x, y]
            xor.extend((b, g, r, a))
    mask_stride = ((w + 31) // 32) * 4
    and_mask = bytes(mask_stride * h)
    header = struct.pack(
        "<IIIHHIIIIII",
        40,
        w,
        h * 2,
        1,
        32,
        0,
        len(xor),
        0,
        0,
        0,
        0,
    )
    return header + bytes(xor) + and_mask


if __name__ == "__main__":
    main()
