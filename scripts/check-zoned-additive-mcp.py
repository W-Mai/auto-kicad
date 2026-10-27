"""Exercise original additive operation families with native whole-board refill over stdio."""
import copy
import hashlib
import json
import os
from pathlib import Path
import sys
from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
records, stages = [], []
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / 'state')), log=str(output / 'stdio.log'))

def call(name, arguments, reject=False):
    try:
        raw = rpc.call('tools/call', dict(name=name, arguments=arguments), timeout=600)
    except RuntimeError as error:
        raw = dict(rpc_error=str(error))
    records.append(dict(name=name, arguments=arguments, response=raw))
    (output / 'mcp.json').write_text(json.dumps(records, indent=2))
    failed = raw.get('isError') or 'rpc_error' in raw or not raw.get('structuredContent', {}).get('ok')
    if reject:
        assert failed, raw
        return raw
    assert not failed, raw
    return raw['structuredContent']['data']

def params(plan, label):
    return dict(plan_id=plan['plan_id'], plan_hash=plan['plan_hash'], request_id=label)

def inspect():
    return call('kicad_inspect_project', dict(endpoint=endpoint, geometry=True))

def make_request(phase, base):
    snap = base['pcb']['snapshot']
    footprints = {f['reference']: f for f in snap['native_footprints']['placements']}
    net = next(t['net'] for t in snap['native_routing']['tracks'] if t['net']['name'] == 'GND')
    net = {k: net[k] for k in ['net_id', 'name']}
    def pad(ref):
        f = footprints[ref]
        return dict(pad_id=f['pads'][0]['pad_id'], footprint_placement_id=f['placement_id'])
    def leg(layer, points):
        return dict(layer=layer, width_nm=250000, points=[dict(x_nm=x*1000000, y_nm=y*1000000) for x,y in points])
    def transition(name, x, y):
        return dict(via_id=name, position=dict(x_nm=x*1000000, y_nm=y*1000000), pad_diameter_nm=600000, drill_diameter_nm=300000)
    via = dict(source_uuid=f'{phase}-via', x_nm=55000000, y_nm=25000000 if phase == 'via' else 30000000, pad_diameter_nm=600000, drill_diameter_nm=300000, net=net)
    track = dict(source_uuid=f'{phase}-track', layer='B.Cu', start_x_nm=50000000, start_y_nm=20000000, end_x_nm=60000000, end_y_nm=20000000, width_nm=250000, start_pad=pad('TP1'), end_pad=pad('TP4'), net=net)
    route = dict(route_id=f'{phase}-route', observed_revision=base['pcb']['revision'], net=net, start=dict(kind='pad', **pad('TP2')), end=dict(kind='pad', **pad('TP3')), legs=[leg('F.Cu', [(20,40),(20,45),(25,45)]), leg('In1.Cu', [(25,45),(35,45)]), leg('F.Cu', [(35,45),(40,45),(40,40)])], vias=[transition('transition-a',25,45), transition('transition-b',35,45)])
    if phase == 'mixed':
        power = next(t['net'] for t in snap['native_routing']['tracks'] if t['net']['name'] == 'VCC')
        route = dict(route_id='power-route', observed_revision=base['pcb']['revision'], net={k:power[k] for k in ['net_id','name']}, start=dict(kind='track_end', source_uuid='40000000-0000-4000-8000-000000000002', end='end'), end=dict(kind='via', source_uuid='50000000-0000-4000-8000-000000000003'), legs=[leg('In1.Cu', [(35,10),(45,10)])], vias=[])
        track.update(layer='F.Cu', start_x_nm=20000000, start_y_nm=40000000, end_x_nm=40000000, end_y_nm=40000000, start_pad=pad('TP2'), end_pad=pad('TP3'))
        payload = dict(batch=[dict(route=route), dict(track=track), dict(via=via), dict(segments=[dict(graphic_id='mixed-drawing', start=dict(x=10000000,y=60000000,unit='Nanometres',space='Board'), end=dict(x=15000000,y=60000000,unit='Nanometres',space='Board'), width_nm=100000, layer='DwgsUser')])])
    else:
        payload = {phase: {'route':route, 'track':track, 'via':via}[phase]}
    return dict(endpoint=endpoint, source_run_id='zoned-additive-acceptance', request_id=f'{phase}-plan', **payload)

