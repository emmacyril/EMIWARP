"""EmiWarp icon — the supplied logo, unaltered, in the specified colours.

Geometry measured off the supplied PNG. No texture, no bloom, no glow: flat
fills exactly as in the original, with only the three tones substituted.
"""
import numpy as np
from PIL import Image, ImageDraw

SS = 8
INK = (0, 0, 0)
BIG_C     = (161, 178, 196)   # #A1B2C4
SMALL_C   = (179, 196, 213)   # #B3C4D5
OVERLAP_C = (200, 217, 234)   # #C8D9EA

BIG   = (0.1747, 0.8343, 0.1075)
SMALL = (0.1270, 0.8728, 0.0641)


def _cov(S, cx, cy, r):
    yy, xx = np.mgrid[0:S, 0:S].astype(np.float32)
    d = np.sqrt((xx - cx * S) ** 2 + (yy - cy * S) ** 2)
    return np.clip((r * S - d) / max(1.0, S * 0.0012), 0, 1)


def render(size):
    S = size * SS
    img = np.zeros((S, S, 3), np.float32)
    img[:] = INK

    cb, cs = _cov(S, *BIG), _cov(S, *SMALL)
    tone = (np.array(BIG_C, np.float32) * (cb * (1 - cs))[..., None]
            + np.array(SMALL_C, np.float32) * (cs * (1 - cb))[..., None]
            + np.array(OVERLAP_C, np.float32) * (cb * cs)[..., None])
    cover = np.clip(cb + cs - cb * cs, 0, 1)
    img = img * (1 - cover[..., None]) + tone

    out = Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).convert("RGBA")
    shape = Image.new("L", (S, S), 0)
    ImageDraw.Draw(shape).rounded_rectangle([0, 0, S - 1, S - 1],
                                            radius=int(S * 0.2237), fill=255)
    out.putalpha(shape)
    return out.resize((size, size), Image.LANCZOS)
