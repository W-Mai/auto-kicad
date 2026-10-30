#!/usr/bin/env python3
"""Qualify exact placement removal while retaining shared and orphan child files."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import xml.etree.ElementTree as ET

from schematic_general_cases import reviewed
from stdio_rpc import Rpc

spec=importlib.util.spec_from_file_location('sheet_helpers',Path(__file__).with_name('check-schematic-sheets.py'))
helpers=importlib.util.module_from_spec(spec);spec.loader.exec_module(helpers)
snapshot=helpers.native.snapshot


def delta(planned):
    old,new=planned['native_preview']['connectivity_before'],planned['native_preview']['connectivity_after']
    def pins(state):
        return {json.dumps(p,sort_keys=True):p for p in state['isolated_no_connect']+[p for n in state['nets'] for p in n['pins']]}
    old_pins,new_pins=pins(old),pins(new)
    result=reviewed(planned,{'removed_nets':[n for n in old['nets'] if n not in new['nets']], 'added_nets':[n for n in new['nets'] if n not in old['nets']],
        'removed_isolated':[p for p in old['isolated_no_connect'] if p not in new['isolated_no_connect']], 'added_isolated':[p for p in new['isolated_no_connect'] if p not in old['isolated_no_connect']],
        'removed_pins':[old_pins[k] for k in old_pins.keys()-new_pins.keys()], 'added_pins':[new_pins[k] for k in new_pins.keys()-old_pins.keys()]})
    for key in ('removed_sheet_headers','added_sheet_headers'):result['erc'][key]=planned['native_preview']['erc_delta'][key]
    return result


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('binary','source','config','output'):parser.add_argument(name,type=Path)
    parser.add_argument('--file');parser.add_argument('--shared',action='store_true')
    args=parser.parse_args()
    binary,source,config=[p.resolve(strict=True) for p in (args.binary,args.source,args.config)]
    project,file=source.parent,args.file or source.name
    for p in (project,config):assert (p/'.auto-kicad-audit-owned').read_text()=='schematic-audit-acceptance\n'
    args.output.mkdir();output=args.output.resolve(strict=True)
    temporary=output/'runtime-tmp';temporary.mkdir()
    oracle_config=output/'oracle-config';shutil.copytree(config,oracle_config)
    oracle_tmp=output/'oracle-tmp';oracle_tmp.mkdir()
    env=dict(os.environ,KICAD_CONFIG_HOME=str(config),AUTO_KICAD_STATE_DIR=str(output/'state'),TMPDIR=str(temporary),PYTHONDONTWRITEBYTECODE='1')
    oracle_env=dict(env,KICAD_CONFIG_HOME=str(oracle_config),TMPDIR=str(oracle_tmp))
    configuration=snapshot(config);transcript=[];cases=[]
    rpc=Rpc([str(binary)],env=env,log=output/'server.log')
    def call(name,request,ok=True):
        response=rpc.call('tools/call',{'name':name,'arguments':request},timeout=600)
        assert not response.get('isError'),response
        value=response['structuredContent'];transcript.append({'tool':name,'request':request,'response':value})
        (output/'transcript.json').write_text(json.dumps(transcript,indent=2)+'\n')
        assert value['ok']==ok,value
        return value['data']
    def observe():return call('kicad_schematic_audit',{'observe':{'project_root':str(project),'root_schematic':str(source)}})
    def release(observed=None,planned=None):
        call('kicad_schematic_audit',{'release':{'observation_ids':[observed['observation_id']] if observed else [],'plan_ids':[planned['plan_id']] if planned else []}})
    def edits(ids):return [{'file':file,'scope':{'kind':'file_all_instances'},'action':{'kind':'remove_sheet','uuid':uuid}} for uuid in ids]
    def plan(observed,requested,ok=True):
        before=snapshot(project)
        value=call('kicad_plan_schematic_change',{'project_root':str(project),'root_schematic':str(source),'source_run_id':'sheet-removal-acceptance','request_id':'case-'+str(len(transcript)),
            'general':{'schema_version':1,'observation_id':observed['observation_id'],'observed_revision':observed['revision'],'edits':requested}},ok)
        assert before==snapshot(project) and snapshot(config)==configuration
        return value
    def native(name,planned,observed):
        directory=output/name;directory.mkdir();before=snapshot(project)
        cli='/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli'
        for kind,command in [('netlist',['sch','export','netlist','--format','kicadxml','-o',str(directory/'saved.xml')]),('erc',['sch','erc','--format','json','--severity-all','-o',str(directory/'erc.json')])]:
            result=subprocess.run([cli,*command,str(source)],env=oracle_env,capture_output=True,timeout=120)
            (directory/(kind+'.stdout')).write_bytes(result.stdout);(directory/(kind+'.stderr')).write_bytes(result.stderr)
            assert result.returncode==0,(kind,result.stderr)
        assert snapshot(project)==before
        xml=ET.parse(directory/'saved.xml').getroot()
        actual=sorted((n.attrib['name'],sorted((p.attrib['ref'],p.attrib['pin']) for p in n.findall('node'))) for n in xml.findall('./nets/net') if n.findall('node'))
        refs={(u['file'],u['sheet_path'],u['symbol_uuid']):u['reference'] for u in observed['units']}
        expected=[]
        for net in planned['native_preview']['connectivity_after']['nets']:
            nodes=[]
            for pin in net['pins']:
                if pin['kind']!='component':continue
                references={refs[(u['file'],u['sheet_path'],u['symbol_uuid'])] for u in pin['units']};assert len(references)==1
                nodes.append((references.pop(),pin['number']))
            if nodes:expected.append((net['name'],sorted(nodes)))
        assert sorted(expected)==actual
        assert helpers.erc_identity((directory/'erc.json').read_text())==helpers.erc_identity(planned['native_preview']['erc_after']['raw_json'])
        return len(xml.findall('./design/sheet'))
    def publish(name,ids):
        print('START',name,flush=True)
        observed=observe();before=snapshot(project);before_text=(project/file).read_text()
        planned=plan(observed,edits(ids));expected=delta(planned)
        parents=sorted({u['sheet_path'] for u in observed['units'] if u['file']==file})
        assert len(parents)==(4 if args.shared else 1)
        prefixes=[parent+'/'+uuid for parent in parents for uuid in ids]
        assert {c['uuid'] for c in planned['changes']}==set(ids)
        assert all(sorted(c['instance_paths'])==parents for c in planned['changes'])
        assert not expected['connectivity']['added_pins'] and not expected['connectivity']['added_isolated']
        for pin in expected['connectivity']['removed_pins']:
            units=pin.get('units',[pin.get('source')])
            assert all(any(u['sheet_path']==prefix or u['sheet_path'].startswith(prefix+'/') for prefix in prefixes) for u in units)
        assert not expected['removed_markers'] and not expected['added_markers']
        identity={'plan_id':planned['plan_id'],'plan_hash':planned['plan_hash']}
        call('kicad_preview_schematic_change',identity,False)
        wrong=json.loads(json.dumps(expected));wrong['erc']['removed_sheet_headers'].pop()
        call('kicad_preview_schematic_change',dict(identity,expectations=wrong),False)
        approved=call('kicad_preview_schematic_change',dict(identity,expectations=expected))
        call('kicad_commit_schematic_change',dict(identity,approval_hash='sha256:'+'0'*64),False)
        assert snapshot(project)==before
        result=call('kicad_commit_schematic_change',dict(identity,approval_hash=approved['approval_hash']));assert result['state']=='committed'
        call('kicad_commit_schematic_change',dict(identity,approval_hash=approved['approval_hash']),False)
        after=snapshot(project);assert before.keys()==after.keys() and {p for p in before if before[p]!=after[p]}=={file}
        before_nodes=list(helpers.native.children(before_text));after_nodes=list(helpers.native.children((project/file).read_text()))
        retained=[n for n in before_nodes if not (helpers.tag(n)=='sheet' and helpers.uid(n) in ids)]
        assert Counter(retained)==Counter(after_nodes)
        sheets=native(name,planned,observed)
        release(observed,planned);cases.append({'name':name,'committed':True,'native_sheets':sheets,'child_files_and_annotations_unchanged':True,'independent_saved_native':True})
        print('PASS',name,sheets,flush=True)
    try:
        rpc.call('initialize',{'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'sheet-removal-acceptance','version':'1'}})
        rpc.send({'jsonrpc':'2.0','method':'notifications/initialized'})
        observed=observe();targets=[o for o in observed['objects'] if o['file']==file and o['kind']=='sheet'];assert len(targets)==(1 if args.shared else 4)
        ids=[o['uuid'] for o in targets]
        plan(observed,edits(['11111111-1111-4111-8111-111111111111']),False)
        plan(observed,edits([ids[0],ids[0]]),False)
        if args.shared:
            request=edits(ids);request[0]['scope']={'kind':'single_instance','path':next(u['sheet_path'] for u in observed['units'] if u['file']==file)}
            plan(observed,request,False)
        else:
            unit=next(u for u in observed['units'] if u['file']!=file)
            plan(observed,edits(ids)+[{'file':unit['file'],'scope':{'kind':'file_all_instances'},'action':{'kind':'set_property','uuid':unit['symbol_uuid'],'name':'AK_DETACHED','value':'must-reject'}}],False)
        release(observed)
        cases.append({'name':'invalid-ownership-duplicate-isolation-or-detached-edit-rejected','source_unchanged':True})
        if not args.shared:publish('remove-one-shared-placement',ids[:1]);ids=ids[1:]
        observed=observe();before=snapshot(project);planned=plan(observed,edits(ids));identity={'plan_id':planned['plan_id'],'plan_hash':planned['plan_hash']}
        approved=call('kicad_preview_schematic_change',dict(identity,expectations=delta(planned)))
        child=next(p for p in project.glob('*.kicad_sch') if p.name!=source.name and p.name!=file)
        original=child.read_bytes()
        try:
            child.write_bytes(original+b'\n')
            call('kicad_commit_schematic_change',dict(identity,approval_hash=approved['approval_hash']),False)
        finally:child.write_bytes(original)
        call('kicad_commit_schematic_change',dict(identity,approval_hash=approved['approval_hash']),False)
        assert snapshot(project)==before
        release(observed,planned);cases.append({'name':'orphan-source-drift-permanently-revokes','source_restored':True})
        publish('remove-final-placement-retain-orphan-files',ids)
        final=observe();assert not any(o['file']==file and o['kind']=='sheet' for o in final['objects']);release(final)
    finally:rpc.close()
    assert rpc.process.returncode==0 and snapshot(config)==configuration
    for root in (temporary,oracle_tmp):
        for child in (root/'org.kicad.kicad/instances',root/'org.kicad.kicad'):
            if child.is_dir() and not list(child.iterdir()):child.rmdir()
        assert not list(root.iterdir()),list(root.iterdir())
    result={'passed':True,'production_stdio':True,'server_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'native_version':'10.0.7','shared_parent':args.shared,
        'cases':cases,'configuration_unchanged':True,'private_snapshots_removed':True,'gui_launched':False}
    (output/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))


if __name__=='__main__':main()
