#!/usr/bin/env python3
"""Install, verify, upgrade, roll back and remove owned local auto-kicad releases."""
import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import platform
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile

SCHEMA = 1
NAMES = ('auto-kicad', 'auto-kicad-replay')
RELEASE = re.compile(r'(?:[0-9A-Za-z][0-9A-Za-z._+-]{0,63}|legacy)-[0-9a-f]{16}')


def digest(path):
    with path.open('rb') as stream:
        value = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b''): value.update(chunk)
        return value.hexdigest()


def regular(path):
    return stat.S_ISREG(path.lstat().st_mode)


def sync_dir(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try: os.fsync(descriptor)
    finally: os.close(descriptor)


def safe_directory(path):
    if path.is_symlink(): raise ValueError(f'symlink directory is not owned: {path}')
    path.mkdir(exist_ok=True)
    if not path.is_dir(): raise ValueError(f'not a directory: {path}')


def atomic_file(path, data, mode=0o600):
    descriptor, temporary = tempfile.mkstemp(prefix='.auto-kicad-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            stream.write(data); stream.flush(); os.fsync(stream.fileno())
            os.fchmod(stream.fileno(), mode)
        os.replace(temporary, path); sync_dir(path.parent)
    finally: Path(temporary).unlink(missing_ok=True)


def atomic_json(path, value):
    atomic_file(path, (json.dumps(value, sort_keys=True, indent=2) + '\n').encode())


def atomic_link(path, target):
    descriptor, temporary = tempfile.mkstemp(prefix='.auto-kicad-', dir=path.parent)
    os.close(descriptor); os.unlink(temporary)
    try:
        os.symlink(target, temporary); os.replace(temporary, path); sync_dir(path.parent)
    finally: Path(temporary).unlink(missing_ok=True)


def launcher(prefix, name):
    installed = prefix / 'lib/auto-kicad' / name
    return ('#!/bin/sh\nPATH="/Applications/KiCad/KiCad.app/Contents/MacOS:${PATH:-/usr/bin:/bin}"\n'
        'export PATH\nexec ' + shlex.quote(str(installed)) + ' "$@"\n').encode()


@contextlib.contextmanager
def lock(root):
    descriptor = os.open(root / '.install.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode): raise ValueError('installer lock is not regular')
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally: os.close(descriptor)


def read_json(path):
    if not regular(path) or path.stat().st_size > 1024 * 1024: raise ValueError(f'invalid metadata: {path}')
    return json.loads(path.read_text())


def release_dir(root, identifier):
    if not RELEASE.fullmatch(identifier): raise ValueError('invalid release identifier')
    result = root / 'versions' / identifier
    if result.is_symlink(): raise ValueError('release directory is a symlink')
    return result


def verify_release(root, identifier):
    folder = release_dir(root, identifier)
    manifest = read_json(folder / 'release.json')
    if manifest.get('schema_version') != SCHEMA or manifest.get('release_id') != identifier:
        raise ValueError(f'release metadata differs: {identifier}')
    binaries = manifest.get('binaries', {})
    seed = json.dumps(dict(build=manifest['build_info'], binaries=binaries), sort_keys=True, separators=(',', ':')).encode()
    expected_id = manifest['build_info']['version'] + '-' + hashlib.sha256(seed).hexdigest()[:16]
    if identifier != expected_id or manifest.get('distribution') != 'local-only': raise ValueError('release content identity changed')
    if 'auto-kicad' not in binaries or not set(binaries) <= set(NAMES): raise ValueError('invalid executable inventory')
    expected = set(binaries) | {'release.json', 'sbom.json'}
    if {p.name for p in folder.iterdir()} != expected: raise ValueError(f'release contains unowned files: {identifier}')
    sbom = folder / 'sbom.json'
    if not regular(sbom) or digest(sbom) != manifest['sbom_sha256']: raise ValueError('SBOM changed')
    for name, record in binaries.items():
        path = folder / name
        if not regular(path) or path.stat().st_size != record['bytes'] or digest(path) != record['sha256']:
            raise ValueError(f'installed executable changed: {name}')
        if stat.S_IMODE(path.stat().st_mode) != record['mode']: raise ValueError(f'executable mode changed: {name}')
    return manifest


def current(root):
    path = root / 'current'
    if not path.is_symlink(): raise ValueError('activation pointer is missing or unowned')
    target = os.readlink(path)
    if not target.startswith('versions/') or '/' in target[len('versions/'):]: raise ValueError('activation pointer escapes owned versions')
    identifier = target[len('versions/'):]
    verify_release(root, identifier)
    return identifier


def installation(root):
    manifest = read_json(root / 'install.json')
    if manifest.get('schema_version') != SCHEMA: raise ValueError('unsupported installation schema')
    if not (root / 'current').is_symlink():
        if (root / 'current').exists(): raise ValueError('activation pointer is unowned')
        if manifest.get('active') is None and manifest.get('activation', {}).get('before') is None:
            return dict(manifest, active=None, previous=None, activation_pending=True)
        if manifest.get('legacy_migration'):
            legacy = verify_release(root, manifest['active'])
            for name, record in legacy['binaries'].items():
                binary = root / name
                if not regular(binary) or digest(binary) != record['sha256']: raise ValueError('legacy migration source changed')
            return dict(manifest, legacy_migration=True)
        raise ValueError('activation pointer is missing')
    activated = current(root)
    # The pointer is the atomic commit point; a killed installer can leave older descriptive metadata.
    known = manifest.get('releases', [])
    pruning = manifest.get('pruning')
    if pruning and pruning not in (activated, manifest.get('previous')) and pruning in known:
        if not release_dir(root, pruning).exists():
            known = [item for item in known if item != pruning]
            manifest = dict(manifest, releases=known)
    if activated not in known: raise ValueError('activated release has no recorded ownership')
    previous = manifest.get('previous')
    pending = manifest.get('activation', {})
    if pending and activated == pending.get('after'):
        previous = pending.get('before')
    return dict(manifest, active=activated, previous=previous)


def native_info(binary):
    completed = subprocess.run([str(binary), '--build-info'], capture_output=True, timeout=15)
    if completed.returncode or len(completed.stdout) > 65536: raise ValueError('binary does not report bounded build information')
    value = json.loads(completed.stdout)
    version = value.get('version', '')
    if not re.fullmatch(r'[0-9A-Za-z][0-9A-Za-z._+-]{0,63}', version): raise ValueError('invalid binary version')
    if value.get('contract_schema') != '1': raise ValueError('unsupported executable contract schema')
    if value.get('target_os') != sys.platform.replace('darwin', 'macos'):
        raise ValueError('executable platform differs from installer host')
    architecture = {'arm64': 'aarch64', 'AMD64': 'x86_64'}.get(platform.machine(), platform.machine())
    if value.get('target_arch') != architecture: raise ValueError('executable architecture differs from installer host')
    if value.get('native_versions') != ['10.0.7']: raise ValueError('unqualified native backend requirement')
    if (value.get('target_os'), value.get('target_arch')) != ('macos', 'aarch64'): raise ValueError('supported release baseline is macOS arm64')
    return value


def sbom(binary, info):
    root = Path(__file__).resolve().parents[1]
    cargo = root / 'Cargo.lock'
    packages = []
    lock_matches = cargo.is_file() and digest(cargo) == info.get('cargo_lock_sha256')
    if not lock_matches and (root / 'sbom.json').is_file():
        packaged = read_json(root / 'sbom.json')
        if (packaged.get('schema_version') != 1
                or packaged.get('format') != 'auto-kicad-dependency-inventory'
                or packaged.get('component') != 'auto-kicad'
                or packaged.get('version') != info.get('version')
                or packaged.get('binary_sha256') != digest(binary)
                or packaged.get('cargo_lock_sha256') != info.get('cargo_lock_sha256')
                or not packaged.get('dependency_lock_verified')
                or not packaged.get('dependency_inventory_complete')
                or not isinstance(packaged.get('packages'), list)
                or not packaged['packages']
                or not all(isinstance(p, dict) and isinstance(p.get('name'), str) and p['name']
                    and isinstance(p.get('version'), str) and p['version']
                    and (p.get('license') or p.get('license_file')) for p in packaged['packages'])):
            raise ValueError('packaged dependency inventory differs from executable')
        return packaged
    if lock_matches:
        try:
            command = subprocess.run(['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=root,
                capture_output=True, timeout=60, check=True)
            if len(command.stdout) > 16 * 1024 * 1024: raise ValueError('dependency inventory exceeds bound')
            metadata = json.loads(command.stdout)
            packages = [dict(name=item['name'], version=item['version'], license=item.get('license'),
                license_file=bool(item.get('license_file')), source=item.get('source')) for item in metadata['packages']]
        except (OSError, subprocess.SubprocessError) as error:
            raise ValueError('cannot capture locked dependency/license inventory') from error
    return dict(schema_version=1, format='auto-kicad-dependency-inventory', component='auto-kicad',
        version=info.get('version'), binary_sha256=digest(binary), packages=packages,
        cargo_lock_sha256=info.get('cargo_lock_sha256'), dependency_lock_verified=lock_matches,
        dependency_inventory_complete=bool(packages), license_decision=('GPL-3.0-or-later; see LICENSE and THIRD_PARTY_NOTICES.md'
            if info.get('license') == 'GPL-3.0-or-later' else 'legacy local-only inventory; executable license metadata unavailable'),
        embedded_protocol_notice='KiCad protocol source headers include GPL-3.0-or-later')


def stage_release(root, sources, info, dependency_inventory):
    rows = {}
    for name, path in sources.items():
        if not regular(path) or not os.access(path, os.X_OK): raise ValueError(f'not an executable regular source: {name}')
        rows[name] = dict(sha256=digest(path), bytes=path.stat().st_size, mode=0o755)
    seed = json.dumps(dict(build=info, binaries=rows), sort_keys=True, separators=(',', ':')).encode()
    identifier = info['version'] + '-' + hashlib.sha256(seed).hexdigest()[:16]
    destination = release_dir(root, identifier)
    if destination.exists():
        found = verify_release(root, identifier)
        if found['binaries'] != rows or found['build_info'] != info: raise ValueError('immutable release identity differs')
        return identifier
    temporary = Path(tempfile.mkdtemp(prefix='.stage-', dir=root / 'versions'))
    try:
        for name, source in sources.items():
            target = temporary / name
            with source.open('rb') as original, target.open('xb') as output:
                shutil.copyfileobj(original, output); output.flush(); os.fsync(output.fileno()); os.fchmod(output.fileno(), 0o755)
            if digest(target) != rows[name]['sha256']: raise ValueError('source executable changed while copying')
        atomic_json(temporary / 'sbom.json', dependency_inventory)
        manifest = dict(schema_version=SCHEMA, release_id=identifier, binaries=rows, build_info=info,
            sbom_sha256=digest(temporary / 'sbom.json'), distribution='local-only')
        atomic_json(temporary / 'release.json', manifest)
        sync_dir(temporary)
        os.rename(temporary, destination); sync_dir(destination.parent)
        verify_release(root, identifier)
        return identifier
    finally:
        if temporary.exists(): shutil.rmtree(temporary)


def adopt_legacy(prefix, root):
    if (root / 'install.json').exists():
        state = installation(root)
        if state.get('legacy_migration'):
            legacy = verify_release(root, state['active'])
            for name, record in legacy['binaries'].items():
                entry = prefix / 'bin' / name
                if not regular(entry) or entry.read_bytes() != launcher(prefix, name): raise ValueError('legacy launcher changed during migration')
                binary = root / name
                if binary.is_symlink():
                    if os.readlink(binary) != 'current/' + name: raise ValueError('legacy pointer changed')
                elif not regular(binary) or digest(binary) != record['sha256']: raise ValueError('legacy source changed')
            atomic_link(root / 'current', 'versions/' + state['active'])
            for name in legacy['binaries']: atomic_link(root / name, 'current/' + name)
            state.pop('legacy_migration', None); atomic_json(root / 'install.json', state)
        return state
    binaries = {}
    for name in NAMES:
        path, entry = root / name, prefix / 'bin' / name
        if path.exists() or path.is_symlink():
            if not regular(path) or not regular(entry) or entry.read_bytes() != launcher(prefix, name):
                raise ValueError(f'legacy layout is not the known installer layout: {name}')
            binaries[name] = path
        elif entry.exists() or entry.is_symlink(): raise ValueError(f'unowned launcher exists: {name}')
    if not binaries: return dict(schema_version=SCHEMA, active=None, previous=None, releases=[])
    if 'auto-kicad' not in binaries: raise ValueError('incomplete legacy runtime')
    info = dict(version='legacy', contract_schema='1', legacy_unversioned=True)
    dependency_inventory = dict(schema_version=1, packages=[], dependency_inventory_complete=False,
        license_decision='legacy local-only inventory; original dependency metadata unavailable')
    identifier = stage_release(root, binaries, info, dependency_inventory)
    manifest = dict(schema_version=SCHEMA, active=identifier, previous=None, releases=[identifier], legacy_migration=True)
    # Record ownership before the activation point; existing regular executables remain usable.
    atomic_json(root / 'install.json', manifest)
    atomic_link(root / 'current', 'versions/' + identifier)
    for name in binaries: atomic_link(root / name, 'current/' + name)
    manifest.pop('legacy_migration'); atomic_json(root / 'install.json', manifest)
    return manifest


def owned_entry(prefix, root, name, present):
    runtime, entry = root / name, prefix / 'bin' / name
    if runtime.is_symlink():
        if os.readlink(runtime) != 'current/' + name: raise ValueError(f'unowned runtime pointer: {name}')
    elif runtime.exists(): raise ValueError(f'unowned runtime file: {name}')
    if entry.exists() or entry.is_symlink():
        if not regular(entry) or entry.read_bytes() != launcher(prefix, name): raise ValueError(f'launcher changed: {name}')
    if present:
        atomic_link(runtime, 'current/' + name)
        atomic_file(entry, launcher(prefix, name), 0o755)


def activate(root, state, identifier):
    verify_release(root, identifier)
    previous = state['active'] if state['active'] != identifier else state.get('previous')
    releases = list(dict.fromkeys([identifier, previous, *state['releases']]))
    releases = [item for item in releases if item]
    prepared = dict(schema_version=SCHEMA, active=state['active'], previous=state.get('previous'), releases=releases,
        activation=dict(before=state['active'], after=identifier))
    atomic_json(root / 'install.json', prepared)
    atomic_link(root / 'current', 'versions/' + identifier)
    committed = dict(schema_version=SCHEMA, active=identifier, previous=previous, releases=releases)
    atomic_json(root / 'install.json', committed)
    return committed


def prune(root, state):
    retained = {state['active'], state.get('previous')}
    removed = []
    for identifier in list(state['releases']):
        if identifier in retained: continue
        verify_release(root, identifier)
        # Persist deletion intent before removing a release so restart can reconcile the absent path.
        state['pruning'] = identifier
        atomic_json(root / 'install.json', state)
        shutil.rmtree(release_dir(root, identifier)); sync_dir(root / 'versions')
        state['releases'].remove(identifier); state.pop('pruning')
        atomic_json(root / 'install.json', state); removed.append(identifier)
    return removed


def status(prefix, root, verify=False):
    if not root.exists() or not (root / 'install.json').exists():
        return dict(installed=False, legacy_layout=(root / 'auto-kicad').exists(), prefix=str(prefix))
    state = installation(root)
    for identifier in state['releases'] if verify else ([state['active']] if state['active'] else []): verify_release(root, identifier)
    if state.get('activation_pending'): return dict(installed=False, activation_pending=True, staged_releases=state['releases'], prefix=str(prefix))
    manifest = verify_release(root, state['active'])
    if state.get('legacy_migration'): return dict(installed=True, legacy_migration_pending=True, prefix=str(prefix), verified=True)
    for name in manifest['binaries']:
        runtime, entry = root / name, prefix / 'bin' / name
        if not runtime.is_symlink() or os.readlink(runtime) != 'current/' + name or not regular(entry) or entry.read_bytes() != launcher(prefix,name):
            raise ValueError(f'activated entry differs: {name}')
    return dict(installed=True, prefix=str(prefix), active=state['active'], previous=state.get('previous'), releases=state['releases'],
        verified=True, build_info=manifest['build_info'], distribution='local-only')


def uninstall(prefix, root):
    state = installation(root)
    for identifier in state['releases']: verify_release(root, identifier)
    names = set().union(*(verify_release(root, identifier)['binaries'] for identifier in state['releases']))
    for name in names: owned_entry(prefix, root, name, False)
    for name in names:
        (prefix / 'bin' / name).unlink(missing_ok=True); (root / name).unlink(missing_ok=True)
    (root / 'current').unlink(missing_ok=True); (root / 'install.json').unlink()
    for identifier in state['releases']: shutil.rmtree(release_dir(root, identifier))
    sync_dir(root); sync_dir(prefix / 'bin')
    return dict(uninstalled=True, project_and_recovery_data_preserved=True, codex_configuration_preserved=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', nargs='?', type=Path)
    parser.add_argument('--replay-binary', type=Path)
    parser.add_argument('--no-rollback', action='store_true', help='Retain only the active owned release after installation')
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    group = parser.add_mutually_exclusive_group()
    group.add_argument('--status', action='store_true')
    group.add_argument('--verify', action='store_true')
    group.add_argument('--rollback', action='store_true')
    group.add_argument('--uninstall', action='store_true')
    args = parser.parse_args()
    action = args.status or args.verify or args.rollback or args.uninstall
    if action and args.no_rollback: parser.error('--no-rollback requires an installation source')
    if action and (args.binary or args.replay_binary): parser.error('installation sources cannot be combined with a lifecycle action')
    if not action and args.binary is None: parser.error('an executable or lifecycle action is required')
    prefix = args.prefix.expanduser().resolve()
    root = prefix / 'lib/auto-kicad'
    if args.status or args.verify:
        print(json.dumps(status(prefix, root, args.verify), indent=2)); return
    safe_directory(prefix); safe_directory(prefix / 'lib'); safe_directory(root)
    safe_directory(root / 'versions'); safe_directory(prefix / 'bin')
    with lock(root):
        if args.uninstall: result = uninstall(prefix, root)
        elif args.rollback:
            state = installation(root)
            if not state.get('previous'): raise ValueError('no previous release is available')
            status(prefix, root, True)
            previous = verify_release(root, state['previous'])
            active = verify_release(root, state['active'])
            result = dict(rolled_back=True, **activate(root, state, state['previous']))
            for name in previous['binaries']: owned_entry(prefix, root, name, True)
            for name in set(active['binaries']) - set(previous['binaries']):
                owned_entry(prefix, root, name, False)
                (prefix / 'bin' / name).unlink(missing_ok=True); (root / name).unlink(missing_ok=True)
            sync_dir(root); sync_dir(prefix / 'bin')
        else:
            binary = args.binary.resolve(strict=True); info = native_info(binary)
            state = adopt_legacy(prefix, root)
            sources = {'auto-kicad': binary}
            if args.replay_binary: sources['auto-kicad-replay'] = args.replay_binary.resolve(strict=True)
            elif state['active']:
                active = verify_release(root, state['active'])
                if 'auto-kicad-replay' in active['binaries']: sources['auto-kicad-replay'] = release_dir(root,state['active']) / 'auto-kicad-replay'
            dependency_inventory = sbom(binary,info)
            identifier = stage_release(root,sources,info,dependency_inventory)
            for name in sources: owned_entry(prefix,root,name,True)
            result = dict(installed=True, **activate(root,state,identifier))
            if args.no_rollback: result['previous'] = None
            result['pruned_releases'] = prune(root,result)
            # Keep activation metadata independent of transient presentation fields.
            clean_state = {key:result[key] for key in ['schema_version','active','previous','releases']}
            atomic_json(root/'install.json', clean_state)
        print(json.dumps(result,indent=2))


if __name__ == '__main__':
    try: main()
    except (ValueError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        print('auto-kicad installation rejected: ' + str(error), file=sys.stderr)
        raise SystemExit(1)
