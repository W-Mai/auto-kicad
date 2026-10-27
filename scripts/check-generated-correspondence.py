"""Independently read candidate snapshots and verify native UUID binding and complete DRC mapping."""
from pathlib import Path
import hashlib
import json
import re
import sys
import pcbnew

root=Path(sys.argv[1]).resolve()
acceptance=json.loads((root/'acceptance.json').read_text())
assert acceptance['passed']
mapping=acceptance['generated_map']
records=[]
def content(path):
    text=path.read_text()
    pattern=r'(?ms)^\t\(zone\n.*?^\t\)'
    return sorted(re.findall(pattern,text)),re.sub(pattern,'',text)
for name in ['preview','source']:
    directory=root/name
    metadata=json.loads((directory/'snapshot.json').read_text())
    bound=directory/'bound.kicad_pcb'
    saved=directory/'result.kicad_pcb'
    inputs={p:p.read_bytes() for p in [bound,saved]}
    assert hashlib.sha256(inputs[saved]).hexdigest()==metadata['input_sha256']
    assert hashlib.sha256(inputs[bound]).hexdigest()==metadata['snapshot_sha256']
    identities={i['native_uuid'] for i in metadata['identities']}
    assert len(identities)==10
    for label,path in [('bound',bound),('saved',saved)]:
        board=pcbnew.LoadBoard(str(path))
        zones=list(board.Zones())
        if label=='bound':
            assert {str(z.m_Uuid.AsString()) for z in zones if z.IsTeardropArea()}==identities
        target=next(z for z in zones if str(z.m_Uuid.AsString())=='80000000-0000-4000-8000-000000000001')
        assert target.GetLocalClearance()==250000
        pcbnew.SaveBoard(str(directory/f'native-{label}.kicad_pcb'),board)
    assert content(directory/'native-bound.kicad_pcb')==content(directory/'native-saved.kicad_pcb')
    assert all(p.read_bytes()==value for p,value in inputs.items())
    records.append(dict(candidate=name,bound_uuids=sorted(identities),native_serialization_equal_except_zone_order=True,clearance_nm=250000,inputs_unchanged=True))
def report(name,number,ids):
    value=json.loads((root/name/f'drc-{number}.json').read_text())
    value.pop('source')
    value.pop('date')
    for field in ['violations','unconnected_items','schematic_parity']:
        for violation in value[field]:
            for item in violation['items']:
                if item.get('uuid') in ids:
                    item['uuid']=ids[item['uuid']]
            violation['items'].sort(key=lambda o:json.dumps(o,sort_keys=True))
        value[field].sort(key=lambda o:json.dumps(o,sort_keys=True))
    return value
assert report('preview',0,mapping)==report('source',0,{})
assert report('preview',0,{})!=report('source',0,{})
for name in ['preview','source']:
    assert report(name,0,{})==report(name,1,{})
typed_verified=False
if acceptance.get('typed_geometry_candidate_verified'):
    candidates=[]
    fields=['native_uuid','correspondence_key','kind','layer','net',
            'text_definition_digest','text_content_digest','native_definition_digest','native_content_digest']
    for name in ['preview','source']:
        candidate=json.loads((root/name/'typed-candidate.json').read_text())
        for stage in ['before','after']:
            items=candidate['generated_'+stage]['identities']
            assert len(items)==10 and all(set(item)==set(fields) for item in items)
            projected=[{key:('' if key=='native_uuid' else item[key]) for key in fields} for item in items]
            digest=hashlib.sha256(json.dumps(projected,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()
            assert candidate[stage]['generated_digest']==digest
        observed=json.loads((root/name/'inspection.json').read_text())
        ids={i['native_uuid'] for i in observed['generated']['identities']}
        zones=[z for z in observed['zones'] if z['source_uuid'] in ids]
        assert len(zones)==10 and all(not z['editable_solid'] and z['refillable'] for z in zones)
        candidates.append(candidate)
    for key in ['before','after','intent_digest','creations']:
        assert candidates[0][key]==candidates[1][key]
    assert not candidates[0]['creations']
    left=candidates[0]['generated_after']['identities']
    right=candidates[1]['generated_after']['identities']
    assert len(left)==len(right)
    assert {a['native_uuid']:b['native_uuid'] for a,b in zip(left,right)}==mapping
    typed_verified=True
result=dict(passed=True,typed_candidates_verified=typed_verified,read_only_inputs=True,records=records,full_raw_drc_mapping_equal=True,unmapped_drc_differs=True,repeated_reports_equal=True)
(root/'independent-native.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(dict(passed=True,candidates=2,generated_per_candidate=10)))
