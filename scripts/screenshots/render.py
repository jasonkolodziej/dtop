"""Render captured terminal cells (from capture.py) to PNG screenshots with a window frame."""
import json, sys, os
from PIL import Image, ImageDraw, ImageFont
from fontTools.ttLib import TTFont

SCALE = 2
SIZE = 13 * SCALE
# Override with DTOP_FONT, DTOP_FONT_BOLD and DTOP_FONT_FALLBACKS (colon-separated) on other systems.
FONTS = {
    False: os.environ.get("DTOP_FONT", "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf"),
    True: os.environ.get("DTOP_FONT_BOLD", "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Bold.ttf"),
}
FALLBACKS = os.environ.get(
    "DTOP_FONT_FALLBACKS",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf:/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
).split(":")

fonts = {bold: ImageFont.truetype(path, SIZE) for bold, path in FONTS.items()}
fallback_fonts = [ImageFont.truetype(path, SIZE) for path in FALLBACKS]
cmaps = {path: set(TTFont(path).getBestCmap().keys()) for path in list(FONTS.values()) + FALLBACKS}

ascent, descent = fonts[False].getmetrics()
CELL_W = round(fonts[False].getlength("M"))
CELL_H = round((ascent + descent) * 1.08)

XTERM = {
    "black": "000000", "red": "cd0000", "green": "00cd00", "brown": "cdcd00", "yellow": "cdcd00", "blue": "0000ee",
    "magenta": "cd00cd", "cyan": "00cdcd", "white": "e5e5e5", "brightblack": "7f7f7f", "brightred": "ff0000",
    "brightgreen": "00ff00", "brightyellow": "ffff00", "brightblue": "5c5cff", "brightmagenta": "ff00ff",
    "brightcyan": "00ffff", "brightwhite": "ffffff",
}
TERMINAL = {"dark": ("0f131a", "d8dee9"), "light": ("fbfcfd", "1e2a36")}


def rgb(value, default):
    if value in (None, "default"):
        value = default
    value = XTERM.get(value, value)
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def font_for(char, bold):
    point = ord(char[0])
    if point in cmaps[FONTS[bold]]:
        return fonts[bold]
    for path, font in zip(FALLBACKS, fallback_fonts):
        if point in cmaps[path]:
            return font
    return fonts[bold]


BLOCKS_LOWER = {chr(0x2581 + i): (i + 1) / 8 for i in range(8)}      # ▁..█ (bottom-anchored)
BLOCKS_LEFT = {chr(0x258F - i): (i + 1) / 8 for i in range(7)}       # ▏..▉ (left-anchored)
BRAILLE_BITS = [(0, 0, 0x01), (0, 1, 0x02), (0, 2, 0x04), (1, 0, 0x08), (1, 1, 0x10), (1, 2, 0x20), (0, 3, 0x40), (1, 3, 0x80)]


def draw_cell(draw, x, y, char, fg, bg):
    left, top = x * CELL_W, y * CELL_H
    draw.rectangle([left, top, left + CELL_W - 1, top + CELL_H - 1], fill=bg)
    if not char or char == " ":
        return
    point = ord(char[0])
    if 0x2800 <= point <= 0x28FF:
        bits = point - 0x2800
        radius = max(1, CELL_W // 7)
        for column, row, bit in BRAILLE_BITS:
            if bits & bit:
                cx = left + CELL_W * (0.3 + 0.4 * column)
                cy = top + CELL_H * (0.14 + 0.24 * row)
                draw.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], fill=fg)
        return
    if char == "█":
        draw.rectangle([left, top, left + CELL_W - 1, top + CELL_H - 1], fill=fg)
        return
    if char in BLOCKS_LOWER:
        height = CELL_H * BLOCKS_LOWER[char]
        draw.rectangle([left, top + CELL_H - height, left + CELL_W - 1, top + CELL_H - 1], fill=fg)
        return
    if char in BLOCKS_LEFT:
        draw.rectangle([left, top, left + CELL_W * BLOCKS_LEFT[char] - 1, top + CELL_H - 1], fill=fg)
        return
    return "glyph"


def render(path, out, title):
    data = json.load(open(path))
    cols, rows, kind = data["cols"], data["rows"], data["kind"]
    term_bg, term_fg = TERMINAL[kind]
    canvas = Image.new("RGB", (cols * CELL_W, rows * CELL_H), rgb(term_bg, term_bg))
    draw = ImageDraw.Draw(canvas)
    glyphs = []
    for y, row in enumerate(data["cells"]):
        for x, (char, fg, bg, bold, reverse) in enumerate(row):
            fore, back = rgb(fg, term_fg), rgb(bg, term_bg)
            if reverse:
                fore, back = back, fore
            if draw_cell(draw, x, y, char, fore, back) == "glyph":
                glyphs.append((x, y, char, fore, bold))
    for x, y, char, fore, bold in glyphs:
        font = font_for(char, bold)
        draw.text((x * CELL_W, y * CELL_H + (CELL_H - (ascent + descent)) / 2), char, font=font, fill=fore)

    # Window chrome.
    pad, bar = 22 * SCALE // 2, 30 * SCALE // 2 + 8
    frame_bg = (30, 33, 40) if kind == "dark" else (226, 230, 236)
    framed = Image.new("RGB", (canvas.width + pad * 2, canvas.height + pad * 2 + bar), frame_bg)
    framed.paste(canvas, (pad, pad + bar))
    fdraw = ImageDraw.Draw(framed)
    for index, color in enumerate([(255, 95, 86), (255, 189, 46), (39, 201, 63)]):
        cx, cy = pad + 10 * SCALE + index * 20 * SCALE, pad + bar // 2 - 4
        fdraw.ellipse([cx - 6 * SCALE, cy - 6 * SCALE, cx + 6 * SCALE, cy + 6 * SCALE], fill=color)
    label_color = (160, 166, 178) if kind == "dark" else (90, 98, 110)
    label_font = ImageFont.truetype(FALLBACKS[1], 12 * SCALE)
    width = fdraw.textlength(title, font=label_font)
    fdraw.text(((framed.width - width) / 2, pad + bar // 2 - 4 - 8 * SCALE), title, font=label_font, fill=label_color)
    # Terminal output uses few colors, so a 256-color palette keeps files small without visible loss.
    framed.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(out, optimize=True)
    return framed.size


if __name__ == "__main__":
    src, dst = sys.argv[1], sys.argv[2]
    os.makedirs(dst, exist_ok=True)
    for name in sorted(os.listdir(src)):
        if name.endswith(".json"):
            stem = name[:-5]
            size = render(os.path.join(src, name), os.path.join(dst, stem + ".png"), f"dtop — {stem.replace('-', ' ')}")
            print(stem, size)
