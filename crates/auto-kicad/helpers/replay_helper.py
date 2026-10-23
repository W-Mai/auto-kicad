"""Prepare and validate isolated native PCB replay data using bundled KiCad Python."""

import collections
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys

import wx

_app = wx.App(False)
import pcbnew as k  # noqa: E402 - wx must be initialized before KiCad imports.

CLI = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
WIDTH = 0.2
CLEARANCE = 0.2


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, board):
    if not k.SaveBoard(str(path), board):
        raise RuntimeError("KiCad board save failed")


def pad_records(board):
    result = []
    for fp in board.GetFootprints():
        for pad in fp.Pads():
            p, size, box = pad.GetPosition(), pad.GetSize(), pad.GetBoundingBox()
            result.append(
                {
                    "reference": fp.GetReference(),
                    "number": pad.GetNumber(),
                    "x": p.x / 1e6,
                    "y": p.y / 1e6,
                    "size": [size.x, size.y],
                    "shape": int(pad.GetShape()),
                    "round": pad.GetShape() == k.PAD_SHAPE_CIRCLE,
                    "angle": pad.GetOrientationDegrees(),
                    "net": pad.GetNetname(),
                    "box": [
                        box.GetX() / 1e6,
                        box.GetY() / 1e6,
                        box.GetRight() / 1e6,
                        box.GetBottom() / 1e6,
                    ],
                    "copper_layers": [
                        layer
                        for layer in range(64)
                        if pad.IsOnLayer(layer) and k.IsCopperLayer(layer)
                    ],
                }
            )
    return sorted(result, key=lambda p: (p["reference"], p["number"], p["x"], p["y"]))


def prepare(source, root):
    board = k.LoadBoard(str(source))
    if list(board.GetTracks()) or list(board.Zones()):
        raise RuntimeError("Replay input must be an unrouted board without zones")
    pads = pad_records(board)
    fps = list(board.GetFootprints())
    if not fps or len(fps) > 1000 or len(pads) > 20000:
        raise RuntimeError("Unsupported replay input size")
    if any(fp.GetLayer() != k.F_Cu for fp in fps):
        raise RuntimeError("This replay supports front-side components only")
    if any(p["copper_layers"] != [k.F_Cu] for p in pads):
        raise RuntimeError("This replay supports front SMT copper pads only")
    if len({fp.GetReference() for fp in fps}) != len(fps):
        raise RuntimeError("Component references must be unique")
    # Board-level silk from the demo export must not appear before component placement.
    for drawing in list(board.GetDrawings()):
        if drawing.GetLayer() != k.Edge_Cuts:
            board.RemoveNative(drawing)
    root.mkdir()
    project = root / "board.kicad_pcb"
    save(project, board)
    project.with_suffix(".kicad_pro").write_bytes(
        source.with_suffix(".kicad_pro").read_bytes()
    )
    edges = board.GetBoardEdgesBoundingBox()
    bounds = [
        edges.GetX() / 1e6,
        edges.GetY() / 1e6,
        edges.GetRight() / 1e6,
        edges.GetBottom() / 1e6,
    ]
    board.DeleteAllFootprints()
    save(root / "empty.kicad_pcb", board)
    (root / "source.json").write_text(
        json.dumps(
            {
                "source": str(source),
                "source_sha256": digest(source),
                "project_sha256": digest(source.with_suffix(".kicad_pro")),
                "components": len(fps),
                "bounds": bounds,
                "pads": pads,
                "presentation": "Board-level silk hidden during placement playback",
            },
            indent=2,
        )
    )
    print(json.dumps({"prepared_components": len(fps), "pads": len(pads)}))


def point_distance(p, a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    if dx == dy == 0:
        return math.dist(p, a)
    t = max(0, min(1, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)))
    return math.dist(p, (a[0] + t * dx, a[1] + t * dy))


def segment_distance(a, b, c, d):
    def cross(p, q, r):
        return (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])

    ab_c, ab_d, cd_a, cd_b = (
        cross(a, b, c),
        cross(a, b, d),
        cross(c, d, a),
        cross(c, d, b),
    )
    if ab_c * ab_d < 0 and cd_a * cd_b < 0:
        return 0
    return min(
        point_distance(a, c, d),
        point_distance(b, c, d),
        point_distance(c, a, b),
        point_distance(d, a, b),
    )


