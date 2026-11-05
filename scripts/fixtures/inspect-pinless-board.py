"""Independently read saved pinless footprints using KiCad's native physical APIs."""
import json
import sys
import wx
app = wx.App(False)
import pcbnew

board = pcbnew.LoadBoard(sys.argv[1])
footprints = []
for fp in board.GetFootprints():
    pads = []
    for pad in fp.Pads():
        drill = pad.GetDrillSize(); pos = pad.GetPosition(); size = pad.GetSize()
        pads.append(dict(uuid=pad.m_Uuid.AsString(), number=pad.GetNumber(), net=pad.GetNetname(),
            attribute='np_thru_hole' if pad.GetAttribute() == pcbnew.PAD_ATTRIB_NPTH else str(pad.GetAttribute()),
            drill_nm=[drill.x, drill.y], size_nm=[size.x, size.y], position_nm=[pos.x, pos.y], layers=list(pad.GetLayerSet().Seq())))
    position = fp.GetPosition()
    footprints.append(dict(uuid=fp.m_Uuid.AsString(), reference=fp.GetReference(), value=fp.GetValue(),
        library=fp.GetFPID().GetUniStringLibId(), position_nm=[position.x, position.y],
        orientation_mdeg=round(fp.GetOrientation().AsDegrees() * 1000) % 360000,
        side='back' if fp.IsFlipped() else 'front', pads=pads))
print(json.dumps(dict(version=pcbnew.Version(), footprints=footprints), indent=2), flush=True)
