"""Generate saved schematic fixtures from installed sources for native acceptance."""
import json
import re
import shutil
import sys
import uuid
from pathlib import Path

SHARED=Path('/Applications/KiCad/KiCad.app/Contents/SharedSupport')
SO='SOIC-8_3.9x4.9mm_P1.27mm'
QFN='QFN-32-1EP_5x5mm_P0.5mm_EP3.65x3.65mm_ThermalVias'
ROOT='11111111-1111-4111-8111-111111111111'
def uid(): return str(uuid.uuid4())
def block(text,start):
    depth=0; quoted=False; escaped=False
    for end in range(start,len(text)):
        c=text[end]
        if escaped: escaped=False; continue
        if c=='\\' and quoted: escaped=True; continue
        if c=='"': quoted=not quoted
        if not quoted:
            if c=='(': depth+=1
            if c==')':
                depth-=1
                if depth==0: return text[start:end+1]
    raise ValueError('unclosed source symbol')
def fixture(root):
    root=Path(root)
    library=(SHARED/'symbols/Timer.kicad_sym').read_text()
    timer=block(library,library.index('(symbol "NE555D"'))
    timer=timer.replace('(symbol "NE555D"','(symbol "Timer:NE555D"',1)
    def generic(name,count):
        pins=''.join(f'(pin passive line (at -5.08 {-n*2.54:g} 0) (length 2.54) (name "P{n}" (effects (font (size 1 1)))) (number "{n}" (effects (font (size 1 1)))))' for n in range(1,count+1))
        return f'(symbol "Acceptance:{name}" (in_bom yes) (on_board yes) (property "Reference" "U" (at 0 0 0) (effects (font (size 1 1)))) (property "Value" "{name}" (at 0 0 0) (effects (font (size 1 1)))) (symbol "{name}_1_1" {pins}))'
    definitions=[timer,generic('QFN33',33),generic('R',2)]
    parts=[('U1','Timer:NE555D','NE555D',f'Package_SO:{SO}',8,30,30),('U2','Timer:NE555D','NE555D',f'LocalIC:{SO}',8,70,30),('U3','Acceptance:QFN33','QFN33',f'LocalQFN:{QFN}',33,110,30),('U4','Timer:NE555D','NE555D',f'Package_SO:{SO}',8,150,30),('U5','Acceptance:QFN33','QFN33',f'Package_DFN_QFN:{QFN}',33,210,30),('R1','Acceptance:R','1k','Resistor_SMD:R_0603_1608Metric',2,180,30)]
    instances=[]
    for ref,lib,value,footprint,count,x,y in parts:
        props=''.join(f'(property "{key}" "{val}" (at {x} {y} 0) (effects (font (size 1 1))))' for key,val in [('Reference',ref),('Value',value),('Footprint',footprint)])
        pins=''.join(f'(pin "{n}" (uuid "{uid()}"))' for n in range(1,count+1))
        instances.append(f'(symbol (lib_id "{lib}") (at {x} {y} 0) (unit 1) (in_bom yes) (on_board yes) (dnp no) (uuid "{uid()}") {props} {pins} (instances (project "board" (path "/{ROOT}" (reference "{ref}") (unit 1)))))')
    # Timer pin 1 is at (0, -10.16) in symbol coordinates; R pin 2 is at (-5.08, -5.08).
    labels=''.join(f'(label "CONNECTED" (at {x:g} {y:g} 0) (effects (font (size 1 1)) (justify left bottom)) (uuid "{uid()}"))' for x,y in [(150,40.16),(174.92,35.08)])
    text=f'(kicad_sch (version 20260306) (generator eeschema) (uuid "{ROOT}") (paper "A4") (lib_symbols {"".join(definitions)}) {"".join(instances)} {labels} (embedded_fonts no))'
    (root/'board.kicad_sch').write_text(text)
    (root/'BoardParts.pretty').mkdir()
    shutil.copyfile(SHARED/f'footprints/Package_SO.pretty/{SO}.kicad_mod',root/f'BoardParts.pretty/{SO}.kicad_mod')
    (root/'QfnParts.pretty').mkdir()
    qfn=(SHARED/f'footprints/Package_DFN_QFN.pretty/{QFN}.kicad_mod').read_text()
    # A distinct fixture library isolates physical-pad handling from unsupported fabrication attributes.
    assert qfn.count('(property pad_prop_heatsink)')==11
    (root/f'QfnParts.pretty/{QFN}.kicad_mod').write_text(qfn.replace('(property pad_prop_heatsink)',''))
    (root/'fp-lib-table').write_text('(fp_lib_table (version 7) (lib (name "LocalIC") (type "KiCad") (uri "${KIPRJMOD}/BoardParts.pretty") (options "") (descr "Acceptance alias")) (lib (name "LocalQFN") (type "KiCad") (uri "${KIPRJMOD}/QfnParts.pretty") (options "") (descr "Physical pad fixture")))')
    print(json.dumps({'fixture':str(root),'symbols':len(parts)}))

