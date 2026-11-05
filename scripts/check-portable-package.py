#!/usr/bin/env python3
"""Exercise portable packaging against synthetic payloads; never builds the app."""
import argparse
import hashlib
import os
import shutil
from pathlib import Path
import subprocess
import sys
import zipfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--fixture-dir', required=True, type=Path)
parser.add_argument('--makensis', required=True, type=Path)
parser.add_argument('--nsisdir', required=True, type=Path)
parser.add_argument('--scan-data', required=True, type=Path)
parser.add_argument('--scan-receipt', required=True, type=Path)
parser.add_argument('--scan-tool', required=True, type=Path)
args = parser.parse_args()
root = args.fixture_dir.resolve()
root.mkdir(parents=True, exist_ok=False)
script = Path(__file__).with_name('package-portable.py').resolve()
bins = root / 'binaries'
bins.mkdir()
for binary in ('clipmonger', 'clipmonger-daemon', 'clipmonger-cli'):
    for suffix in ('', '.exe'):
        (bins / (binary + suffix)).write_text('synthetic binary\n')
notices = root / 'notices.txt'
subprocess.run([sys.executable, str(script.with_name('render-rust-notices.py')), str(args.scan_data), str(notices), '--receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], check=True)
ext = root / 'extension'
for name in tuple(name + '.js' for name in ('capture', 'codec', 'config', 'extension', 'hotkeys', 'ipc', 'paste', 'pasteplan', 'permbind', 'permhotkeys', 'placement', 'placer', 'polddiff', 'prefs', 'relay', 'report', 'sha256')) + ('metadata.json', 'stylesheet.css', 'schemas/org.gnome.shell.extensions.clipmonger.gschema.xml'):
    path = ext / name
    path.parent.mkdir(parents=True, exist_ok=True)
    if name.endswith('.gschema.xml'):
        shutil.copyfile(script.parent.parent / 'gnome-ext' / name, path)
    else:
        path.write_text('synthetic extension\n')


def run(*argv, ok=True):
    result = subprocess.run([sys.executable, str(script), *map(str, argv), '--scan-data', str(args.scan_data), '--scan-receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], capture_output=True, text=True)
    if (result.returncode == 0) != ok:
        raise AssertionError(f'{argv}: exit {result.returncode}: {result.stderr}')
    return result


for platform in ('windows', 'linux'):
    out = root / platform
    command = ['build', '--platform', platform, '--version', '0.1.0', '--binaries', bins, '--notices', notices, '--output', out]
    if platform == 'linux':
        command += ['--extension', ext]
    run(*command)
    archive = next(out.glob('*.zip'))
    run('check', '--platform', platform, archive)
    assert (out / 'SHA256SUMS').read_text() == f'{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n'
    run(*command, ok=False)
    second = root / (platform + '-second')
    second_command = command.copy()
    second_command[second_command.index('--output') + 1] = second
    run(*second_command)
    assert archive.read_bytes() == next(second.glob('*.zip')).read_bytes()
    with zipfile.ZipFile(archive) as source:
        payload = {i.filename: source.read(i) for i in source.infolist()}
    for missing in ('portable.marker', 'licenses/RUST-NOTICES.txt', 'licenses/fonts/README.md', 'clipmonger' + ('.exe' if platform == 'windows' else '')):
        bad = root / (platform + '-' + missing.replace('/', '_') + '.zip')
        with zipfile.ZipFile(bad, 'w') as target:
            for name, data in payload.items():
                if name != missing:
                    target.writestr(name, data)
        run('check', '--platform', platform, bad, ok=False)
    if platform == 'linux':
        schema_name = 'gnome-extension/schemas/gschemas.compiled'
        assert schema_name in payload and payload[schema_name]
        for kind in ('missing', 'corrupt'):
            bad_schema = root / ('linux-schema-' + kind + '.zip')
            with zipfile.ZipFile(bad_schema, 'w') as target:
                for name, data in payload.items():
                    if name == schema_name and kind == 'missing':
                        continue
                    target.writestr(name, b'corrupt schema' if name == schema_name else data)
            run('check', '--platform', platform, bad_schema, ok=False)
        schema_dir = root / 'packaged-schema'
        schema_dir.mkdir()
        (schema_dir / 'gschemas.compiled').write_bytes(payload[schema_name])
        env = os.environ.copy()
        env.update(GSETTINGS_BACKEND='memory', XDG_DATA_DIRS=str(schema_dir), XDG_DATA_HOME=str(schema_dir))
        loaded = subprocess.run(['gsettings', '--schemadir', str(schema_dir), 'list-keys', 'org.gnome.shell.extensions.clipmonger'], env=env, capture_output=True, text=True, check=True)
        assert set(loaded.stdout.splitlines()) == {'denylist', 'popup', 'profile-dir'}
    bad = root / (platform + '-unsafe.zip')
    with zipfile.ZipFile(bad, 'w') as target:
        for name, data in payload.items():
            target.writestr(name, data)
        target.writestr('../escape', b'x')
    run('check', '--platform', platform, bad, ok=False)
    print(platform + ': PASS (synthetic payload, determinism and negative controls)')
# Input failures must be rejected by the real builder as well as its checker.
missing = bins / 'clipmonger-daemon.exe'
missing.rename(bins / 'held-daemon.exe')
run('build', '--platform', 'windows', '--version', '0.1.0', '--binaries', bins,
    '--notices', notices, '--output', root / 'missing-binary', ok=False)
(bins / 'held-daemon.exe').rename(missing)
empty = root / 'empty-notices.txt'
empty.write_text('')
run('build', '--platform', 'windows', '--version', '0.1.0', '--binaries', bins,
    '--notices', empty, '--output', root / 'empty-notices', ok=False)
(ext / 'ipc.js').unlink()
run('build', '--platform', 'linux', '--version', '0.1.0', '--binaries', bins,
    '--notices', notices, '--extension', ext, '--output', root / 'missing-extension', ok=False)
print('builder rejects missing binary, empty notices, and missing extension runtime: PASS')
installer = root / 'synthetic-installer.exe'
installer_input = root / 'installer-input'
installer_input.mkdir()
for name in ('clipmonger.exe', 'clipmonger-daemon.exe', 'clipmonger-cli.exe'):
    shutil.copyfile(bins / name, installer_input / name)
shutil.copytree(script.parent.parent / 'licenses', installer_input / 'licenses')
shutil.copyfile(notices, installer_input / 'licenses/RUST-NOTICES.txt')
builder = Path(__file__).with_name('build-windows-installer.py')
subprocess.run([sys.executable, str(builder), '--input', str(installer_input), '--output', str(installer),
                '--makensis', str(args.makensis), '--nsisdir', str(args.nsisdir), '--scan-data', str(args.scan_data), '--scan-receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], check=True, stdout=subprocess.DEVNULL)
bundle = root / 'release'
run('bundle', '--version', '0.1.0', '--windows', next((root / 'windows').glob('*.zip')),
    '--linux', next((root / 'linux').glob('*.zip')), '--installer', installer, '--output', bundle)
checker = Path(__file__).with_name('check-release-artifacts.py')
def check_bundle(ok):
    result = subprocess.run([sys.executable, str(checker), '--version', '0.1.0', str(bundle), '--scan-data', str(args.scan_data), '--scan-receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], capture_output=True, text=True)
    if (result.returncode == 0) != ok:
        raise AssertionError(f'bundle check: {result.stdout} {result.stderr}')
check_bundle(True)
# Replacing notices while updating the ordinary checksum must still fail.
windows_zip = bundle / 'clipmonger-0.1.0-windows-x86_64.zip'
original_zip = windows_zip.read_bytes()
with zipfile.ZipFile(windows_zip) as original:
    contents = [(entry, original.read(entry)) for entry in original.infolist()]
with zipfile.ZipFile(windows_zip, 'w') as changed:
    for entry, data in contents:
        changed.writestr(entry, b'fabricated notice' if entry.filename == 'licenses/RUST-NOTICES.txt' else data)
checksum_file = bundle / 'SHA256SUMS'
original_checksums = checksum_file.read_text()
old_hash = hashlib.sha256(original_zip).hexdigest()
checksum_file.write_text(original_checksums.replace(old_hash, hashlib.sha256(windows_zip.read_bytes()).hexdigest()))
check_bundle(False)
windows_zip.write_bytes(original_zip)
checksum_file.write_text(original_checksums)
with zipfile.ZipFile(windows_zip, 'w') as changed:
    for entry, data in contents:
        changed.writestr(entry, b'fake font notice' if entry.filename == 'licenses/fonts/README.md' else data)
checksum_file.write_text(original_checksums.replace(old_hash, hashlib.sha256(windows_zip.read_bytes()).hexdigest()))
check_bundle(False)
windows_zip.write_bytes(original_zip)
checksum_file.write_text(original_checksums)
corrupt = root / 'corrupt-scan.json'
corrupt.write_bytes(args.scan_data.read_bytes() + b' ')
check = subprocess.run([sys.executable, str(checker), '--version', '0.1.0', str(bundle), '--scan-data', str(corrupt), '--scan-receipt', str(args.scan_receipt), '--scan-tool', str(args.scan_tool)], capture_output=True)
assert check.returncode != 0
bad_receipt = root / 'corrupt-receipt.json'
bad_receipt.write_bytes(args.scan_receipt.read_bytes() + b' ')
check = subprocess.run([sys.executable, str(checker), '--version', '0.1.0', str(bundle), '--scan-data', str(args.scan_data), '--scan-receipt', str(bad_receipt), '--scan-tool', str(args.scan_tool)], capture_output=True)
assert check.returncode != 0
changed_zip = root / 'mismatched-windows.zip'
with zipfile.ZipFile(next((root / 'windows').glob('*.zip'))) as original, zipfile.ZipFile(changed_zip, 'w') as changed:
    for entry in original.infolist():
        data = original.read(entry)
        changed.writestr(entry, b'other binary' if entry.filename == 'clipmonger.exe' else data)
inspector = Path(__file__).with_name('check-windows-installer.py')
assert subprocess.run([sys.executable, str(inspector), str(installer), str(changed_zip)], capture_output=True).returncode != 0
(bundle / 'clipmonger-0.1.0-windows-x86_64-setup.exe').write_bytes(b'tampered')
check_bundle(False)
print('combined checksums, NSIS contents and tampering control: PASS (synthetic inputs)')
print('No app build, Cargo license generation, native Windows execution, or release artifact was tested. Notices were rendered from the supplied scan evidence; its origin must be assessed separately.')
