#!/usr/bin/env python3
"""Render cargo-about JSON only when its license texts have local provenance.

Run cargo-about separately with the frozen workspace and target options; this
script does not execute Cargo or prove that its input represents that graph.
"""
import argparse
import hashlib
import json
from pathlib import Path
import sys
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
ALLOW = {'MIT', 'Apache-2.0', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib', 'Unicode-3.0', 'BSL-1.0', 'MPL-2.0', 'Unlicense'}
FONT = {'OFL-1.1', 'Ubuntu-font-1.0'}


def verify_cached_package(crate, lock):
    identity = (crate['name'], crate['version'], crate['source'])
    matches = [p for p in lock['package'] if (p['name'], p['version'], p.get('source')) == identity]
    if len(matches) != 1 or not matches[0].get('checksum'):
        raise ValueError(f'missing locked registry checksum: {identity}')
    root = Path(crate['manifest_path']).parent
    archive = root.parent.parent.parent / 'cache' / root.parent.name / (root.name + '.crate')
    if hashlib.sha256(archive.read_bytes()).hexdigest() != matches[0]['checksum']:
        raise ValueError(f'cached crate checksum mismatch: {identity}')
    with tarfile.open(archive, 'r:gz') as tar:
        for member in tar.getmembers():
            if not member.isfile():
                continue
            relative = Path(member.name).relative_to(root.name)
            if '..' in relative.parts:
                raise ValueError(f'unsafe crate member: {member.name}')
            path = root / relative
            if path.is_symlink() or not path.is_file() or path.read_bytes() != tar.extractfile(member).read():
                raise ValueError(f'cached package content mismatch: {identity} {relative}')
            if crate['name'] in {'ecolor', 'egui', 'egui-winit', 'emath', 'epaint'} and ('notice' in path.name.lower() or 'copyright' in path.name.lower()):
                raise ValueError(f'additional attribution file requires review: {identity} {relative}')


def render(data, lock, clarifications=None, workspace_license=None):
    locked = {(p['name'], p['version'], p.get('source')) for p in lock['package']}
    crates = data['crates']
    if not crates or not data['licenses']:
        raise ValueError('empty crate or license list')
    identities = {(x['package']['name'], x['package']['version'], x['package']['source']) for x in crates}
    if len(identities) != len(crates) or not identities <= locked:
        raise ValueError('crate identities duplicate or absent from Cargo.lock')
    covered = set()
    blocks = ['Rust dependency notices\n']
    for license in data['licenses']:
        license_id, text, uses = license['id'], license['text'], license['used_by']
        if license_id not in ALLOW | FONT or not uses or not text.strip():
            raise ValueError(f'unaccepted, empty or unused license: {license_id}')
        groups = {}
        if license['source_path']:
            source_path = Path(license['source_path']).resolve(strict=True)
            if not any(source_path.is_relative_to(Path(u['crate']['manifest_path']).parent.resolve(strict=True)) for u in uses):
                raise ValueError(f'license source outside associated crates: {license_id}')
        for use in uses:
            c = use['crate']
            identity = (c['name'], c['version'], c['source'])
            if identity not in identities:
                raise ValueError(f'license references unknown crate: {identity}')
            if license_id in FONT and (c['name'], c['version']) != ('epaint_default_fonts', '0.37.0'):
                raise ValueError(f'font exception used by unexpected crate: {identity}')
            source = license['source_path']
            chosen = text
            chosen_id = license_id
            if not source:
                if c['source'] is None and workspace_license is not None:
                    chosen = workspace_license.read_text(encoding='utf-8')
                elif c['name'] in {'ecolor', 'egui', 'egui-winit', 'emath', 'epaint'} and c['version'] == '0.37.0' and license_id == 'MIT' and c['license'] == 'MIT OR Apache-2.0':
                    verify_cached_package(c, lock)
                    canonical = ROOT / 'licenses/rust-upstream/apache-2.0.txt'
                    raw = canonical.read_bytes()
                    if hashlib.sha256(raw).hexdigest() != 'cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30':
                        raise ValueError('canonical Apache-2.0 text mismatch')
                    chosen, chosen_id = raw.decode('utf-8'), 'Apache-2.0'
                else:
                    key = (c['name'], c['version'], license_id)
                    item = (clarifications or {}).get(key)
                    if item is None:
                        raise ValueError(f'license text lacks local source: {identity} {license_id}')
                    if item.get('binding') == 'whole-published-payload-match':
                        if identity[:2] != ('winapi-x86_64-pc-windows-gnu', '0.4.0') or item.get('archive_sha256') != '712e227841d057c1ee1cd2fb22fa7e5a5461ae8e48fa2ca79ec42cfc1931183f':
                            raise ValueError(f'unsupported archive-bound clarification: {identity}')
                        verify_cached_package(c, lock)
                    else:
                        vcs_path = Path(c['manifest_path']).parent / '.cargo_vcs_info.json'
                        vcs = json.loads(vcs_path.read_text(encoding='utf-8'))
                        if vcs['git']['sha1'] != item['commit']:
                            raise ValueError(f'upstream commit mismatch: {identity}')
                    raw = item['path'].read_bytes()
                    if hashlib.sha256(raw).hexdigest() != item['sha256']:
                        raise ValueError(f'upstream text hash mismatch: {identity}')
                    chosen = raw.decode('utf-8')
            elif identity == ('wayland-protocols-plasma', '0.3.12', 'registry+https://github.com/rust-lang/crates.io-index') and license_id == 'MIT' and source.endswith('/plasma-wayland-protocols/LICENSES/MIT.txt'):
                verify_cached_package(c, lock)
                raw = (Path(c['manifest_path']).parent / 'LICENSE.txt').read_bytes()
                if hashlib.sha256(raw).hexdigest() != '1cb6f4dee8c649f175736380495f0af0c753c87f22d50931692968bb24bf2161':
                    raise ValueError('plasma crate root MIT license mismatch')
                chosen = raw.decode('utf-8')
            else:
                source_text = Path(source).read_text(encoding='utf-8').replace('\r\n', '\n').strip()
                if source_text != text.replace('\r\n', '\n').strip():
                    raise ValueError(f'license text differs from its source: {license_id}')
            if not chosen.strip() or (chosen_id == 'MIT' and ('<copyright holders>' in chosen.lower() or '<year>' in chosen.lower())):
                raise ValueError(f'generic placeholder license: {identity}')
            covered.add(identity)
            groups.setdefault((chosen_id, chosen), []).append(f"{c['name']} {c['version']}")
        for (selected_id, chosen), names in sorted(groups.items()):
            blocks.append(f"{'=' * 72}\n{selected_id}\nUsed by: {', '.join(sorted(names))}\n\n{chosen.rstrip()}\n")
    if covered != identities:
        raise ValueError('one or more crates lack license text')
    return '\n'.join(blocks)


def read_clarifications():
    root = ROOT / 'licenses/rust-upstream'
    records = json.loads((root / 'sources.json').read_text(encoding='utf-8'))
    result = {}
    for record in records:
        name = record['file']
        if Path(name).name != name or not name.endswith('.txt'):
            raise ValueError('invalid clarification filename')
        item = {'commit': record['commit'], 'sha256': record['sha256'], 'path': root / name, 'binding': record.get('binding'), 'archive_sha256': record.get('archive_sha256')}
        for crate in record['crates']:
            name, version = crate.rsplit('@', 1)
            key = (name, version, record['license'])
            if key in result:
                raise ValueError(f'duplicate clarification: {key}')
            result[key] = item
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('data', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--receipt', required=True, type=Path)
    parser.add_argument('--scan-tool', required=True, type=Path)
    args = parser.parse_args()
    from notice_provenance import verified_notices
    result = verified_notices(args.data, args.receipt, args.scan_tool)
    with args.output.open('xb') as f:
        f.write(result)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, json.JSONDecodeError) as exc:
        print(f'render-rust-notices: {exc}', file=sys.stderr)
        sys.exit(1)
