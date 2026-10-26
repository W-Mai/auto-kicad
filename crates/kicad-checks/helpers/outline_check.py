"""Read a private saved-board snapshot with the pinned native geometry parser."""
from collections import Counter
import hashlib
import json
import math
from pathlib import Path
import sys

import pcbnew


def require(condition, message):
    if not condition:
        raise ValueError(message)


def arc_bounds(start, mid, end):
    """Outward integer bounds for the exact circular sweep through three integer points."""
    ax, ay = start
    ux, uy = mid[0] - ax, mid[1] - ay
    vx, vy = end[0] - ax, end[1] - ay
    cross = ux * vy - uy * vx
    require(cross != 0, "arc controls are collinear")
    den = 2 * cross
    unorm, vnorm = ux * ux + uy * uy, vx * vx + vy * vy
    cx = ax * den + unorm * vy - vnorm * uy
    cy = ay * den + ux * vnorm - vx * unorm
    if den < 0:
        den, cx, cy = -den, -cx, -cy
    a = (ax * den - cx, ay * den - cy)
    b = (end[0] * den - cx, end[1] * den - cy)
    radius2 = a[0] * a[0] + a[1] * a[1]
    floor_radius = math.isqrt(radius2)
    ceil_radius = floor_radius + (floor_radius * floor_radius != radius2)
    if cross < 0:
        a, b = b, a
    det = lambda u, v: u[0] * v[1] - u[1] * v[0]
    sweep = det(a, b)
    bounds = [min(start[0], mid[0], end[0]), min(start[1], mid[1], end[1]),
              max(start[0], mid[0], end[0]), max(start[1], mid[1], end[1])]
    for qx, qy in [(1,0), (0,1), (-1,0), (0,-1)]:
        q = (qx, qy)
        first, last = det(a, q), det(q, b)
        inside = (first >= 0 and last >= 0) if sweep >= 0 else (first >= 0 or last >= 0)
        if not inside:
            continue
        lo_x, hi_x = sorted((cx + qx * floor_radius, cx + qx * ceil_radius))
        lo_y, hi_y = sorted((cy + qy * floor_radius, cy + qy * ceil_radius))
        bounds = [min(bounds[0], lo_x // den), min(bounds[1], lo_y // den),
                  max(bounds[2], -(-hi_x // den)), max(bounds[3], -(-hi_y // den))]
    return tuple(bounds)


def inspect(path):
    require(pcbnew.GetBuildVersion() == "10.0.7", "unsupported native version")
    data = path.read_bytes()
    require(0 < len(data) <= 32 * 1024 * 1024, "board size exceeds bound")
    board = pcbnew.LoadBoard(str(path))
    require(isinstance(board, pcbnew.BOARD), "native board load failed")
    shapes = list(board.GetDrawings())
    for footprint in board.GetFootprints():
        shapes.extend(footprint.GraphicalItems())
    edges = [shape for shape in shapes if shape.GetLayer() == pcbnew.Edge_Cuts]
    require(0 < len(edges) <= 4096, "outline count is empty or exceeds bound")
    supported = (pcbnew.SHAPE_T_SEGMENT, pcbnew.SHAPE_T_ARC, pcbnew.SHAPE_T_CIRCLE,
                 pcbnew.SHAPE_T_RECT, pcbnew.SHAPE_T_POLY)
    boxes = []
    endpoints = Counter()
    for shape in edges:
        if isinstance(shape, pcbnew.PCB_SHAPE) and shape.GetShape() in (pcbnew.SHAPE_T_SEGMENT, pcbnew.SHAPE_T_ARC):
            a, b = shape.GetStart(), shape.GetEnd()
            require((a.x, a.y) != (b.x, b.y), "open primitive has identical endpoints")
            endpoints.update([(a.x, a.y), (b.x, b.y)])
    require(all(count == 2 for count in endpoints.values()),
            "outline endpoints do not form exact unbranched closed chains")
    for shape in edges:
        require(isinstance(shape, pcbnew.PCB_SHAPE) and shape.GetShape() in supported,
                "unsupported Edge.Cuts item")
        if shape.GetShape() == pcbnew.SHAPE_T_ARC:
            points = [(p.x, p.y) for p in (shape.GetStart(), shape.GetArcMid(), shape.GetEnd())]
            boxes.append(arc_bounds(*points))
            continue
        # Only the private in-memory copy changes; centerline bounds exclude stroke width.
        shape.SetWidth(0)
        box = shape.GetBoundingBox()
        x, y, width, height = box.GetX(), box.GetY(), box.GetWidth(), box.GetHeight()
        require(width >= 0 and height >= 0, "native bounds overflowed")
        boxes.append((x, y, x + width, y + height))
    bounds = [min(b[0] for b in boxes), min(b[1] for b in boxes),
              max(b[2] for b in boxes), max(b[3] for b in boxes)]
    require(all(abs(v) <= 2_000_000_000 for v in bounds), "native curve exceeds coordinate bounds")
    require(max(bounds[2] - bounds[0], bounds[3] - bounds[1]) <= 1_000_000_000,
            "native curve span exceeds bound")
    # Native KD-tree queries use a strict radius; radius zero finds no neighbors.
    # Exact endpoint degree checks above prevent the 1 nm search from snapping gaps.
    board.SetOutlinesChainingEpsilon(1)
    polygon = pcbnew.SHAPE_POLY_SET()
    require(board.GetBoardPolygonOutlines(polygon, False, None, False, False),
            "native boundary construction rejected")
    count = polygon.OutlineCount()
    require(0 < count <= 4096 and polygon.FullPointCount() <= 500_000,
            "native polygon count exceeds bound")
    holes = [polygon.HoleCount(i) for i in range(count)]
    require(sum(holes) <= 4096, "native hole count exceeds bound")
    areas = [polygon.UnitSet(i).Area() for i in range(count)]
    require(all(math.isfinite(area) and area > 0 for area in areas),
            "native boundary area is empty or invalid")
    project = path.with_suffix('.kicad_pro').read_bytes()
    require(path.read_bytes() == data, "snapshot changed during native read")
    return dict(kicad_version=pcbnew.GetBuildVersion(),
                board_sha256=hashlib.sha256(data).hexdigest(),
                project_sha256=hashlib.sha256(project).hexdigest(),
                edge_count=len(edges), outer_count=count, holes=holes,
                filled_areas_nm2=areas, bounds_nm=bounds,
                point_count=polygon.FullPointCount(), chaining_epsilon_nm=1, exact_endpoint_chains=True,
                inferred_outline=False)


if __name__ == '__main__':
    require(len(sys.argv) == 2, "expected snapshot path")
    try:
        print(json.dumps(inspect(Path(sys.argv[1])), allow_nan=False))
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(2)
