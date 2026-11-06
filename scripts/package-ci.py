#!/usr/bin/env python3
"""Package the verified macOS MCP executable, license notices and corresponding source."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile

sys.dont_write_bytecode = True
from stdio_rpc import Rpc

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'aarch64-apple-darwin'


def run(command, **kwargs):
    return subprocess.check_output(command, cwd=ROOT, text=True, **kwargs)


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def archive(folder, destination, name, timestamp):
    with destination.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=timestamp) as compressed:
        with tarfile.open(fileobj=compressed, mode='w') as tar:
            for path in [folder, *sorted(folder.rglob('*'))]:
                if path.is_symlink():
                    raise ValueError(f'symlinks are not packaged: {path}')
                info = tar.gettarinfo(str(path), str(Path(name) / path.relative_to(folder)))
                info.uid = info.gid = 0
                info.uname = info.gname = ''
                info.mtime = timestamp
                info.mode = 0o755 if path.is_dir() or os.access(path, os.X_OK) else 0o644
                if path.is_file():
                    with path.open('rb') as stream:
                        tar.addfile(info, stream)
                else:
                    tar.addfile(info)


def verify_tools(names, data):
    matrix = json.loads((ROOT / 'schemas/capability-matrix.json').read_text())
    declared = matrix['advertised_tools']
    always = set(declared['always'])
    conditional = set(declared['conditional'])
    schematic = set(next(entry for entry in matrix['capabilities']
                         if entry['id'] == 'schematic.fixed_two_resistor_change')['mcp_tools'])
    recovery = {'kicad_plan_recovery', 'kicad_preview_recovery', 'kicad_commit_recovery'}
    assert conditional == schematic | recovery, declared
    statuses = {entry['id']: entry['status'] for entry in data['capabilities']}
    status = statuses['schematic.fixed_two_resistor_change']
    assert status in {'verified', 'unavailable'}, status
    expected = always | recovery | (schematic if status == 'verified' else set())
    assert names == expected, dict(expected=sorted(expected), actual=sorted(names))
    assert set(data['advertised_tools']) == names, data['advertised_tools']
    return dict(schematic_backend_available=status == 'verified',
                unavailable_optional_tools=sorted((always | conditional) - names))


def smoke(binary, output):
    version = run([str(binary), '--version'], timeout=15).strip()
    info = json.loads(run([str(binary), '--build-info'], timeout=15))
    assert info['target_os'] == 'macos' and info['target_arch'] == 'aarch64', info
    assert info['license'] == 'GPL-3.0-or-later', info
    assert info['repository'] == 'https://github.com/W-Mai/auto-kicad', info
    assert info['cargo_lock_sha256'] == sha(ROOT / 'Cargo.lock'), info
    assert version == 'auto-kicad ' + info['version'], version
    assert 'standard input/output' in run([str(binary), '--help'], timeout=15)
    with tempfile.TemporaryDirectory(prefix='auto-kicad-ci-smoke-') as temporary:
        scratch = Path(temporary) / 'tmp'
        scratch.mkdir()
        environment = dict(os.environ, AUTO_KICAD_STATE_DIR=str(Path(temporary) / 'state'), TMPDIR=str(scratch))
        rpc = Rpc([str(binary)], env=environment, log=output / 'smoke.log')
        try:
            rpc.call('initialize', dict(protocolVersion='2025-03-26', capabilities={},
                                       clientInfo=dict(name='auto-kicad-ci', version='1')))
            rpc.send(dict(jsonrpc='2.0', method='notifications/initialized'))
            names = {tool['name'] for tool in rpc.call('tools/list', {})['tools']}
            response = rpc.call('tools/call', dict(name='kicad_capabilities', arguments={}))
            assert not response.get('isError') and response['structuredContent']['ok'], response
            availability = verify_tools(names, response['structuredContent']['data'])
        finally:
            rpc.close()
    result = dict(passed=True, version=version, tools=sorted(names),
                  native_editor_started=False, native_cad_acceptance=False, **availability)
    write_json(output / 'smoke.json', result)
    return info


def copy_notices(package, destination):
    root = Path(package['manifest_path']).parent
    copied = []
    for path in sorted(root.rglob('*')):
        if not path.is_file() or path.is_symlink():
            continue
        if not path.name.lower().startswith(('license', 'licence', 'copying', 'notice', 'copyright')):
            continue
        relative = path.relative_to(root)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(path, target)
        copied.append(str(relative))
    return copied


def package(binary, output):
    assert not run(['git', 'status', '--porcelain', '--untracked-files=no']).strip(), 'package a clean source checkout'
    output.mkdir(parents=True)
    info = smoke(binary, output)
    revision = run(['git', 'rev-parse', 'HEAD']).strip()
    timestamp = int(run(['git', 'log', '-1', '--format=%ct']).strip())
    metadata = json.loads(run(['cargo', 'metadata', '--locked', '--format-version', '1']))
    version = info['version']
    binary_name = f'auto-kicad-{version}-{TARGET}'
    source_name = f'auto-kicad-{version}-source'
    with tempfile.TemporaryDirectory(prefix='auto-kicad-ci-package-') as temporary:
        staging = Path(temporary)
        payload = staging / binary_name
        source = staging / source_name
        payload.mkdir()
        source.mkdir()
        shutil.copyfile(binary, payload / 'auto-kicad')
        (payload / 'auto-kicad').chmod(0o755)
        for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
            shutil.copyfile(ROOT / name, payload / name)
        (payload / 'scripts').mkdir()
        for name in ['install-local.py']:
            shutil.copyfile(ROOT / 'scripts' / name, payload / 'scripts' / name)
        packages = []
        for item in sorted(metadata['packages'], key=lambda item: (item['name'], item['version'])):
            assert item.get('license') or item.get('license_file'), item['name']
            notices = []
            if item.get('source') is not None:
                notices = copy_notices(item, payload / 'dependency-licenses' / (item['name'] + '-' + item['version']))
            packages.append(dict(name=item['name'], version=item['version'], license=item.get('license'),
                                 license_file=bool(item.get('license_file')), source=item.get('source'), notices=notices))
        inventory = dict(schema_version=1, format='auto-kicad-dependency-inventory', component='auto-kicad',
                         version=version, binary_sha256=sha(binary), cargo_lock_sha256=sha(ROOT / 'Cargo.lock'),
                         dependency_lock_verified=True, dependency_inventory_complete=True, packages=packages,
                         license_decision='GPL-3.0-or-later; see LICENSE and THIRD_PARTY_NOTICES.md',
                         embedded_protocol_notice='KiCad protocol source headers include GPL-3.0-or-later',
                         source_revision=revision, corresponding_source=source_name + '.tar.gz')
        write_json(payload / 'sbom.json', inventory)
        write_json(payload / 'build-info.json', dict(info, source_revision=revision))
        shutil.copyfile(output / 'smoke.json', payload / 'smoke.json')
        (payload / 'README.md').write_text(f'''# auto-kicad {version}

MCP executable for macOS arm64 and KiCad 10.0.7. This archive is unsigned. KiCad and its libraries must be installed separately.

Run from this extracted directory:

```sh
python3 scripts/install-local.py ./auto-kicad --no-rollback
codex mcp add auto-kicad -- "$HOME/.local/bin/auto-kicad"
```

Set `tool_timeout_sec = 600` in the existing Codex MCP configuration section and start a new session. The installer adds the KiCad CLI directory to PATH. Source revision: `{revision}`.

The matching `{source_name}.tar.gz` archive contains source and vendored Cargo dependencies. Package documentation: https://github.com/W-Mai/auto-kicad/blob/main/docs/releases.md
''')
        tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=ROOT).decode().split('\0')
        for name in filter(None, tracked):
            original = ROOT / name
            assert original.is_file() and not original.is_symlink(), name
            target = source / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, target)
        vendor = source / 'vendor'
        configuration = run(['cargo', 'vendor', '--locked', '--offline', '--respect-source-config',
                             '--versioned-dirs', str(vendor)], stderr=subprocess.DEVNULL)
        configuration = configuration.replace(str(vendor), 'vendor')
        assert 'directory = "vendor"' in configuration, configuration
        config = source / '.cargo/config.toml'
        config.parent.mkdir(exist_ok=True)
        assert not config.exists(), 'source already declares Cargo source configuration'
        config.write_text(configuration)
        with tempfile.TemporaryDirectory(prefix='auto-kicad-source-cargo-') as cargo_home:
            independent = subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--offline', '--format-version', '1'], cwd=source,
                env=dict(os.environ, CARGO_HOME=cargo_home), text=True)
            observed = json.loads(independent)
            expected_packages = sorted((p['name'], p['version']) for p in metadata['packages'])
            assert sorted((p['name'], p['version']) for p in observed['packages']) == expected_packages
        write_json(source / 'source-info.json', dict(revision=revision, version=version, target=TARGET,
                                                    cargo_lock_sha256=sha(ROOT / 'Cargo.lock')))
        # The packaged installer must work away from this Cargo checkout.
        prefix = staging / 'installed'
        installer = payload / 'scripts/install-local.py'
        subprocess.run([sys.executable, str(installer), str(payload / 'auto-kicad'), '--prefix', str(prefix), '--no-rollback'],
                       check=True, stdout=subprocess.DEVNULL)
        subprocess.run([sys.executable, str(installer), '--prefix', str(prefix), '--verify'],
                       check=True, stdout=subprocess.DEVNULL)
        subprocess.run([sys.executable, str(installer), '--prefix', str(prefix), '--uninstall'],
                       check=True, stdout=subprocess.DEVNULL)
        archive(payload, output / (binary_name + '.tar.gz'), binary_name, timestamp)
        archive(source, output / (source_name + '.tar.gz'), source_name, timestamp)
    write_json(output / 'build-info.json', dict(info, source_revision=revision))
    (output / 'RELEASE_NOTES.md').write_text(f'''macOS arm64 MCP executable for KiCad 10.0.7.

Includes the local installer, dependency/license inventory, native CLI/MCP smoke results and matching source with vendored dependencies. Builds are unsigned. Download both binary and source archives; verify them against SHA256SUMS.

Source: https://github.com/W-Mai/auto-kicad/tree/{revision}
''')
    (output / 'smoke.log').unlink()
    checksums = []
    for path in sorted(output.iterdir()):
        assert path.is_file() and not path.is_symlink(), path
        checksums.append(sha(path) + '  ' + path.name)
    (output / 'SHA256SUMS').write_text('\n'.join(checksums) + '\n')
    print(json.dumps(dict(version=version, target=TARGET, source_revision=revision,
                         packages=len(packages), artifacts=sorted(p.name for p in output.iterdir()))), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    package(args.binary.resolve(strict=True), args.output.resolve())
