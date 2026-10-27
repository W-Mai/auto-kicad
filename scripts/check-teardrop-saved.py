"""Independently verify generated native zone boundaries in retained saved boards."""
import hashlib
import json
from pathlib import Path
import sys
import pcbnew

root=Path(sys.argv[1]).resolve()
observed=json.loads((root/'observations.json').read_text())
assert observed['kicad_version']=='10.0.7'

def canonical(points):
    points=tuple(map(tuple,points))
    reverse=tuple(reversed(points))
    return min(seq[i:]+seq[:i] for seq in [points,reverse] for i in range(len(seq)))

records=[]
for stage in observed['stages']:
    path=root/f'stage-{stage["stage"]}.kicad_pcb'
    before=path.read_bytes()
    board=pcbnew.LoadBoard(str(path))
    expected={}
    for zone in stage['zones']:
        if zone['native_type']!=4: continue
        data=zone['independent_fields']
        key=(data['layer_name'],data['net'],data['priority'],canonical(data['points']))
        assert key not in expected
        expected[key]=zone['id']
    actual=set()
    for zone in board.Zones():
        if not zone.IsTeardropArea(): continue
        outline=zone.Outline()
        assert outline.OutlineCount()==1 and outline.HoleCount(0)==0
        line=outline.COutline(0)
        assert line.ArcCount()==0 and line.IsClosed()
        pts=[(line.CPoint(i).x,line.CPoint(i).y) for i in range(line.PointCount())]
        key=(board.GetLayerName(zone.GetLayer()),zone.GetNetname(),zone.GetAssignedPriority(),canonical(pts))
        assert key not in actual and key in expected
        actual.add(key)
    assert actual==set(expected) and len(actual)==4
    assert sorted(k[0] for k in actual)==['F.Cu','F.Cu','F.Mask','F.Mask']
    assert before==path.read_bytes()
    records.append(dict(stage=stage['stage'],generated_count=len(actual),sha256=hashlib.sha256(before).hexdigest(),native_identity_map=[dict(native_uuid=expected[k],layer=k[0],net=k[1],priority=k[2],point_count=len(k[3])) for k in sorted(actual)]))
(root/'independent-saved.json').write_text(json.dumps(dict(passed=True,read_only=True,records=records),indent=2)+'\n')
print(json.dumps(dict(passed=True,read_only=True,saved_boards=len(records),generated_per_board=4)))
