#!/usr/bin/env python3
"""Qualify existing sheet and hierarchical pin edits through production stdio and native KiCad."""
import argparse
from collections import Counter
from decimal import Decimal
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import threading
import xml.etree.ElementTree as ET

from schematic_general_cases import reviewed
from stdio_rpc import Rpc

spec = importlib.util.spec_from_file_location('native', Path(__file__).with_name('check-schematic-inheritance.py'))
native = importlib.util.module_from_spec(spec); spec.loader.exec_module(native)


def uid(node):
    return re.search(r'\(uuid "([0-9a-f-]{36})"\)', node)[1]


def tag(node):
    return re.match(r'\(([\w_]+)', node)[1]


def point(x, y):
    return {'x_nm': int(Decimal(str(x)) * 1000000), 'y_nm': int(Decimal(str(y)) * 1000000)}


def contact(node):
    fields = re.search(r'\(at ([^()]*)\)', node)[1].split()
    return point(*fields[:2]), {'0': 'zero', '90': 'ccw90', '180': 'half', '270': 'ccw270'}[fields[2]]


def pins(sheet):
    return {uid(node): node for node in native.children(sheet) if tag(node) == 'pin'}


def pin_request(node):
    name, shape = re.match(r'\(pin ("(?:\\.|[^"\\])*") (\w+)', node).groups()
    position, rotation = contact(node)
    return {'name': json.loads(name), 'shape': shape, 'position': position, 'rotation': rotation}


def verify_geometry(before, after, update):
    def rectangle(node):
        items = list(native.children(node))
        at = next(n for n in items if tag(n) == 'at')
        size = next(n for n in items if tag(n) == 'size')
        xy = re.fullmatch(r'\(at ([^()]*)\)', at)[1].split()
        wh = re.fullmatch(r'\(size ([^()]*)\)', size)[1].split()
        return {'position': point(*xy), 'width_nm': int(Decimal(wh[0]) * 1000000), 'height_nm': int(Decimal(wh[1]) * 1000000)}
    original, actual = rectangle(before), rectangle(after)
    expected = update.get('rectangle', original)
    assert actual == expected, (actual, expected)
    dx = expected['position']['x_nm'] - original['position']['x_nm']
    dy = expected['position']['y_nm'] - original['position']['y_nm']
    old_fields = [n for n in native.children(before) if tag(n) == 'property']
    new_fields = [n for n in native.children(after) if tag(n) == 'property']
    assert len(old_fields) == len(new_fields)
    for old, new in zip(old_fields, new_fields):
        old_position, old_rotation = contact(old)
        position, rotation = contact(new)
        assert rotation == old_rotation
        assert position == {'x_nm': old_position['x_nm'] + dx, 'y_nm': old_position['y_nm'] + dy}


def fields(node, excluded):
    return Counter(n for n in native.children(node) if tag(n) not in excluded)


