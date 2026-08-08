#!/usr/bin/env python3
"""Generate public/og.png and public/favicon.png from the app artwork.

The share card is built from the same pieces as the page — the sampled gradient, the app
icon, and Zen Maru Gothic for the wordmark — so a link preview looks like the site rather
than like a screenshot of it.

Regenerate with:  bun run og
"""

import io
import pathlib
import sys

import numpy as np
from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

HERE = pathlib.Path(__file__).resolve().parent
SITE = HERE.parent
ICON = SITE.parent / "app-icon.png"
FONTS = SITE / "node_modules/@fontsource"
OG = SITE / "public/og.png"
FAVICON = SITE / "public/favicon.png"

W, H = 1200, 630
INK = (56, 24, 37)
MUTED = (92, 69, 80)
# The three gradient stops sampled from yamete.png, matching tokens.css.
STOPS = [(0.0, (198, 233, 252)), (0.42, (241, 228, 220)), (1.0, (255, 218, 199))]
ANGLE_DEG = 163


def load_font(pkg: str, filename: str, size: int) -> ImageFont.FreeTypeFont:
    """PIL cannot read woff2, so unpack it to an in-memory TTF first."""
    path = FONTS / pkg / "files" / filename
    if not path.exists():
        print(f"missing {path}\nrun `bun install` first", file=sys.stderr)
        raise SystemExit(1)
    font = TTFont(path)
    buf = io.BytesIO()
    font.flavor = None
    font.save(buf)
    buf.seek(0)
    return ImageFont.truetype(buf, size)


def gradient() -> Image.Image:
    """A linear gradient along ANGLE_DEG, matching the CSS on the page."""
    # CSS angles run clockwise from "to top"; convert to a vector in image space.
    theta = np.deg2rad(ANGLE_DEG)
    dx, dy = np.sin(theta), -np.cos(theta)
    xs = np.linspace(0, W - 1, W)[None, :]
    ys = np.linspace(0, H - 1, H)[:, None]
    proj = xs * dx + ys * dy
    t = (proj - proj.min()) / (proj.max() - proj.min())

    out = np.zeros((H, W, 3))
    for (t0, c0), (t1, c1) in zip(STOPS, STOPS[1:]):
        band = (t >= t0) & (t <= t1)
        local = np.clip((t - t0) / (t1 - t0), 0, 1)
        for ch in range(3):
            out[..., ch] = np.where(band, c0[ch] + (c1[ch] - c0[ch]) * local, out[..., ch])
    return Image.fromarray(out.astype("uint8"), "RGB")


def main() -> int:
    card = gradient()
    icon = Image.open(ICON).convert("RGBA")

    size = 300
    card.paste(icon.resize((size, size), Image.LANCZOS), (W - size - 90, (H - size) // 2), icon.resize((size, size), Image.LANCZOS))

    d = ImageDraw.Draw(card)
    kana = load_font("zen-maru-gothic", "zen-maru-gothic-japanese-700-normal.woff2", 132)
    body = load_font("zen-kaku-gothic-new", "zen-kaku-gothic-new-latin-500-normal.woff2", 40)
    small = load_font("zen-kaku-gothic-new", "zen-kaku-gothic-new-latin-400-normal.woff2", 25)

    x = 90
    d.text((x, 150), "やめて", font=kana, fill=INK)
    d.text((x + 4, 320), "Slap detection for", font=body, fill=INK)
    d.text((x + 4, 372), "Apple Silicon MacBooks", font=body, fill=INK)
    d.text((x + 4, 448), "Hit the laptop, it makes a noise.", font=small, fill=MUTED)

    OG.parent.mkdir(parents=True, exist_ok=True)
    card.save(OG, optimize=True)
    icon.resize((256, 256), Image.LANCZOS).save(FAVICON, optimize=True)

    print(f"wrote {OG} ({OG.stat().st_size // 1024} KB)")
    print(f"wrote {FAVICON} ({FAVICON.stat().st_size // 1024} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
