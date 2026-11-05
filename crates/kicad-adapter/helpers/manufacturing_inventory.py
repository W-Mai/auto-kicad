"""Read native fabrication layers and complete physical drill records without modifying input files."""
import hashlib
import json
from pathlib import Path
import sys
import wx
app = wx.App(False)
import pcbnew

path = Path(sys.argv[1]).resolve(strict=True)
assert pcbnew.GetBuildVersion() == '10.0.7'
original = path.read_bytes()
assert 0 < len(original) <= 128 * 1024 * 1024
board = pcbnew.LoadBoard(str(path))
layers = [str(pcbnew.LayerName(layer)) for layer in board.GetEnabledLayers().Seq() if pcbnew.IsCopperLayer(layer)]
assert layers and layers[0] == 'F.Cu' and 'B.Cu' in layers
holes = []
for footprint in board.GetFootprints():
    for pad in footprint.Pads():
        drill = pad.GetDrillSize()
        if drill.x <= 0 and drill.y <= 0: continue
        position = pad.GetPosition()
        holes.append(dict(uuid=pad.m_Uuid.AsString(), kind='pad', position_nm=[position.x, position.y], drill_nm=[drill.x, drill.y],
            orientation_mdeg=round(pad.GetOrientation().AsDegrees() * 1000) % 360000,
            plated=pad.GetAttribute() != pcbnew.PAD_ATTRIB_NPTH, layers=['F.Cu', 'B.Cu']))
for item in board.GetTracks():
    if not isinstance(item, pcbnew.PCB_VIA): continue
    position = item.GetPosition(); diameter = item.GetDrillValue()
    holes.append(dict(uuid=item.m_Uuid.AsString(), kind='via', position_nm=[position.x, position.y], drill_nm=[diameter, diameter],
        orientation_mdeg=0, plated=True, layers=[str(pcbnew.LayerName(item.TopLayer())), str(pcbnew.LayerName(item.BottomLayer()))]))
assert len(holes) <= 100000 and path.read_bytes() == original
print(json.dumps(dict(schema_version=1, native_version='10.0.7', board_sha256=hashlib.sha256(original).hexdigest(),
    copper_layers=layers, physical_holes=holes, has_zones=bool(list(board.Zones()))), separators=(',', ':')), flush=True)
