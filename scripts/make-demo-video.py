#!/usr/bin/env python3
"""Render the Clipmonger demo video from a storyboard of screenshots.

    python3 scripts/make-demo-video.py STORYBOARD.json OUT.mp4 [--gif OUT.gif]

The storyboard is a JSON list of scenes; image paths are relative to the
storyboard file:

    {"type": "title", "text": "Clipmonger", "sub": "...", "dur": 2.5}
    {"type": "shot", "image": "a.png", "caption": "...", "dur": 3,
     "keys": ["Ctrl", "Alt", "H"], "keys_at": 0.4, "typed": false,
     "zoom": [x, y, w, h]}            # optional slow zoom into this box
    {"type": "grid", "title": "...", "cols": 3, "delay": 0.2, "hold": 2.5,
     "images": [{"image": "gnome.png", "label": "GNOME 50"}, ...]}

Scenes cross-fade into each other. Frames are drawn with Pillow and piped to
ffmpeg (H.264, yuv420p, 30 fps).
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS = 1280, 720, 30
FADE = 0.4
BG = (24, 26, 31)
FG = (236, 238, 242)
MUTED = (160, 166, 178)
ACCENT = (86, 156, 255)
FONT_DIR = Path('/usr/share/fonts/truetype')


def font(size, bold=False):
    names = ['dejavu/DejaVuSans-Bold.ttf' if bold else 'dejavu/DejaVuSans.ttf']
    for name in names:
        path = FONT_DIR / name
        if path.exists():
            return ImageFont.truetype(str(path), size)
    return ImageFont.load_default()


def ease(t):
    t = max(0.0, min(1.0, t))
    return t * t * (3 - 2 * t)


def fit(img, box_w, box_h):
    scale = min(box_w / img.width, box_h / img.height)
    return img.resize((max(1, int(img.width * scale)), max(1, int(img.height * scale))), Image.LANCZOS)


def text_center(draw, y, text, fnt, fill):
    w = draw.textlength(text, font=fnt)
    draw.text(((W - w) / 2, y), text, font=fnt, fill=fill)


class Title:
    def __init__(self, scene, base):
        self.text, self.sub = scene['text'], scene.get('sub', '')
        self.dur = scene.get('dur', 2.5)

    def frame(self, t):
        img = Image.new('RGB', (W, H), BG)
        d = ImageDraw.Draw(img)
        a = ease(t / 0.6)
        rise = int((1 - a) * 24)
        col = tuple(int(BG[i] + (FG[i] - BG[i]) * a) for i in range(3))
        size = 64
        while size > 24 and d.textlength(self.text, font=font(size, True)) > W - 120:
            size -= 4
        text_center(d, H / 2 - 70 + rise, self.text, font(size, True), col)
        if self.sub:
            b = ease((t - 0.3) / 0.6)
            col2 = tuple(int(BG[i] + (MUTED[i] - BG[i]) * b) for i in range(3))
            text_center(d, H / 2 + 20 + rise, self.sub, font(28), col2)
        bar = int(ease((t - 0.2) / 0.8) * 160)
        d.rectangle([(W - bar) / 2, H / 2 + 80, (W + bar) / 2, H / 2 + 84], fill=ACCENT)
        return img


def keycaps(base, keys, appear, t, joiner='+'):
    """Draw keycaps that pop in one by one, then a 'pressed' flash."""
    if t < appear:
        return base
    img = base.copy()
    d = ImageDraw.Draw(img, 'RGBA')
    f = font(30, True)
    pad, gap, kh = 22, 14, 62
    widths = [d.textlength(k, font=f) + 2 * pad for k in keys]
    plus = d.textlength(joiner, font=f) if joiner else 0
    total = sum(widths) + (len(keys) - 1) * (plus + 2 * gap)
    x = (W - total) / 2
    y = H - kh - 70
    local = t - appear
    d.rounded_rectangle([x - 26, y - 20, x + total + 26, y + kh + 20], 18, fill=(0, 0, 0, 150))
    for i, (k, kw) in enumerate(zip(keys, widths)):
        a = ease((local - i * 0.12) / 0.18)
        if a <= 0:
            break
        pressed = local > len(keys) * 0.12 + 0.25
        drop = 4 if pressed else 0
        top = y + (1 - a) * 18 + drop
        d.rounded_rectangle([x, top + 6, x + kw, top + kh + 6 - drop], 12, fill=(10, 12, 16, int(255 * a)))
        fill = (ACCENT if pressed else (58, 62, 72)) + (int(255 * a),)
        d.rounded_rectangle([x, top, x + kw, top + kh - drop], 12, fill=fill, outline=(120, 126, 140, int(255 * a)), width=2)
        d.text((x + pad, top + (kh - drop - 36) / 2), k, font=f, fill=FG + (int(255 * a),))
        x += kw
        if i < len(keys) - 1:
            if joiner:
                d.text((x + gap, y + (kh - 36) / 2), joiner, font=f, fill=MUTED + (int(255 * a),))
            x += plus + 2 * gap
    return img


def caption(img, text, t):
    if not text:
        return img
    a = ease(t / 0.5)
    d = ImageDraw.Draw(img, 'RGBA')
    f = font(28, True)
    w = d.textlength(text, font=f)
    x, y = (W - w) / 2, 26 - (1 - a) * 12
    d.rounded_rectangle([x - 22, y - 12, x + w + 22, y + 46], 14, fill=(0, 0, 0, int(170 * a)))
    d.text((x, y), text, font=f, fill=FG + (int(255 * a),))
    return img


class Shot:
    def __init__(self, scene, base):
        self.src = Image.open(base / scene['image']).convert('RGB')
        self.dur = scene.get('dur', 3.0)
        self.caption = scene.get('caption', '')
        self.keys = scene.get('keys')
        self.keys_at = scene.get('keys_at', 0.4)
        self.joiner = '' if scene.get('typed') else '+'
        self.zoom = scene.get('zoom')
        # Blurred, darkened backdrop so any aspect ratio fills 16:9.
        back = self.src.resize((W, H)).filter(ImageFilter.GaussianBlur(24))
        self.back = Image.blend(back, Image.new('RGB', (W, H), BG), 0.55)

    def frame(self, t):
        src = self.src
        if self.zoom:
            x, y, w, h = self.zoom
            k = ease(t / self.dur)
            box = (x * k, y * k, src.width + (x + w - src.width) * k, src.height + (y + h - src.height) * k)
            src = src.crop(tuple(int(v) for v in box))
        shot = fit(src, W - 80, H - 80)
        img = self.back.copy()
        ox, oy = (W - shot.width) // 2, (H - shot.height) // 2
        shadow = Image.new('RGBA', (shot.width + 40, shot.height + 40), (0, 0, 0, 0))
        ImageDraw.Draw(shadow).rounded_rectangle([20, 20, shot.width + 20, shot.height + 20], 12, fill=(0, 0, 0, 140))
        img.paste(shadow.filter(ImageFilter.GaussianBlur(12)), (ox - 20, oy - 12), shadow.filter(ImageFilter.GaussianBlur(12)))
        img.paste(shot, (ox, oy))
        if self.keys:
            img = keycaps(img, self.keys, self.keys_at, t, self.joiner)
        return caption(img, self.caption, t)


class Grid:
    def __init__(self, scene, base):
        self.items = [(Image.open(base / i['image']).convert('RGB'), i.get('label', '')) for i in scene['images']]
        self.cols = scene.get('cols', 3)
        self.delay = scene.get('delay', 0.2)
        self.title = scene.get('title', '')
        self.dur = len(self.items) * self.delay + 0.5 + scene.get('hold', 2.5)
        rows = (len(self.items) + self.cols - 1) // self.cols
        top = 90 if self.title else 30
        gap = 16
        self.cw = (W - 60 - gap * (self.cols - 1)) // self.cols
        self.ch = (H - top - 30 - gap * (rows - 1)) // rows
        self.cells = []
        for n, (im, label) in enumerate(self.items):
            r, c = divmod(n, self.cols)
            x = 30 + c * (self.cw + gap)
            y = top + r * (self.ch + gap)
            tile = fit(im, self.cw, self.ch - 30)
            self.cells.append((x, y, tile, label))

    def frame(self, t):
        img = Image.new('RGB', (W, H), BG)
        d = ImageDraw.Draw(img, 'RGBA')
        if self.title:
            text_center(d, 28, self.title, font(34, True), FG)
        f = font(18, True)
        for n, (x, y, tile, label) in enumerate(self.cells):
            a = ease((t - n * self.delay) / 0.3)
            if a <= 0:
                continue
            s = 0.85 + 0.15 * a
            tw, th = max(1, int(tile.width * s)), max(1, int(tile.height * s))
            small = tile.resize((tw, th))
            if a < 1:
                small = Image.blend(Image.new('RGB', small.size, BG), small, a)
            px = x + (self.cw - tw) // 2
            py = y + (self.ch - 30 - th) // 2
            img.paste(small, (px, py))
            lw = d.textlength(label, font=f)
            d.text((x + (self.cw - lw) / 2, y + self.ch - 24), label, font=f, fill=FG + (int(255 * a),))
        return img


KINDS = {'title': Title, 'shot': Shot, 'grid': Grid}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('storyboard')
    ap.add_argument('out')
    ap.add_argument('--gif', help='also write a smaller animated GIF')
    args = ap.parse_args()
    board = Path(args.storyboard)
    scenes = [KINDS[s['type']](s, board.parent) for s in json.loads(board.read_text())]

    ff = subprocess.Popen(['ffmpeg', '-y', '-loglevel', 'error', '-f', 'rawvideo', '-pix_fmt', 'rgb24',
                           '-s', f'{W}x{H}', '-r', str(FPS), '-i', '-', '-c:v', 'libx264', '-preset', 'slow',
                           '-crf', '22', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', args.out],
                          stdin=subprocess.PIPE)
    prev_last = None
    for scene in scenes:
        frames = int(scene.dur * FPS)
        for i in range(frames):
            t = i / FPS
            img = scene.frame(t)
            if prev_last is not None and t < FADE:
                img = Image.blend(prev_last, img, ease(t / FADE))
            ff.stdin.write(img.tobytes())
        prev_last = scene.frame(frames / FPS)
    ff.stdin.close()
    if ff.wait() != 0:
        sys.exit('ffmpeg failed')
    if args.gif:
        palette = f'fps=12,scale=800:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=128[p];[b][p]paletteuse=dither=bayer:bayer_scale=4'
        subprocess.run(['ffmpeg', '-y', '-loglevel', 'error', '-i', args.out, '-vf', palette, args.gif], check=True)


if __name__ == '__main__':
    main()
