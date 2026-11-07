#!/usr/bin/env python3
"""Paste target: a GTK3 text window that saves its text to OUT on every change.
  editor.py [OUT] [TITLE]   (default /tmp/editor.txt; title shows 'root' when uid 0)"""
import os, sys, gi
gi.require_version('Gtk', '3.0'); gi.require_version('Gdk', '3.0')
from gi.repository import Gtk, Gdk, GdkPixbuf
out = sys.argv[1] if len(sys.argv) > 1 else ('/tmp/root-editor.txt' if os.getuid() == 0 else '/tmp/editor.txt')
title = sys.argv[2] if len(sys.argv) > 2 else ('cm-editor (root)' if os.getuid() == 0 else 'cm-editor')
win = Gtk.Window(title=title); win.set_default_size(700, 450)
view = Gtk.TextView(); buf = view.get_buffer()
def save(*_):
    s, e = buf.get_bounds(); open(out, 'w').write(buf.get_text(s, e, True))
buf.connect('changed', save)
# Image pastes: record the image size.
def paste_img(_v):
    img = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).wait_for_image()
    if img is not None and not Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD).wait_is_text_available():
        open(out + '.image', 'w').write(f'{img.get_width()}x{img.get_height()}\n')
view.connect('paste-clipboard', paste_img)
sw = Gtk.ScrolledWindow(); sw.add(view); win.add(sw)
win.connect('destroy', Gtk.main_quit); save(); win.show_all(); view.grab_focus(); Gtk.main()
