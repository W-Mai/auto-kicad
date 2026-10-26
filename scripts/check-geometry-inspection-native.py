"""Independently compare MCP geometry with the saved native source without writing it."""
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
sys.dont_write_bytecode = True
import pcbnew

source, output = map(Path,sys.argv[1:])
before=source.read_bytes()
summary=json.loads((output/'geometry.json').read_text())
board=pcbnew.LoadBoard(str(source))
zones={str(z.m_Uuid.AsString()):z for z in board.Zones()}
assert set(zones)=={z['source_uuid'] for z in summary['zones']}
spec=importlib.util.spec_from_file_location('zone_intent_reader',Path(__file__).with_name('check-geometry-request.py'))
reader=importlib.util.module_from_spec(spec)
spec.loader.exec_module(reader)
definitions=0
for item in summary['zones']:
    zone=zones[item['source_uuid']]
    assert item['lock_state']==('Locked' if zone.IsLocked() else 'Unlocked')
    assert item['name']==zone.GetZoneName()
    assert (item['net']['name'] if item['net'] else '')==zone.GetNetname()
    assert sorted(item['copper_layers'])==sorted(board.GetLayerName(l) for l in zone.GetLayerSet().Seq())
    assert item['filled']==zone.IsFilled()
    assert item['fill_polygon_count']==sum(zone.GetFilledPolysList(l).OutlineCount() for l in zone.GetLayerSet().Seq())
    if item['definition'] is not None:
        reader.check(board,zone,item['definition'])
        definitions+=1
    else:
        assert item['unsupported_reason'] and not item['editable_solid']
    assert item['definition_digest'].startswith('sha256:')
drawings={str(s.m_Uuid.AsString()):s for s in board.GetDrawings() if s.GetLayer()==pcbnew.Edge_Cuts}
assert set(drawings)=={s['source_uuid'] for s in summary['outlines']}
def point(p): return dict(x_nm=p.x,y_nm=p.y)
for item in summary['outlines']:
    shape=drawings[item['source_uuid']]
    assert item['lock_state']==('Locked' if shape.IsLocked() else 'Unlocked')
    assert item['width_nm']==shape.GetWidth()
    geometry=item['geometry']
    if geometry is None:
        assert item['unsupported_reason']
        continue
    kind=geometry['kind']
    if kind=='segment':
        assert shape.GetShape()==pcbnew.SHAPE_T_SEGMENT
        assert geometry['start']==point(shape.GetStart()) and geometry['end']==point(shape.GetEnd())
    elif kind=='arc':
        assert shape.GetShape()==pcbnew.SHAPE_T_ARC
        assert geometry['start']==point(shape.GetStart()) and geometry['mid']==point(shape.GetArcMid()) and geometry['end']==point(shape.GetEnd())
    elif kind=='circle':
        assert shape.GetShape()==pcbnew.SHAPE_T_CIRCLE
        assert geometry['center']==point(shape.GetCenter()) and geometry['radius_point']==point(shape.GetEnd())
    elif kind=='rectangle':
        assert shape.GetShape()==pcbnew.SHAPE_T_RECTANGLE
        assert geometry['top_left']==point(shape.GetStart()) and geometry['bottom_right']==point(shape.GetEnd())
    elif kind=='polygon':
        assert shape.GetShape()==pcbnew.SHAPE_T_POLY
        poly=shape.GetPolyShape()
        actual=dict(exterior=dict(points=reader.points(poly.COutline(0))),holes=[dict(points=reader.points(poly.CHole(0,i))) for i in range(poly.HoleCount(0))])
        assert geometry['polygon']==actual
    else: raise AssertionError(kind)
mcp=json.loads((output/'mcp.json').read_text())
snapshot=mcp['observed']['structuredContent']['data']['pcb']['snapshot']
opaque=snapshot.get('opaque_graphics',[])
native_opaque={str(s.m_Uuid.AsString()):s for s in board.GetDrawings() if isinstance(s,pcbnew.PCB_SHAPE) and s.GetLayer() in [pcbnew.Edge_Cuts,pcbnew.Dwgs_User] and s.GetShape()!=pcbnew.SHAPE_T_SEGMENT}
assert {g['source_uuid'] for g in opaque}==set(native_opaque)
for item in opaque:
    expected='EdgeCuts' if native_opaque[item['source_uuid']].GetLayer()==pcbnew.Edge_Cuts else 'DwgsUser'
    assert item['layer']==expected and len(item['native_digest'])==64
assert before==source.read_bytes()
record=dict(opaque_graphics=len(opaque),read_only=True,sha256=hashlib.sha256(before).hexdigest(),zones=len(zones),outlines=len(drawings),complete_editable_definitions=definitions,locked_zones=sum(z.IsLocked() for z in zones.values()))
(output/'independent-native.json').write_text(json.dumps(record,indent=2)+'\n')
print(json.dumps(record))
