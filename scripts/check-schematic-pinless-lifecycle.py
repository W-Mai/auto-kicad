#!/usr/bin/env python3
"""Exercise complete source-authored pinless components through production MCP."""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
from stdio_rpc import Rpc
from schematic_general_cases import reviewed

PYTHON = '/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9'
CLI = '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'

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

def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def snapshot(root):return {str(p.relative_to(root)):sha(p) for p in root.rglob('*') if p.is_file() and p.name!='.auto-kicad-writer.lock'}

class Client:
    def __init__(self,binary,case):
        self.binary=Path(binary);self.case=Path(case)
        self.project=self.case/'project';self.config=self.case/'config'
        self.source=self.project/'sync.kicad_sch';self.pro=self.project/'sync.kicad_pro';self.board=self.project/'sync.kicad_pcb'
        self.temporary=self.case/'runtime-tmp';self.temporary.mkdir()
        self.state=self.case/'state';self.transcript=[];self.launches=0;self.rpc=None
        self.env=dict(os.environ,AUTO_KICAD_STATE_DIR=str(self.state),KICAD_CONFIG_HOME=str(self.config),
            TMPDIR=str(self.temporary),PYTHONDONTWRITEBYTECODE='1')
    def start(self):
        assert self.rpc is None
        self.launches+=1
        self.rpc=Rpc([str(self.binary)],env=self.env,log=self.case/f'server-{self.launches}.log')
        self.rpc.call('initialize',dict(protocolVersion='2025-03-26',capabilities={},clientInfo=dict(name='project-sync-recovery-acceptance',version='1')))
        self.rpc.send(dict(jsonrpc='2.0',method='notifications/initialized'))
    def close(self):
        if self.rpc:self.rpc.close();self.rpc=None
        (self.case/'transcript.json').write_text(json.dumps(self.transcript,indent=2)+'\n')
    def call(self,name,request,ok=True):
        response=self.rpc.call('tools/call',dict(name=name,arguments=request),timeout=900)
        assert not response.get('isError'),(name,response)
        envelope=response['structuredContent']
        self.transcript.append(dict(tool=name,request=request,response=envelope))
        assert envelope['ok']==ok,(name,envelope)
        return envelope['data']
    def observe(self):
        return self.call('kicad_inspect_project',dict(schematic_pcb_sync=dict(kind='observe',project_root=str(self.project),root_schematic=str(self.source),project_netclasses=True)))

    def plan(self, observed, intent, ok=True):
        return self.call('kicad_plan_change', dict(source_run_id='pinless-lifecycle', request_id=f'plan-{len(self.transcript)}',
            schematic_pcb_sync=dict(schema_version=1, observation_id=observed['observation_id'], observed_revision=observed['revision'], intent=intent)), ok=ok)

    @staticmethod
    def identity(plan): return dict(plan_id=plan['plan_id'], plan_hash=plan['plan_hash'])

    def commit_request(self, plan, approved):
        return dict(self.identity(plan), approval_hash=approved['approval_hash'], validation_evidence_hash=approved['validation_evidence_hash'])


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def node_uuid(node):
    matches = re.findall(rb'\(uuid "([^"\\]+)"\)', node)
    return matches[0].decode() if matches else None


def canonical_report(report):
    result = copy.deepcopy(report)
    for key in ['date', 'source']: result.pop(key, None)
    for section in ['violations', 'unconnected_items', 'schematic_parity']:
        if section in result:
            for item in result[section]:
                if 'items' in item: item['items'].sort(key=lambda r: json.dumps(r, sort_keys=True))
            result[section].sort(key=lambda r: json.dumps(r, sort_keys=True))
    if 'sheets' in result:
        for sheet in result['sheets']:
            for item in sheet['violations']:
                item['items'].sort(key=lambda r: json.dumps(r, sort_keys=True))
            sheet['violations'].sort(key=lambda r: json.dumps(r, sort_keys=True))
        result['sheets'].sort(key=lambda r: json.dumps(r, sort_keys=True))
    return result


