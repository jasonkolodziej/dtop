#!/usr/bin/env python3
"""Builds dtop's brand files in docs/brand/ from the logo mark's geometry.

    python3 scripts/brand/build.py            # SVGs only (needs fontTools)
    python3 scripts/brand/build.py --png      # also PNG exports (needs Node with Playwright)

The mark is a terminal prompt beside three rising metric bars on a rounded dark tile. The
wordmark is "dtop" in JetBrains Mono ExtraBold (SIL Open Font License), converted to outlines so
the SVGs look the same everywhere, with no font needed. Point DTOP_BRAND_FONT at the .ttf if it is
not in the usual place.
"""
import os
import subprocess
import sys
from pathlib import Path

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "docs" / "brand"
FONT = os.environ.get("DTOP_BRAND_FONT", "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-ExtraBold.ttf")

# Brand palette. The in-terminal mark uses the active theme's accent, success, memory and gpu roles,
# which are close to these in the default "harbor" theme.
COLORS = {
    "accent": "#4fa3e8",   # prompt chevron and baseline
    "green": "#4fd18b",    # short bar
    "purple": "#b58af0",   # middle bar
    "pink": "#f0679a",     # tall bar
    "tile": "#111823",
    "tileStroke": "#2a3a52",
    "ink": "#0a0e14",      # page background, wordmark on light backgrounds
    "paper": "#f2f6fb",    # wordmark on dark backgrounds
}


def mark(indent="  "):
    """The mark's shapes in a 160×160 coordinate space."""
    c = COLORS
    return "\n".join(indent + line for line in [
        f'<rect x="4" y="4" width="152" height="152" rx="38" fill="{c["tile"]}" stroke="{c["tileStroke"]}" stroke-width="3"/>',
        f'<path d="M40 54 L66 80 L40 106" fill="none" stroke="{c["accent"]}" stroke-width="13" stroke-linecap="round" stroke-linejoin="round"/>',
        f'<rect x="82" y="80" width="13" height="30" rx="3" fill="{c["green"]}"/>',
        f'<rect x="101" y="62" width="13" height="48" rx="3" fill="{c["purple"]}"/>',
        f'<rect x="120" y="42" width="13" height="68" rx="3" fill="{c["pink"]}"/>',
        f'<rect x="40" y="120" width="93" height="6" rx="3" fill="{c["accent"]}" opacity="0.35"/>',
    ])


def icon_svg():
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" width="160" height="160" viewBox="0 0 160 160" '
        'role="img" aria-label="dtop">\n'
        "  <title>dtop</title>\n"
        f"{mark()}\n"
        "</svg>\n"
    )


def wordmark(text="dtop", size=160.0, tracking=-5.7):
    """Returns (svg path data, width, cap-free ascent box) for `text` at `size` px, baseline at y=0."""
    font = TTFont(FONT)
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    scale = size / font["head"].unitsPerEm
    pen = SVGPathPen(glyphs)
    bounds = BoundsPen(glyphs)
    x = 0.0
    for index, char in enumerate(text):
        name = cmap[ord(char)]
        transform = (scale, 0, 0, -scale, x, 0)  # font units are y-up; SVG is y-down
        glyphs[name].draw(TransformPen(pen, transform))
        glyphs[name].draw(TransformPen(bounds, transform))
        x += glyphs[name].width * scale
        if index < len(text) - 1:
            x += tracking
    return pen.getCommands(), bounds.bounds  # bounds: (xMin, yMin, xMax, yMax), y-down


def logo_svg(ink):
    """Horizontal lockup: the mark, then the wordmark, vertically centered on it."""
    path, (x_min, y_min, x_max, y_max) = wordmark()
    gap = 34
    text_height = y_max - y_min
    # Center the wordmark's ink on the mark's 160 px height.
    dy = (160 - text_height) / 2 - y_min
    dx = 160 + gap - x_min
    width = round(160 + gap + (x_max - x_min))
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="160" viewBox="0 0 {width} 160" '
        'role="img" aria-label="dtop">\n'
        "  <title>dtop</title>\n"
        f"{mark()}\n"
        f'  <path transform="translate({dx:.2f} {dy:.2f})" fill="{ink}" d="{path}"/>\n'
        "</svg>\n"
    )


def write(name, text):
    (OUT / name).write_text(text)
    print("wrote", (OUT / name).relative_to(ROOT))


def pngs():
    """PNG exports of the icon, rendered by Playwright's Chromium through png.js."""
    subprocess.run(["node", str(Path(__file__).with_name("png.js"))], check=True)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    write("dtop-icon.svg", icon_svg())
    write("dtop-logo-dark.svg", logo_svg(COLORS["paper"]))  # for dark backgrounds
    write("dtop-logo-light.svg", logo_svg(COLORS["ink"]))   # for light backgrounds
    if "--png" in sys.argv:
        pngs()


if __name__ == "__main__":
    main()
