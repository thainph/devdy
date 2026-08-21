#!/usr/bin/env python3
# Build co-registered "talking head" mouth-flap frames for the DY Cyber Fox from
# the master mouth sheet (fox-assets/head.png: a 4x3 grid of 12 full-head sprites,
# each a different eye+mouth pose on a transparent background with a soft glow).
#
# Pipeline: crop each head by its (hardcoded) component box -> keep real alpha,
# drop the low-alpha glow -> scale every frame by ONE common factor and register
# it by (ear-tip TOP, horizontal CENTER) so the head stays rock-steady while only
# the mouth/eyes change -> export WebP onto the same 1024x1024 canvas the other
# rig layers use, so CyberFox can drop them in exactly like head.webp.
#
# Frame index -> pose (see contact sheet):
#   0..3  eyes OPEN     : mouth closed / small / open / wide
#   4,5   eyes HAPPY    : mouth closed / wide
#   6,7   eyes OPEN     : mouth closed / "o"
#   8..11 eyes SLEEPY   : mouth closed / closed / closed / open
#
# Requires: Pillow, cwebp on PATH. No AI redraw / no generative fill.
import os, subprocess, tempfile
from PIL import Image
import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, 'fox-assets/head.png')
OUT = os.path.join(ROOT, 'fox-assets/generated/layers-2_5d/talk')
CANVAS = 1024

# --- registration knobs (tuned against the existing rig) --------------------
# Sized/positioned so the sprite's eye sockets land on the rig's cyber eyes
# (interocular ~174px, eye-line cy~440), which also makes the talking head match
# the normal head's size instead of overpowering it.
WF = 0.44        # frame width as a fraction of the canvas (common to all frames)
TOP_Y = 249      # canvas y for every frame's ear-tip top (keeps the head fixed)
ALPHA_KEEP = 100 # alpha below this is glow/backdrop -> dropped

# Connected-component boxes of the 12 heads in head.png (x, y, w, h), row-major.
BOXES = [
    (37, 67, 350, 256), (409, 66, 353, 261), (783, 66, 351, 262), (1151, 66, 353, 266),
    (33, 370, 356, 265), (406, 367, 357, 268), (780, 370, 353, 264), (1151, 363, 354, 270),
    (34, 677, 356, 256), (406, 677, 357, 257), (778, 677, 357, 259), (1148, 678, 356, 257),
]
REF_W = 355.0     # reference native head width -> common scale = WF*CANVAS/REF_W


def extract(sheet, box, pad=8):
    x, y, w, h = box
    x0, y0 = max(0, x - pad), max(0, y - pad)
    x1, y1 = min(sheet.shape[1], x + w + pad), min(sheet.shape[0], y + h + pad)
    sub = sheet[y0:y1, x0:x1].copy()
    a = sub[:, :, 3]
    sub[:, :, 3] = np.where(a >= ALPHA_KEEP, a, 0)
    ys, xs = np.where(sub[:, :, 3] > 0)
    return sub[ys.min():ys.max() + 1, xs.min():xs.max() + 1]


def main():
    os.makedirs(OUT, exist_ok=True)
    sheet = np.array(Image.open(SRC).convert('RGBA'))
    s = WF * CANVAS / REF_W
    for i, box in enumerate(BOXES):
        crop = extract(sheet, box)
        fr = Image.fromarray(crop)
        dw, dh = int(round(fr.width * s)), int(round(fr.height * s))
        fr = fr.resize((dw, dh), Image.LANCZOS)
        canvas = Image.new('RGBA', (CANVAS, CANVAS), (0, 0, 0, 0))
        canvas.alpha_composite(fr, (int(CANVAS * 0.5 - dw / 2), int(TOP_Y)))
        with tempfile.NamedTemporaryFile(suffix='.png', delete=False) as tmp:
            canvas.save(tmp.name)
            dst = os.path.join(OUT, f'{i:02d}.webp')
            subprocess.run(['cwebp', '-quiet', '-q', '92', '-alpha_q', '100',
                            tmp.name, '-o', dst], check=True)
        os.unlink(tmp.name)
        print(f'#{i:02d} -> talk/{i:02d}.webp  ({dw}x{dh})')
    print('done. WF=%.2f TOP_Y=%d' % (WF, TOP_Y))


if __name__ == '__main__':
    main()
