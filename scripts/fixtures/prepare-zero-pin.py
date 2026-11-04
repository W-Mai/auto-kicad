"""Prepare an owned native zero-pin metadata control; this is not an MCP creation claim."""
import json
from pathlib import Path
import sys
import wx
app = wx.App(False)
import pcbnew as k

project = Path(sys.argv[1]).resolve(strict=True)
assert (project / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
assert k.GetBuildVersion() == '10.0.7'
board = k.LoadBoard(str(project / 'sync.kicad_pcb'))
fp = k.FootprintLoad('/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/MountingHole.pretty', 'MountingHole_4.3mm_M4', True)
assert fp and all(not pad.GetNumber() and not pad.GetNetname() for pad in fp.Pads())
fp.SetFPID(k.LIB_ID('MountingHole', 'MountingHole_4.3mm_M4'))
fp.SetUuid(k.KIID('e2222222-2222-4222-8222-222222222222'))
fp.SetReference('H1'); fp.SetValue('M4 mounting hole')
fp.SetPosition(k.VECTOR2I(45000000, 30000000))
fp.SetPath(k.KIID_PATH('/e1111111-1111-4111-8111-111111111111'))
board.Add(fp)
assert k.SaveBoard(str(project / 'sync.kicad_pcb'), board)
print(json.dumps(dict(native_version='10.0.7', prepared_footprint=fp.m_Uuid.AsString(),
    pads=[dict(uuid=p.m_Uuid.AsString(), number=p.GetNumber(), net=p.GetNetname()) for p in fp.Pads()],
    native_fixture_preparation=True, mcp_creation_claimed=False)))
