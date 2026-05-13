"""Generate src-tauri/icons/tray-icon-template.png.

This is a macOS menu-bar template image: the system reads only the alpha
channel and tints the opaque pixels to match light/dark/highlighted menu
bar appearance. Re-run after editing the constants below:

    python3 src-tauri/icons/tray-icon-template.gen.py

Requires Pillow (pip install Pillow).
"""

from pathlib import Path
from PIL import Image, ImageDraw, ImageChops


# Width is elongated past height so the rounded rect can fit the
# bracket-and-dots cutout without crowding.
W_LOGICAL, H_LOGICAL = 52, 44

# Render at 4x then downsample with Lanczos: PIL's drawing primitives
# are not anti-aliased, so supersampling is what produces smooth
# diagonals and round caps.
SCALE = 4
W, H = W_LOGICAL * SCALE, H_LOGICAL * SCALE

PAD = 3
CORNER = 7
STROKE = 4

LEFT_BRACKET = [(19, 12), (10, 22), (19, 32)]
RIGHT_BRACKET = [(33, 12), (42, 22), (33, 32)]

# Side+middle radii (4 + 5.5) exceed center spacing (6), so the three
# circles overlap into a connected pill instead of staying as discrete
# dots.
DOTS = [(20, 22, 4.0), (26, 22, 5.5), (32, 22, 4.0)]


def _scaled(p):
    return (int(round(p[0] * SCALE)), int(round(p[1] * SCALE)))


def _stroke_polyline(draw, pts, w):
    for i in range(len(pts) - 1):
        draw.line([_scaled(pts[i]), _scaled(pts[i + 1])], fill=255, width=w)
    r = w // 2
    for p in pts:
        x, y = _scaled(p)
        draw.ellipse((x - r, y - r, x + r, y + r), fill=255)


def _dot(draw, cx, cy, r):
    cx, cy, rr = cx * SCALE, cy * SCALE, r * SCALE
    draw.ellipse((cx - rr, cy - rr, cx + rr, cy + rr), fill=255)


def main():
    bg = Image.new("L", (W, H), 0)
    ImageDraw.Draw(bg).rounded_rectangle(
        [
            (PAD * SCALE, PAD * SCALE),
            ((W_LOGICAL - PAD) * SCALE - 1, (H_LOGICAL - PAD) * SCALE - 1),
        ],
        radius=CORNER * SCALE,
        fill=255,
    )

    cut = Image.new("L", (W, H), 0)
    dc = ImageDraw.Draw(cut)
    _stroke_polyline(dc, LEFT_BRACKET, STROKE * SCALE)
    _stroke_polyline(dc, RIGHT_BRACKET, STROKE * SCALE)
    for cx, cy, r in DOTS:
        _dot(dc, cx, cy, r)

    # bg − cut: the rounded rect stays opaque except where the bracket
    # and dot shapes punch through to transparent.
    alpha = ImageChops.subtract(bg, cut)
    zero = Image.new("L", (W, H), 0)
    img = Image.merge("RGBA", (zero, zero, zero, alpha)).resize(
        (W_LOGICAL, H_LOGICAL), Image.LANCZOS
    )

    out = Path(__file__).parent / "tray-icon-template.png"
    img.save(out)
    print(f"wrote {out} {img.size}")


if __name__ == "__main__":
    main()
