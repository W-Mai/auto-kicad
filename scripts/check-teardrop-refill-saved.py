"""Read saved and CLI outputs through native KiCad; compare all generated boundaries and fills."""
from pathlib import Path
import hashlib
import json
import sys
import pcbnew

root=Path(sys.argv[1]).resolve()
accepted=json.loads((root/'acceptance.json').read_text())
assert accepted['passed'] and accepted['kicad_version']=='10.0.7'

def ring(line):
    assert line.IsClosed() and line.ArcCount()==0
    pts=tuple((line.CPoint(i).x,line.CPoint(i).y) for i in range(line.PointCount()))
    assert 3<=len(pts)<=1024
    return min(seq[i:]+seq[:i] for seq in [pts,tuple(reversed(pts))] for i in range(len(seq)))

def polygons(poly):
    return tuple(sorted((ring(poly.COutline(i)),tuple(sorted(ring(poly.CHole(i,j)) for j in range(poly.HoleCount(i))))) for i in range(poly.OutlineCount())))

def read(path):
    before=path.read_bytes()
    board=pcbnew.LoadBoard(str(path))
    generated=[]
    for zone in board.Zones():
        if not zone.IsTeardropArea(): continue
        layers=list(zone.GetLayerSet().Seq())
        assert len(layers)==1
        layer=layers[0]
        filled=zone.GetFilledPolysList(layer)
        record=dict(layer=board.GetLayerName(layer),net=zone.GetNetname(),priority=zone.GetAssignedPriority(),locked=bool(zone.IsLocked()),filled=bool(zone.IsFilled()),outline=polygons(zone.Outline()),fill=polygons(filled),islands=tuple(sorted((ring(filled.COutline(i)),bool(zone.IsIsland(layer,i))) for i in range(filled.OutlineCount()))),minimum=zone.GetMinThickness(),clearance=zone.GetLocalClearance(),connection=int(zone.GetPadConnection()),gap=zone.GetThermalReliefGap(),spoke=zone.GetThermalReliefSpokeWidth(),island_mode=int(zone.GetIslandRemovalMode()))
        generated.append(json.dumps(record,sort_keys=True))
    assert before==path.read_bytes()
    assert len(generated)==accepted['expected_generated']
    assert len(set(generated))==len(generated)
    return sorted(generated),hashlib.sha256(before).hexdigest()

records=[]
for stage in accepted['stages']:
    number=stage['stage']
    saved,sha=read(root/f'stage-{number}.kicad_pcb')
    cli,cli_sha=read(root/f'stage-{number}-cli.kicad_pcb')
    assert saved==cli
    summaries=[json.loads(v) for v in saved]
    if accepted['expected_generated']==10:
        assert sorted(v['layer'] for v in summaries)==sorted(['F.Cu']*3+['B.Cu']*2+['F.Mask']*3+['B.Mask']*2)
    records.append(dict(stage=number,saved_sha256=sha,cli_sha256=cli_sha,generated=summaries))
result=dict(passed=True,read_only=True,kicad_version=pcbnew.Version(),records=records)
(root/'independent-saved.json').write_text(json.dumps(result,indent=2)+'\n')
print(json.dumps(dict(passed=True,stages=len(records),generated_per_stage=accepted['expected_generated'])))
