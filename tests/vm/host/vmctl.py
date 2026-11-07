#!/usr/bin/env python3
"""Drive a libvirt test VM from the host: keys, typing, pointer, screenshots.

  vmctl.py DOM key ctrl+alt+t          one chord (names: see KEYS, a-z, 0-9, f1-f12)
  vmctl.py DOM type 'text\\n'           type a string (US layout); \\n = Enter
  vmctl.py DOM click X Y [right]       click at screen pixel X,Y
  vmctl.py DOM move X Y                move the pointer
  vmctl.py DOM shot out.png            screenshot (PNG)
  vmctl.py DOM size                    screen size

Pointer coordinates are screen pixels; they are scaled to the USB tablet's
0..32767 range using the current screenshot size.
"""
import io
import json
import sys
import time

import libvirt
import libvirt_qemu
from PIL import Image

URI = 'qemu:///system'

# Linux evdev key codes.
KEYS = {
    'esc': 1, '1': 2, '2': 3, '3': 4, '4': 5, '5': 6, '6': 7, '7': 8, '8': 9, '9': 10, '0': 11,
    'minus': 12, 'equal': 13, 'backspace': 14, 'tab': 15,
    'q': 16, 'w': 17, 'e': 18, 'r': 19, 't': 20, 'y': 21, 'u': 22, 'i': 23, 'o': 24, 'p': 25,
    'leftbrace': 26, 'rightbrace': 27, 'enter': 28, 'ctrl': 29,
    'a': 30, 's': 31, 'd': 32, 'f': 33, 'g': 34, 'h': 35, 'j': 36, 'k': 37, 'l': 38,
    'semicolon': 39, 'apostrophe': 40, 'grave': 41, 'shift': 42, 'backslash': 43,
    'z': 44, 'x': 45, 'c': 46, 'v': 47, 'b': 48, 'n': 49, 'm': 50,
    'comma': 51, 'dot': 52, 'slash': 53, 'rightshift': 54, 'alt': 56, 'space': 57, 'capslock': 58,
    'f1': 59, 'f2': 60, 'f3': 61, 'f4': 62, 'f5': 63, 'f6': 64, 'f7': 65, 'f8': 66, 'f9': 67, 'f10': 68,
    'f11': 87, 'f12': 88, 'rightctrl': 97, 'rightalt': 100, 'home': 102, 'up': 103, 'pageup': 104,
    'left': 105, 'right': 106, 'end': 107, 'down': 108, 'pagedown': 109, 'insert': 110, 'delete': 111,
    'super': 125, 'menu': 127,
}
PLAIN = {' ': 'space', '-': 'minus', '=': 'equal', '[': 'leftbrace', ']': 'rightbrace', ';': 'semicolon',
         "'": 'apostrophe', '`': 'grave', '\\': 'backslash', ',': 'comma', '.': 'dot', '/': 'slash',
         '\n': 'enter', '\t': 'tab'}
SHIFTED = {'!': '1', '@': '2', '#': '3', '$': '4', '%': '5', '^': '6', '&': '7', '*': '8', '(': '9', ')': '0',
           '_': 'minus', '+': 'equal', '{': 'leftbrace', '}': 'rightbrace', ':': 'semicolon', '"': 'apostrophe',
           '~': 'grave', '|': 'backslash', '<': 'comma', '>': 'dot', '?': 'slash'}


def domain(name):
    return libvirt.open(URI).lookupByName(name)


def send(dom, names, hold=60):
    dom.sendKey(libvirt.VIR_KEYCODE_SET_LINUX, hold, [KEYS[n] for n in names], len(names), 0)


def char_keys(ch):
    if ch in PLAIN:
        return [PLAIN[ch]]
    if ch in SHIFTED:
        return ['shift', SHIFTED[ch]]
    if ch.isupper():
        return ['shift', ch.lower()]
    if ch.lower() in KEYS:
        return [ch]
    raise SystemExit(f'cannot type {ch!r}')


def type_text(dom, text, pace=0.03):
    for ch in text:
        send(dom, char_keys(ch), hold=40)
        time.sleep(pace)


def screenshot(dom):
    conn = dom.connect()
    stream = conn.newStream(0)
    dom.screenshot(stream, 0, 0)
    data = bytearray()
    while True:
        chunk = stream.recv(1 << 20)
        if not chunk:
            break
        data += chunk
    stream.finish()
    return Image.open(io.BytesIO(bytes(data)))


def qmp(dom, cmd, args):
    return libvirt_qemu.qemuMonitorCommand(dom, json.dumps({'execute': cmd, 'arguments': args}), 0)


def move(dom, x, y):
    w, h = screenshot(dom).size
    events = [{'type': 'abs', 'data': {'axis': 'x', 'value': int(x * 32767 / max(w - 1, 1))}},
              {'type': 'abs', 'data': {'axis': 'y', 'value': int(y * 32767 / max(h - 1, 1))}}]
    qmp(dom, 'input-send-event', {'events': events})


def click(dom, x, y, button='left'):
    move(dom, x, y)
    time.sleep(0.1)
    for down in (True, False):
        qmp(dom, 'input-send-event', {'events': [{'type': 'btn', 'data': {'down': down, 'button': button}}]})
        time.sleep(0.08)


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    dom = domain(sys.argv[1])
    cmd, args = sys.argv[2], sys.argv[3:]
    if cmd == 'key':
        for chord in args:
            send(dom, chord.lower().split('+'))
            time.sleep(0.15)
    elif cmd == 'type':
        type_text(dom, ' '.join(args).replace('\\n', '\n'), pace=float(__import__('os').environ.get('PACE', '0.08')))
    elif cmd == 'click':
        click(dom, int(args[0]), int(args[1]), args[2] if len(args) > 2 else 'left')
    elif cmd == 'move':
        move(dom, int(args[0]), int(args[1]))
    elif cmd == 'shot':
        screenshot(dom).convert('RGB').save(args[0])
    elif cmd == 'size':
        print(*screenshot(dom).size)
    else:
        raise SystemExit(__doc__)


if __name__ == '__main__':
    main()
