#!/usr/bin/env python3
"""Put test data on the clipboard and keep owning it (GTK3).
  cmcopy.py text TEXT | big N | image WxH | noise WxH | file PATH...
Exits when another app takes the clipboard, or after 600 s."""
import os, sys, gi
gi.require_version('Gtk', '3.0'); gi.require_version('Gdk', '3.0')
from gi.repository import Gtk, Gdk, GdkPixbuf, GLib

kind, args = sys.argv[1], sys.argv[2:]
cb = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
if kind == 'text':
    cb.set_text(' '.join(args), -1)
elif kind == 'big':
    n = int(args[0]); cb.set_text(('clipmonger-big ' * (n // 15 + 1))[:n], -1)
elif kind in ('image', 'noise'):
    w, h = map(int, args[0].split('x'))
    if kind == 'image':
        px = bytes([(x * 255 // w) if c == 0 else (y * 255 // h) if c == 1 else 128
                    for y in range(h) for x in range(w) for c in range(3)])
    else:
        px = os.urandom(w * h * 3)
    pb = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(px), GdkPixbuf.Colorspace.RGB, False, 8, w, h, w * 3)
    cb.set_image(pb)
elif kind == 'file':
    uris = '\n'.join(GLib.filename_to_uri(os.path.abspath(p)) for p in args)
    targets = [Gtk.TargetEntry.new('text/uri-list', 0, 0), Gtk.TargetEntry.new('x-special/gnome-copied-files', 0, 1)]
    def get(_cb, sel, info, _d):
        data = uris if info == 0 else 'copy\n' + uris
        sel.set(sel.get_target(), 8, data.encode())
    cb.set_with_data(targets, get, lambda *a: None, None)
else:
    raise SystemExit(__doc__)
cb.store()
cb.connect('owner-change', lambda *a: GLib.timeout_add(300, Gtk.main_quit))
GLib.timeout_add_seconds(600, Gtk.main_quit)
Gtk.main()
