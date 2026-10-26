"""Cross-check IPC zone/shape identities against the native live-board serializer."""
from pathlib import Path
import json
import sys
import pcbnew

output = Path(sys.argv[1])
observed = json.loads((output / 'native.json').read_text())
board = pcbnew.LoadBoard(str(output / 'live.kicad_pcb'))
zones = {str(z.m_Uuid.AsString()): z for z in board.Zones()}
assert set(zones) == {z['id'] for z in observed['zones']}
native_shapes = {s['id'] for s in observed['shapes']}
saved_shapes = {str(s.m_Uuid.AsString()) for s in board.GetDrawings() if isinstance(s, pcbnew.PCB_SHAPE)}
assert native_shapes == saved_shapes, (native_shapes-saved_shapes, saved_shapes-native_shapes)
def native_layer(value):
    if value == 3:
        return pcbnew.F_Cu
    if value == 34:
        return pcbnew.B_Cu
    if 4 <= value <= 33:
        return getattr(pcbnew, f"In{value-3}_Cu")
    raise AssertionError(f"unexpected non-copper zone layer: {value}")

locked = []
filled = 0
for zone in observed['zones']:
    decoded = zones[zone['id']]
    assert sorted(native_layer(layer) for layer in zone['layers']) == sorted(int(layer) for layer in decoded.GetLayerSet().Seq())
    assert zone['filled'] == bool(decoded.IsFilled())
    assert zone['text_locked'] == bool(decoded.IsLocked())
    assert zone['outline_polygons'] == decoded.Outline().OutlineCount()
    if decoded.IsLocked():
        locked.append(dict(id=zone['id'], native_locked=zone['native_locked'], live_text_locked=True))
    for layer in zone['filled_layers']:
        polygons = decoded.GetFilledPolysList(native_layer(layer['layer']))
        assert layer['polygons'] == polygons.OutlineCount(), (zone['id'], layer)
        assert layer['nodes'] == polygons.TotalVertices(), (zone['id'], layer, polygons.TotalVertices())
        filled += layer['polygons']
result = dict(passed=True, native_text_zone_ids_match=True, native_text_shape_ids_match=True,
    zone_layers_and_fill_polygons_match=True, all_live_text_locks_match=True, zone_count=len(zones), shape_count=len(native_shapes),
    filled_polygons=filled, locked_zones=locked, scope='read-only native protocol evidence; not zone editing, refill or MCP acceptance')
(output / 'result.json').write_text(json.dumps(result,indent=2))
print(json.dumps(result,indent=2))
