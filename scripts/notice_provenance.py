"""Validate per-run scan evidence and render from the current Cargo cache."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parent.parent
TOOL_SHA = 'e902609296601f873c08ca912acea0c37b375128ea3940eb307a76080afb8211'
CONFIG_SHA = 'f1f8a05bfa523d1233ad9661db8d97b086a27754283d5dc580cb36dddb38078d'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def wrapper_sha():
    return digest(ROOT / 'scripts/notice-scan-wrapper.py')


def validate_receipt(receipt, data, capture=False):
    for name, expected in [('status', 'PASS'), ('direct_exit', 0), ('effective_gate_exit', 0), ('source_changed', False), ('cleanup_verified', True), ('evidence_complete', True), ('resource_violation', False), ('next_calls_must_hold', False)]:
        if receipt.get(name) != expected:
            raise ValueError(f'guard receipt failed {name}')
    if receipt['before'] != receipt['after']:
        raise ValueError('guard source identities changed during scan')
    for name, value in receipt['before'].items():
        if name in ('Cargo.toml', 'Cargo.lock', 'deny.toml') or name.endswith('/Cargo.toml'):
            if digest(ROOT / name) != value:
                raise ValueError(f'locked graph input differs from scan: {name}')
    command = receipt['command']
    name = 'clipmonger-notice-scan-' + wrapper_sha()
    if (len(command) != 8 or command[:2] != ['cargo', name]
            or command[2::2] != ['--tool', '--config', '--output-file']
            or any(not Path(command[i]).is_absolute() for i in (3, 5, 7))):
        raise ValueError('unexpected generator wrapper or arguments')
    if capture and Path(command[-1]).resolve(strict=True) != data.resolve(strict=True):
        raise ValueError('scan output is not the file named in the receipt')



def relocate(data, cargo_home):
    source_root = cargo_home / 'registry/src'
    crates = [item['package'] for item in data['crates']]
    old_to_new = {}
    for crate in crates:
        old = Path(crate['manifest_path']).parent
        if crate['source'] is None:
            new = ROOT / 'crates' / old.name
        else:
            candidates = [p / old.name for p in source_root.iterdir() if (p / old.name / 'Cargo.toml').is_file()]
            if len(candidates) != 1:
                raise ValueError(f'cannot locate unique cached crate: {old.name}')
            new = candidates[0]
        old_to_new[str(old)] = new
    def mapped(path):
        matches = [(old, new) for old, new in old_to_new.items() if path == old or path.startswith(old + '/')]
        if len(matches) != 1:
            raise ValueError(f'path does not map to a scan crate: {path}')
        old, new = matches[0]
        return str(new / path[len(old):].lstrip('/'))
    for crate in crates:
        crate['manifest_path'] = mapped(crate['manifest_path'])
    for license in data['licenses']:
        if license['source_path']:
            license['source_path'] = mapped(license['source_path'])
        for use in license['used_by']:
            use['crate']['manifest_path'] = mapped(use['crate']['manifest_path'])
    return data


def verified_notices(data_path, receipt_path, tool_path):
    data_path = Path(data_path).resolve(strict=True)
    receipt_path = Path(receipt_path).resolve(strict=True)
    tool_path = Path(tool_path).resolve(strict=True)
    manifest = json.loads(receipt_path.with_suffix('.manifest.json').read_text())
    if digest(data_path) != manifest['data_sha256'] or digest(receipt_path) != manifest['receipt_sha256']:
        raise ValueError('scan data or guard receipt hash differs from run manifest')
    if manifest.get('wrapper_sha256') != wrapper_sha() or manifest.get('tool_sha256') != TOOL_SHA or manifest.get('config_sha256') != CONFIG_SHA or digest(tool_path) != TOOL_SHA or digest(ROOT / 'packaging/about.toml') != CONFIG_SHA:
        raise ValueError('generator tool or configuration differs from approved version')
    receipt = json.loads(receipt_path.read_text())
    validate_receipt(receipt, data_path)
    cargo_home = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))).absolute()
    data = relocate(json.loads(data_path.read_text()), cargo_home)
    spec = importlib.util.spec_from_file_location('notice_renderer', ROOT / 'scripts/render-rust-notices.py')
    renderer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(renderer)
    with (ROOT / 'Cargo.lock').open('rb') as f:
        lock = tomllib.load(f)
    return renderer.render(data, lock, renderer.read_clarifications(), ROOT / 'LICENSE').encode('utf-8')
