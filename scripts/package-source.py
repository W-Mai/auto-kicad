#!/usr/bin/env python3
"""Build matching source and dependency-license assets for cargo-dist releases."""

import json
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]


def main():
    packaging = runpy.run_path(str(ROOT / 'scripts/package-ci.py'))
    run = packaging['run']
    write_json = packaging['write_json']
    metadata = json.loads(run(['cargo', 'metadata', '--locked', '--format-version', '1']))
    app = next(item for item in metadata['packages'] if item['name'] == 'auto-kicad')
    revision = run(['git', 'rev-parse', 'HEAD']).strip()
    timestamp = int(run(['git', 'log', '-1', '--format=%ct']).strip())
    output = ROOT / 'target/dist'
    output.mkdir(parents=True, exist_ok=True)
    source_name = f'auto-kicad-{app["version"]}-source'
    with tempfile.TemporaryDirectory(prefix='auto-kicad-dist-source-') as temporary:
        stage = Path(temporary)
        source = stage / source_name
        notices = stage / 'auto-kicad-dependency-notices'
        source.mkdir()
        notices.mkdir()
        packages = []
        for item in sorted(metadata['packages'], key=lambda value: (value['name'], value['version'])):
            if not (item.get('license') or item.get('license_file')):
                raise ValueError('missing dependency license: ' + item['name'])
            copied = []
            if item.get('source') is not None:
                copied = packaging['copy_notices'](item, notices / (item['name'] + '-' + item['version']))
            packages.append(dict(name=item['name'], version=item['version'], source=item.get('source'),
                                 license=item.get('license'), license_file=bool(item.get('license_file')),
                                 notices=copied))
        inventory = dict(schema_version=1, format='auto-kicad-release-dependency-inventory',
                         component='auto-kicad', version=app['version'], source_revision=revision,
                         cargo_lock_sha256=packaging['sha'](ROOT / 'Cargo.lock'),
                         license='GPL-3.0-or-later', packages=packages,
                         corresponding_source='auto-kicad-source.tar.gz')
        write_json(output / 'dependency-inventory.json', inventory)
        write_json(notices / 'dependency-inventory.json', inventory)
        for name in ['LICENSE', 'THIRD_PARTY_NOTICES.md']:
            shutil.copyfile(ROOT / name, notices / name)
        tracked = subprocess.check_output(['git', 'ls-files', '-z'], cwd=ROOT).decode().split('\0')
        for name in filter(None, tracked):
            original = ROOT / name
            if not original.is_file() or original.is_symlink():
                raise ValueError('source entry is not a regular file: ' + name)
            destination = source / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(original, destination)
        vendor = source / 'vendor'
        configuration = run(['cargo', 'vendor', '--locked', '--offline', '--respect-source-config',
                             '--versioned-dirs', str(vendor)], stderr=subprocess.DEVNULL)
        configuration = configuration.replace(str(vendor), 'vendor')
        if 'directory = "vendor"' not in configuration:
            raise ValueError('vendor configuration is not source-relative')
        config = source / '.cargo/config.toml'
        config.parent.mkdir(exist_ok=True)
        if config.exists():
            raise ValueError('source already has Cargo source configuration')
        config.write_text(configuration)
        write_json(source / 'source-info.json', dict(version=app['version'], revision=revision,
                   cargo_lock_sha256=inventory['cargo_lock_sha256']))
        write_json(source / 'dependency-inventory.json', inventory)
        packaging['archive'](source, output / 'auto-kicad-source.tar.gz', source_name, timestamp)
        packaging['archive'](notices, output / 'auto-kicad-dependency-notices.tar.gz', notices.name, timestamp)
    print(json.dumps(dict(source_revision=revision, packages=len(packages),
                          artifacts=sorted(path.name for path in output.iterdir()))))


if __name__ == '__main__':
    main()