def run_phase(phase):
    before = inspect()
    assert before['geometry']['status'] == 'observed'
    request = make_request(phase, before)
    saved = board.read_bytes()
    planned = call('kicad_plan_change', request)
    evidence = planned['geometry_evidence']
    kinds = evidence['additions']['kinds']
    expected = {'route':['AddCopperRoute'], 'track':['AddStraightTrack'], 'via':['AddThroughVia'], 'mixed':['AddCopperRoute','AddStraightTrack','AddThroughVia','AddBoardGraphicSegments']}[phase]
    assert [kind['kind'] for kind in kinds] == expected, kinds
    assert evidence['context']['zones'] and evidence['affected_net_ids']
    preview = call('kicad_preview_change', params(planned, f'{phase}-preview'))
    assert preview['state'] == 'awaiting_approval', preview
    validation = preview['shadow_validation']
    assert validation['shadow_cleaned'] and validation['source_unchanged'] and validation['normalized_snapshot_matches']
    assert board.read_bytes() == saved
    approval = dict(params(planned, f'{phase}-commit'), **{k:preview[k] for k in ['approval_hash','validation_evidence_hash']})
    call('kicad_commit_change', dict(approval, approval_hash='invalid'), reject=True)
    assert board.read_bytes() == saved
    committed = call('kicad_commit_change', approval)
    assert committed['state'] == 'committed', committed
    result = committed['outcome']['geometry']
    candidate = validation['geometry']
    assert result['candidate']['before'] == candidate['candidate']['before']
    assert result['candidate']['after'] == candidate['candidate']['after']
    allocation = evidence['additions']['identities']
    for value in [candidate, result]:
        for family in ['track', 'via']:
            creations = [c for c in value['candidate']['creations'] if c['kind'] == family]
            assert [c['native_uuid'] for c in creations] == allocation[family+'s']
            assert [c['logical_id'] for c in creations] == [str(i) for i in range(len(creations))]
    for id_ in allocation['tracks'] + allocation['vias']:
        assert result['creation_map'][id_] == id_

    assert not result['delta']['introduced']
    if os.environ.get('AUTO_KICAD_ACCEPTANCE_REQUIRE_GENERATED') == '1':
        for value in [candidate, result]:
            assert value['candidate']['generated_after']['identities'], 'expected actual generated copper'
            private = value['after_drc_input']
            assert private and private['connectivity'], 'expected private DRC connectivity proof'
            assert private['connectivity'] == value['after_connectivity']['drc_input']
            assert private['snapshot']['snapshot_sha256'] == private['connectivity']['native']['board_sha256']

    assert result['after']['normalized']['unconnected_items'] <= result['baseline']['normalized']['unconnected_items']
    for value in [candidate, result]:
        for prefix in ['baseline','after']:
            proof = value[prefix+'_connectivity']
            assert proof and proof['report_digest']
            assert proof['live']['closed']['native']['endpoints'] == proof['drc_input']['native']['endpoints']
            assert proof['live']['closed']['native']['components'] == proof['drc_input']['native']['components']
    assert result['baseline_connectivity']['live']['closed']['native']['board_sha256'] == hashlib.sha256(saved).hexdigest()
    assert result['after_connectivity']['live']['closed']['native']['board_sha256'] == hashlib.sha256(board.read_bytes()).hexdigest()
    call('kicad_commit_change', approval, reject=True)
    journals = [json.loads(p.read_text()) for p in (output/'state').rglob('record.json')]
    journal = next(r for r in journals if r['plan_hash'] == planned['plan_hash'])
    assert journal['state'] == 'committed' and journal['outcome']['geometry'] == result
    assert not (board.parent/'.auto-kicad-geometry-pending').exists()
    after = inspect()['pcb']['snapshot']
    physical = evidence['additions']['batch']
    for container, field, native in [('native_routing','tracks','tracks'), ('native_routing','vias','vias'), ('board','graphics','graphics')]:
        assert len(after[container][field]) - len(before['pcb']['snapshot'][container][field]) == len(physical[native])
    stages.append(dict(family=phase, plan_hash=planned['plan_hash'], result=result, preview=validation, exact_counts=True, durable_evidence=True))
    (output/'result.json').write_text(json.dumps(dict(production_stdio=True, stages=stages, complete=False), indent=2))

def verify_rule_drift():
    request = make_request('via', inspect())
    request['request_id'] = 'rule-drift-plan'
    request['via']['source_uuid'] = 'rule-drift-via'
    request['via']['x_nm'] = 57000000
    saved = board.read_bytes()
    planned = call('kicad_plan_change', request)
    preview = call('kicad_preview_change', params(planned, 'rule-drift-preview'))
    assert preview['state'] == 'awaiting_approval'
    approval = dict(params(planned, 'rule-drift-commit'), **{k:preview[k] for k in ['approval_hash','validation_evidence_hash']})
    custom = board.with_suffix('.kicad_dru')
    original = custom.read_bytes() if custom.exists() else None
    try:
        custom.write_bytes((original or b'(version 1)\n') + b'\n# acceptance-only changed rule input\n')
        rejected = call('kicad_commit_change', approval, reject=True)
        assert 'source files changed since planning' in json.dumps(rejected)
    finally:
        if original is None:
            custom.unlink()
        else:
            custom.write_bytes(original)
    call('kicad_commit_change', approval, reject=True)
    call('kicad_preview_change', params(planned, 'rule-drift-repreview'), reject=True)
    assert board.read_bytes() == saved
    return dict(custom_rule_drift_rejected=True, restored_input_cannot_replay=True, source_unchanged=True)

try:
    rpc.call('initialize', dict(protocolVersion='2025-03-26', capabilities={}, clientInfo=dict(name='zoned-additive-acceptance', version='1')))
    rpc.send(dict(jsonrpc='2.0', method='notifications/initialized'))
    (output/'binary.json').write_text(json.dumps(dict(path=binary, sha256=hashlib.sha256(Path(binary).read_bytes()).hexdigest()), indent=2))
    original = board.read_bytes()
    request = make_request('route', inspect())
    invalid = copy.deepcopy(request)
    invalid['route']['observed_revision'] = 'sha256:'+'0'*64
    call('kicad_plan_change', invalid, reject=True)
    invalid = copy.deepcopy(request)
    invalid['route']['legs'][0]['width_nm'] = 100000
    call('kicad_plan_change', invalid, reject=True)
    assert board.read_bytes() == original
    for phase in ['route','track','via','mixed']:
        run_phase(phase)
    drift = verify_rule_drift()
    (output/'result.json').write_text(json.dumps(dict(production_stdio=True, stages=stages, drift=drift, complete=True, scope='Four additive operation families on a populated synthetic zoned board'), indent=2))
finally:
    rpc.close()
