"""Capture native authored-zone ownership without merging disconnected zone islands."""
import hashlib
import json
from pathlib import Path
import sys
import wx

app = wx.App(False)
import pcbnew

MAX_INPUT = 32 * 1024 * 1024
MAX_ENDPOINTS = 4096
MAX_ZONES = 256


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path):
    require(path.is_file() and not path.is_symlink(), 'region input is not regular')
    size = path.stat().st_size
    require(0 < size <= MAX_INPUT, 'region input exceeds bounds')
    data = path.read_bytes()
    require(len(data) == size, 'region input changed during capture')
    return data


def main():
    require(len(sys.argv) == 2, 'one private board path required')
    path = Path(sys.argv[1])
    original = read(path)
    project_path = path.with_suffix('.kicad_pro')
    project = read(project_path)
    require(pcbnew.GetBuildVersion() == '10.0.7', 'unqualified native region version')
    board = pcbnew.LoadBoard(str(path))
    require(board is not None and board.BuildConnectivity(), 'native region connectivity failed')
    endpoints = {}

    def add(item):
        identifier = item.m_Uuid.AsString()
        require(identifier not in endpoints and len(endpoints) < MAX_ENDPOINTS, 'duplicate or excessive region endpoint')
        endpoints[identifier] = item.GetNetname()

    for item in board.GetTracks():
        add(item)
    for item in board.GetDrawings():
        if item.GetClass() == 'PCB_SHAPE' and item.IsOnCopperLayer():
            add(item)
    for footprint in board.GetFootprints():
        for pad in footprint.Pads():
            add(pad)
        for item in footprint.GraphicalItems():
            if item.GetClass() == 'PCB_SHAPE' and item.IsOnCopperLayer():
                add(item)
    connectivity = board.GetConnectivity()
    ordinary = []
    generated_count = 0
    zones = list(board.Zones())
    require(len(zones) <= MAX_ZONES, 'native zone count exceeds bound')
    seen = set()
    for zone in zones:
        if zone.IsTeardropArea():
            generated_count += 1
            continue
        identifier = zone.m_Uuid.AsString()
        require(identifier not in seen, 'duplicate authored native zone')
        seen.add(identifier)
        terminals = set()
        # This union checks ownership of a selected entire zone. It never supplies graph edges.
        members = connectivity.GetConnectedItems(zone, 0)
        require(len(members) <= MAX_ENDPOINTS + MAX_ZONES, 'native zone ownership exceeds bound')
        for item in members:
            if item.GetClass() == 'ZONE':
                continue
            item_id = item.m_Uuid.AsString()
            require(item_id in endpoints, 'zone contacts an unobserved native endpoint')
            require(endpoints[item_id] == zone.GetNetname(), 'native zone ownership crosses nets')
            terminals.add(item_id)
        ordinary.append(dict(uuid=identifier, net=zone.GetNetname(), locked=zone.IsLocked(),
            rule_area=zone.GetIsRuleArea(), layers=sorted(str(pcbnew.LayerName(layer)) for layer in zone.GetLayerSet().Seq()),
            filled=zone.IsFilled(), terminals=sorted(terminals)))
    require(read(path) == original and read(project_path) == project, 'region reader changed private inputs')
    report = dict(schema_version=1, native_version='10.0.7', board_sha256=hashlib.sha256(original).hexdigest(),
        project_sha256=hashlib.sha256(project).hexdigest(), endpoints=endpoints,
        zones=sorted(ordinary, key=lambda item: item['uuid']), generated_count=generated_count)
    output = json.dumps(report, ensure_ascii=False, separators=(',', ':'), allow_nan=False)
    require(len(output.encode('utf-8')) <= 4 * 1024 * 1024, 'region ownership report exceeds byte bound')
    print(output, flush=True)


if __name__ == '__main__':
    main()
