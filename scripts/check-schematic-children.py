"""Exercise child-file authoring through production MCP and independent saved native reads."""
import argparse
from collections import Counter
import hashlib
import gzip
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import xml.etree.ElementTree as ET

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
from stdio_rpc import Rpc

spec = importlib.util.spec_from_file_location('removal', SCRIPTS / 'check-schematic-sheet-removal.py')
removal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(removal)
snapshot = removal.snapshot
helpers = removal.helpers
CLI = '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'


def exact_erc(text):
    value = json.loads(text, parse_int=lambda v: ('number', v), parse_float=lambda v: ('number', v))
    del value['date'], value['source']
    for sheet in value['sheets']:
        for violation in sheet['violations']:
            violation['items'].sort(key=lambda x: json.dumps(x, sort_keys=True))
        sheet['violations'].sort(key=lambda x: json.dumps(x, sort_keys=True))
    value['sheets'].sort(key=lambda x: x['uuid_path'])
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'): parser.add_argument(name, type=Path)
    parser.add_argument('--file')
    parser.add_argument('--shared', action='store_true')
    parser.add_argument('--probe', action='store_true')
    args = parser.parse_args()
    binary, source, config = [p.resolve(strict=True) for p in (args.binary, args.source, args.config)]
    project = source.parent
    for p in (project, config):
        assert (p / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    host = args.file or source.name
    output = args.output.resolve()
    output.mkdir()
    temporary = output / 'runtime-tmp'; temporary.mkdir()
    oracle_tmp = output / 'oracle-tmp'; oracle_tmp.mkdir()
    oracle_config = output / 'oracle-config'; shutil.copytree(config, oracle_config)
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / 'state'), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    oracle_env = dict(env, TMPDIR=str(oracle_tmp), KICAD_CONFIG_HOME=str(oracle_config))
    configuration = snapshot(config)
    original = snapshot(project)
    original_host = (project / host).read_text()
    transcript, cases = [], []
    rpc = Rpc([str(binary)], env=env, log=output / 'server.log')

    def call(name, request, ok=True):
        response = rpc.call('tools/call', {'name': name, 'arguments': request}, timeout=600)
        assert not response.get('isError'), response
        value = response['structuredContent']
        transcript.append({'tool': name, 'request': request, 'response': value})
        with gzip.open(output / 'transcript.json.gz', 'wt') as stream:
            json.dump(transcript, stream, indent=2)
        assert value['ok'] == ok, value
        return value['data']

    def observe():
        return call('kicad_schematic_audit', {'observe': {'project_root': str(project), 'root_schematic': str(source)}})

    def release(observation=None, planned=None):
        call('kicad_schematic_audit', {'release': {'observation_ids': [observation['observation_id']] if observation else [], 'plan_ids': [planned['plan_id']] if planned else []}})

    def inspect(observation, file):
        before = snapshot(project)
        result = call('kicad_schematic_audit', {'inspect_child': {'observation_id': observation['observation_id'], 'observed_revision': observation['revision'], 'file': file}})
        assert not result['publication_authorized'] and snapshot(project) == before
        return result

    def edit(action, file=host):
        return {'file': file, 'scope': {'kind': 'file_all_instances'}, 'action': action}

    def plan(observation, edits, ok=True):
        before = snapshot(project)
        value = call('kicad_plan_schematic_change', {'project_root': str(project), 'root_schematic': str(source), 'source_run_id': 'child-authoring-acceptance', 'request_id': 'case-' + str(len(transcript)),
            'general': {'schema_version': 1, 'observation_id': observation['observation_id'], 'observed_revision': observation['revision'], 'edits': edits}}, ok)
        assert snapshot(project) == before and snapshot(config) == configuration
        return value

    def independent(name, planned, observed):
        directory = output / name; directory.mkdir()
        before = snapshot(project)
        for kind, command in [('netlist', ['sch', 'export', 'netlist', '--format', 'kicadxml', '-o', str(directory / 'saved.xml')]), ('erc', ['sch', 'erc', '--format', 'json', '--severity-all', '-o', str(directory / 'erc.json')])]:
            result = subprocess.run([CLI, *command, str(source)], env=oracle_env, capture_output=True, timeout=120)
            (directory / (kind + '.stdout')).write_bytes(result.stdout)
            (directory / (kind + '.stderr')).write_bytes(result.stderr)
            assert result.returncode == 0, (kind, result.stderr)
        assert snapshot(project) == before
        xml = ET.parse(directory / 'saved.xml').getroot()
        actual = sorted((n.attrib['name'], n.attrib['class'], sorted((p.attrib['ref'], p.attrib['pin']) for p in n.findall('node'))) for n in xml.findall('./nets/net') if n.findall('node'))
        refs = {(u['file'], u['sheet_path'], u['symbol_uuid']): u['reference'] for u in observed['units']}
        expected = []
        for net in planned['native_preview']['connectivity_after']['nets']:
            nodes = []
            for pin in net['pins']:
                if pin['kind'] != 'component': continue
                values = {refs[(u['file'], u['sheet_path'], u['symbol_uuid'])] for u in pin['units']}
                assert len(values) == 1
                nodes.append((values.pop(), pin['number']))
            if nodes: expected.append((net['name'], net['class'], sorted(nodes)))
        assert sorted(expected) == actual, (name, 'saved native complete nets differ')
        assert exact_erc((directory / 'erc.json').read_text()) == exact_erc(planned['native_preview']['erc_after']['raw_json'])
        assert len(xml.findall('./design/sheet')) == len(observed['sheets'])

    def approve(planned):
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        call('kicad_preview_schematic_change', identity, False)
        return identity, call('kicad_preview_schematic_change', dict(identity, expectations=removal.delta(planned)))

    def publish(name, edits):
        print('START', name, flush=True)
        observation = observe(); before = snapshot(project)
        planned = plan(observation, edits)
        identity, approved = approve(planned)
        call('kicad_commit_schematic_change', dict(identity, approval_hash='sha256:' + '0' * 64), False)
        assert snapshot(project) == before
        committed = call('kicad_commit_schematic_change', dict(identity, approval_hash=approved['approval_hash']))
        assert committed['state'] == 'committed'
        call('kicad_commit_schematic_change', dict(identity, approval_hash=approved['approval_hash']), False)
        after = snapshot(project)
        for file, digest in before.items():
            if not file.endswith('.kicad_sch'): assert after[file] == digest
        fresh = observe()
        independent(name, planned, fresh)
        cases.append({'name': name, 'committed': True, 'new_files': sorted(after.keys() - before.keys()), 'changed_files': sorted(p for p in before if before[p] != after[p]), 'saved_native_netlist_erc': True})
        release(observation, planned); release(fresh)
        print('PASS', name, flush=True)
        return planned

    def parents(observation):
        return sorted(s['sheet_path'] for s in observation['sheets'] if s['file'] == host)

    def mappings(observation, inspection=None):
        rows = inspection['sheets'] if inspection else [{'relative_path': '/', 'symbols': []}]
        pages, references = [], []
        for pi, parent in enumerate(parents(observation)):
            grouped = {}
            for si, sheet in enumerate(rows):
                pages.append({'parent_instance': parent, 'relative_path': sheet['relative_path'], 'page': str(99 + pi * 16 + si)})
                for symbol in sheet['symbols']:
                    key = (sheet['relative_path'], symbol['reference'])
                    if key not in grouped:
                        grouped[key] = ('#PWR' if symbol['reference'].startswith('#') else 'X') + str(10000 + pi * 1000 + len(grouped))
                    references.append({'parent_instance': parent, 'relative_path': sheet['relative_path'], 'symbol_uuid': symbol['uuid'], 'reference': grouped[key]})
        return {'pages': pages, 'references': references}

    def creation(observation, destination='created.kicad_sch'):
        return {'kind': 'create_sheet', 'key': 'child-placement', 'name': 'AK_CHILD', 'rectangle': {'position': {'x_nm': 600000000, 'y_nm': 400000000}, 'width_nm': 20000000, 'height_nm': 20000000},
            'child': {'kind': 'create_empty', 'file': destination}, **mappings(observation)}

    try:
        rpc.call('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'child-authoring-acceptance', 'version': '1'}})
        rpc.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        observation = observe()
        (output / 'initial-observation.json').write_text(json.dumps(observation, indent=2) + '\n')
        orphan = inspect(observation, 'orphan.kicad_sch')
        (output / 'orphan-inspection.json').write_text(json.dumps(orphan, indent=2) + '\n')
        if args.probe:
            release(observation)
            print('PROBE PASS', len(observation['sheets']), len(orphan['sheets']), flush=True)
            return
        action = creation(observation)
        for name, wrong in [('missing-pages', dict(action, pages=[])), ('extra-page', dict(action, pages=action['pages'] + [dict(action['pages'][0], relative_path='/11111111-1111-4111-8111-111111111111')])), ('collision-page', dict(action, pages=[dict(p, page='1') for p in action['pages']])), ('external', dict(action, child={'kind': 'create_empty', 'file': '../outside.kicad_sch'})), ('missing-parent', dict(action, child={'kind': 'create_empty', 'file': 'missing/child.kicad_sch'}))]:
            plan(observation, [edit(wrong)], False)
            cases.append({'name': name, 'rejected': True})
        original_orphan = (project / 'orphan.kicad_sch').read_bytes()
        attach = dict(action, child={'kind': 'attach_existing', 'file': 'orphan.kicad_sch', 'source_revision': orphan['source_revision']}, **mappings(observation, orphan))
        plan(observation, [edit(dict(attach, references=[]))], False)
        try:
            (project / 'orphan.kicad_sch').write_bytes(original_orphan + b'\n')
            plan(observation, [edit(attach)], False)
        finally: (project / 'orphan.kicad_sch').write_bytes(original_orphan)
        release(observation)
        created = publish('create-empty-child', [edit(action)])
        sheet_id = next(c['uuid'] for c in created['changes'] if c['kind'] == 'Sheet')
        observation = observe()
        child_paths = sorted(s['sheet_path'] for s in observation['sheets'] if s['file'] == 'created.kicad_sch')
        assert len(child_paths) == (4 if args.shared else 1)
        symbols = {'kind': 'create_symbol', 'key': 'created-child-resistor', 'lib_id': 'Device:R', 'reference': 'R9500', 'value': '1k', 'unit': 1, 'transform': {'position': {'x_nm': 100000000, 'y_nm': 100000000}}, 'instance_references': {path: 'R' + str(9500 + i) for i, path in enumerate(child_paths)} if len(child_paths) > 1 else {}}
        release(observation)
        publish('create-symbol-in-new-child', [edit(symbols, 'created.kicad_sch')])
        retained_created = (project / 'created.kicad_sch').read_bytes()
        observation = observe(); orphan = inspect(observation, 'orphan.kicad_sch')
        retarget = {'kind': 'retarget_sheet', 'uuid': sheet_id, 'child': {'kind': 'attach_existing', 'file': 'orphan.kicad_sch', 'source_revision': orphan['source_revision']}, **mappings(observation, orphan)}
        release(observation)
        publish('retarget-populated-multiunit-child', [edit(retarget)])
        assert (project / 'created.kicad_sch').read_bytes() == retained_created
        retained_orphan = (project / 'orphan.kicad_sch').read_bytes()
        observation = observe()
        retarget_empty = {'kind': 'retarget_sheet', 'uuid': sheet_id, 'child': {'kind': 'create_empty', 'file': 'second.kicad_sch'}, **mappings(observation)}
        release(observation)
        publish('retarget-to-second-empty-child', [edit(retarget_empty)])
        assert (project / 'orphan.kicad_sch').read_bytes() == retained_orphan
        observation = observe(); child = inspect(observation, 'created.kicad_sch')
        reattach = {'kind': 'retarget_sheet', 'uuid': sheet_id, 'child': {'kind': 'attach_existing', 'file': 'created.kicad_sch', 'source_revision': child['source_revision']}, **mappings(observation, child)}
        for row in reattach['references']: row['reference'] = 'R' + str(9500 + parents(observation).index(row['parent_instance']))
        release(observation)
        publish('reattach-with-existing-annotations', [edit(reattach)])
        assert (project / 'created.kicad_sch').read_bytes() == retained_created
        publish('remove-retain-all-child-files', [edit({'kind': 'remove_sheet', 'uuid': sheet_id})])
        assert (project / 'created.kicad_sch').read_bytes() == retained_created
        assert (project / 'orphan.kicad_sch').read_bytes() == retained_orphan
        assert Counter(helpers.native.children(original_host)) == Counter(helpers.native.children((project / host).read_text()))
        assert all(snapshot(project)[p] == digest for p, digest in original.items() if p not in (host, 'orphan.kicad_sch'))
    finally:
        rpc.close()
    assert rpc.process.returncode == 0 and snapshot(config) == configuration
    for root in (temporary, oracle_tmp):
        for child in (root / 'org.kicad.kicad/instances', root / 'org.kicad.kicad'):
            if child.is_dir() and not list(child.iterdir()): child.rmdir()
        assert not list(root.iterdir()), list(root.iterdir())
    result = {'passed': True, 'shared_parent': args.shared, 'server_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'cases': cases, 'configuration_unchanged': True, 'private_snapshots_removed': True, 'gui_launched': False}
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2), flush=True)


if __name__ == '__main__': main()
