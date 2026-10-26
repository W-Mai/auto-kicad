"""Verify real geometry faults and pending-marker isolation using production stdio."""
import contextlib
import json
import os
from pathlib import Path
import sys
import time
from stdio_rpc import Rpc

mode, endpoint, real_endpoint, board_arg, output_arg, binary, control_arg = sys.argv[1:]
board, output, control = Path(board_arg), Path(output_arg), Path(control_arg)
transcript = []
children = []

def start(state):
    rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(state)), log=str(output / f'stdio-{len(children)}.log'))
    children.append(rpc)
    rpc.call('initialize', dict(protocolVersion='2025-03-26', capabilities={}, clientInfo=dict(name='geometry-recovery', version='1')))
    rpc.send(dict(jsonrpc='2.0', method='notifications/initialized'))
    return rpc

def call(rpc, name, arguments, reject=False):
    try:
        result = rpc.call('tools/call', dict(name=name, arguments=arguments), timeout=600)
    except RuntimeError as error:
        result = dict(rpc_error=str(error))
    transcript.append(dict(tool=name, arguments=arguments, response=result))
    (output / 'mcp.json').write_text(json.dumps(transcript, indent=2))
    ok = result.get('structuredContent', {}).get('ok', False) and not result.get('isError')
    assert bool(ok) != reject, result
    return result.get('structuredContent', {}).get('data', result)

def prepare(rpc, ep, label):
    observed = call(rpc, 'kicad_inspect_project', dict(endpoint=ep, geometry=True))
    geometry = observed['geometry']['observation']
    zone = next(z for z in geometry['zones'] if z.get('editable_solid') and z['lock_state'] == 'Unlocked')
    request = dict(observed_revision=geometry['observed_revision'], outline=None,
                   zones=[dict(kind='update', source_uuid=zone['source_uuid'], patch=dict(clearance_nm=zone['definition']['clearance_nm'] + 10000))], refill='all')
    if label in ['state', 'other-state']:
        # A drawing plan reaches the writer gate without refilling a partially saved zone.
        operation = dict(segments=[dict(graphic_id='pending-marker-probe',
                         start=dict(x=1000000, y=1000000, unit="Nanometres", space="Board"), end=dict(x=2000000, y=1000000, unit="Nanometres", space="Board"),
                         width_nm=100000, layer='DwgsUser')])
    else:
        operation = dict(geometry=request)
    plan = call(rpc, 'kicad_plan_change', dict(endpoint=ep, source_run_id=label, request_id=label, **operation))
    params = dict(plan_id=plan['plan_id'], plan_hash=plan['plan_hash'], request_id=label)
    preview = call(rpc, 'kicad_preview_change', params)
    assert preview['state'] == 'awaiting_approval', preview
    return dict(params, approval_hash=preview['approval_hash'], validation_evidence_hash=preview['validation_evidence_hash']), request

def await_file(path, timeout=60):
    deadline = time.monotonic() + timeout
    while not path.exists():
        assert time.monotonic() < deadline, f'timed out waiting for {path}'
        time.sleep(0.02)

try:
    original = board.read_bytes()
    rpc = start(output / 'state')
    approval, request = prepare(rpc, endpoint, mode)
    (output / 'request-0.json').write_text(json.dumps(request, indent=2))
    if mode == 'normal':
        committed = call(rpc, 'kicad_commit_change', approval)
        assert committed['state'] == 'committed', committed
        result = committed['outcome']['geometry']
        assert not result['delta']['introduced']
        assert not (board.parent / '.auto-kicad-geometry-pending').exists()
        (output / 'stage-0.kicad_pcb').write_bytes(board.read_bytes())
        (output / 'result.json').write_text(json.dumps(dict(passed=True, kicad_version="10.0.7", stages=[dict(stage=0, native_creation_ids={}, outline_creation_ids=[], candidate=result['candidate'])], outcome=committed), indent=2))
    else:
        (control / 'arm').write_text(mode)
        if mode == 'interrupt_before_write':
            rpc.send(dict(jsonrpc='2.0', id=900000, method='tools/call', params=dict(name='kicad_commit_change', arguments=approval)))
            await_file(control / 'fault.json')
            rpc.process.kill()
            rpc.process.wait(timeout=10)
            (control / 'release').touch()
            assert board.read_bytes() == original
        else:
            unknown = call(rpc, 'kicad_commit_change', approval, reject=True)
            assert unknown['state'] == 'unknown', unknown
            assert board.read_bytes() != original
            call(rpc, 'kicad_commit_change', approval, reject=True)
        fault = json.loads((control / 'fault.json').read_text())
        assert fault['native_success'] and fault['mode'] == mode
        journals = list((output / 'state').rglob('record.json'))
        assert len(journals) == 1
        record = json.loads(journals[0].read_text())
        assert record['state'] == ('in_flight' if mode == 'interrupt_before_write' else 'unknown'), record
        assert bool(record['post_attempt']) == (mode != 'interrupt_before_write')
        marker = board.parent / '.auto-kicad-geometry-pending'
        assert json.loads(marker.read_text())['attempt_id'] == record['attempt_id']
        retained = board.read_bytes()
        (output / 'fault-result.kicad_pcb').write_bytes(retained)
        for state_name in ['state', 'other-state']:
            other = start(output / state_name)
            next_approval, _ = prepare(other, real_endpoint, state_name)
            rejected = call(other, 'kicad_commit_change', next_approval, reject=True)
            assert 'guarded recovery before further project writes' in json.dumps(rejected), rejected
            assert board.read_bytes() == retained and marker.exists()
        closed_rejection = call(other, 'kicad_plan_recovery', dict(project_dir=str(board.parent), attempt_id=record['attempt_id'], action='restore_backups'), reject=True)
        # Use the originating store to exercise the editor-lock refusal, not missing history.
        origin = start(output / 'state')
        closed_rejection = call(origin, 'kicad_plan_recovery', dict(project_dir=str(board.parent), attempt_id=record['attempt_id'], action='restore_backups'), reject=True)
        assert 'editor lock' in json.dumps(closed_rejection), closed_rejection
        commands = [json.loads(line) for line in (control / 'commands.jsonl').read_text().splitlines()]
        assert not [c for c in commands if c['after_fault']], commands[-5:]
        (output / 'result.json').write_text(json.dumps(dict(passed=True, mode=mode, attempt_id=record['attempt_id'], journal_state=record['state'], source_changed=retained != original, same_state_blocked=True, other_state_blocked=True, open_editor_recovery_rejected=True, no_post_fault_ipc=True, fault=fault), indent=2))
finally:
    (output / 'mcp.json').write_text(json.dumps(transcript, indent=2))
    for rpc in children:
        with contextlib.suppress(BrokenPipeError):
            rpc.close()
