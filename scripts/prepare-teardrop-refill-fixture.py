"""Extend a native pad/via fixture with back-mask and front/back track-end generation."""
from pathlib import Path
import json
import sys
import pcbnew

source, output=map(Path,sys.argv[1:])
output.mkdir()
board=pcbnew.LoadBoard(str(source))
via=next(t for t in board.GetTracks() if isinstance(t,pcbnew.PCB_VIA))
net=via.GetNetCode()
def track(start,end,width,layer):
    item=pcbnew.PCB_TRACK(board)
    item.SetStart(pcbnew.VECTOR2I(*start))
    item.SetEnd(pcbnew.VECTOR2I(*end))
    item.SetWidth(width)
    item.SetLayer(layer)
    item.SetNetCode(net)
    item.SetHasSolderMask(True)
    board.Add(item)

track((24_000_000,40_000_000),(30_000_000,40_000_000),250_000,pcbnew.B_Cu)
for layer,y in [(pcbnew.F_Cu,46_000_000),(pcbnew.B_Cu,50_000_000)]:
    track((18_000_000,y),(22_000_000,y),250_000,layer)
    track((22_000_000,y),(28_000_000,y),1_000_000,layer)
target=output/'teardrops.kicad_pcb'
pcbnew.SaveBoard(str(target),board)
project=json.loads(source.with_suffix('.kicad_pro').read_text())
settings=project.setdefault('board',{}).setdefault('design_settings',{})
settings['teardrop_options']=[dict(td_onpthpad=True,td_onroundshapesonly=False,td_onsmdpad=True,td_ontrackend=True,td_onvia=True)]
settings['teardrop_parameters']=[dict(td_target_name='td_track_end',td_enabled=True,td_maxlen=1.0,td_maxheight=2.0,td_length_ratio=0.5,td_height_ratio=1.0,td_curve_segcount=0,td_width_to_size_filter_ratio=0.9,td_allow_use_two_tracks=True,td_on_pad_in_zone=False)]
target.with_suffix('.kicad_pro').write_text(json.dumps(project,indent=2)+'\n')
print(target)
