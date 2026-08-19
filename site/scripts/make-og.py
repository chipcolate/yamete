#!/usr/bin/env python3
"""Generate public/og.png and public/favicon.png from the app artwork.

The share card is built from the same pieces as the page — the sampled gradient, the app
icon, and Zen Maru Gothic for the wordmark — so a link preview looks like the site rather
than like a screenshot of it.

The tab favicon is the laptop on a transparent canvas.

Regenerate with:  bun run og
"""

import io
import pathlib
import sys
from collections import deque

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


def _flood(pix, w: int, h: int, seeds: list[tuple[int, int]], fuzz: int) -> None:
    """Knock out pixels within `fuzz` of each seed's colour, 4-connected."""

    def close(c, seed) -> bool:
        return max(abs(c[0] - seed[0]), abs(c[1] - seed[1]), abs(c[2] - seed[2])) <= fuzz

    seen = bytearray(w * h)
    q: deque[tuple[int, int]] = deque()
    for x, y in seeds:
        if not (0 <= x < w and 0 <= y < h):
            continue
        r, g, b, a = pix[x, y]
        if a == 0:
            continue
        seed = (r, g, b)
        q.clear()
        i = y * w + x
        if seen[i]:
            continue
        seen[i] = 1
        q.append((x, y))
        while q:
            cx, cy = q.popleft()
            cr, cg, cb, _ = pix[cx, cy]
            pix[cx, cy] = (cr, cg, cb, 0)
            for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                if not (0 <= nx < w and 0 <= ny < h):
                    continue
                ni = ny * w + nx
                if seen[ni]:
                    continue
                nr, ng, nb, na = pix[nx, ny]
                if na == 0 or (nr < 90 and ng < 70 and nb < 80):
                    seen[ni] = 1
                    continue
                if close((nr, ng, nb), seed):
                    seen[ni] = 1
                    q.append((nx, ny))


def laptop_mark(icon: Image.Image, size: int = 256) -> Image.Image:
    """The cartoon laptop, with the squircle tile and drop shadow knocked out."""
    im = icon.convert("RGBA")
    w, h = im.size
    pix = im.load()
    _flood(
        pix,
        w,
        h,
        [
            (200, 200),
            (512, 200),
            (800, 200),
            (200, 400),
            (800, 400),
            (200, 600),
            (800, 650),
            (200, 800),
            (512, 920),
            (800, 800),
            (150, 300),
            (870, 300),
            (150, 500),
            (870, 500),
            (300, 180),
            (700, 180),
            (400, 160),
            (600, 160),
            (250, 850),
            (750, 850),
            (400, 880),
            (624, 880),
        ],
        fuzz=32,
    )
    # Peach oval under the chassis is the tile's drop shadow, not the drawing.
    _flood(
        pix,
        w,
        h,
        [
            (300, 790),
            (400, 800),
            (512, 810),
            (650, 800),
            (750, 790),
            (250, 760),
            (780, 760),
            (350, 820),
            (600, 820),
        ],
        fuzz=28,
    )

    label = [-1] * (w * h)
    sizes: list[int] = []
    for y in range(h):
        for x in range(w):
            i = y * w + x
            if pix[x, y][3] == 0 or label[i] >= 0:
                continue
            cid = len(sizes)
            n = 0
            q: deque[tuple[int, int]] = deque([(x, y)])
            label[i] = cid
            while q:
                cx, cy = q.popleft()
                n += 1
                for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                    if not (0 <= nx < w and 0 <= ny < h):
                        continue
                    ni = ny * w + nx
                    if pix[nx, ny][3] == 0 or label[ni] >= 0:
                        continue
                    label[ni] = cid
                    q.append((nx, ny))
            sizes.append(n)
    keep = max(range(len(sizes)), key=lambda i: sizes[i])
    minx, miny, maxx, maxy = w, h, 0, 0
    for y in range(h):
        for x in range(w):
            i = y * w + x
            if pix[x, y][3] == 0:
                continue
            if label[i] != keep:
                pix[x, y] = (0, 0, 0, 0)
                continue
            minx = min(minx, x)
            miny = min(miny, y)
            maxx = max(maxx, x)
            maxy = max(maxy, y)

    pad = int(0.07 * max(maxx - minx + 1, maxy - miny + 1))
    minx = max(0, minx - pad)
    miny = max(0, miny - pad)
    maxx = min(w - 1, maxx + pad)
    maxy = min(h - 1, maxy + pad)
    cropped = im.crop((minx, miny, maxx + 1, maxy + 1))
    side = max(cropped.size)
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.paste(cropped, ((side - cropped.size[0]) // 2, (side - cropped.size[1]) // 2), cropped)
    return canvas.resize((size, size), Image.LANCZOS)


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
    laptop_mark(icon).save(FAVICON, optimize=True)

    print(f"wrote {OG} ({OG.stat().st_size // 1024} KB)")
    print(f"wrote {FAVICON} ({FAVICON.stat().st_size // 1024} KB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
