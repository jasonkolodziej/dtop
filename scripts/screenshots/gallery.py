"""Combine the per-theme renders into one comparison sheet: python3 gallery.py PNG_DIR OUT.png"""
import sys
from PIL import Image, ImageDraw, ImageFont
names = ["containers", "theme-daylight", "theme-midnight", "theme-ember", "theme-forest", "theme-glacier", "theme-dusk", "theme-contrast"]
labels = ["harbor (default)", "daylight", "midnight", "ember", "forest", "glacier", "dusk", "contrast"]
thumbs = [Image.open(f"{sys.argv[1]}/{n}.png").convert("RGB") for n in names]
w, h = thumbs[0].width // 2, thumbs[0].height // 2
gap, label_h = 24, 56
font = ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 30)  # any bold sans works
sheet = Image.new("RGB", (2 * w + 3 * gap, 4 * (h + label_h) + 5 * gap - 20), (22, 24, 30))
draw = ImageDraw.Draw(sheet)
for i, (img, label) in enumerate(zip(thumbs, labels)):
    col, row = i % 2, i // 2
    x, y = gap + col * (w + gap), gap + row * (h + label_h + gap)
    draw.text((x + 6, y + 8), label, font=font, fill=(220, 224, 232))
    sheet.paste(img.resize((w, h), Image.LANCZOS), (x, y + label_h))
sheet.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(sys.argv[2], optimize=True)
print(sheet.size)