def intersects_box(a, b, box, margin):
    low, high = 0.0, 1.0
    for axis in (0, 1):
        minimum, maximum = box[axis] - margin, box[axis + 2] + margin
        delta = b[axis] - a[axis]
        if abs(delta) < 1e-12:
            if a[axis] < minimum or a[axis] > maximum:
                return False
        else:
            x, y = (minimum - a[axis]) / delta, (maximum - a[axis]) / delta
            low, high = max(low, min(x, y)), min(high, max(x, y))
            if low > high:
                return False
    return True


def paths(a, b):
    dx, dy = b[0] - a[0], b[1] - a[1]
    diagonal = min(abs(dx), abs(dy))
    sx, sy = math.copysign(1, dx), math.copysign(1, dy)
    candidates = [
        [a, b],
        [a, (a[0], b[1]), b],
        [a, (b[0], a[1]), b],
        [a, (a[0] + sx * diagonal, a[1] + sy * diagonal), b],
        [a, (b[0] - sx * diagonal, b[1] - sy * diagonal), b],
    ]
    unique = set()
    for points in sorted(
        candidates, key=lambda p: sum(math.dist(x, y) for x, y in zip(p, p[1:]))
    ):
        points = tuple(
            p
            for i, p in enumerate(points)
            if i == 0 or math.dist(p, points[i - 1]) > 1e-9
        )
        if points not in unique:
            unique.add(points)
            yield points


def drc(board_path, report_path):
    process = subprocess.run(
        [
            CLI,
            "pcb",
            "drc",
            "--format",
            "json",
            "--output",
            str(report_path),
            str(board_path),
        ],
        capture_output=True,
        timeout=60,
    )
    if process.returncode not in (0, 5) or not report_path.exists():
        raise RuntimeError(
            "KiCad DRC failed: " + process.stderr.decode(errors="replace")[-2000:]
        )
    return json.loads(report_path.read_text())


def violation_key(item):
    return (
        item["type"],
        item["severity"],
        tuple(sorted(p["uuid"] for p in item["items"])),
    )


def introduced(baseline, after):
    old = collections.Counter(violation_key(v) for v in baseline["violations"])
    result = []
    for item in after["violations"]:
        key = violation_key(item)
        if old[key]:
            old[key] -= 1
        else:
            result.append(item)
    return result


def make_track(board, a, b, net):
    track = k.PCB_TRACK(board)
    track.SetStart(k.VECTOR2I(round(a[0] * 1e6), round(a[1] * 1e6)))
    track.SetEnd(k.VECTOR2I(round(b[0] * 1e6), round(b[1] * 1e6)))
    track.SetWidth(round(WIDTH * 1e6))
    track.SetLayer(k.F_Cu)
    track.SetNet(board.FindNet(net))
    board.Add(track)
    return track


