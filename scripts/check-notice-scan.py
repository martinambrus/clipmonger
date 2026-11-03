#!/usr/bin/env python3
"""Check that recorded notice evidence rejects changed inputs; no Cargo."""
import argparse
import json
from pathlib import Path
import shutil
import tempfile
import notice_provenance as provenance

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--scan-data', required=True, type=Path)
parser.add_argument('--scan-receipt', required=True, type=Path)
parser.add_argument('--scan-tool', required=True, type=Path)
a = parser.parse_args()
expected = provenance.verified_notices(a.scan_data, a.scan_receipt, a.scan_tool)
with tempfile.TemporaryDirectory(prefix='notice-evidence-') as temp:
    root = Path(temp)
    data = root / 'data.json'
    receipt = root / 'receipt.json'
    shutil.copyfile(a.scan_data, data)
    shutil.copyfile(a.scan_receipt, receipt)
    shutil.copyfile(a.scan_receipt.with_suffix('.manifest.json'), receipt.with_suffix('.manifest.json'))
    assert provenance.verified_notices(data, receipt, a.scan_tool) == expected
    def must_fail(d, r, t):
        try:
            provenance.verified_notices(d, r, t)
        except (OSError, ValueError, KeyError):
            return
        raise AssertionError('modified evidence was accepted')
    subset = json.loads(data.read_text())
    subset['crates'] = subset['crates'][:1]
    data.write_text(json.dumps(subset))
    must_fail(data, receipt, a.scan_tool)
    shutil.copyfile(a.scan_data, data)
    receipt.write_bytes(receipt.read_bytes() + b' ')
    must_fail(data, receipt, a.scan_tool)
    shutil.copyfile(a.scan_receipt, receipt)
    bad_tool = root / 'tool'
    bad_tool.write_bytes(b'not cargo-about')
    must_fail(data, receipt, bad_tool)
    fake_root = root / 'source'
    (fake_root / 'packaging').mkdir(parents=True)
    (fake_root / 'packaging/about.toml').write_text('accepted = []\n')
    old_root = provenance.ROOT
    provenance.ROOT = fake_root
    try:
        must_fail(data, receipt, a.scan_tool)
    finally:
        provenance.ROOT = old_root
print('Copied evidence accepted; subset, receipt, tool and config tampering rejected (fixture only).')
