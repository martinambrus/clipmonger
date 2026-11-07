#!/usr/bin/env python3
"""Print the clipboard: targets, then text (or image size)."""
import gi
gi.require_version('Gtk', '3.0'); gi.require_version('Gdk', '3.0')
from gi.repository import Gtk, Gdk
cb = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
ok, targets = cb.wait_for_targets()
print('targets:', [t.name() for t in targets] if ok else [])
img = cb.wait_for_image()
if img is not None:
    print(f'image: {img.get_width()}x{img.get_height()}')
txt = cb.wait_for_text()
if txt is not None:
    print(f'text({len(txt)}): {txt[:200]!r}')
