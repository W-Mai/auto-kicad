#!/usr/bin/env python3
"""Qualify component-unit creation and insertion through production stdio and native KiCad."""
import argparse
import copy
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
inheritance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inheritance)


def snapshot(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file() and p.name != '.auto-kicad-writer.lock'}


def canonical(value):
    return json.dumps(value, sort_keys=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'):
        parser.add_argument(name, type=Path)
    parser.add_argument('--file')
    args = parser.parse_args()
    binary, source, config = [p.resolve(strict=True) for p in (args.binary, args.source, args.config)]
    project, file = source.parent, args.file or source.name
    for directory in (project, config):
        assert (directory / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    args.output.mkdir()
    output = args.output.resolve(strict=True)
    temporary = output / 'runtime-tmp'
    temporary.mkdir()
    configuration = snapshot(config)
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / 'state'), KICAD_CONFIG_HOME=str(config),
               TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    transcript, cases = [], []
    rpc = Rpc([str(binary)], env=env, log=output / 'server.log')

    def call(name, parameters, ok=True):
        response = rpc.call('tools/call', {'name': name, 'arguments': parameters}, timeout=600)
        assert not response.get('isError'), response
        envelope = response['structuredContent']
        transcript.append({'tool': name, 'request': parameters, 'response': envelope})
        (output / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
        assert envelope['ok'] == ok, envelope
        return envelope['data']

    def observe():
        return call('kicad_schematic_audit', {'observe': {'project_root': str(project), 'root_schematic': str(source)}})

    def release(observation=None, plan=None):
        call('kicad_schematic_audit', {'release': {'observation_ids': [observation['observation_id']] if observation else [],
                                               'plan_ids': [plan['plan_id']] if plan else []}})

    def edit(action):
        return {'file': file, 'scope': {'kind': 'file_all_instances'}, 'action': action}

    def prepare(observed, edits, ok=True):
        before = snapshot(project)
        result = call('kicad_plan_schematic_change', {'project_root': str(project), 'root_schematic': str(source),
                      'source_run_id': 'multiunit-acceptance', 'request_id': 'case-' + str(len(transcript)),
                      'general': {'schema_version': 1, 'observation_id': observed['observation_id'],
                                  'observed_revision': observed['revision'], 'edits': edits}}, ok=ok)
        assert snapshot(project) == before and snapshot(config) == configuration
        return result

    def negative(name, observed, edits):
        prepare(observed, edits, ok=False)
        cases.append({'name': name, 'source_unchanged': True})

    def point(x, y):
        return {'x_nm': int(round(x * 1_000_000)), 'y_nm': int(round(y * 1_000_000))}

    def create(unit, reference, lib='Inherited_A:Leaf', instances=None, y=650.24):
        return edit({'kind': 'create_symbol', 'key': reference + '-' + str(unit), 'lib_id': lib,
                     'reference': reference, 'value': 'complete-component', 'unit': unit,
                     'transform': {'position': point(850.9 + (unit - 1) * 50.8, y)},
                     **({'instance_references': instances} if instances is not None else {})})

    def wires(count, x=850.9, y=650.24, spacing=50.8):
        result = []
        for unit in range(count):
            start = x + unit * spacing
            result.append(edit({'kind': 'create_wire', 'key': f'common-contact-{unit}',
                                'start': point(start, y - 5.08), 'end': point(start, y - 10.16)}))
            if unit < count - 1:
                result.append(edit({'kind': 'create_wire', 'key': f'common-bus-{unit}',
                                    'start': point(start, y - 10.16), 'end': point(start + spacing, y - 10.16)}))
        return result

    def expectation(planned, source_uuids, expected_numbers, old_uuid=None):
        native = planned['native_preview']
        has = lambda pin, ids: any(unit['symbol_uuid'] in ids for unit in pin.get('units', []))
        selected_before = [n for n in native['connectivity_before']['nets'] if any(has(p, {old_uuid}) for p in n['pins'])] if old_uuid else []
        selected_after = [n for n in native['connectivity_after']['nets'] if any(has(p, source_uuids) for p in n['pins'])]
        old_pins = [p for n in selected_before for p in n['pins'] if has(p, {old_uuid})]
        new_pins = [p for n in selected_after for p in n['pins'] if has(p, source_uuids)]
        assert {p['number'] for p in new_pins} == expected_numbers
        assert all({u['symbol_uuid'] for u in p['units']} == source_uuids for p in new_pins)
        assert len({canonical(p) for p in new_pins}) == len(new_pins)
        unaffected_before = [n for n in native['connectivity_before']['nets'] if n not in selected_before]
        unaffected_after = [n for n in native['connectivity_after']['nets'] if n not in selected_after]
        assert unaffected_before == unaffected_after
        assert all(len(n['pins']) == 1 for n in selected_after)
        delta = {'removed_nets': selected_before, 'added_nets': selected_after, 'removed_pins': old_pins, 'added_pins': new_pins}
        expected = reviewed(planned, delta)
        introduced = [json.loads(row['diagnostic_json']) for row in expected['erc']['introduced']]
        assert all(row['type'] == 'pin_not_connected' for row in introduced), introduced
        # Native ERC deduplicates the same physical source-pin UUID across reused sheets.
        assert len(introduced) == (2 if old_uuid else len(expected_numbers) - 1), introduced
        assert len(new_pins) == len(expected_numbers) * len(paths)
        return expected

    def publish(name, observed, edits, numbers, old_uuid=None):
        before = snapshot(project)
        original_text = (project / file).read_text()
        original_nodes = list(inheritance.children(original_text))
        planned = prepare(observed, edits)
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        uuids = {row['uuid'] for row in planned['changes'] if row['kind'] == 'Symbol'}
        if old_uuid:
            uuids.add(old_uuid)
        expected = expectation(planned, uuids, numbers, old_uuid)
        call('kicad_preview_schematic_change', identity, ok=False)
        approval = call('kicad_preview_schematic_change', dict(identity, expectations=expected))
        assert snapshot(project) == before
        call('kicad_commit_schematic_change', dict(identity, approval_hash='sha256:' + '0' * 64), ok=False)
        assert snapshot(project) == before
        committed = call('kicad_commit_schematic_change', dict(identity, approval_hash=approval['approval_hash']))
        assert committed['state'] == 'committed'
        call('kicad_commit_schematic_change', dict(identity, approval_hash=approval['approval_hash']), ok=False)
        after = snapshot(project)
        assert before.keys() == after.keys() and {p for p in before if before[p] != after[p]} == {file}
        new_nodes = list(inheritance.children((project / file).read_text()))
        for node in original_nodes:
            if node.startswith('(lib_symbols'):
                new_cache = next(n for n in new_nodes if n.startswith('(lib_symbols'))
                for definition in inheritance.children(node):
                    assert new_cache.count(definition) == node.count(definition)
            else:
                assert new_nodes.count(node) == original_nodes.count(node), node[:100]
        if old_uuid and not args.file:
            added = next(n for n in new_nodes if re.match(r'\(symbol\s', n) and any(f'(uuid "{u}")' in n for u in uuids - {old_uuid}))
            assert '(property "AK_CUSTOM" "keep-source-metadata"' in added
            assert '(property "Footprint" "Connector_PinHeader_2.54mm:PinHeader_1x05_P2.54mm_Vertical"' in added
        assert snapshot(config) == configuration
        release(observation=observed, plan=planned)
        cases.append({'name': name, 'published': True, 'source_units': sorted(uuids), 'pins_per_instance': sorted(numbers)})
        return uuids

    try:
        rpc.call('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'multiunit-acceptance', 'version': '1'}})
        rpc.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        observed = observe()
        paths = sorted({unit['sheet_path'] for unit in observed['units'] if unit['file'] == file})
        assert paths
        refs = {path: 'U' + str(9910 + index) for index, path in enumerate(paths)}
        reference = refs[paths[0]]
        first, second = create(1, reference, instances=refs), create(2, reference, instances=refs)
        for name, modify in [('duplicate-slot', lambda a: a['action'].update(unit=1)),
                             ('inconsistent-value', lambda a: a['action'].update(value='different')),
                             ('inconsistent-flags', lambda a: a['action'].update(flags={'dnp': True})),
                             ('inconsistent-library', lambda a: a['action'].update(lib_id='Triple:Triple')),
                             ('invalid-unit', lambda a: a['action'].update(unit=99))]:
            invalid = copy.deepcopy(second)
            modify(invalid)
            negative(name, observed, [first, invalid])
        if len(paths) > 1:
            missing = copy.deepcopy(first)
            del missing['action']['instance_references']
            negative('reused-sheet-needs-explicit-references', observed, [missing])
            extra = copy.deepcopy(first)
            extra['action']['instance_references']['/unobserved'] = 'U9999'
            negative('extra-instance-path', observed, [extra])
            partial = copy.deepcopy(first)
            del partial['action']['instance_references'][paths[-1]]
            negative('missing-instance-path', observed, [partial])
            repeated = copy.deepcopy(first)
            repeated['action']['instance_references'] = dict.fromkeys(paths, reference)
            negative('repeated-instance-reference', observed, [repeated])
        negative('unwired-common-pin-ambiguous-net', observed, [first, second])
        publish('complete-two-unit-group', observed, [first, second] + wires(2), {'1', '2', '3', '4', '5'})
        observed = observe()
        triple_refs = {path: 'U' + str(9930 + index) for index, path in enumerate(paths)}
        triple = [create(unit, triple_refs[paths[0]], 'Triple:Triple', triple_refs, 701.04) for unit in (1, 2, 3)]
        publish('complete-three-unit-group', observed, triple + wires(3, y=701.04), {str(n) for n in range(1, 8)})
        observed = observe()
        if args.file:
            # Create an intentionally incomplete component in every observed instance, then inherit its annotations.
            partial_refs = {path: 'U' + str(9950 + index) for index, path in enumerate(paths)}
            partial = prepare(observed, [create(1, partial_refs[paths[0]], instances=partial_refs, y=749.3)])
            identity = {'plan_id': partial['plan_id'], 'plan_hash': partial['plan_hash']}
            uuid = partial['changes'][0]['uuid']
            native = partial['native_preview']
            nets = [n for n in native['connectivity_after']['nets'] if any(any(u['symbol_uuid'] == uuid for u in p.get('units', [])) for p in n['pins'])]
            approval = call('kicad_preview_schematic_change', dict(identity, expectations=reviewed(partial, {'added_nets': nets, 'added_pins': [p for n in nets for p in n['pins']]})))
            assert call('kicad_commit_schematic_change', dict(identity, approval_hash=approval['approval_hash']))['state'] == 'committed'
            release(observation=observed, plan=partial)
            observed = observe()
            x, y, spacing = 850.9, 749.3, 50.8
        else:
            anchor = next(u for u in observed['units'] if u['reference'] == 'U9901')
            uuid, x, y, spacing = anchor['symbol_uuid'], 660.4, 508, 101.6
        insertion = edit({'kind': 'add_symbol_unit', 'key': 'missing-unit', 'uuid': uuid, 'unit': 2,
                          'transform': {'position': point(x + spacing, y)}})
        # Move around U9902 in the original fixture when joining common contacts.
        for name, action in [('member-value-conflict', {'kind': 'set_property', 'uuid': uuid, 'name': 'Value', 'value': 'changed'}),
                             ('member-transform-conflict', {'kind': 'transform_symbol', 'uuid': uuid, 'transform': {'position': point(x, y)}})]:
            negative(name, observed, [insertion, edit(action)])
        duplicate = copy.deepcopy(insertion)
        duplicate['action']['key'] = 'duplicate-unit'
        negative('duplicate-inserted-slot', observed, [insertion, duplicate])
        occupied = copy.deepcopy(insertion)
        occupied['action']['unit'] = 1
        negative('occupied-inserted-slot', observed, [occupied])
        negative('unwired-inserted-common-pin', observed, [insertion])
        publish('insert-missing-unit-with-observed-metadata', observed, [insertion] + wires(2, x=x, y=y, spacing=spacing), {'1', '2', '3', '4', '5'}, uuid)
    finally:
        rpc.close()
        (output / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
    assert rpc.process.returncode == 0 and snapshot(config) == configuration
    cache = temporary / 'org.kicad.kicad/instances'
    if cache.is_dir() and not list(cache.iterdir()):
        cache.rmdir()
    cache = temporary / 'org.kicad.kicad'
    if cache.is_dir() and not list(cache.iterdir()):
        cache.rmdir()
    assert not list(temporary.iterdir()), list(temporary.iterdir())
    # A separate native export checks saved component grouping and unique pin membership.
    cli = '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'
    oracle_config = output / 'oracle-config'
    shutil.copytree(config, oracle_config)
    oracle_tmp = output / 'oracle-tmp'
    oracle_tmp.mkdir()
    oracle_env = dict(env, KICAD_CONFIG_HOME=str(oracle_config), TMPDIR=str(oracle_tmp))
    for name, command in [('netlist', ['sch', 'export', 'netlist', '--format', 'kicadxml', '-o', str(output / 'saved.xml'), str(source)]),
                          ('erc', ['sch', 'erc', '--format', 'json', '--severity-all', '-o', str(output / 'saved-erc.json'), str(source)])]:
        result = subprocess.run([cli, *command], env=oracle_env, capture_output=True, timeout=120)
        (output / (name + '.stdout')).write_bytes(result.stdout)
        (output / (name + '.stderr')).write_bytes(result.stderr)
        assert result.returncode == 0, result.stderr
    netlist = ET.parse(output / 'saved.xml').getroot()
    for ref, count in [(r, 2) for r in refs.values()] + [(r, 3) for r in triple_refs.values()]:
        component = [c for c in netlist.findall('./components/comp') if c.attrib['ref'] == ref]
        assert len(component) == 1 and len(component[0].findall('./units/unit')) == count
        nodes = [n.attrib['pin'] for net in netlist.findall('./nets/net') for n in net.findall('node') if n.attrib['ref'] == ref]
        assert len(nodes) == len(set(nodes)) == 2 * count + 1
    if not args.file:
        component = next(c for c in netlist.findall('./components/comp') if c.attrib['ref'] == 'U9901')
        assert len(component.findall('./units/unit')) == 2
        assert component.findtext('footprint') == 'Connector_PinHeader_2.54mm:PinHeader_1x05_P2.54mm_Vertical'
        assert component.find('./fields/field[@name="AK_CUSTOM"]').text == 'keep-source-metadata'
    cache = oracle_tmp / 'org.kicad.kicad/instances'
    if cache.is_dir() and not list(cache.iterdir()):
        cache.rmdir()
    cache = oracle_tmp / 'org.kicad.kicad'
    if cache.is_dir() and not list(cache.iterdir()):
        cache.rmdir()
    assert not list(oracle_tmp.iterdir()) and not list(temporary.iterdir())
    assert snapshot(config) == configuration
    result = {'passed': True, 'production_stdio': True, 'gui_launched': False, 'instances': len(paths), 'cases': cases,
              'configuration_unchanged': snapshot(config) == configuration, 'private_snapshots_removed': not list(temporary.iterdir())}
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
