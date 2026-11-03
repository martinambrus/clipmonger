#!/usr/bin/env python3
"""Record output hashes immediately after a guarded cargo-about scan."""
import argparse
import json
from pathlib import Path
from notice_provenance import CONFIG_SHA, ROOT, TOOL_SHA, digest, validate_receipt, wrapper_sha

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--scan-data', required=True, type=Path)
parser.add_argument('--scan-receipt', required=True, type=Path)
parser.add_argument('--scan-tool', required=True, type=Path)
parser.add_argument('--scan-wrapper', required=True, type=Path)
args = parser.parse_args()
try:
    data = args.scan_data.resolve(strict=True)
    receipt_path = args.scan_receipt.resolve(strict=True)
    receipt = json.loads(receipt_path.read_text())
    validate_receipt(receipt, data, capture=True)
    wrapper = args.scan_wrapper.resolve(strict=True)
    if wrapper.name != 'cargo-clipmonger-notice-scan-' + wrapper_sha() or digest(wrapper) != wrapper_sha():
        raise ValueError('wrapper identity or source differs from reviewed wrapper')
    if Path(receipt['command'][3]).resolve(strict=True) != args.scan_tool.resolve(strict=True):
        raise ValueError('receipt tool differs from supplied tool')
    if digest(args.scan_tool.resolve(strict=True)) != TOOL_SHA or digest(ROOT / 'packaging/about.toml') != CONFIG_SHA:
        raise ValueError('generator tool or configuration differs from approved version')
    if digest(Path(receipt['command'][-3]).resolve(strict=True)) != CONFIG_SHA:
        raise ValueError('receipt does not name the approved generator config')
    with receipt_path.with_suffix('.manifest.json').open('x') as f:
        json.dump({'data_sha256': digest(data), 'receipt_sha256': digest(receipt_path), 'wrapper_sha256': wrapper_sha(), 'tool_sha256': TOOL_SHA, 'config_sha256': CONFIG_SHA}, f, indent=2)
        f.write('\n')
    print('Scan manifest recorded; it is an integrity record, not a signature.')
except (OSError, ValueError, KeyError, TypeError) as exc:
    parser.exit(1, f'record-notice-scan: {exc}\n')
