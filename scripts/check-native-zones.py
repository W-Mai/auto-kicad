"""Independently read saved native zone edits and compare them with the acceptance intent."""
import hashlib
import json
from pathlib import Path
import sys

import pcbnew

output = Path(sys.argv[1]).resolve()
result = json.loads((output / 'result.json').read_text())
assert result['kicad_version'] == '10.0.7'
identifier = result['created_uuid']
exterior = [(10, 30), (35, 30), (35, 60), (25, 60), (25, 50), (20, 50), (20, 60), (10, 60)]
holes = [[(12, 32), (16, 32), (16, 36), (12, 36)], [(27, 40), (31, 40), (31, 44), (27, 44)]]


def points(line):
    assert line.IsClosed()
    assert line.ArcCount() == 0
    return [(line.CPoint(i).x, line.CPoint(i).y) for i in range(line.PointCount())]


records = []
for filename, expected in [
    ('created.kicad_pcb', dict(name='created concave with holes', priority=3, net='GND', layers=[0, 4], clearance=200000, minimum=200000, connection=1, gap=500000, spoke=500000, island=1)),
    ('updated-0.kicad_pcb', dict(name='patched exact zone', priority=7, net='GND', layers=[0, 4], clearance=300000, minimum=200000, connection=1, gap=500000, spoke=500000, island=1)),
    ('updated-1.kicad_pcb', dict(name='patched exact zone', priority=7, net='VCC', layers=[0, 2], clearance=300000, minimum=150000, connection=3, gap=350000, spoke=400000, island=2, area=2000000000000)),
]:
    path = output / filename
    before = path.read_bytes()
    board = pcbnew.LoadBoard(str(path))
    matches = [z for z in board.Zones() if str(z.m_Uuid.AsString()) == identifier]
    assert len(matches) == 1
    zone = matches[0]
    actual = dict(name=zone.GetZoneName(), priority=zone.GetAssignedPriority(), net=zone.GetNetname(), layers=list(zone.GetLayerSet().Seq()), clearance=zone.GetLocalClearance(), minimum=zone.GetMinThickness(), connection=zone.GetPadConnection(), gap=zone.GetThermalReliefGap(), spoke=zone.GetThermalReliefSpokeWidth(), island=zone.GetIslandRemovalMode())
    if 'area' in expected:
        actual['area'] = zone.GetMinIslandArea()
    assert actual == expected, (filename, actual, expected)
    outline = zone.Outline()
    assert outline.OutlineCount() == 1 and outline.HoleCount(0) == 2
    assert points(outline.COutline(0)) == [(x * 1000000, y * 1000000) for x, y in exterior]
    assert [points(outline.CHole(0, i)) for i in range(2)] == [[(x * 1000000, y * 1000000) for x, y in hole] for hole in holes]
    assert path.read_bytes() == before
    records.append(dict(file=filename, sha256=hashlib.sha256(before).hexdigest(), exact_intent=True, exterior_vertices=len(exterior), hole_vertices=[len(h) for h in holes], properties=actual))

(output / 'independent-saved-intent.json').write_text(json.dumps(dict(records=records, read_only=True), indent=2) + '\n')
print(json.dumps(dict(verified_saved_files=len(records), exact_intent=True, read_only=True)))
