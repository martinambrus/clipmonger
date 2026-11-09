#!/usr/bin/env python3
"""Generate every Clipmonger icon from the masters in assets/icons/source.

make-icons.py [--import DIR]

--import reads icon-big.png, icon-small.png, icon-small-degraded.png and
icon-small-paused.png from DIR and replaces the masters first. Only the alpha
channel of the artwork is changed: nearly opaque pixels become opaque and
nearly invisible ones transparent. Antialiased edges stay. The artwork is then
trimmed to its outline and centred on a 1024 px transparent square.

The generated files are committed, so building needs no Pillow:
  assets/icons/tray/<state>-<edge>.rgba   raw RGBA, tray and menu bar
  assets/icons/window-64.rgba             raw RGBA, X11 window icon
  packaging/linux/icons/hicolor/          launcher icons
  packaging/windows/clipmonger.ico        .exe, installer, windows
  packaging/macos/Clipmonger.icns         app bundle
"""
import argparse
import io
import struct
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / 'assets/icons'
SOURCE = ASSETS / 'source'
APP_ID = 'com.martinambrus.clipmonger'
MASTER = 1024
STATES = ('normal', 'paused', 'degraded')
IMPORTS = {'app': 'icon-big.png', 'tray-normal': 'icon-small.png',
           'tray-degraded': 'icon-small-degraded.png', 'tray-paused': 'icon-small-paused.png'}
TRAY_EDGES = (16, 20, 22, 24, 32, 48)
LINUX_EDGES = (16, 22, 24, 32, 48, 64, 128, 256, 512)
ICO_EDGES = (16, 20, 24, 32, 40, 48, 64, 128, 256)
# Below this edge the detailed artwork turns to mud: use the simplified mark.
DETAILED_FROM = 64
# Apple's icon grid leaves the artwork 824 of 1024 px.
MACOS_BODY = 824 / 1024


def clean_alpha(image):
    alpha = image.getchannel('A').point(lambda v: 255 if v >= 224 else 0 if v <= 15 else v)
    image = image.copy()
    image.putalpha(alpha)
    return image


def outline(image):
    return image.getchannel('A').point(lambda v: 255 if v >= 64 else 0).getbbox()


def square_master(image, box):
    image = image.crop(box)
    side = max(image.size)
    out = Image.new('RGBA', (side, side), (0, 0, 0, 0))
    out.paste(image, ((side - image.width) // 2, (side - image.height) // 2))
    return out.resize((MASTER, MASTER), Image.LANCZOS)


def import_masters(folder):
    images = {name: clean_alpha(Image.open(folder / file).convert('RGBA')) for name, file in IMPORTS.items()}
    # One box for the three states, so the icon does not shift when one replaces another.
    boxes = [outline(images[f'tray-{state}']) for state in STATES]
    shared = (min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes))
    SOURCE.mkdir(parents=True, exist_ok=True)
    for name, image in images.items():
        box = outline(image) if name == 'app' else shared
        square_master(image, box).save(SOURCE / f'{name}.png', optimize=True)


def scaled(image, edge):
    return image.resize((edge, edge), Image.LANCZOS)


def app_at(app, mark, edge):
    return scaled(app if edge >= DETAILED_FROM else mark, edge)


def ico_entry(image):
    """A PNG at 256 px, an uncompressed DIB below it (older loaders need those)."""
    if image.width >= 256:
        out = io.BytesIO()
        image.save(out, 'PNG', optimize=True)
        return out.getvalue()
    edge = image.width
    pixels = bytearray()
    for y in range(edge - 1, -1, -1):
        for x in range(edge):
            r, g, b, a = image.getpixel((x, y))
            pixels += bytes((b, g, r, a))
    mask_row = ((edge + 31) // 32) * 4
    header = struct.pack('<IiiHHIIiiII', 40, edge, edge * 2, 1, 32, 0, len(pixels) + mask_row * edge, 0, 0, 0, 0)
    return header + bytes(pixels) + bytes(mask_row * edge)


def write_ico(path, images):
    offset = 6 + 16 * len(images)
    directory = struct.pack('<HHH', 0, 1, len(images))
    blobs = b''
    for image in images:
        blob = ico_entry(image)
        byte_edge = 0 if image.width >= 256 else image.width
        directory += struct.pack('<BBBBHHII', byte_edge, byte_edge, 0, 0, 1, 32, len(blob), offset)
        offset += len(blob)
        blobs += blob
    path.write_bytes(directory + blobs)


def macos_canvas(image):
    body = round(image.width * MACOS_BODY)
    out = Image.new('RGBA', image.size, (0, 0, 0, 0))
    inner = scaled(image, body)
    at = (image.width - body) // 2
    out.paste(inner, (at, at), inner)
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--import', dest='folder', type=Path)
    args = parser.parse_args()
    if args.folder:
        import_masters(args.folder)
    app = Image.open(SOURCE / 'app.png').convert('RGBA')
    mark = Image.open(SOURCE / 'tray-normal.png').convert('RGBA')

    (ASSETS / 'tray').mkdir(parents=True, exist_ok=True)
    for state in STATES:
        master = Image.open(SOURCE / f'tray-{state}.png').convert('RGBA')
        for edge in TRAY_EDGES:
            (ASSETS / 'tray' / f'{state}-{edge}.rgba').write_bytes(scaled(master, edge).tobytes())
    (ASSETS / 'window-64.rgba').write_bytes(scaled(app, 64).tobytes())

    for edge in LINUX_EDGES:
        folder = ROOT / f'packaging/linux/icons/hicolor/{edge}x{edge}/apps'
        folder.mkdir(parents=True, exist_ok=True)
        app_at(app, mark, edge).save(folder / f'{APP_ID}.png', optimize=True)

    write_ico(ROOT / 'packaging/windows/clipmonger.ico', [app_at(app, mark, edge) for edge in ICO_EDGES])

    (ROOT / 'packaging/macos').mkdir(parents=True, exist_ok=True)
    sized = [macos_canvas(app_at(app, mark, edge)) for edge in (16, 32, 64, 128, 256, 512)]
    macos_canvas(app).save(ROOT / 'packaging/macos/Clipmonger.icns', format='ICNS', append_images=sized)


if __name__ == '__main__':
    main()
