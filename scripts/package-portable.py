#!/usr/bin/env python3
"""Build or validate deterministic portable archives from prepared release inputs."""
import argparse
import hashlib
from pathlib import Path
import re
import stat
import sys
import zipfile
from notice_provenance import verified_notices

ROOT = Path(__file__).resolve().parent.parent
FONTS = ('Hack-Regular.txt', 'OFL.txt', 'README.md', 'UFL.txt', 'egui-icons.txt', 'emoji-icon-font-mit-license.txt')
BINS = ('clipmonger', 'clipmonger-daemon', 'clipmonger-cli')
EXTENSION = tuple(name + '.js' for name in ('capture', 'codec', 'config', 'extension', 'hotkeys', 'ipc', 'paste', 'polddiff', 'prefs', 'relay', 'report', 'sha256')) + ('metadata.json', 'stylesheet.css', 'schemas/org.gnome.shell.extensions.clipmonger.gschema.xml')
README = b'Extract to a writable directory and run clipmonger. Data is stored beside the binaries.\n'


def contents(platform, bins, notices, extension=None):
    suffix = '.exe' if platform == 'windows' else ''
    result = {name + suffix: (bins / (name + suffix)).read_bytes() for name in BINS}
    for path in sorted((ROOT / 'licenses').rglob('*')):
        if path.is_symlink():
            raise ValueError(f'symlink in licenses tree: {path}')
        if path.is_file():
            result['licenses/' + path.relative_to(ROOT / 'licenses').as_posix()] = path.read_bytes()
    for name in FONTS:
        if not result.get(f'licenses/fonts/{name}'):
            raise ValueError(f'font license missing or empty: {name}')
    if 'licenses/RUST-NOTICES.txt' in result:
        raise ValueError('licenses tree conflicts with generated Rust notices')
    result['licenses/RUST-NOTICES.txt'] = notices.read_bytes()
    if not result['licenses/RUST-NOTICES.txt'].strip():
        raise ValueError('Rust notices are empty')
    result['portable.marker'] = b''
    result['README.txt'] = README
    if platform == 'linux':
        if extension is None:
            raise ValueError('Linux requires a built GNOME extension directory')
        for name in EXTENSION:
            path = extension / name
            if path.is_symlink() or not path.is_file():
                raise ValueError(f'extension missing or symlinked: {name}')
            result['gnome-extension/' + name] = path.read_bytes()
            if not result['gnome-extension/' + name]:
                raise ValueError(f'extension file empty: {name}')
    return result


