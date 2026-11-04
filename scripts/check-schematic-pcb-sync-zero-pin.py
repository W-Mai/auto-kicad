#!/usr/bin/env python3
"""Qualify existing zero-pin metadata on an owned synchronized two-resistor fixture with KiCad 10.0.7."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from stdio_rpc import Rpc
from schematic_general_cases import reviewed

def root_nodes(data):
    depth, quoted, escaped, start, out = 0, False, False, None, []
    for index, char in enumerate(data):
        if quoted:
            if escaped: escaped = False
            elif char == 92: escaped = True
            elif char == 34: quoted = False
        elif char == 34: quoted = True
        elif char == 40:
            depth += 1
            if depth == 2: start = index
        elif char == 41:
            if depth == 2: out.append(data[start:index + 1])
            depth -= 1; assert depth >= 0
    assert depth == 0 and not quoted and out
    return out

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'): parser.add_argument(name, type=Path)
    args = parser.parse_args()
    binary, input_source, input_config = [p.resolve(strict=True) for p in [args.binary, args.source, args.config]]
    assert input_source.name == 'sync.kicad_sch'
    for owned in [input_source.parent, input_config]:
        assert (owned / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    run = args.output.resolve()
    assert not run.is_relative_to(input_source.parent) and not run.is_relative_to(input_config)
    run.mkdir()
    project, config = run / 'project', run / 'config'
    shutil.copytree(input_source.parent, project); shutil.copytree(input_config, config)
    source, board = project / 'sync.kicad_sch', project / 'sync.kicad_pcb'
    private = run / 'runtime-tmp'; private.mkdir()
    env = dict(os.environ, KICAD_CONFIG_HOME=str(config), AUTO_KICAD_STATE_DIR=str(run / 'state'), TMPDIR=str(private), PYTHONDONTWRITEBYTECODE='1')
    library = Path('/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols/Mechanical.kicad_sym').read_bytes()
    definition = next(n for n in root_nodes(library) if n.startswith(b'(symbol "MountingHole"'))
    definition = definition.replace(b'(symbol "MountingHole"', b'(symbol "Mechanical:MountingHole"', 1)
    data = source.read_bytes(); assert data.count(b'(lib_symbols') == 1
    assert any(n.startswith(b'(uuid ') and b'11111111-1111-4111-8111-111111111111' in n for n in root_nodes(data))
    assert b'e1111111-1111-4111-8111-111111111111' not in data
    data = data.replace(b'(lib_symbols', b'(lib_symbols\n' + definition, 1)
    symbol = b'''(symbol (lib_id "Mechanical:MountingHole") (at 50.8 50.8 0) (unit 1)
      (in_bom no) (on_board yes) (dnp no) (uuid "e1111111-1111-4111-8111-111111111111")
      (property "Reference" "H1" (at 50.8 48.26 0) (effects (font (size 1.27 1.27))))
      (property "Value" "M4 mounting hole reviewed" (at 50.8 53.34 0) (effects (font (size 1.27 1.27))))
      (property "Footprint" "MountingHole:MountingHole_4.3mm_M4" (at 50.8 50.8 0) (effects (font (size 1.27 1.27)) hide))
      (instances (project "sync" (path "/11111111-1111-4111-8111-111111111111" (reference "H1") (unit 1)))))'''
    end = data.rfind(b')'); source.write_bytes(data[:end] + symbol + b'\n' + data[end:])
    native = '/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9'
    with (run / 'native-preparation.log').open('w') as log:
        prepared = subprocess.run([native, '-I', '-B', str(Path(__file__).with_name('fixtures') / 'prepare-zero-pin.py'), str(project)], env=env,
            stdout=subprocess.PIPE, stderr=log, check=True, timeout=60)
    (run / 'native-preparation.json').write_bytes(prepared.stdout)
    (run / 'before.kicad_pcb').write_bytes(board.read_bytes())
    def snapshot(folder):
        return {str(p.relative_to(folder)): hashlib.sha256(p.read_bytes()).hexdigest()
                for p in folder.rglob('*') if p.is_file() and p.name != '.auto-kicad-writer.lock'}
    input_snapshot, input_configuration = snapshot(input_source.parent), snapshot(input_config)
    before, configuration = snapshot(project), snapshot(config)
    transcript = []
    rpc = Rpc([str(binary)], env=env, log=run / 'server.log')
    def call(name, request, ok=True):
        response = rpc.call('tools/call', dict(name=name, arguments=request), timeout=600)
        transcript.append(dict(tool=name, request=request, response=response))
        (run / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
        envelope = response.get('structuredContent', {})
        assert not response.get('isError') and envelope.get('ok') == ok, (name, response)
        return envelope['data']
    def observe():
        return call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='observe', project_root=str(project), root_schematic=str(source))))
    def plan(observed, action, ok=True):
        return call('kicad_plan_change', dict(source_run_id='zero-pin-control', request_id=f'case-{len(transcript)}',
            schematic_pcb_sync=dict(schema_version=1, observation_id=observed['observation_id'], observed_revision=observed['revision'], intent=dict(schema_version=1, actions=[action], copper_nets=[]))), ok=ok)
    try:
        rpc.call('initialize', dict(protocolVersion='2025-03-26', capabilities={}, clientInfo=dict(name='zero-pin-native-mcp', version='1')))
        rpc.send(dict(jsonrpc='2.0', method='notifications/initialized'))
        initial = observe(); component = next(c for c in initial['components'] if c['reference'] == 'H1')
        fp = next(fp for fp in initial['footprints'].values() if fp['reference'] == 'H1')
        assert component['pins'] == {} and component['units'] == [dict(name='A', pins=[])]
        refused = plan(initial, dict(kind='add', component_id=component['id'], fields=[], placement=dict(position_nm=[45000000,30000000], orientation_mdeg=0, side='front')), ok=False)
        assert 'zero-pin footprint creation/replacement' in refused['reason']
        assert snapshot(project) == before
        planned = plan(initial, dict(kind='relink', component_id=component['id'], footprint_uuid=fp['uuid'], fields=[], update_nets=False))
        assert planned['has_changes'] and not planned['drc_delta']['introduced']
        expected = dict(differences=planned['differences'], drc={k:planned['drc_delta'][k] for k in ['introduced','resolved']})
        approval = call('kicad_preview_change', dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'], schematic_pcb_sync=expected))
        result = call('kicad_commit_change', dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'], approval_hash=approval['approval_hash'], validation_evidence_hash=approval['validation_evidence_hash']))
        assert result['state'] == 'committed'
        call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='release', plan_ids=[planned['plan_id']])))
        print('Actual zero-pin native relink/metadata publication passed', flush=True)
        final = observe(); updated = next(f for f in final['footprints'].values() if f['reference'] == 'H1')
        assert final['copper'] == initial['copper'] and updated['pads'] == fp['pads'] and updated['path'] == component['canonical_path']
        for uuid, record in initial['footprints'].items():
            if uuid != fp['uuid']: assert final['footprints'][uuid] == record
        retained_source = source.read_bytes(); unchanged_board = board.read_bytes()
        source.write_bytes(retained_source.replace(b'"MountingHole:MountingHole_4.3mm_M4"', b'"MountingHole:MountingHole_3.2mm_M3"'))
        try:
            changed = observe(); target = next(c for c in changed['components'] if c['reference'] == 'H1')
            refused = plan(changed, dict(kind='replace', component_id=target['id'], footprint_uuid=fp['uuid'], fields=[]), ok=False)
            assert 'zero-pin footprint creation/replacement' in refused['reason'] and board.read_bytes() == unchanged_board
        finally:
            source.write_bytes(retained_source)
        # Existing source metadata also uses the same verified empty-component model.
        observed = call('kicad_schematic_audit', dict(observe=dict(project_root=str(project), root_schematic=str(source))))
        planned = call('kicad_plan_schematic_change', dict(project_root=str(project), root_schematic=str(source), source_run_id='zero-pin', request_id='existing-value',
            general=dict(schema_version=1, observation_id=observed['observation_id'], observed_revision=observed['revision'], edits=[dict(file=source.name,
            scope=dict(kind='file_all_instances'), action=dict(kind='set_property', uuid='e1111111-1111-4111-8111-111111111111', name='Value', value='M4 mounting hole approved'))])))
        approved = call('kicad_preview_schematic_change', dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'], expectations=reviewed(planned)))
        committed = call('kicad_commit_schematic_change', dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'], approval_hash=approved['approval_hash']))
        assert committed['state'] == 'committed'
        call('kicad_schematic_audit', dict(release=dict(observation_ids=[observed['observation_id']], plan_ids=[planned['plan_id']])))
        after_source = observe(); target = next(c for c in after_source['components'] if c['reference'] == 'H1')
        update = plan(after_source, dict(kind='update', component_id=target['id'], fields=[], update_nets=False))
        assert update['differences'] == [dict(uuid=fp['uuid'], field='property:Value', before='M4 mounting hole reviewed', after='M4 mounting hole approved')]
        assert not update['drc_delta']['introduced']
        expected = dict(differences=update['differences'], drc={k:update['drc_delta'][k] for k in ['introduced','resolved']})
        approval = call('kicad_preview_change', dict(plan_id=update['plan_id'], plan_hash=update['plan_hash'], schematic_pcb_sync=expected))
        committed = call('kicad_commit_change', dict(plan_id=update['plan_id'], plan_hash=update['plan_hash'], approval_hash=approval['approval_hash'], validation_evidence_hash=approval['validation_evidence_hash']))
        assert committed['state'] == 'committed'
        call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='release', plan_ids=[update['plan_id']])))
        final = observe(); updated = next(f for f in final['footprints'].values() if f['reference'] == 'H1')
        assert updated['value'] == target['value'] == 'M4 mounting hole approved' and final['copper'] == initial['copper']
        assert updated['pads'] == fp['pads']
        for uuid, record in initial['footprints'].items():
            if uuid != fp['uuid']: assert final['footprints'][uuid] == record
        assert snapshot(config) == configuration
        changed = snapshot(project)
        assert set(changed) == set(before) and {p for p in before if changed[p] != before[p]} == {source.name, board.name}
        (run / 'result.kicad_pcb').write_bytes(board.read_bytes()); (run / 'result.kicad_sch').write_bytes(source.read_bytes())
        (run / 'result.json').write_text(json.dumps(dict(passed=True, server_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
            actual_stdio=True, zero_pin_relink_native_publication=True, zero_pin_source_value_publication=True,
            zero_pin_source_and_board_value_match=True,
            zero_pin_add_and_replace_rejected=True, unnumbered_pad_records_unchanged=True, unrelated_objects_unchanged=True,
            native_prepared_synthetic_control=True, real_project_gate_claimed=False, gui_launched=False, product_complete=False), indent=2) + '\n')
    finally:
        rpc.close()
        assert not [p for p in private.rglob('*') if p.is_file()]
    assert input_snapshot == snapshot(input_source.parent) and input_configuration == snapshot(input_config)
    print(run, flush=True)

if __name__ == '__main__':
    main()