def qualify(binary, case, profile, origin, source_name, input_config, real=False, position_override=None):
    original_inputs = {'project': snapshot(origin), 'config': snapshot(input_config)}
    shutil.copytree(origin, case / 'project'); shutil.copytree(input_config, case / 'config')
    client = Client(binary, case)
    client.source = client.project / source_name
    client.pro = client.source.with_suffix('.kicad_pro')
    client.board = client.source.with_suffix('.kicad_pcb')
    if profile == 'legacy-front':
        assert b'(version 20250114)' in client.source.read_bytes(), 'legacy acceptance requires an already synchronized legacy source/PCB pair'
    initial_files, configuration = snapshot(client.project), snapshot(client.config)
    original_source = client.source.read_bytes(); original_board = client.board.read_bytes()
    (case / 'original.kicad_sch').write_bytes(original_source)
    (case / 'original.kicad_pcb').write_bytes(original_board)
    original_board_nodes = root_nodes(original_board)
    steps, controls = [], []
    hole = 'MountingHole:MountingHole_3.2mm_M3'
    replacement = 'MountingHole:MountingHole_4.3mm_M4'
    logo = profile == 'no-pad'
    if logo:
        hole = 'Symbol:KiCad-Logo2_5mm_SilkScreen'
        replacement = 'Symbol:KiCad-Logo2_6mm_SilkScreen'
    back = profile in ['back-oblique', 'real', 'no-pad']
    angle = 123456 if back else (37123 if logo else 0)
    side = 'back' if back else 'front'
    position = [151450000, 113100000] if real else [60000000, 29000000]
    if logo:
        angle = 37123
        position = [45000000, 27835000]
    if position_override is not None: position = position_override
    baseline_observed = None
    target_uuid = None

    def unchanged(before):
        assert snapshot(client.project) == before
        assert snapshot(client.config) == configuration

    def source_observe():
        return client.call('kicad_schematic_audit', dict(observe=dict(project_root=str(client.project), root_schematic=str(client.source))))

    def source_plan(observed, action, ok=True):
        return client.call('kicad_plan_schematic_change', dict(project_root=str(client.project), root_schematic=str(client.source),
            source_run_id='empty-lifecycle', request_id=f'source-{len(client.transcript)}', general=dict(schema_version=1,
            observation_id=observed['observation_id'], observed_revision=observed['revision'], edits=[dict(file=client.source.name,
            scope=dict(kind='file_all_instances'), action=action)])), ok=ok)

    def source_publish(action):
        before = snapshot(client.project); board = client.board.read_bytes(); old = client.source.read_bytes()
        observed = source_observe(); plan = source_plan(observed, action)
        expected = reviewed(plan)
        assert not expected['erc']['introduced'] and not expected['erc']['resolved'], plan
        approved = client.call('kicad_preview_schematic_change', dict(client.identity(plan), expectations=expected))
        unchanged(before)
        committed = client.call('kicad_commit_schematic_change', dict(client.identity(plan), approval_hash=approved['approval_hash']))
        assert committed['state'] == 'committed'
        after = snapshot(client.project)
        assert set(before) == set(after) and {key for key in before if before[key] != after[key]} == {client.source.name}
        assert client.board.read_bytes() == board and snapshot(client.config) == configuration
        new = client.source.read_bytes()
        changed_uuid = plan['changes'][0]['uuid']
        def protected(data):
            return [node for node in root_nodes(data) if not node.startswith(b'(lib_symbols') and
                not (re.match(rb'\(symbol\s', node) and node_uuid(node) == changed_uuid)]
        assert protected(old) == protected(new), 'unselected source root changed'
        old_lib = next(node for node in root_nodes(old) if node.startswith(b'(lib_symbols'))
        new_lib = next(node for node in root_nodes(new) if node.startswith(b'(lib_symbols'))
        if action['kind'] == 'create_symbol':
            assert all(node in root_nodes(new_lib) for node in root_nodes(old_lib))
        else: assert old_lib == new_lib
        steps.append(dict(kind='source-' + action['kind'], plan_hash=plan['plan_hash'], uuid=changed_uuid))
        print(profile + ': source ' + action['kind'] + ' committed', flush=True)
        client.call('kicad_schematic_audit', dict(release=dict(observation_ids=[observed['observation_id']], plan_ids=[plan['plan_id']])))
        return changed_uuid

    def component(observed):
        result = next(c for c in observed['components'] if c['reference'] == 'H900')
        assert result['pins'] == {} and result['units'] == [dict(name='A', pins=[])]
        return result

    def footprint(observed):
        return next(fp for fp in observed['footprints'].values() if fp['reference'] == 'H900')

    def preserve(observed):
        assert observed['copper'] == baseline_observed['copper']
        for identity, fp in baseline_observed['footprints'].items(): assert observed['footprints'][identity] == fp
        retained = [node for node in root_nodes(client.board.read_bytes()) if not (node.startswith(b'(footprint ') and node_uuid(node) == target_uuid)]
        assert retained == original_board_nodes, 'original complete PCB root bytes/order changed'

    def board_plan(observed, action, ok=True):
        return client.plan(observed, dict(schema_version=1, actions=[action], copper_nets=[]), ok=ok)

    def board_publish(observed, action):
        nonlocal target_uuid
        before = snapshot(client.project)
        plan = board_plan(observed, action); assert plan['has_changes'] and not plan['drc_delta']['metadata_changed']
        assert not plan['drc_delta']['introduced'], plan['drc_delta']
        write(case / f"{len(steps):02d}-{action['kind']}-preview.json", plan)
        expected = dict(differences=plan['differences'], drc={key: plan['drc_delta'][key] for key in ['introduced', 'resolved']})
        client.call('kicad_preview_change', client.identity(plan), ok=False)
        approved = client.call('kicad_preview_change', dict(client.identity(plan), schematic_pcb_sync=expected))
        client.call('kicad_commit_change', dict(client.commit_request(plan, approved), approval_hash='wrong'), ok=False)
        unchanged(before)
        committed = client.call('kicad_commit_change', client.commit_request(plan, approved)); assert committed['state'] == 'committed'
        assert sha(client.board) == plan['native_preview']['candidate_board_sha256']
        after = snapshot(client.project)
        assert set(after) == set(before) and {key for key in before if after[key] != before[key]} == {client.board.name}
        assert snapshot(client.config) == configuration
        client.call('kicad_commit_change', client.commit_request(plan, approved), ok=False)
        assert snapshot(client.project) == after
        actual = client.observe()
        if action['kind'] != 'remove':
            fp = footprint(actual); target_uuid = fp['uuid']
            assert fp['path'] == component(actual)['canonical_path'] and fp['library'] == component(actual)['footprint']
            assert len(fp['pads']) == (0 if logo else 1)
        preserve(actual)
        name = f"{len(steps):02d}-{action['kind']}"
        saved = case / (name + '.kicad_pcb'); saved.write_bytes(client.board.read_bytes())
        write(case / (name + '-observed.json'), actual)
        independent(saved, plan, action, None if action['kind'] == 'remove' else component(actual)['footprint'])
        steps.append(dict(kind='pcb-' + action['kind'], plan_hash=plan['plan_hash'], target_uuid=target_uuid))
        print(profile + ': PCB ' + action['kind'] + ' committed and independently checked', flush=True)
        client.call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='release', observation_ids=[observed['observation_id'], actual['observation_id']], plan_ids=[plan['plan_id']])) )
        return actual

    def independent(saved, plan, action, expected_library):
        label = saved.stem
        oracle = case / (label + '-oracle'); oracle.mkdir()
        for name in ['project', 'config']: shutil.copytree(case / name, oracle / name)
        common = oracle / 'config/10.0/kicad_common.json'
        language = re.compile(r'("language"\s*:\s*)"(?:[^"\\]|\\.)*"')
        text = common.read_text(); assert len(list(language.finditer(text))) == 1
        common.write_text(language.sub(lambda match: match.group(1) + '"English"', text))
        assert json.loads(common.read_text())['system']['language'] == 'English'
        shutil.copy2(saved, oracle / 'project' / client.board.name)
        private = oracle / 'tmp'; private.mkdir()
        env = dict(client.env, KICAD_CONFIG_HOME=str(oracle / 'config'), TMPDIR=str(private), LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8')
        native = oracle / 'native.json'
        with native.open('w') as out, (oracle / 'native.log').open('w') as log:
            subprocess.run([PYTHON, '-I', '-B', str(Path(__file__).with_name('fixtures') / 'inspect-pinless-board.py'), str(oracle / 'project' / client.board.name)], env=env, stdout=out, stderr=log, timeout=90, check=True)
        report = json.loads(native.read_text()); assert report['version'] == '10.0.7'
        targets = [fp for fp in report['footprints'] if fp['reference'] == 'H900']
        if action['kind'] == 'remove': assert not targets
        else:
            assert len(targets) == 1
            fp = targets[0]; assert fp['uuid'] == target_uuid and fp['position_nm'] == position
            assert abs(fp['orientation_mdeg'] - angle) <= 1 and fp['side'] == side
            assert fp['library'] == expected_library
            assert len(fp['pads']) == (0 if logo else 1)
            assert all(pad['number'] == '' and pad['net'] == '' and pad['attribute'] == 'np_thru_hole' for pad in fp['pads'])
            if fp['pads']:
                diameter = 4300000 if fp['library'] == replacement and replacement != hole else 3200000
                assert fp['pads'][0]['drill_nm'] == [diameter, diameter]
        drc_path = oracle / 'drc.json'
        with (oracle / 'drc.log').open('w') as log:
            result = subprocess.run([CLI, 'pcb', 'drc', '--format', 'json', '--schematic-parity', '--all-track-errors', '--severity-all', '--output', str(drc_path), str(oracle / 'project' / client.board.name)], env=env, stdout=log, stderr=subprocess.STDOUT, timeout=90)
        assert result.returncode == 0, (oracle / 'drc.log').read_text()
        actual = json.loads(drc_path.read_text()); expected = plan['native_preview']['drc_after']['complete_report']['raw_json']
        if isinstance(expected, str): expected = json.loads(expected)
        assert canonical_report(actual) == canonical_report(expected), 'complete independent native DRC differs'
        assert not [p for p in private.rglob('*') if p.is_file()]

    try:
        client.start(); baseline_observed = client.observe()
        client.call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='release', observation_ids=[baseline_observed['observation_id']])))
        assert not any(c['reference'] == 'H900' for c in baseline_observed['components'])
        create = dict(kind='create_symbol', key='mount-create', lib_id='Mechanical:MountingHole', reference='H900', value='Enclosure mounting hole', unit=1,
            transform=dict(position=dict(x_nm=76200000, y_nm=76200000)), flags=dict(in_bom=False, on_board=True, dnp=False))
        observed = source_observe()
        for field, value in [('unit', 0), ('unit', 2), ('body_style', 2)]:
            action = copy.deepcopy(create); action[field] = value
            source_plan(observed, action, ok=False); controls.append('invalid-' + field + '-' + str(value))
            unchanged(initial_files)
        client.call('kicad_schematic_audit', dict(release=dict(observation_ids=[observed['observation_id']])))
        source_uuid = source_publish(create)
        observed = source_observe()
        invalid = [dict(kind='assign_footprint', uuid=source_uuid, footprint='Resistor_SMD:R_0603_1608Metric'),
            dict(kind='assign_footprint', uuid=source_uuid, footprint=hole, pin_pad_map={'1': '1'}),
            dict(kind='assign_footprint', uuid='ffffffff-ffff-4fff-8fff-ffffffffffff', footprint=hole)]
        if not real:
            pinful = next(c['reference'] for c in baseline_observed['components'] if c['pins'])
            resistor = next(unit for unit in observed['units'] if unit['reference'] == pinful)
            invalid.append(dict(kind='assign_footprint', uuid=resistor['symbol_uuid'], footprint=hole))
        baseline = snapshot(client.project)
        for action in invalid:
            source_plan(observed, action, ok=False); unchanged(baseline)
        client.call('kicad_schematic_audit', dict(release=dict(observation_ids=[observed['observation_id']])))
        controls.append('exact-empty-numbered-pad-bijection-and-source-identity')
        source_publish(dict(kind='assign_footprint', uuid=source_uuid, footprint=hole))
        observed = client.observe(); target = component(observed)
        assert target['footprint'] == hole and not any(fp['reference'] == 'H900' for fp in observed['footprints'].values())
        actual = board_publish(observed, dict(kind='add', component_id=target['id'], fields=[], placement=dict(position_nm=position, orientation_mdeg=angle, side=side)))
        before_fp = footprint(actual)
        if not logo:
            source_publish(dict(kind='assign_footprint', uuid=source_uuid, footprint=replacement))
            observed = client.observe(); target = component(observed)
            actual = board_publish(observed, dict(kind='replace', component_id=target['id'], footprint_uuid=target_uuid, fields=[]))
            fp = footprint(actual)
            assert fp['uuid'] == before_fp['uuid'] and fp['path'] == before_fp['path']
            # Changing physical pad cardinality is never implicit, including empty numbered-pad sets.
            source_publish(dict(kind='assign_footprint', uuid=source_uuid, footprint='Symbol:KiCad-Logo2_5mm_SilkScreen'))
            observed = client.observe(); baseline = snapshot(client.project)
            board_plan(observed, dict(kind='replace', component_id=component(observed)['id'], footprint_uuid=target_uuid, fields=[]), ok=False)
            unchanged(baseline); controls.append('physical-pad-cardinality-refused')
            client.call('kicad_inspect_project', dict(schematic_pcb_sync=dict(kind='release', observation_ids=[observed['observation_id']])))
            source_publish(dict(kind='assign_footprint', uuid=source_uuid, footprint=replacement))
        else:
            source_publish(dict(kind='assign_footprint', uuid=source_uuid, footprint=replacement))
            observed = client.observe()
            actual = board_publish(observed, dict(kind='replace', component_id=component(observed)['id'], footprint_uuid=target_uuid, fields=[]))
        if not real:
            source_publish(dict(kind='delete', uuid=source_uuid, object_kind='symbol'))
            observed = client.observe(); assert not any(c['reference'] == 'H900' for c in observed['components'])
            board_publish(observed, dict(kind='remove', footprint_uuid=target_uuid))
            assert root_nodes(client.board.read_bytes()) == original_board_nodes
        final = client.observe(); preserve(final)
        assert snapshot(client.config) == configuration
        changed = snapshot(client.project)
        assert set(changed) == set(initial_files)
        assert {key for key in initial_files if initial_files[key] != changed[key]} <= {client.source.name, client.board.name}
        artifacts = []
        records = []
        for path in client.state.rglob('record.json'):
            record = json.loads(path.read_text()); assert record['state'] == 'committed', record
            records.append(dict(path=str(path), sha256=sha(path), schema_version=record['schema_version']))
            def walk(value):
                if isinstance(value, dict):
                    if {'path', 'sha256', 'bytes'} <= value.keys():
                        artifact = Path(value['path']); assert artifact.is_file() and sha(artifact) == value['sha256'].removeprefix('sha256:')
                        if 'bytes' in value: assert artifact.stat().st_size == value['bytes']
                        artifacts.append(dict(path=str(artifact), sha256=sha(artifact)))
                    for child in value.values(): walk(child)
                elif isinstance(value, list):
                    for child in value: walk(child)
            walk(record)
        assert len(records) == len(steps)
        write(case / 'journals.json', dict(records=records, artifacts=artifacts))
        (case / 'result.kicad_sch').write_bytes(client.source.read_bytes())
        (case / 'result.kicad_pcb').write_bytes(client.board.read_bytes())
        result = dict(passed=True, profile=profile, binary_sha256=sha(binary), actual_source_create_assign_and_native_pcb_add=True,
            pcb_replace=True, source_and_pcb_remove=not real, retained_useful_real_addition=real,
            steps=steps, controls=controls, original_pcb_roots_and_copper_preserved=True, independent_native_and_complete_drc=True,
            journal_count=len(records), artifact_references_rehashed=len(artifacts), original_inputs_unchanged=True,
            real_final_gate_claimed=False, gui_editor_launched=False, product_complete=False)
    finally:
        client.close()
    # Codex can leave its own Node compilation cache; only this task's temporary directory is cleaned.
    for path in client.temporary.iterdir():
        if path.name.startswith('node-compile-cache'):
            assert path.is_dir() and not path.is_symlink(); shutil.rmtree(path)
    assert not [p for p in client.temporary.rglob('*') if p.is_file()]
    assert snapshot(origin) == original_inputs['project'] and snapshot(input_config) == original_inputs['config']
    result['private_roots_empty'] = True
    write(case / 'result.json', result)
    print(json.dumps(result), flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['binary', 'source', 'config', 'output']: parser.add_argument(name, type=Path)
    parser.add_argument('--profile', choices=['front', 'back-oblique', 'legacy-front', 'no-pad', 'real'], default='front')
    parser.add_argument('--position-nm', nargs=2, type=int)
    args = parser.parse_args()
    binary, source, config = [path.resolve(strict=True) for path in [args.binary, args.source, args.config]]
    for folder in [source.parent, config]:
        assert (folder / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    assert source.suffix == '.kicad_sch'
    run = args.output.resolve(); assert not run.is_relative_to(source.parent) and not run.is_relative_to(config)
    run.mkdir(); print(run, flush=True)
    shutil.copy2(__file__, run / 'driver-source.py')
    qualify(binary, run, args.profile, source.parent, source.name, config, args.profile == 'real', args.position_nm)


if __name__ == '__main__': main()