def validate(path, platform):
    suffix = '.exe' if platform == 'windows' else ''
    license_root = ROOT / 'licenses'
    license_files = {f'licenses/{p.relative_to(license_root).as_posix()}' for p in license_root.rglob('*') if p.is_file() and not p.is_symlink()}
    required = {name + suffix for name in BINS} | license_files | {f'licenses/fonts/{name}' for name in FONTS} | {'licenses/RUST-NOTICES.txt', 'portable.marker', 'README.txt'}
    if platform == 'linux':
        required |= {'gnome-extension/' + name for name in EXTENSION}
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        names = [i.filename for i in infos]
        if len(names) != len(set(names)):
            raise ValueError('duplicate archive entries')
        for info in infos:
            name = info.filename
            if (not name or name.startswith('/') or '\\' in name or ':' in name or any(ord(c) < 32 for c in name) or any(p in ('', '.', '..') for p in name.split('/')) or stat.S_ISLNK(info.external_attr >> 16)):
                raise ValueError(f'unsafe archive entry: {name}')
            if name not in required:
                raise ValueError(f'unexpected archive entry: {name}')
        missing = required - set(names)
        if missing:
            raise ValueError(f'missing archive entries: {sorted(missing)}')
        for name in required - {'portable.marker'}:
            if not archive.read(name).strip():
                raise ValueError(f'empty required entry: {name}')
        for name in license_files:
            if archive.read(name) != (ROOT / name).read_bytes():
                raise ValueError(f'license differs from source: {name}')
        if archive.read('portable.marker'):
            raise ValueError('portable.marker must be empty')
        bad = archive.testzip()
        if bad:
            raise ValueError(f'corrupt archive entry: {bad}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    build = sub.add_parser('build')
    build.add_argument('--platform', choices=('windows', 'linux'), required=True)
    build.add_argument('--version', required=True)
    build.add_argument('--binaries', type=Path, required=True)
    build.add_argument('--notices', type=Path, required=True, help='reviewed generated locked-graph notices')
    build.add_argument('--extension', type=Path)
    build.add_argument('--output', type=Path, required=True, help='new output directory')
    bundle = sub.add_parser('bundle', help='combine prepared artifacts into a new release directory')
    bundle.add_argument('--version', required=True)
    bundle.add_argument('--windows', type=Path, required=True)
    bundle.add_argument('--linux', type=Path, required=True)
    bundle.add_argument('--installer', type=Path, required=True)
    bundle.add_argument('--output', type=Path, required=True)
    check = sub.add_parser('check')
    check.add_argument('--platform', choices=('windows', 'linux'), required=True)
    check.add_argument('archive', type=Path)
    for command in (build, bundle, check):
        command.add_argument('--scan-data', required=True, type=Path)
        command.add_argument('--scan-receipt', required=True, type=Path)
        command.add_argument('--scan-tool', required=True, type=Path)
    args = parser.parse_args()
    expected_notices = verified_notices(args.scan_data, args.scan_receipt, args.scan_tool)
    if args.command == 'check':
        validate(args.archive, args.platform)
        with zipfile.ZipFile(args.archive) as archive:
            if archive.read('licenses/RUST-NOTICES.txt') != expected_notices:
                raise ValueError('archive notices differ from verified scan')
        return
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.]+)?', args.version):
        raise ValueError('invalid version')
    if args.command == 'bundle':
        validate(args.windows, 'windows')
        validate(args.linux, 'linux')
        for archive_path in (args.windows, args.linux):
            with zipfile.ZipFile(archive_path) as archive:
                if archive.read('licenses/RUST-NOTICES.txt') != expected_notices:
                    raise ValueError('archive notices differ from verified scan')
        if args.installer.is_symlink() or not args.installer.is_file() or not args.installer.stat().st_size:
            raise ValueError('installer missing, empty or symlinked')
        args.output.mkdir(parents=True, exist_ok=False)
        artifacts = {
            f'clipmonger-{args.version}-windows-x86_64.zip': args.windows,
            f'clipmonger-{args.version}-linux-x86_64.zip': args.linux,
            f'clipmonger-{args.version}-windows-x86_64-setup.exe': args.installer,
        }
        lines = []
        for name, source in sorted(artifacts.items()):
            data = source.read_bytes()
            (args.output / name).write_bytes(data)
            lines.append(f'{hashlib.sha256(data).hexdigest()}  {name}\n')
        (args.output / 'SHA256SUMS').write_text(''.join(lines), encoding='ascii')
        return
    payload = contents(args.platform, args.binaries, args.notices, args.extension)
    if payload['licenses/RUST-NOTICES.txt'] != expected_notices:
        raise ValueError('notices differ from verified scan')
    args.output.mkdir(parents=True, exist_ok=False)
    path = args.output / f'clipmonger-{args.version}-{args.platform}-x86_64.zip'
    with zipfile.ZipFile(path, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, data in sorted(payload.items()):
            info = zipfile.ZipInfo(name, (1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = ((0o100755 if args.platform == 'linux' and name in BINS else 0o100644) << 16)
            archive.writestr(info, data, compresslevel=9)
    validate(path, args.platform)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    (args.output / 'SHA256SUMS').write_text(f'{digest}  {path.name}\n', encoding='ascii')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, zipfile.BadZipFile) as exc:
        print(f'package-portable: {exc}', file=sys.stderr)
        sys.exit(1)