from stdio_rpc import Rpc
import os

def check(endpoint,board_arg,output_arg,binary):
    board=Path(board_arg); output=Path(output_arg); records=[]
    rpc=Rpc([binary],env=dict(os.environ,AUTO_KICAD_STATE_DIR=str(output/'state')),log=str(output/'stdio.log'))
    def call(name,args,success=True):
        try:
            response=rpc.call('tools/call',dict(name=name,arguments=args))
        except RuntimeError as error:
            if success: raise
            value=dict(ok=False,error=str(error))
            records.append(dict(tool=name,response=value)); (output/'mcp.json').write_text(json.dumps(records,indent=2)+'\n')
            return value
        value=response.get('structuredContent',response)
        records.append(dict(tool=name,response=value)); (output/'mcp.json').write_text(json.dumps(records,indent=2)+'\n')
        if success: assert not response.get('isError') and value['ok'], value
        return value['data'] if success else value
    def place(ref,nickname,entry,x,y,value):
        return dict(target_id='placed-'+ref,library_nickname=nickname,library_entry_name=entry,x_nm=round(x*1e6),y_nm=round(y*1e6),reference=ref,value=value)
    try:
        rpc.call('initialize',dict(protocolVersion='2025-03-26',capabilities={},clientInfo=dict(name='general-footprint-acceptance',version='1')))
        rpc.send(dict(jsonrpc='2.0',method='notifications/initialized'))
        baseline=call('kicad_validate_project',dict(endpoint=endpoint))
        cases=[('installed',dict(footprint=place('U1','Package_SO',SO,20,20,'NE555D'))),('alias',dict(footprint=place('U2','LocalIC',SO,40,20,'NE555D'))),('qfn',dict(footprint=place('U3','LocalQFN',QFN,55,40,'QFN33'))),('connected',dict(assembly=dict(placements=[place('U4','Package_SO',SO,70,20,'NE555D'),place('R1','Resistor_SMD','R_0603_1608Metric',61.5,18.095,'1k')],connections=[dict(track_id='ic-resistor',start=dict(target_id='placed-U4',pad_number='1'),end=dict(target_id='placed-R1',pad_number='2'),width_nm=200000)])))]
        saved=board.read_bytes()
        unsupported=call('kicad_plan_change',dict(endpoint=endpoint,source_run_id='general-footprints',request_id='unsupported-property',footprint=place('U5','Package_DFN_QFN',QFN,55,60,'QFN33')))
        rejected=call('kicad_preview_change',{key:unsupported[key] for key in ('plan_id','plan_hash')},False)
        assert not rejected['ok'] and 'unsupported pad fabrication property' in json.dumps(rejected)
        assert board.read_bytes()==saved and rejected['data']['shadow_validation']['shadow_cleaned']
        for name,operation in cases:
            placement=operation.get('footprint') or operation['assembly']['placements'][0]
            inspected=call('kicad_inspect_project',dict(endpoint=endpoint,footprint_source={key:placement[key] for key in ('library_nickname','library_entry_name')}))
            assert inspected['footprint_source']['status']=='resolved'
            if name=='alias':
                for index,path in enumerate([board.parent/'fp-lib-table',board.parent/f'BoardParts.pretty/{SO}.kicad_mod']):
                    drift_plan=call('kicad_plan_change',dict(endpoint=endpoint,source_run_id='general-footprints',request_id=f'drift-{index}',**operation))
                    drift_params={key:drift_plan[key] for key in ('plan_id','plan_hash')}
                    drift_preview=call('kicad_preview_change',drift_params)
                    approval=dict(drift_params,**{key:drift_preview[key] for key in ('approval_hash','validation_evidence_hash')})
                    original=path.read_bytes(); unchanged=board.read_bytes()
                    try:
                        path.write_bytes(original+b'\n')
                        refusal=call('kicad_commit_change',approval,False)
                        assert refusal.get('data',{}).get('state')=='rejected' and board.read_bytes()==unchanged,refusal
                    finally: path.write_bytes(original)
                    replay=call('kicad_commit_change',approval,False)
                    assert not replay['ok'] and board.read_bytes()==unchanged,replay
            plan=call('kicad_plan_change',dict(endpoint=endpoint,source_run_id='general-footprints',request_id=name,**operation))
            params={key:plan[key] for key in ('plan_id','plan_hash')}
            saved=board.read_bytes()
            preview=call('kicad_preview_change',params)
            assert board.read_bytes()==saved and preview['shadow_validation']['shadow_cleaned']
            wrong=call('kicad_commit_change',dict(params,approval_hash='0'*64,validation_evidence_hash=preview['validation_evidence_hash']),False)
            assert not wrong['ok'] and board.read_bytes()==saved
            committed=call('kicad_commit_change',dict(params,**{key:preview[key] for key in ('approval_hash','validation_evidence_hash')}))
            assert committed['state']=='committed'
            print(json.dumps(dict(case=name,passed=True)),flush=True)
        after=call('kicad_inspect_project',dict(endpoint=endpoint))['pcb']['snapshot']
        footprints={fp['reference']:fp for fp in after['native_footprints']['placements']}
        assert len(footprints)==5
        assert len(footprints['U1']['pads'])==8 and len(footprints['U2']['pads'])==8
        assert footprints['U2']['library_identifier']['nickname']=='LocalIC'
        assert footprints['U3']['library_identifier']['nickname']=='LocalQFN'
        pads=footprints['U3']['pads']; assert len(pads)==52
        assert sum(p['number']=='' for p in pads)==9 and sum(p['number']=='33' for p in pads)==11
        assert len({p['source_uuid'] for p in pads})==52
        assert all(p['net'] is None for p in pads if not p['number'])
        tracks=after['native_routing']['tracks']; assert len(tracks)==1
        assert tracks[0]['net']['name']=='/CONNECTED'
        final=call('kicad_validate_project',dict(endpoint=endpoint,baseline_id=baseline['baseline_id']))
        assert not final['delta']['introduced'] and final['report']['unconnected_items']<=baseline['report']['unconnected_items']
        (output/'result.json').write_text(json.dumps(dict(passed=True,cases=[name for name,_ in cases],footprints=5,physical_pads=sum(len(fp['pads']) for fp in footprints.values()),qfn_physical_pads=52,qfn_unnumbered=9,qfn_pad33=11,qfn_source='project fixture without fabrication attributes',official_thermal_qfn='unsupported; rejected without mutation',table_and_source_drift_rejected=True,stale_approval_rejected=True,baseline=baseline,final=final),indent=2)+'\n')
    finally: rpc.close()

if __name__=='__main__':
    if sys.argv[1]=='fixture': fixture(sys.argv[2])
    elif sys.argv[1]=='check': check(*sys.argv[2:])
    else: raise SystemExit('expected fixture or check')
