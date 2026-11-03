#!/usr/bin/env python3
"""Synthetic generator binding controls; does not run Cargo or scan a graph."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import notice_provenance as p

sha = lambda b: hashlib.sha256(b).hexdigest()
with tempfile.TemporaryDirectory(prefix='notice-binding-') as tmp:
    root = Path(tmp)
    tool = root / 'tool'
    spy = root / 'spy.json'
    tool.write_text('#!/usr/bin/env python3\nimport json, os, sys\nopen(os.environ["SPY"], "w").write(json.dumps(sys.argv))\n')
    tool.chmod(0o755)
    config = root / 'about.toml'
    config.write_text('fixture')
    source = (p.ROOT / 'scripts/notice-scan-wrapper.py').read_text()
    source = source.replace(p.TOOL_SHA, sha(tool.read_bytes())).replace(p.CONFIG_SHA, sha(config.read_bytes()))
    wrapper_sha = sha(source.encode())
    name = 'clipmonger-notice-scan-' + wrapper_sha
    wrapper = root / ('cargo-' + name)
    wrapper.write_text(source)
    wrapper.chmod(0o755)
    env = dict(os.environ, SPY=str(spy))
    args = [str(wrapper), name, '--tool', str(tool), '--config', str(config), '--output-file', str(root / 'out')]
    def run(argv):
        return subprocess.run(argv, env=env, capture_output=True)
    assert run(args).returncode == 0
    assert json.loads(spy.read_text()) == [str(tool), 'about', 'generate', '--frozen', '--workspace', '--all-features', '--target', 'x86_64-pc-windows-gnu', '--target', 'x86_64-unknown-linux-gnu', '--fail', '--format', 'json', '--config', str(config), '--output-file', str(root / 'out')]
    other = root / 'other'
    other.write_text('wrong')
    assert run(args[:3] + [str(other)] + args[4:]).returncode != 0
    assert run(args[:5] + [str(other)] + args[6:]).returncode != 0
    renamed = root / 'cargo-clipmonger-notice-scan-wrong'
    renamed.write_text(source)
    renamed.chmod(0o755)
    assert run([str(renamed), 'clipmonger-notice-scan-wrong', *args[2:]]).returncode != 0
    receipt = {'status': 'PASS', 'direct_exit': 0, 'effective_gate_exit': 0, 'source_changed': False, 'cleanup_verified': True, 'evidence_complete': True, 'resource_violation': False, 'next_calls_must_hold': False, 'before': {}, 'after': {}, 'command': ['cargo', 'about', 'generate', '--frozen', '--workspace', '--all-features', '--target', 'x86_64-pc-windows-gnu', '--target', 'x86_64-unknown-linux-gnu', '--fail', '--format', 'json', '--config', str(config), '--output-file', str(root / 'out')]}
    try:
        p.validate_receipt(receipt, root / 'out')
    except ValueError:
        pass
    else:
        raise AssertionError('ordinary cargo-about receipt accepted')
    receipt['command'] = ['cargo', name, '--tool', str(tool), '--config', str(config), '--output-file', str(root / 'out')]
    original = p.wrapper_sha
    p.wrapper_sha = lambda: wrapper_sha
    try:
        p.validate_receipt(receipt, root / 'out')
        receipt['command'][1] += 'bad'
        try:
            p.validate_receipt(receipt, root / 'out')
        except ValueError:
            pass
        else:
            raise AssertionError('wrapper identity mismatch accepted')
    finally:
        p.wrapper_sha = original
print('Synthetic command spy and rejection controls passed; no fresh graph scanned.')
