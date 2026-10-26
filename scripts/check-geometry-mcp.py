"""Exercise exclusive geometry planning, actual preview, approval and durable native commit."""
import copy
import json
import os
from pathlib import Path
import shutil
import sys
import time
from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary, requests_arg = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
records=[]
rpc=Rpc([binary],env=dict(os.environ,AUTO_KICAD_STATE_DIR=str(output/'state')),log=str(output/'stdio.log'))

def call(name,args,reject=False):
    started=time.monotonic()
    try:
        raw=rpc.call('tools/call',dict(name=name,arguments=args),timeout=600)
    except RuntimeError as error:
        raw={'rpc_error':str(error)}
    records.append(dict(name=name,arguments=args,response=raw,elapsed_seconds=time.monotonic()-started))
    (output/'mcp.json').write_text(json.dumps(records,indent=2))
    error=raw.get('isError') or 'rpc_error' in raw or not raw.get('structuredContent',{}).get('ok')
    if reject:
        assert error,raw
        return raw
    assert not error,raw
    return raw['structuredContent']['data']

def params(plan,label):
    return dict(plan_id=plan['plan_id'],plan_hash=plan['plan_hash'],request_id=label)

def bind(value,summary,created):
    if isinstance(value,list): return [bind(v,summary,created) for v in value]
    if isinstance(value,dict): return {k:bind(v,summary,created) for k,v in value.items()}
    if value=='${revision}': return summary['observed_revision']
    if value=='${outline_ids}': return [o['source_uuid'] for o in summary['outlines']]
    if value=='${first_solid_zone}': return next(z['source_uuid'] for z in summary['zones'] if z.get('definition') and z['lock_state']=='Unlocked')
    if isinstance(value,str) and value.startswith('${net:'):
        return next(n['net_id'] for n in summary['nets'] if n['name']==value[6:-1])
    if isinstance(value,str) and value.startswith('${created:'): return created[value[10:-1]]
    return value

try:
    rpc.call('initialize',dict(protocolVersion='2025-03-26',capabilities={},clientInfo=dict(name='geometry-mcp-acceptance',version='1')))
    rpc.send(dict(jsonrpc='2.0',method='notifications/initialized'))
    tools=rpc.call('tools/list',{})['tools']
    assert 'geometry' in next(t for t in tools if t['name']=='kicad_plan_change')['inputSchema']['properties']
    stages=[]; created={}
    for index,template in enumerate(json.loads(Path(requests_arg).read_text())):
        inspected=call('kicad_inspect_project',dict(endpoint=endpoint,geometry=True))
        summary=inspected['geometry']['observation']
        geometry=bind(template,summary,created)
        request=dict(endpoint=endpoint,source_run_id='geometry-acceptance',geometry=geometry,request_id=f'geometry-{index}')
        original=board.read_bytes()
        invalid=copy.deepcopy(request); invalid['geometry']['observed_revision']='sha256:'+'0'*64
        call('kicad_plan_change',invalid,reject=True)
        invalid=copy.deepcopy(request); invalid['segments']=[]
        call('kicad_plan_change',invalid,reject=True)
        if index==0:
            drift=call('kicad_plan_change',dict(request,request_id='geometry-rule-drift'))
            drift_preview=call('kicad_preview_change',params(drift,'drift-preview'))
            assert drift_preview['state']=='awaiting_approval',drift_preview
            drift_approval=dict(params(drift,'drift-commit'),**{key:drift_preview[key] for key in ['approval_hash','validation_evidence_hash']})
            rules=board.with_suffix('.kicad_dru')
            rules_before=rules.read_bytes() if rules.exists() else None
            try:
                rules.write_bytes((rules_before or b'(version 1)\n')+b'\n# acceptance evidence drift\n')
                call('kicad_commit_change',drift_approval,reject=True)
            finally:
                if rules_before is None: rules.unlink()
                else: rules.write_bytes(rules_before)
            call('kicad_commit_change',drift_approval,reject=True)
            call('kicad_preview_change',params(drift,'revoked-preview'),reject=True)
            assert board.read_bytes()==original
        planned=call('kicad_plan_change',request)
        preview=call('kicad_preview_change',params(planned,f'preview-{index}'))
        assert preview['state']=='awaiting_approval',preview
        proof=preview['shadow_validation']
        assert proof['source_unchanged'] and proof['shadow_cleaned'] and proof['reopened']
        assert proof['geometry']['candidate']['before']==planned['geometry_evidence']['baseline']
        assert board.read_bytes()==original
        call('kicad_preview_change',params(planned,'duplicate-preview'),reject=True)
        approval=dict(params(planned,f'commit-{index}'),**{key:preview[key] for key in ['approval_hash','validation_evidence_hash']})
        call('kicad_commit_change',dict(approval,approval_hash='invalid'),reject=True)
        assert board.read_bytes()==original
        committed=call('kicad_commit_change',approval)
        assert committed['state']=='committed',committed
        result=committed['outcome']['geometry']
        assert result['candidate']['before']==proof['geometry']['candidate']['before']
        assert result['candidate']['after']==proof['geometry']['candidate']['after']
        assert not result['delta']['introduced']
        assert all(a!=b for a,b in result['creation_map'].items())
        call('kicad_commit_change',approval,reject=True)
        journals=[json.loads(p.read_text()) for p in (output/'state').rglob('record.json')]
        journal=next(r for r in journals if r['plan_hash']==planned['plan_hash'])
        assert journal['state']=='committed',journal
        assert not (board.parent/'.auto-kicad-geometry-pending').exists()
        assert journal['outcome']['geometry']['candidate']==result['candidate']
        assert len(journals)==index+1
        call('kicad_inspect_project',dict(endpoint=endpoint,geometry=True))
        outline_ids=[]
        for identity in result['candidate']['creations']:
            if identity['kind']=='zone': created[identity['logical_id']]=identity['native_uuid']
            else: outline_ids.append(identity['native_uuid'])
        shutil.copyfile(board,output/f'stage-{index}.kicad_pcb')
        (output/f'request-{index}.json').write_text(json.dumps(geometry,indent=2))
        stages.append(dict(stage=index,native_creation_ids=created.copy(),outline_creation_ids=outline_ids,candidate=result['candidate'],drc_delta=result['delta'],creation_map=result['creation_map']))
        (output/'result.json').write_text(json.dumps(dict(kicad_version='10.0.7',production_stdio=True,stages=stages,approval_and_replay_refused=True,duplicate_preview_refused=True,rule_drift_permanently_revokes_approval=True),indent=2))
finally:
    (output/'mcp.json').write_text(json.dumps(records,indent=2))
    rpc.close()
