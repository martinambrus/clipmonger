#!/usr/bin/env python3
"""Synthetic controls for notice rendering; does not run Cargo."""
import importlib.util
import hashlib
import json
from pathlib import Path
import tempfile

spec = importlib.util.spec_from_file_location('notices', Path(__file__).with_name('render-rust-notices.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with tempfile.TemporaryDirectory(prefix='clipmonger-notices-') as temp:
    source = Path(temp) / 'LICENSE'
    source.write_text('Copyright example\nPermission granted.\n')
    crate = {'name': 'example', 'version': '1.0.0', 'source': 'registry+example', 'manifest_path': str(Path(temp) / 'Cargo.toml')}
    data = {'crates': [{'package': crate}], 'licenses': [{'id': 'MIT', 'text': source.read_text(), 'source_path': str(source), 'used_by': [{'crate': crate}]}]}
    lock = {'package': [crate.copy()]}
    assert 'example 1.0.0' in module.render(data, lock)
    def rejects():
        try:
            module.render(data, lock)
        except ValueError:
            return
        raise AssertionError('invalid notice accepted')
    data['licenses'][0]['source_path'] = None
    rejects()
    data['licenses'][0]['source_path'] = str(source)
    data['licenses'][0]['text'] = 'Copyright (c) <year> <copyright holders>'
    rejects()
    data['licenses'][0]['text'] = 'Copyright different source'
    rejects()
    data['licenses'][0]['text'] = source.read_text()
    data['licenses'][0]['id'] = 'OFL-1.1'
    rejects()
    data['licenses'][0]['id'] = 'MIT'
    crate['version'] = '2.0.0'
    rejects()
    crate['version'] = '1.0.0'
    data['licenses'][0]['source_path'] = None
    crate['manifest_path'] = str(Path(temp) / 'Cargo.toml')
    (Path(temp) / '.cargo_vcs_info.json').write_text(json.dumps({'git': {'sha1': 'abc'}}))
    overrides = {('example', '1.0.0', 'MIT'): {'path': source, 'sha256': hashlib.sha256(source.read_bytes()).hexdigest(), 'commit': 'abc'}}
    assert 'example 1.0.0' in module.render(data, lock, overrides)
    overrides[('example', '1.0.0', 'MIT')]['commit'] = 'wrong'
    try:
        module.render(data, lock, overrides)
    except ValueError:
        pass
    else:
        raise AssertionError('mismatched VCS commit accepted')
print('Notice provenance/lock/font-exception negative controls: PASS (synthetic only).')
