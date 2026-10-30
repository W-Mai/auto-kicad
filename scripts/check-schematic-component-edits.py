#!/usr/bin/env python3
"""Qualify explicit complete-component edits through production stdio and native KiCad."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import xml.etree.ElementTree as ET

from schematic_general_cases import reviewed
from stdio_rpc import Rpc

spec = importlib.util.spec_from_file_location('inheritance', Path(__file__).with_name('check-schematic-inheritance.py'))
native = importlib.util.module_from_spec(spec); spec.loader.exec_module(native)


def snapshot(root):
    assert not any(p.is_symlink() for p in root.rglob('*'))
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest() for p in root.rglob('*') if p.is_file() and p.name != '.auto-kicad-writer.lock'}


def cleanup(temporary):
    for directory in [temporary / 'org.kicad.kicad/instances', temporary / 'org.kicad.kicad']:
        if directory.is_dir() and not list(directory.iterdir()):
            directory.rmdir()
    assert not list(temporary.iterdir()), list(temporary.iterdir())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'):
        parser.add_argument(name, type=Path)
    parser.add_argument('--file')
    args = parser.parse_args()
    binary, source, config = [p.resolve(strict=True) for p in (args.binary, args.source, args.config)]
    server_digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    project, file = source.parent, args.file or source.name
    for directory in (project, config):
        assert (directory / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    args.output.mkdir()
    output = args.output.resolve(strict=True)
    temporary = output / 'runtime-tmp'; temporary.mkdir()
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / 'state'), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    configuration = snapshot(config)
    transcript, cases = [], []
    rpc = Rpc([str(binary)], env=env, log=output / 'server.log')

    def call(name, parameters, ok=True):
        response = rpc.call('tools/call', {'name': name, 'arguments': parameters}, timeout=600)
        assert not response.get('isError'), response
        value = response['structuredContent']
        transcript.append({'tool': name, 'request': parameters, 'response': value})
        (output / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
        assert value['ok'] == ok, value
        return value['data']

    def observe():
        return call('kicad_schematic_audit', {'observe': {'project_root': str(project), 'root_schematic': str(source)}})

    def release(observed=None, planned=None):
        return call('kicad_schematic_audit', {'release': {'observation_ids': [observed['observation_id']] if observed else [], 'plan_ids': [planned['plan_id']] if planned else []}})

    def prepare(observed, edits, ok=True):
        before = snapshot(project)
        result = call('kicad_plan_schematic_change', {'project_root': str(project), 'root_schematic': str(source), 'source_run_id': 'component-edit-acceptance',
                      'request_id': 'case-' + str(len(transcript)), 'general': {'schema_version': 1, 'observation_id': observed['observation_id'],
                      'observed_revision': observed['revision'], 'edits': edits}}, ok=ok)
        assert snapshot(project) == before and snapshot(config) == configuration
        return result

    def edits_for(ids, action, path=None):
        return [{'file': file, 'scope': {'kind': 'single_instance', 'path': path} if path else {'kind': 'file_all_instances'},
                 'action': dict(action, uuid=uuid)} for uuid in sorted(ids)]

    def reject(name, observed, edits):
        prepare(observed, edits, ok=False)
        cases.append({'name': name, 'source_unchanged': True})

    def connectivity_expected(planned, renames):
        if not renames:
            expected = reviewed(planned)
            assert not any(expected['erc'].values()), expected['erc']
            return expected
        removed, added = [], []
        for net in planned['native_preview']['connectivity_before']['nets']:
            changed = net
            for ids, path, old, new in renames:
                relevant = [pin for pin in net['pins'] if {u['symbol_uuid'] for u in pin.get('units', [])} == ids
                            and all(u['sheet_path'] == path for u in pin['units'])]
                if relevant:
                    assert len(relevant) == len(net['pins']) == 1 and old in net['name']
                    changed = dict(net, name=net['name'].replace(old, new))
                    break
            if changed != net:
                removed.append(net); added.append(changed)
        assert len(removed) == sum(5 if len(ids) == 2 else 7 for ids, _, _, _ in renames)
        expected = reviewed(planned, {'removed_nets': removed, 'added_nets': added})
        for row in expected['erc']['introduced'] + expected['erc']['resolved']:
            diagnostic = json.loads(row['diagnostic_json'])
            assert diagnostic['type'] == 'pin_not_connected', diagnostic
        return expected

    def source_preserved(before_text, edits):
        after_text = (project / file).read_text()
        old_nodes, new_nodes = list(native.children(before_text)), list(native.children(after_text))
        touched = {e['action']['uuid'] for e in edits}
        for old in old_nodes:
            match = re.search(r'\(uuid "([0-9a-f-]{36})"\)', old)
            if not re.match(r'\(symbol\s', old) or not match or match[1] not in touched:
                assert new_nodes.count(old) == old_nodes.count(old), old[:100]
                continue
            changed = next(n for n in new_nodes if re.match(r'\(symbol\s', n) and f'(uuid "{match[1]}")' in n)
            actions = [e['action'] for e in edits if e['action']['uuid'] == match[1]]
            kinds = {a['kind'] for a in actions}
            fields = set()
            if 'set_reference' in kinds: fields.add('Reference')
            if 'assign_footprint' in kinds: fields.add('Footprint')
            if 'set_property' in kinds: fields.add('Value')
            tags = {'instances'} if 'set_reference' in kinds else set()
            if 'set_flags' in kinds: tags.update(('dnp', 'on_board', 'in_bom'))
            def protected(node):
                tag = re.match(r'\((\w+)', node)[1]
                return tag not in tags and not any(node.startswith('(property ' + json.dumps(name)) for name in fields)
            assert Counter(filter(protected, native.children(old))) == Counter(filter(protected, native.children(changed)))
            if 'set_reference' in kinds:
                for instances in [n for n in native.children(old) if n.startswith('(instances')]:
                    for entry in native.children(instances):
                        if entry.startswith('(project "foreign-annotations"'):
                            assert changed.count(entry) == 1
                        else:
                            obsolete = [p for p in native.children(entry) if 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb' in p]
                            assert len(obsolete) == 1 and changed.count(obsolete[0]) == 1

    def publish(name, edits, renames=None):
        observed = observe(); before = snapshot(project); before_text = (project / file).read_text()
        planned = prepare(observed, edits)
        assert {c['uuid'] for c in planned['changes']} == {e['action']['uuid'] for e in edits}
        expected = connectivity_expected(planned, renames or [])
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        if renames: call('kicad_preview_schematic_change', identity, ok=False)
        preview = call('kicad_preview_schematic_change', dict(identity, expectations=expected))
        call('kicad_commit_schematic_change', dict(identity, approval_hash='sha256:' + '0' * 64), ok=False)
        assert snapshot(project) == before
        committed = call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']))
        assert committed['state'] == 'committed'
        call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        after = snapshot(project)
        assert after.keys() == before.keys() and {p for p in before if before[p] != after[p]} == {file}
        source_preserved(before_text, edits)
        assert snapshot(config) == configuration
        release(observed, planned)
        cases.append({'name': name, 'published': True, 'source_units': len(edits)})

    try:
        rpc.call('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'component-edit-acceptance', 'version': '1'}})
        rpc.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        observed = observe()
        pair = {u['symbol_uuid'] for u in observed['units'] if u['reference'] == 'U9910' and u['file'] == file}
        triple = {u['symbol_uuid'] for u in observed['units'] if u['reference'] == 'U9930' and u['file'] == file}
        assert len(pair) == 2 and len(triple) == 3
        paths = sorted({u['sheet_path'] for u in observed['units'] if u['symbol_uuid'] in pair})
        pair_path, triple_path = paths[0], paths[-1]
        pair_old = next(u['reference'] for u in observed['units'] if u['symbol_uuid'] in pair and u['sheet_path'] == pair_path)
        triple_old = next(u['reference'] for u in observed['units'] if u['symbol_uuid'] in triple and u['sheet_path'] == triple_path)
        scope_pair = pair_path if args.file else None
        scope_triple = triple_path if args.file else None
        for name, action in [('partial-value', {'kind': 'set_property', 'name': 'Value', 'value': 'partial'}),
                             ('partial-flags', {'kind': 'set_flags', 'flags': {'dnp': True}}),
                             ('partial-footprint', {'kind': 'assign_footprint', 'footprint': 'Connector_PinHeader_2.54mm:PinHeader_1x05_P2.54mm_Vertical'}),
                             ('partial-reference', {'kind': 'set_reference', 'reference': 'U9990'})]:
            reject(name, observed, edits_for(pair, action, scope_pair if name == 'partial-reference' else None)[:1])
        conflict = edits_for(pair, {'kind': 'set_reference', 'reference': 'U9990'}, scope_pair)
        conflict[-1]['action']['reference'] = 'U9991'
        reject('conflicting-member-reference', observed, conflict)
        reject('global-component-collision', observed, edits_for(pair, {'kind': 'set_reference', 'reference': next(u['reference'] for u in observed['units'] if u['symbol_uuid'] not in pair and not u['reference'].startswith('#'))}, scope_pair))
        wrong_map = {str(n): str(n) for n in range(1, 6)}; wrong_map['3'] = '4'
        reject('common-pin-mapping-not-bijective', observed, edits_for(pair, {'kind': 'assign_footprint', 'footprint': 'Connector_PinHeader_2.54mm:PinHeader_1x05_P2.54mm_Vertical', 'pin_pad_map': wrong_map}))
        nonidentity = {str(n): str(n) for n in range(1, 6)}; nonidentity['1'], nonidentity['2'] = '2', '1'
        reject('unrepresentable-native-pin-remapping', observed, edits_for(pair, {'kind': 'assign_footprint', 'footprint': 'Connector_PinHeader_2.54mm:PinHeader_1x05_P2.54mm_Vertical', 'pin_pad_map': nonidentity}))
        release(observed)
        publish('complete-pair-reference', edits_for(pair, {'kind': 'set_reference', 'reference': 'U9990'}, scope_pair), [(pair, pair_path, pair_old, 'U9990')])
        publish('complete-triple-reference', edits_for(triple, {'kind': 'set_reference', 'reference': 'U9993'}, scope_triple), [(triple, triple_path, triple_old, 'U9993')])
        for ids, size, name in [(pair, 5, 'pair'), (triple, 7, 'triple')]:
            publish('complete-' + name + '-footprint', edits_for(ids, {'kind': 'assign_footprint', 'footprint': f'Connector_PinHeader_2.54mm:PinHeader_1x0{size}_P2.54mm_Vertical'}))
            publish('complete-' + name + '-value', edits_for(ids, {'kind': 'set_property', 'name': 'Value', 'value': name + '-qualified'}))
        publish('complete-pair-flags', edits_for(pair, {'kind': 'set_flags', 'flags': {'in_bom': False, 'on_board': False, 'dnp': True}}))
        publish('complete-pair-flags-restored', edits_for(pair, {'kind': 'set_flags', 'flags': {'in_bom': True, 'on_board': True, 'dnp': False}}))
        publish('explicit-component-reference-swap', edits_for(pair, {'kind': 'set_reference', 'reference': 'U9993'}, scope_pair) + edits_for(triple, {'kind': 'set_reference', 'reference': 'U9990'}, scope_triple),
                [(pair, pair_path, 'U9990', 'U9993'), (triple, triple_path, 'U9993', 'U9990')])
        observed = observe(); before = snapshot(project)
        planned = prepare(observed, edits_for(pair, {'kind': 'set_property', 'name': 'Value', 'value': 'drift-control'}))
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        preview = call('kicad_preview_schematic_change', dict(identity, expectations=connectivity_expected(planned, [])))
        saved = (project / file).read_bytes()
        try:
            (project / file).write_bytes(saved + b'\n')
            call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        finally:
            (project / file).write_bytes(saved)
        call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        assert snapshot(project) == before and snapshot(config) == configuration
        release(observed, planned)
        cases.append({'name': 'source-drift-permanently-revokes-approval', 'source_restored': True})
        final = observe()
        for ids, count, value in [(pair, 2, 'pair-qualified'), (triple, 3, 'triple-qualified')]:
            selected = [u for u in final['units'] if u['symbol_uuid'] in ids]
            assert len(selected) == count * len(paths)
            for path in paths:
                assert len({u['reference'] for u in selected if u['sheet_path'] == path}) == 1
        release(final)
    finally:
        rpc.close()
        (output / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
    assert rpc.process.returncode == 0 and snapshot(config) == configuration
    cleanup(temporary)
    oracle_config, oracle_tmp = output / 'oracle-config', output / 'oracle-tmp'
    shutil.copytree(config, oracle_config); oracle_tmp.mkdir()
    oracle_env = dict(env, KICAD_CONFIG_HOME=str(oracle_config), TMPDIR=str(oracle_tmp))
    cli = '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'
    for name, command in [('netlist', ['sch', 'export', 'netlist', '--format', 'kicadxml', '-o', str(output / 'saved.xml'), str(source)]),
                          ('erc', ['sch', 'erc', '--format', 'json', '--severity-all', '-o', str(output / 'saved-erc.json'), str(source)])]:
        completed = subprocess.run([cli, *command], env=oracle_env, capture_output=True, timeout=120)
        (output / (name + '.stdout')).write_bytes(completed.stdout); (output / (name + '.stderr')).write_bytes(completed.stderr)
        assert completed.returncode == 0, completed.stderr
    xml = ET.parse(output / 'saved.xml').getroot()
    for ids, count, value in [(pair, 2, 'pair-qualified'), (triple, 3, 'triple-qualified')]:
        components = [c for c in xml.findall('./components/comp') if set(c.findtext('tstamps').split()) == ids]
        assert len(components) == len(paths)
        for component in components:
            assert len(component.findall('./units/unit')) == count and component.findtext('value') == value
            assert component.findtext('footprint') == f'Connector_PinHeader_2.54mm:PinHeader_1x0{2 * count + 1}_P2.54mm_Vertical'
            pins = [n.attrib['pin'] for net in xml.findall('./nets/net') for n in net.findall('node') if n.attrib['ref'] == component.attrib['ref']]
            assert len(pins) == len(set(pins)) == 2 * count + 1
    cleanup(oracle_tmp)
    assert snapshot(config) == configuration
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == server_digest
    result = {'server_sha256': server_digest, 'passed': True, 'production_stdio': True, 'native_version': '10.0.7', 'instances': len(paths), 'cases': cases,
              'private_snapshots_removed': True, 'configuration_unchanged': True, 'gui_launched': False}
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
