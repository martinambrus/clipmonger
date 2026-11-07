#!/usr/bin/env python3
"""Copy TEXT marked as a password (x-kde-passwordManagerHint=secret), GTK3."""
import sys, gi
gi.require_version('Gtk', '3.0'); gi.require_version('Gdk', '3.0')
from gi.repository import Gtk, Gdk, GLib
text = ' '.join(sys.argv[1:]) or 'hunter2-secret'
names = ['UTF8_STRING', 'text/plain;charset=utf-8', 'text/plain', 'x-kde-passwordManagerHint']
targets = [Gtk.TargetEntry.new(n, 0, i) for i, n in enumerate(names)]
def get(_cb, sel, info, _d):
    sel.set(sel.get_target(), 8, b'secret' if info == 3 else text.encode())
cb = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
cb.set_with_data(targets, get, lambda *a: None, None)
cb.connect('owner-change', lambda *a: GLib.timeout_add(300, Gtk.main_quit))
GLib.timeout_add_seconds(600, Gtk.main_quit)
Gtk.main()
