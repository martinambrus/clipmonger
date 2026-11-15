#!/usr/bin/env python3
"""T-1106: backup-ZIP cross-version restore matrix (seg-11 §3).

For every older release given with --old VERSION=DIR (a directory holding that
release's clipmonger-daemon and clipmonger-cli), and for the build under test
itself:
  1. run that daemon on its own Xvfb display in an isolated HOME/profile;
  2. capture two texts (one with Unicode) and an image through a GTK clipboard
     owner, pin one text, create a permanent clip in group "Matrix";
  3. write a backup with `backup.create`.
Then the daemon under test (--new DIR) restores each archive into a fresh
profile with `backup.restore` and must serve the same texts, the image row,
the pin and the permanent clip. Exit 0 only when every cell passes.

Nothing touches the desktop session: each daemon has its own display, HOME,
XDG dirs and runtime dir, and no session D-Bus.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

COPY = r'''
import sys, gi
gi.require_version("Gtk", "3.0"); gi.require_version("Gdk", "3.0")
from gi.repository import Gtk, Gdk, GdkPixbuf, GLib
cb = Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD)
kind, value = sys.argv[1], sys.argv[2]
if kind == "text":
    cb.set_text(value, -1)
else:
    w, h = 64, 32
    px = bytes((x * 4) % 256 if c == 0 else (y * 8) % 256 if c == 1 else 99
               for y in range(h) for x in range(w) for c in range(3))
    cb.set_image(GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(px), GdkPixbuf.Colorspace.RGB, False, 8, w, h, w * 3))
GLib.timeout_add(4000, Gtk.main_quit)
Gtk.main()
'''

TEXTS = ["matrix plain text", "matrix ünïcödé ✓ 漢字"]


class Ipc:
    def __init__(self, profile: Path):
        rv = json.loads((profile / "rendezvous.json").read_text())
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(30)
        self.sock.connect(rv["socket"])
        self.buf = b""
        self.n = 0
        token = (profile / "token").read_text().strip()
        self._send("hello", {"min": 1, "max": 1, "token": token, "client": "t1106"}, "hello-0")

    def _send(self, method, params, rid):
        frame = {"v": "ipc/v1", "id": rid, "kind": "req", "method": method, "params": params}
        self.sock.sendall(json.dumps(frame).encode() + b"\n")
        while True:
            while b"\n" not in self.buf:
                chunk = self.sock.recv(1 << 16)
                if not chunk:
                    raise RuntimeError("connection closed")
                self.buf += chunk
            line, self.buf = self.buf.split(b"\n", 1)
            reply = json.loads(line)
            if reply.get("id") == rid and reply.get("kind") == "res":
                if reply.get("error"):
                    raise RuntimeError(f"{method}: {reply['error']}")
                return reply.get("result")

    def call(self, method, params=None):
        self.n += 1
        return self._send(method, params or {}, f"r{self.n}")


class Daemon:
    def __init__(self, bindir: Path, root: Path, display: int):
        self.root = root
        self.profile = root / "profile"
        for d in ("home", "run", "profile"):
            (root / d).mkdir(mode=0o700, parents=True, exist_ok=True)
        self.env = {
            "PATH": "/usr/bin:/bin", "HOME": str(root / "home"),
            "XDG_RUNTIME_DIR": str(root / "run"), "DISPLAY": f":{display}",
            "XDG_CONFIG_HOME": str(root / "home/.config"),
            "XDG_DATA_HOME": str(root / "home/.local/share"),
            "CLIPMONGER_NO_UI_SPAWN": "1", "LANG": "C.UTF-8",
        }
        self.proc = subprocess.Popen([str(bindir / "clipmonger-daemon"), "--profile-dir", str(self.profile)],
                                     env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.time() + 20
        while time.time() < deadline:
            if (self.profile / "rendezvous.json").exists() and (self.profile / "token").exists():
                try:
                    self.ipc = Ipc(self.profile)
                    return
                except OSError:
                    pass
            time.sleep(0.2)
        raise RuntimeError("daemon did not come up")

    def copy(self, kind, value):
        subprocess.run([sys.executable, "-c", COPY, kind, value], env={**self.env, "GDK_BACKEND": "x11"},
                       check=True, timeout=30)

    def history(self):
        return self.ipc.call("history.list", {"limit": 50}).get("clips", [])

    def stop(self):
        try:
            self.ipc.call("daemon.stop")
        except Exception:  # the daemon may close before replying
            pass
        try:
            self.proc.wait(10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def free_display():
    for n in range(260, 300):
        if not Path(f"/tmp/.X{n}-lock").exists():
            return n
    raise SystemExit("no free X display")


def make_backup(version, bindir, work, xdisplay, log):
    d = Daemon(bindir, work / f"src-{version}", xdisplay)
    try:
        for text in TEXTS:
            d.copy("text", text)
            time.sleep(1.5)
        d.copy("image", "")
        time.sleep(2)
        rows = d.history()
        pin = next(c for c in rows if c.get("preview", "").startswith("matrix plain"))
        d.ipc.call("history.pin", {"clip": pin["id"], "pinned": True})
        d.ipc.call("permanent.create", {"group": "Matrix", "name": "Sig", "body": f"permanent from {version}"})
        dest = work / f"backup-{version}.zip"
        d.ipc.call("backup.create", {"dest": str(dest)})
        log(f"{version}: backup with {len(rows)} rows -> {dest.name} ({dest.stat().st_size} bytes)")
        return dest
    finally:
        d.stop()


def check_restore(version, archive, newbin, work, xdisplay, log):
    d = Daemon(newbin, work / f"dst-{version}", xdisplay)
    problems = []
    try:
        d.ipc.call("backup.restore", {"src": str(archive)})
        rows = d.history()
        previews = [c.get("preview", "") for c in rows]
        for text in TEXTS:
            if not any(p.startswith(text) for p in previews):
                problems.append(f"text missing: {text!r}")
        if not any("image/png" in (c.get("formats") or []) for c in rows):
            problems.append("image row missing")
        if not any(c.get("pinned") and c.get("preview", "").startswith("matrix plain") for c in rows):
            problems.append("pin lost")
        perms = d.ipc.call("permanent.list").get("permanents", [])
        if not any(p.get("body") == f"permanent from {version}" and p.get("group") == "Matrix" for p in perms):
            problems.append(f"permanent clip missing: {perms}")
        info = d.ipc.call("db.info", {"check": True})
        if info.get("health") != "ok":
            problems.append(f"integrity: {info.get('health_detail')}")
    finally:
        d.stop()
    log(f"{version} -> new: {'PASS' if not problems else 'FAIL ' + '; '.join(problems)}")
    return not problems


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--old", action="append", default=[], help="VERSION=DIR")
    ap.add_argument("--new", required=True, type=Path)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    lines = []

    def log(msg):
        print(msg, flush=True)
        lines.append(msg)

    work = Path(tempfile.mkdtemp(prefix="cm-t1106-"))
    xdisplay = free_display()
    xvfb = subprocess.Popen(["Xvfb", f":{xdisplay}", "-screen", "0", "1024x768x24", "-nolisten", "tcp"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ok = True
    try:
        time.sleep(2)
        sources = [(v.split("=", 1)[0], Path(v.split("=", 1)[1])) for v in args.old] + [("new", args.new)]
        for version, bindir in sources:
            try:
                archive = make_backup(version, bindir, work, xdisplay, log)
                ok &= check_restore(version, archive, args.new, work, xdisplay, log)
            except Exception as error:  # report the cell, keep going
                log(f"{version}: ERROR {error}")
                ok = False
    finally:
        xvfb.terminate()
        xvfb.wait(10)
        shutil.rmtree(work, ignore_errors=True)
    (args.out / "t1106.txt").write_text("\n".join(lines) + f"\nRESULT: {'PASS' if ok else 'FAIL'}\n")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
