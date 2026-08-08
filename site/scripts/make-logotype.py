#!/usr/bin/env python3
"""Render やめて as SVG outlines so the page ships no Japanese webfont.

The Zen Maru Gothic japanese subset is 1.4 MB at weight 700. We set three kana, once,
as a logotype — paying 1.4 MB for that is absurd, and a system-font fallback would render
the wordmark in whatever the OS happens to have. Outlines are ~4 KB, pixel-identical
everywhere, and inherit currentColor.

Regenerate with:  bun run logotype
"""

import pathlib
import sys

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont

HERE = pathlib.Path(__file__).resolve().parent
FONT = (
    HERE.parent
    / "node_modules/@fontsource/zen-maru-gothic/files/zen-maru-gothic-japanese-700-normal.woff2"
)
OUT = HERE.parent / "src/assets/yamete-logotype.svg"

TEXT = "やめて"
TRACKING = 0.04  # em, opened up slightly — at display size the default is tight


def main() -> int:
    if not FONT.exists():
        print(f"missing {FONT}\nrun `bun install` first", file=sys.stderr)
        return 1

    font = TTFont(FONT)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    upm = font["head"].unitsPerEm
    hmtx = font["hmtx"]

    paths, x = [], 0.0
    for ch in TEXT:
        name = cmap.get(ord(ch))
        if name is None:
            print(f"{ch!r} (U+{ord(ch):04X}) is not in the font", file=sys.stderr)
            return 1
        pen = SVGPathPen(glyphs)
        glyphs[name].draw(pen)
        d = pen.getCommands()
        if d:
            paths.append(f'<path transform="translate({x:.0f} 0)" d="{d}"/>')
        x += hmtx[name][0] + TRACKING * upm

    width = x - TRACKING * upm
    # Flip the y axis: font coordinates run upwards, SVG's run down.
    body = "\n    ".join(paths)
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0f} {upm}" role="img" aria-label="{TEXT}">
  <title>{TEXT}</title>
  <g fill="currentColor" transform="translate(0 {upm * 0.88:.0f}) scale(1 -1)">
    {body}
  </g>
</svg>
"""
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(svg)
    print(f"wrote {OUT}  ({len(svg) / 1024:.1f} KB, {len(paths)} glyphs, viewBox 0 0 {width:.0f} {upm})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
