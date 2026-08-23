"""Three EmiWarp mark directions, rendered at 16x and downsampled.

The first attempt failed because it was four disconnected flat shapes. Every
direction here is ONE connected form with dimensional treatment and tuned
proportions — the spine's width equals the arm height, the gaps equal each
other, and the chevron depth is a constant fraction of arm length.
"""
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

SS = 16
INK    = (11, 14, 19)
SLATE  = (18, 22, 29)
AMBER  = (240, 177, 85)
EMBER  = (196, 85, 47)
PAPER  = (245, 242, 236)

# --- monogram geometry in a 64x64 design space ------------------------------
# Spine width == arm height == 9. Gaps == 7.5. Chevron depth == 7.
SPINE = (14.0, 11.0, 23.0, 53.0)
ARMS = [
    [(23, 11), (45, 11), (52, 15.5), (45, 20), (23, 20)],
    [(23, 27.5), (38, 27.5), (45, 32), (38, 36.5), (23, 36.5)],
    [(23, 44), (45, 44), (52, 48.5), (45, 53), (23, 53)],
]
BOX = (14.0, 11.0, 52.0, 53.0)


def _mask(S, frac, pad_shift=0.0):
    """Glyph coverage mask at SS resolution."""
    m = Image.new("L", (S, S), 0)
    d = ImageDraw.Draw(m)
    bw, bh = BOX[2] - BOX[0], BOX[3] - BOX[1]
    scale = (S * frac) / bw
    ox = (S - bw * scale) / 2 - BOX[0] * scale
    oy = (S - bh * scale) / 2 - BOX[1] * scale + pad_shift * S
    p = lambda x, y: (ox + x * scale, oy + y * scale)
    r = 2.6 * scale
    d.rounded_rectangle([*p(SPINE[0], SPINE[1]), *p(SPINE[2], SPINE[3])], radius=r, fill=255)
    for arm in ARMS:
        d.polygon([p(x, y) for x, y in arm], fill=255)
        # Round the arm's outer corners without rounding the chevron point.
        d.rounded_rectangle([*p(arm[0][0], arm[0][1]), *p(arm[1][0], arm[-1][1])],
                            radius=r, fill=255)
    return m


def _linear(S, top, bottom, angle=True):
    """Vertical (or diagonal) gradient as an RGB array."""
    y = np.linspace(0, 1, S)[:, None]
    x = np.linspace(0, 1, S)[None, :]
    t = (0.72 * y + 0.28 * x) if angle else np.repeat(y, S, axis=1)
    t = np.clip(t, 0, 1)[..., None]
    return (np.array(top) * (1 - t) + np.array(bottom) * t).astype(np.uint8)


def _tile(S, base_top, base_bottom, radius_frac=0.2237):
    tile = Image.fromarray(_linear(S, base_top, base_bottom)).convert("RGBA")
    shape = Image.new("L", (S, S), 0)
    ImageDraw.Draw(shape).rounded_rectangle([0, 0, S - 1, S - 1],
                                            radius=int(S * radius_frac), fill=255)
    tile.putalpha(shape)
    return tile, shape


def direction_a(size, frac=0.58):
    """Ink tile, warm gradient monogram, soft glow. Terminal-dark, premium."""
    S = size * SS
    tile, shape = _tile(S, (26, 31, 40), INK)
    m = _mask(S, frac)

    glow = m.filter(ImageFilter.GaussianBlur(S * 0.055)).point(lambda v: int(v * 0.5))
    tile.alpha_composite(Image.merge("RGBA", (
        Image.new("L", (S, S), EMBER[0]), Image.new("L", (S, S), EMBER[1]),
        Image.new("L", (S, S), EMBER[2]), glow)))

    grad = Image.fromarray(_linear(S, AMBER, EMBER)).convert("RGBA")
    grad.putalpha(m)
    tile.alpha_composite(grad)

    # Lit top edge: the glyph offset up by a hair, kept only where it overhangs.
    lip = m.transform(m.size, Image.AFFINE, (1, 0, 0, 0, 1, S * 0.006))
    edge = Image.new("RGBA", (S, S), (255, 236, 205, 0))
    edge.putalpha(Image.fromarray(
        np.clip(np.array(m, int) - np.array(lip, int), 0, 255).astype(np.uint8)
    ).point(lambda v: int(v * 0.55)))
    tile.alpha_composite(edge)

    tile.putalpha(Image.fromarray(
        (np.array(tile.split()[3], int) * (np.array(shape, int) / 255)).astype(np.uint8)))
    return tile.resize((size, size), Image.LANCZOS)


def direction_b(size, frac=0.58):
    """Warm gradient tile, monogram knocked out to ink. Bold, high-contrast."""
    S = size * SS
    tile, shape = _tile(S, AMBER, EMBER)
    m = _mask(S, frac)
    knock = Image.new("RGBA", (S, S), (*INK, 0))
    knock.putalpha(m)
    tile.alpha_composite(knock)
    tile.putalpha(Image.fromarray(
        (np.array(tile.split()[3], int) * (np.array(shape, int) / 255)).astype(np.uint8)))
    return tile.resize((size, size), Image.LANCZOS)


def direction_c(size, frac=0.62):
    """Warp gate: three nested chevrons receding. Motion through depth."""
    S = size * SS
    tile, shape = _tile(S, (26, 31, 40), INK)
    d = ImageDraw.Draw(tile, "RGBA")
    cx, cy = S / 2, S / 2
    w = S * frac
    for i, (sc, alpha, col) in enumerate([(1.0, 255, AMBER), (0.66, 170, (225, 140, 66)), (0.36, 95, EMBER)]):
        half, thick = w * sc / 2, S * 0.085 * (0.62 + 0.38 * sc)
        d.line([(cx - half, cy - half * 0.82), (cx + half * 0.5, cy), (cx - half, cy + half * 0.82)],
               fill=(*col, alpha), width=int(thick), joint="curve")
    tile.putalpha(Image.fromarray(
        (np.array(tile.split()[3], int) * (np.array(shape, int) / 255)).astype(np.uint8)))
    return tile.resize((size, size), Image.LANCZOS)


DIRECTIONS = {"A": direction_a, "B": direction_b, "C": direction_c}
