#!/usr/bin/env python3
"""Generate a combined maximum-size geometry request without modifying a board."""
import argparse
import json
from pathlib import Path


def point(x, y):
    return dict(x_nm=x, y_nm=y)


def rectangle(x, y, width, height, divisions=1):
    corners = [(x, y), (x + width, y), (x + width, y + height), (x, y + height)]
    result = []
    for a, b in zip(corners, corners[1:] + corners[:1]):
        for i in range(divisions):
            result.append(point(a[0] + (b[0]-a[0])*i//divisions, a[1] + (b[1]-a[1])*i//divisions))
    return result


def loop(label, points):
    return dict(loop_id=label, width_nm=50000,
                edges=[dict(kind="segment", start=a, end=b)
                       for a, b in zip(points, points[1:] + points[:1])])


def request():
    loops = [loop("maximum-outer", rectangle(0, 0, 100000000, 80000000, 4))]
    for i in range(31):
        loops.append(loop(f"cutout-{i}", rectangle(50000000+(i%8)*5000000,
                                                  43000000+(i//8)*7000000, 2000000, 2000000, 4)))
    zones = []
    for i in range(32):
        x, y = 3000000+(i%8)*5500000, 43000000+(i//8)*7000000
        exterior = rectangle(x, y, 4480000, 4480000, 112)
        holes = [dict(points=rectangle(x+600000+(j%4)*900000,
                                      y+600000+(j//4)*900000, 300000, 300000))
                 for j in range(16)]
        zones.append(dict(kind="create", zone_id=f"maximum-zone-{i}", definition=dict(
            name=f"Maximum zone {i}", net=dict(net_id="${net:GND}", name="GND"),
            layers=["In2.Cu"], polygon=dict(exterior=dict(points=exterior), holes=holes),
            priority=i, clearance_nm=200000, min_thickness_nm=200000, connection="thermal",
            thermal_gap_nm=500000, thermal_spoke_width_nm=500000,
            island_removal="never", min_island_area_nm2=10000000000000)))
    result = dict(observed_revision="${revision}", outline=dict(remove_ids="${outline_ids}", loops=loops),
                  zones=zones, refill="all")
    assert len(loops) == 32 and sum(len(p["edges"]) for p in loops) == 512
    assert len(zones) == 32
    assert all(len(z["definition"]["polygon"]["exterior"]["points"]) +
               sum(len(h["points"]) for h in z["definition"]["polygon"]["holes"]) == 512
               for z in zones)
    return result


def prepare_fixture(source, output):
    import hashlib
    import shutil
    import pcbnew
    original = source.read_bytes()
    output.mkdir()
    board = pcbnew.LoadBoard(str(source))
    assert pcbnew.Version() == "10.0.7"
    net = next(p.GetNetCode() for f in board.GetFootprints() for p in f.Pads() if p.GetNetname() == "GND")
    original_edges = [s for s in board.GetDrawings() if s.GetLayer() == pcbnew.Edge_Cuts]
    assert len(original_edges) == 4
    for index, original_edge in enumerate(original_edges):
        assert original_edge.GetShape() == pcbnew.SHAPE_T_SEGMENT
        start, end = original_edge.GetStart(), original_edge.GetEnd()
        for part in range(25):
            edge = pcbnew.PCB_SHAPE(board)
            edge.SetShape(pcbnew.SHAPE_T_SEGMENT)
            edge.SetLayer(pcbnew.Edge_Cuts)
            edge.SetWidth(original_edge.GetWidth())
            edge.SetUuidDirect(pcbnew.KIID(f"a0000000-0000-4000-8000-{index*25+part+1:012d}"))
            edge.SetStart(pcbnew.VECTOR2I(start.x+(end.x-start.x)*part//25, start.y+(end.y-start.y)*part//25))
            edge.SetEnd(pcbnew.VECTOR2I(start.x+(end.x-start.x)*(part+1)//25, start.y+(end.y-start.y)*(part+1)//25))
            board.Add(edge)
        board.Remove(original_edge)
    assert sum(s.GetLayer() == pcbnew.Edge_Cuts for s in board.GetDrawings()) == 100
    ids = []
    for i in range(32):
        via = pcbnew.PCB_VIA(board)
        via.SetUuidDirect(pcbnew.KIID(f"90000000-0000-4000-8000-{i+1:012d}"))
        via.SetPosition(pcbnew.VECTOR2I(3300000+(i%8)*5500000, 43300000+(i//8)*7000000))
        via.SetWidth(600000)
        via.SetDrill(300000)
        via.SetLayerPair(pcbnew.F_Cu, pcbnew.B_Cu)
        via.SetNetCode(net)
        board.Add(via)
        ids.append(str(via.m_Uuid.AsString()))
    target = output/"board.kicad_pcb"
    assert pcbnew.SaveBoard(str(target), board)
    for suffix in [".kicad_pro", ".kicad_dru", ".kicad_sch"]:
        path = source.with_suffix(suffix)
        if path.exists():
            shutil.copyfile(path, target.with_suffix(suffix))
    assert source.read_bytes() == original
    (output/"fixture.json").write_text(json.dumps(dict(source=str(source), original_sha256=hashlib.sha256(original).hexdigest(),
                                                      ground_vias=ids, outline_removals=100, board_sha256=hashlib.sha256(target.read_bytes()).hexdigest()), indent=2)+"\n")



if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="New JSON file")
    parser.add_argument("--fixture-source", type=Path)
    parser.add_argument("--fixture-output", type=Path)
    args = parser.parse_args()
    if bool(args.fixture_source) != bool(args.fixture_output):
        parser.error("fixture source and new output are required together")
    if args.fixture_source:
        prepare_fixture(args.fixture_source, args.fixture_output)
    with args.output.open("x") as stream:
        json.dump([request()], stream, separators=(",", ":"))
        stream.write("\n")
    print(json.dumps(dict(path=str(args.output), loops=32, outline_edges=512,
                         zone_mutations=32, vertices_per_zone=512, holes_per_zone=16,
                         bytes=args.output.stat().st_size)))
