"""Read bounded electrical components from a private, saved KiCad board snapshot."""
import hashlib
import json
from pathlib import Path
import sys

import pcbnew

MAX_BYTES = 32 * 1024 * 1024
MAX_ITEMS = 4096


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path):
    require(path.is_file() and not path.is_symlink(), "snapshot must be a regular file")
    size = path.stat().st_size
    require(0 < size <= MAX_BYTES, "snapshot exceeds byte bound")
    with path.open('rb') as file:
        data = file.read(MAX_BYTES + 1)
    require(len(data) == size, "snapshot changed while reading")
    return data


def identifier(item):
    value = item.m_Uuid.AsString()
    require(len(value) == 36, "invalid native UUID")
    return value


def main():
    require(len(sys.argv) == 2, "one snapshot path required")
    board_path = Path(sys.argv[1])
    project_path = board_path.with_suffix('.kicad_pro')
    board_bytes, project_bytes = read(board_path), read(project_path)
    require(pcbnew.GetBuildVersion() == '10.0.7', "unverified native version")
    board = pcbnew.LoadBoard(str(board_path))
    require(board is not None, "native board load failed")
    require(board.BuildConnectivity(), "native connectivity construction failed")
    connectivity = board.GetConnectivity()
    records = {}
    native = {}

    def add(item):
        require(len(native) < MAX_ITEMS, "native endpoint count exceeds bound")
        uuid = identifier(item)
        require(uuid not in native, "duplicate native endpoint")
        kind = item.GetClass()
        require(kind in ['PAD', 'PCB_TRACK', 'PCB_ARC', 'PCB_VIA', 'PCB_SHAPE'], "unsupported endpoint kind")
        if kind == 'PCB_SHAPE':
            require(item.IsOnCopperLayer(), "non-copper graphic endpoint")
        if kind == 'PCB_VIA':
            require(item.GetViaType() == pcbnew.VIATYPE_THROUGH, "unsupported via type")
        position = item.GetPosition()
        net = item.GetNetname()
        require(len(net.encode('utf-8')) <= 4096 and '\0' not in net, "invalid net name")
        require(abs(position.x) <= 2_000_000_000 and abs(position.y) <= 2_000_000_000, "endpoint position exceeds bound")
        native[uuid] = item
        records[uuid] = dict(uuid=uuid, kind=kind, net=net, position_nm=[position.x, position.y])

    for item in board.GetTracks():
        add(item)
    for drawing in board.GetDrawings():
        if drawing.GetClass() == 'PCB_SHAPE' and drawing.IsOnCopperLayer():
            add(drawing)
    for footprint in board.GetFootprints():
        for pad in footprint.Pads():
            add(pad)
        for drawing in footprint.GraphicalItems():
            if drawing.GetClass() == 'PCB_SHAPE' and drawing.IsOnCopperLayer():
                add(drawing)
    membership = {}
    for uuid, item in native.items():
        members = {uuid}
        # Native traversal respects individual filled islands. A returned zone UUID is never a graph node.
        connected = connectivity.GetConnectedItems(item, 0)
        require(len(connected) <= MAX_ITEMS + 256, "native component exceeds bound")
        for member in connected:
            if member.GetClass() == 'ZONE':
                continue
            member_id = identifier(member)
            require(member_id in native, "native component has an unobserved endpoint: " + member.GetClass() + ":" + member_id)
            require(records[member_id]['net'] == records[uuid]['net'], "native component crosses nets")
            members.add(member_id)
        membership[uuid] = tuple(sorted(members))
    # Query every supported endpoint; overlap alone must never manufacture a component union.
    for uuid, members in membership.items():
        require(all(membership[member] == members for member in members), "asymmetric or inconsistent native component")
        require(records[uuid]['net'] or members == (uuid,), "connected netless endpoints unsupported")
    components = sorted(set(membership.values()))
    indices = {members: index for index, members in enumerate(components)}
    for uuid, record in records.items():
        record['component'] = indices[membership[uuid]]
    require(read(board_path) == board_bytes and read(project_path) == project_bytes, "saved inputs changed")
    result = dict(kicad_version='10.0.7', board_sha256=hashlib.sha256(board_bytes).hexdigest(),
                  project_sha256=hashlib.sha256(project_bytes).hexdigest(),
                  endpoints=[records[key] for key in sorted(records)], components=components)
    output = json.dumps(result, ensure_ascii=False, separators=(',', ':'))
    require(len(output.encode('utf-8')) <= 2 * 1024 * 1024, "connectivity output exceeds bound")
    print(output)


if __name__ == '__main__':
    main()
