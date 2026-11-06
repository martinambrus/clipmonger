#!/usr/bin/env python3
"""Fail when a Linux binary needs a glibc newer than the supported floor.

The floor is the glibc of the oldest distro Clipmonger is tested on
(Debian 12: 2.36). seg-12 §2, T-1203.
"""
import argparse
import re
import subprocess
import sys

FLOOR = '2.36'


def version(text):
    return tuple(int(part) for part in text.split('.'))


def needed(path):
    """Every GLIBC_x.y version the binary requires (readelf -V)."""
    out = subprocess.run(['readelf', '-V', '-W', path], capture_output=True, text=True, check=True).stdout
    return sorted(set(re.findall(r'Name: GLIBC_([0-9.]+)', out)), key=version)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binaries', nargs='+')
    parser.add_argument('--floor', default=FLOOR)
    args = parser.parse_args()
    failed = False
    for path in args.binaries:
        versions = needed(path)
        newest = versions[-1] if versions else 'none'
        ok = not versions or version(newest) <= version(args.floor)
        failed |= not ok
        print(f"{'ok  ' if ok else 'FAIL'} {path}: needs glibc {newest} (floor {args.floor})")
    sys.exit(1 if failed else 0)


if __name__ == '__main__':
    main()