def plan(board_path, root):
    board = k.LoadBoard(str(board_path))
    pads = pad_records(board)
    edge = board.GetBoardEdgesBoundingBox()
    bounds = [
        edge.GetX() / 1e6,
        edge.GetY() / 1e6,
        edge.GetRight() / 1e6,
        edge.GetBottom() / 1e6,
    ]
    grouped = collections.defaultdict(list)
    for i, pad in enumerate(pads):
        if pad["net"]:
            grouped[pad["net"]].append(i)
    parents = list(range(len(pads)))

    def find(i):
        while parents[i] != i:
            i = parents[i]
        return i

    candidates = []
    for net, indices in sorted(grouped.items()):
        for at, i in enumerate(indices):
            for j in indices[at + 1 :]:
                a, b = pads[i], pads[j]
                distance = math.hypot(a["x"] - b["x"], a["y"] - b["y"])
                candidates.append((distance, net, i, j))
    routes = []
    segments = []
    blocked = 0

    def clear(a, b, net):
        margin = CLEARANCE + WIDTH / 2 + 0.01
        for p in (a, b):
            if not (
                bounds[0] + margin <= p[0] <= bounds[2] - margin
                and bounds[1] + margin <= p[1] <= bounds[3] - margin
            ):
                return False
        for pad in pads:
            if pad["net"] != net and intersects_box(a, b, pad["box"], margin):
                return False
        return all(
            old_net == net or segment_distance(a, b, c, d) > CLEARANCE + WIDTH + 0.01
            for c, d, old_net in segments
        )

    for _, net, i, j in sorted(candidates):
        if find(i) == find(j):
            continue
        a, b = (pads[i]["x"], pads[i]["y"]), (pads[j]["x"], pads[j]["y"])
        selected = next(
            (p for p in paths(a, b) if all(clear(x, y, net) for x, y in zip(p, p[1:]))),
            None,
        )
        if selected is None:
            blocked += 1
            continue
        parents[find(i)] = find(j)
        segments.extend((x, y, net) for x, y in zip(selected, selected[1:]))
        routes.append(
            {
                "net": net,
                "from": [pads[i]["reference"], pads[i]["number"]],
                "to": [pads[j]["reference"], pads[j]["number"]],
                "points": [[round(x * 1e6), round(y * 1e6)] for x, y in selected],
                "width_nm": round(WIDTH * 1e6),
            }
        )
    baseline = drc(board_path, root / "baseline-drc.json")
    candidate = root / "candidate.kicad_pcb"
    candidate.with_suffix(".kicad_pro").write_bytes(
        board_path.with_suffix(".kicad_pro").read_bytes()
    )
    dropped = []
    for attempt in range(4):
        candidate_board = k.LoadBoard(str(board_path))
        owners = {}
        for index, route in enumerate(routes):
            for a, b in zip(route["points"], route["points"][1:]):
                track = make_track(
                    candidate_board,
                    [v / 1e6 for v in a],
                    [v / 1e6 for v in b],
                    route["net"],
                )
                owners[track.m_Uuid.AsString()] = index
        save(candidate, candidate_board)
        report = drc(candidate, root / "candidate-drc.json")
        violations = introduced(baseline, report)
        if not violations:
            break
        invalid = {
            owners[item["uuid"]]
            for v in violations
            for item in v["items"]
            if item["uuid"] in owners
        }
        if not invalid or attempt == 3:
            raise RuntimeError("Routing candidate introduced unresolved DRC violations")
        dropped.extend(routes[i] for i in sorted(invalid))
        routes = [r for i, r in enumerate(routes) if i not in invalid]
    if not routes:
        raise RuntimeError("No safe routes found")
    payload = {
        "algorithm": "distance-sorted same-net union-find; direct, orthogonal and 45-degree dogleg candidates; inflated pad/track obstacles; KiCad DRC filter",
        "routes": routes,
        "blocked_pairs": blocked,
        "drc_rejected_routes": dropped,
        "baseline_unconnected": len(baseline["unconnected_items"]),
        "planned_unconnected": len(report["unconnected_items"]),
        "introduced_violations": len(violations),
        "baseline_violations": len(baseline["violations"]),
    }
    payload["geometry_sha256"] = hashlib.sha256(
        json.dumps(routes, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    (root / "route-plan.json").write_text(json.dumps(payload, indent=2))
    candidate.unlink()
    candidate.with_suffix(".kicad_pro").unlink()
    print(
        json.dumps(
            {
                "routes": len(routes),
                "segments": sum(len(r["points"]) - 1 for r in routes),
                "unconnected_before": payload["baseline_unconnected"],
                "unconnected_after": payload["planned_unconnected"],
            }
        )
    )


def audit(board_path, prepared, root):
    board = k.LoadBoard(str(board_path))
    original = json.loads((prepared / "source.json").read_text())
    actual = pad_records(board)
    if actual != original["pads"]:
        raise RuntimeError("Replayed pad geometry/net mapping differs from input")
    baseline = json.loads((root / "baseline-drc.json").read_text())
    final = drc(board_path, root / "final-drc.json")
    if introduced(baseline, final):
        raise RuntimeError("Native replay introduced DRC violations")
    plan_data = json.loads((root / "route-plan.json").read_text())
    if len(final["unconnected_items"]) != plan_data["planned_unconnected"]:
        raise RuntimeError("Native connectivity differs from validated route plan")
    if digest(Path(original["source"])) != original["source_sha256"]:
        raise RuntimeError("Original source changed")
    if (
        digest(Path(original["source"]).with_suffix(".kicad_pro"))
        != original["project_sha256"]
    ):
        raise RuntimeError("Original project settings changed")
    summary = {
        "components": len(list(board.GetFootprints())),
        "pads": len(actual),
        "tracks": len(list(board.GetTracks())),
        "unconnected_before": len(baseline["unconnected_items"]),
        "unconnected_after": len(final["unconnected_items"]),
        "introduced_violations": 0,
        "source_unchanged": True,
        "geometry_sha256": plan_data["geometry_sha256"],
    }
    (root / "summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary))


if __name__ == "__main__":
    mode = sys.argv[1]
    if mode == "prepare":
        prepare(Path(sys.argv[2]).resolve(strict=True), Path(sys.argv[3]))
    elif mode == "plan":
        plan(Path(sys.argv[2]).resolve(strict=True), Path(sys.argv[3]))
    elif mode == "audit":
        audit(
            Path(sys.argv[2]).resolve(strict=True), Path(sys.argv[3]), Path(sys.argv[4])
        )
    else:
        raise RuntimeError("Unsupported helper command")
