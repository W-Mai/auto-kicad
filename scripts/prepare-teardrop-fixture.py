"""Create an owned pad/via teardrop fixture using the installed native writer."""
from pathlib import Path
import json
import shutil
import sys
import pcbnew

source, output = map(Path, sys.argv[1:])
output.mkdir()
board = pcbnew.LoadBoard(str(source))
pad = next(p for f in board.GetFootprints() if f.GetReference() == 'TP1' for p in f.Pads())
pad.SetTeardropsEnabled(True)
pad.SetTeardropCurved(False)
pad.SetTeardropPreferZoneConnections(False)
pad.SetTeardropBestLengthRatio(0.5)
pad.SetTeardropBestWidthRatio(1.0)
pad.SetTeardropMaxTrackWidth(0.9)
via = pcbnew.PCB_VIA(board)
via.SetPosition(pcbnew.VECTOR2I(24_000_000, 40_000_000))
via.SetWidth(800_000)
via.SetDrill(300_000)
via.SetLayerPair(pcbnew.F_Cu, pcbnew.B_Cu)
via.SetNetCode(pad.GetNetCode())
via.SetTeardropsEnabled(True)
via.SetTeardropCurved(True)
via.SetTeardropPreferZoneConnections(False)
via.SetTeardropBestLengthRatio(0.7)
via.SetTeardropBestWidthRatio(1.0)
via.SetTeardropMaxTrackWidth(0.9)
board.Add(via)
track = pcbnew.PCB_TRACK(board)
track.SetStart(pad.GetPosition())
track.SetEnd(via.GetPosition())
track.SetWidth(250_000)
track.SetLayer(pcbnew.F_Cu)
track.SetNetCode(pad.GetNetCode())
track.SetHasSolderMask(True)
board.Add(track)
target = output/'teardrops.kicad_pcb'
pcbnew.SaveBoard(str(target),board)
shutil.copy2(source.with_suffix('.kicad_pro'),target.with_suffix('.kicad_pro'))
(output/'fixture.json').write_text(json.dumps(dict(pad=str(pad.m_Uuid.AsString()),via=str(via.m_Uuid.AsString()),track=str(track.m_Uuid.AsString()),source=str(source),target=str(target)),indent=2)+'\n')
