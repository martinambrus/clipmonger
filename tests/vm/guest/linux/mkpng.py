#!/usr/bin/env python3
"""mkpng.py WxH OUT [noise]: write a gradient (or noise) PNG with GdkPixbuf."""
import os, sys, gi
gi.require_version('GdkPixbuf', '2.0')
from gi.repository import GdkPixbuf, GLib
w, h = map(int, sys.argv[1].split('x'))
if len(sys.argv) > 3:
    px = os.urandom(w * h * 3)
else:
    px = bytes([(x * 255 // w) if c == 0 else (y * 255 // h) if c == 1 else 128 for y in range(h) for x in range(w) for c in range(3)])
GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(px), GdkPixbuf.Colorspace.RGB, False, 8, w, h, w * 3).savev(sys.argv[2], 'png', [], [])
