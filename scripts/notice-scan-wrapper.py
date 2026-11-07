#!/usr/bin/env python3
"""Cargo external subcommand for the approved, complete notice graph."""
import hashlib
import os
from pathlib import Path
import sys

TOOL_SHA = 'e902609296601f873c08ca912acea0c37b375128ea3940eb307a76080afb8211'
CONFIG_SHA = 'f1f8a05bfa523d1233ad9661db8d97b086a27754283d5dc580cb36dddb38078d'

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    wrapper = Path(__file__).resolve(strict=True)
    name = 'clipmonger-notice-scan-' + digest(wrapper)
    if wrapper.name != 'cargo-' + name or len(sys.argv) != 8 or sys.argv[1] != name or sys.argv[2::2] != ['--tool', '--config', '--output-file']:
        raise ValueError('unexpected wrapper identity or arguments')
    tool, config, output = map(Path, sys.argv[3::2])
    if not tool.is_absolute() or not config.is_absolute() or not output.is_absolute():
        raise ValueError('tool, config and output must be absolute paths')
    if digest(tool) != TOOL_SHA or digest(config) != CONFIG_SHA:
        raise ValueError('generator tool or configuration differs from approved version')
    os.execv(str(tool), [str(tool), 'about', 'generate', '--frozen', '--workspace', '--all-features', '--target', 'x86_64-pc-windows-gnu', '--target', 'x86_64-unknown-linux-gnu', '--target', 'x86_64-apple-darwin', '--target', 'aarch64-apple-darwin', '--fail', '--format', 'json', '--config', str(config), '--output-file', str(output)])

if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError) as exc:
        sys.exit(f'notice-scan-wrapper: {exc}')