def erc_identity(raw):
    parsed = json.loads(raw) if isinstance(raw, str) else raw
    parsed.pop('date'); parsed.pop('source')
    for sheet in parsed['sheets']:
        for row in sheet['violations']:
            row['items'] = sorted(row['items'], key=lambda x: json.dumps(x, sort_keys=True))
        sheet['violations'] = sorted(sheet['violations'], key=lambda x: json.dumps(x, sort_keys=True))
    parsed['sheets'].sort(key=lambda x: x['uuid_path'])
    return parsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'): parser.add_argument(name, type=Path)
    parser.add_argument('--file'); parser.add_argument('--sheet-name', default='CH2')
    args = parser.parse_args()
    binary, source, config = [p.resolve(strict=True) for p in (args.binary, args.source, args.config)]
    project, file = source.parent, args.file or source.name
    for directory in (project, config):
        assert (directory / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    args.output.mkdir(); output = args.output.resolve(strict=True)
    temporary = output / 'runtime-tmp'; temporary.mkdir()
    oracle_config, oracle_tmp = output / 'oracle-config', output / 'oracle-tmp'
    shutil.copytree(config, oracle_config); oracle_tmp.mkdir()
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / 'state'), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    oracle_env = dict(env, KICAD_CONFIG_HOME=str(oracle_config), TMPDIR=str(oracle_tmp), LC_ALL='en_US.UTF-8', LANG='en_US.UTF-8')
    configuration = native.snapshot(config)
    transcript, cases = [], []
    stopped = threading.Event()
    native_logs = {}
    def capture_native_logs():
        while not stopped.wait(0.02):
            for directory, _, names in os.walk(temporary):
                if 'stderr.log' not in names or len(native_logs) >= 4096:
                    continue
                path = Path(directory) / 'stderr.log'
                try:
                    if path.stat().st_size <= 65_536:
                        native_logs[str(path.relative_to(temporary))] = path.read_text(errors='replace')
                except FileNotFoundError:
                    pass
    tracer = threading.Thread(target=capture_native_logs); tracer.start()
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
        call('kicad_schematic_audit', {'release': {'observation_ids': [observed['observation_id']] if observed else [], 'plan_ids': [planned['plan_id']] if planned else []}})

    def sheet():
        return next(n for n in native.children((project / file).read_text()) if tag(n) == 'sheet' and uid(n) == sheet_id)

    def edits(update):
        return [{'file': file, 'scope': {'kind': 'file_all_instances'}, 'action': {'kind': 'update_sheet', 'uuid': sheet_id, 'update': update}}]

    def plan(observed, requested, ok=True):
        before = native.snapshot(project)
        result = call('kicad_plan_schematic_change', {'project_root': str(project), 'root_schematic': str(source), 'source_run_id': 'sheet-edit-acceptance', 'request_id': 'case-' + str(len(transcript)),
            'general': {'schema_version': 1, 'observation_id': observed['observation_id'], 'observed_revision': observed['revision'], 'edits': requested}}, ok=ok)
        assert native.snapshot(project) == before and native.snapshot(config) == configuration
        return result

    def independent_native(name, planned, observation):
        directory = output / name; directory.mkdir()
        before = native.snapshot(project)
        cli = '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'
        for kind, arguments in [('netlist', ['sch', 'export', 'netlist', '--format', 'kicadxml', '-o', str(directory / 'saved.xml')]), ('erc', ['sch', 'erc', '--format', 'json', '--severity-all', '-o', str(directory / 'erc.json')])]:
            completed = subprocess.run([cli, *arguments, str(source)], env=oracle_env, capture_output=True, timeout=120)
            (directory / (kind + '.stdout')).write_bytes(completed.stdout); (directory / (kind + '.stderr')).write_bytes(completed.stderr)
            assert completed.returncode == 0, (kind, completed.stderr)
        assert native.snapshot(project) == before
        xml = ET.parse(directory / 'saved.xml').getroot()
        actual = sorted((n.attrib['name'], sorted((p.attrib['ref'], p.attrib['pin']) for p in n.findall('node'))) for n in xml.findall('./nets/net') if n.findall('node'))
        refs = {(u['file'], u['sheet_path'], u['symbol_uuid']): u['reference'] for u in observation['units']}
        expected = []
        for net in planned['native_preview']['connectivity_after']['nets']:
            nodes = []
            for pin in net['pins']:
                if pin['kind'] != 'component': continue
                references = {refs[(u['file'], u['sheet_path'], u['symbol_uuid'])] for u in pin['units']}
                assert len(references) == 1
                nodes.append((references.pop(), pin['number']))
            if nodes: expected.append((net['name'], sorted(nodes)))
        assert sorted(expected) == actual, (name, 'independent native component network mismatch')
        assert erc_identity((directory / 'erc.json').read_text()) == erc_identity(planned['native_preview']['erc_after']['raw_json']), name

    def preserve(before_text, update):
        old_sheet = next(n for n in native.children(before_text) if tag(n) == 'sheet' and uid(n) == sheet_id)
        after_text = (project / file).read_text(); new_sheet = sheet()
        verify_geometry(old_sheet, new_sheet, update)
        assert fields(before_text, {'sheet'}) == fields(after_text, {'sheet'})
        assert Counter(n for n in native.children(before_text) if tag(n) == 'sheet' and uid(n) != sheet_id) == Counter(n for n in native.children(after_text) if tag(n) == 'sheet' and uid(n) != sheet_id)
        assert fields(old_sheet, {'at', 'size', 'property', 'pin'}) == fields(new_sheet, {'at', 'size', 'property', 'pin'})
        old_properties = [n for n in native.children(old_sheet) if tag(n) == 'property']
        new_properties = [n for n in native.children(new_sheet) if tag(n) == 'property']
        assert len(old_properties) == len(new_properties)
        for old, new in zip(old_properties, new_properties):
            assert fields(old, {'at'}) == fields(new, {'at'})
            key, value = re.match(r'\(property ("[^"]*") ("(?:\\.|[^"\\])*")', old).groups()
            actual_key, actual_value = re.match(r'\(property ("[^"]*") ("(?:\\.|[^"\\])*")', new).groups()
            assert key == actual_key and json.loads(actual_value) == (update['name'] if json.loads(key) == 'Sheetname' and 'name' in update else json.loads(value))
            if 'rectangle' not in update: assert contact(old) == contact(new)
        old_pins, new_pins = pins(old_sheet), pins(new_sheet)
        changed = {p['uuid']: p for p in update.get('pins', []) if p['kind'] != 'create'}
        for uuid, old in old_pins.items():
            if uuid not in changed: assert new_pins[uuid] == old
            elif changed[uuid]['kind'] == 'delete': assert uuid not in new_pins
            else:
                assert pin_request(new_pins[uuid]) == changed[uuid]['pin']
                assert fields(old, {'at'}) == fields(new_pins[uuid], {'at'})
        created = [pin_request(n) for uuid, n in new_pins.items() if uuid not in old_pins]
        assert sorted(created, key=lambda p: p['name']) == sorted([p['pin'] for p in update.get('pins', []) if p['kind'] == 'create'], key=lambda p: p['name'])
        assert new_pins.keys() == old_pins.keys() - {u for u, p in changed.items() if p['kind'] == 'delete'} | (new_pins.keys() - old_pins.keys())

    def publish(name, update):
        print('START', name, flush=True)
        observation = observe(); before = native.snapshot(project); before_text = (project / file).read_text()
        planned = plan(observation, edits(update))
        assert len(planned['changes']) == 1 and planned['changes'][0]['kind'] == 'Sheet'
        assert len(planned['changes'][0]['instance_paths']) == instances
        preview_native = planned['native_preview']
        old, new = preview_native['connectivity_before'], preview_native['connectivity_after']
        assert old['isolated_no_connect'] == new['isolated_no_connect']
        assert Counter(json.dumps(p, sort_keys=True) for n in old['nets'] for p in n['pins']) == Counter(json.dumps(p, sort_keys=True) for n in new['nets'] for p in n['pins'])
        expected = reviewed(planned, {'removed_nets': [n for n in old['nets'] if n not in new['nets']], 'added_nets': [n for n in new['nets'] if n not in old['nets']]})
        for key in ('removed_sheet_headers', 'added_sheet_headers'): expected['erc'][key] = preview_native['erc_delta'][key]
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        if any(expected['erc'].values()) or old != new:
            call('kicad_preview_schematic_change', identity, ok=False)
        if expected['erc']['added_sheet_headers']:
            wrong = json.loads(json.dumps(expected)); wrong['erc']['added_sheet_headers'].pop()
            call('kicad_preview_schematic_change', dict(identity, expectations=wrong), ok=False)
        preview = call('kicad_preview_schematic_change', dict(identity, expectations=expected))
        call('kicad_commit_schematic_change', dict(identity, approval_hash='sha256:' + '0' * 64), ok=False)
        assert native.snapshot(project) == before
        committed = call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']))
        assert committed['state'] == 'committed'
        call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        after = native.snapshot(project)
        assert after.keys() == before.keys() and {p for p in before if before[p] != after[p]} == {file}
        preserve(before_text, update)
        independent_native(name, planned, observation)
        assert native.snapshot(config) == configuration
        release(observation, planned)
        cases.append({'name': name, 'published': True, 'affected_parent_instances': instances, 'independent_saved_native': True})
        print('PASS', name, flush=True)

    try:
        rpc.call('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'sheet-edit-acceptance', 'version': '1'}})
        rpc.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        initial = observe()
        selected = next(o for o in initial['objects'] if o['file'] == file and o['kind'] == 'sheet' and f'(property "Sheetname" "{args.sheet_name}"' in o['definition'])
        sheet_id = selected['uuid']; original_sheet = selected['definition']
        instances = len({u['sheet_path'] for u in initial['units'] if u['file'] == file})
        assert instances in (1, 4), instances
        old_rectangle = re.search(r'\(at ([^()]*)\)', original_sheet)[1].split()
        width, height = re.search(r'\(size ([^()]*)\)', original_sheet)[1].split()
        x, y, width, height = map(Decimal, [*old_rectangle, width, height])
        original_pins = {uuid: pin_request(node) for uuid, node in pins(original_sheet).items()}
        rectangle = lambda px, py, w=width, h=height: {'position': point(px, py), 'width_nm': int(w * 1000000), 'height_nm': int(h * 1000000)}
        base = rectangle(x, y)
        invalid = [('empty', {}), ('duplicate-sibling-name', {'name': 'CH1'} if instances == 1 else {'name': 'bad/name'}),
                   ('invalid-dimension', {'rectangle': dict(base, width_nm=0)}), ('off-grid', {'rectangle': dict(base, width_nm=1)}),
                   ('unowned-pin', {'pins': [{'kind': 'delete', 'uuid': '11111111-1111-4111-8111-111111111111'}]}),
                   ('nested-operation-bound', {'pins': [{'kind': 'create', 'key': f'excess-{i}', 'pin': {'name': f'P{i}', 'shape': 'passive', 'position': point(x, y + 1), 'rotation': 'half'}} for i in range(128)]})]
        if original_pins:
            id0 = next(iter(original_pins)); p0 = original_pins[id0]
            invalid += [('implicit-pin-drag', {'rectangle': rectangle(x + 1, y + 1)}), ('repeated-pin-edit', {'pins': [{'kind': 'delete', 'uuid': id0}] * 2}),
                        ('wrong-pin-direction', {'pins': [{'kind': 'update', 'uuid': id0, 'pin': dict(p0, rotation='half')}]}),
                        ('duplicate-pin-name', {'pins': [{'kind': 'create', 'key': 'duplicate', 'pin': dict(p0, position=point(x + width, y + 1))}]})]
        for name, update in invalid:
            plan(initial, edits(update), ok=False); cases.append({'name': name, 'source_unchanged': True})
        if instances == 4:
            requested = edits({'name': args.sheet_name + '_ISOLATED'}); requested[0]['scope'] = {'kind': 'single_instance', 'path': next(u['sheet_path'] for u in initial['units'] if u['file'] == file)}
            plan(initial, requested, ok=False); cases.append({'name': 'reused-parent-isolation-rejected', 'source_unchanged': True})
        release(initial)
        publish('sheet-rename', {'name': args.sheet_name + '_QUALIFIED'})
        publish('sheet-name-restored', {'name': args.sheet_name})
        dx, dy = Decimal('12.7'), Decimal('12.7')
        moved = [{'kind': 'update', 'uuid': uuid, 'pin': dict(p, position={'x_nm': p['position']['x_nm'] + int(dx * 1000000), 'y_nm': p['position']['y_nm'] + int(dy * 1000000)})} for uuid, p in original_pins.items()]
        restored = [{'kind': 'update', 'uuid': uuid, 'pin': p} for uuid, p in original_pins.items()]
        publish('sheet-move-explicit-pins', {'rectangle': rectangle(x + dx, y + dy), 'pins': moved})
        publish('sheet-move-restored', {'rectangle': base, 'pins': restored})
        resized = [{'kind': 'update', 'uuid': uuid, 'pin': dict(p, position={'x_nm': p['position']['x_nm'] + 2540000, 'y_nm': p['position']['y_nm']})} for uuid, p in original_pins.items()]
        publish('sheet-resize-explicit-pins', {'rectangle': rectangle(x, y, width + Decimal('2.54')), 'pins': resized})
        publish('sheet-size-restored', {'rectangle': base, 'pins': restored})
        additions = []
        for name, px, py, rotation in [('RIGHT', x + width, y + height / 4, 'zero'), ('LEFT', x, y + height / 2, 'half'), ('TOP', x + width / 2, y, 'ccw90'), ('BOTTOM', x + width / 2, y + height, 'ccw270')]:
            additions.append({'kind': 'create', 'key': 'extra-' + name, 'pin': {'name': 'AK_EXTRA_' + name, 'shape': 'passive', 'position': point(px, py), 'rotation': rotation}})
        publish('four-edge-pin-create', {'pins': additions})
        extra = {uuid: pin_request(node) for uuid, node in pins(sheet()).items() if pin_request(node)['name'].startswith('AK_EXTRA_')}
        left_id, left = next((uuid, p) for uuid, p in extra.items() if p['name'] == 'AK_EXTRA_LEFT')
        publish('pin-update', {'pins': [{'kind': 'update', 'uuid': left_id, 'pin': dict(left, name='AK_EXTRA_LEFT_UPDATED', shape='input', position=point(x, y + height * Decimal('0.75')))}]})
        publish('explicit-pin-delete', {'pins': [{'kind': 'delete', 'uuid': uuid} for uuid in extra]})
        if original_pins:
            uuid, pin = next(iter(original_pins.items()))
            publish('connected-pin-rename', {'pins': [{'kind': 'update', 'uuid': uuid, 'pin': dict(pin, name='AK_OTHER_PORT')}]})
            publish('connected-pin-name-restored', {'pins': [{'kind': 'update', 'uuid': uuid, 'pin': pin}]})
        observed = observe(); before = native.snapshot(project)
        planned = plan(observed, edits({'name': args.sheet_name + '_DRIFT'}))
        identity = {'plan_id': planned['plan_id'], 'plan_hash': planned['plan_hash']}
        p = planned['native_preview']; expected = reviewed(planned, {'removed_nets': [n for n in p['connectivity_before']['nets'] if n not in p['connectivity_after']['nets']], 'added_nets': [n for n in p['connectivity_after']['nets'] if n not in p['connectivity_before']['nets']]})
        for key in ('removed_sheet_headers', 'added_sheet_headers'): expected['erc'][key] = p['erc_delta'][key]
        preview = call('kicad_preview_schematic_change', dict(identity, expectations=expected))
        saved = (project / file).read_bytes()
        try:
            (project / file).write_bytes(saved + b'\n')
            call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        finally: (project / file).write_bytes(saved)
        call('kicad_commit_schematic_change', dict(identity, approval_hash=preview['approval_hash']), ok=False)
        assert native.snapshot(project) == before
        release(observed, planned)
        cases.append({'name': 'source-drift-permanently-revokes', 'source_restored': True})
    finally:
        rpc.close()
        stopped.set(); tracer.join(timeout=10)
        assert not tracer.is_alive()
        (output / 'native-stderr.json').write_text(json.dumps(native_logs, indent=2) + '\n')
    assert rpc.process.returncode == 0 and native.snapshot(config) == configuration
    for directory in (temporary, oracle_tmp):
        for child in [directory / 'org.kicad.kicad/instances', directory / 'org.kicad.kicad']:
            if child.is_dir() and not list(child.iterdir()): child.rmdir()
        assert not list(directory.iterdir()), list(directory.iterdir())
    result = {'passed': True, 'production_stdio': True, 'server_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'native_version': '10.0.7', 'instances': instances,
              'cases': cases, 'configuration_unchanged': True, 'private_snapshots_removed': True, 'gui_launched': False}
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__': main()
