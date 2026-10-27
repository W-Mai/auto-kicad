#!/usr/bin/env python3
"""Prepare an owned populated zoned fixture with KiCad's bundled Python reader."""
import argparse
import hashlib
import json
from pathlib import Path

import pcbnew

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path, help="New directory; existing paths are refused")
parser.add_argument("--teardrops", action="store_true")
args = parser.parse_args()
assert pcbnew.Version().startswith("10.0.7"), "Requires KiCad 10.0.7"
root = Path(__file__).resolve().parent / "fixtures"
text = (root / "layered-route-anchors.kicad_pcb").read_text()
zones = []
for index, layer in enumerate(["In1.Cu", "B.Cu"]):
    zones.append(f'''(zone (net "GND") (layer "{layer}") (uuid "80000000-0000-4000-8000-{index+1:012d}")
        (name "retained plane {index}") (hatch edge 0.5) (connect_pads (clearance 0.2)) (min_thickness 0.2)
        (fill (thermal_gap 0.5) (thermal_bridge_width 0.5) (island_removal_mode 1))
        (polygon (pts (xy 2 2) (xy 98 2) (xy 98 78) (xy 2 78))))''')
end = text.rindex(")")
text = text[:end] + "".join(zones) + text[end:]
args.output.mkdir()
board_path = args.output / "board.kicad_pcb"
project_path = board_path.with_suffix(".kicad_pro")
board_path.write_text(text)
project_path.write_bytes((root / "layered-routing-rules.kicad_pro").read_bytes())
board = pcbnew.LoadBoard(str(board_path))
enabled = []
if args.teardrops:
    for footprint in board.GetFootprints():
        if footprint.GetReference() in ["TP2", "TP3"]:
            for pad in footprint.Pads():
                pad.SetTeardropsEnabled(True)
                pad.SetTeardropCurved(False)
                pad.SetTeardropPreferZoneConnections(False)
                pad.SetTeardropBestLengthRatio(0.5)
                pad.SetTeardropBestWidthRatio(1.0)
                pad.SetTeardropMaxTrackWidth(0.9)
                enabled.append(str(pad.m_Uuid.AsString()))
    assert len(enabled) == 2
assert len(list(board.Zones())) == 2
assert pcbnew.SaveBoard(str(board_path), board)
reloaded = pcbnew.LoadBoard(str(board_path))
assert len(list(reloaded.Zones())) == 2
assert sorted(str(p.m_Uuid.AsString()) for f in reloaded.GetFootprints() for p in f.Pads() if p.GetTeardropsEnabled()) == sorted(enabled)
record = dict(version=pcbnew.Version(), teardrop_pads=enabled,
              sha256={p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                      for p in [board_path, project_path]})
(args.output / "fixture.json").write_text(json.dumps(record, indent=2) + "\n")
print(json.dumps(record))
