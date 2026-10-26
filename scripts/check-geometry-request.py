#!/usr/bin/env python3
"""Read saved KiCad zones independently and compare each supplied JSON intent field."""
import hashlib
import json
from pathlib import Path
import sys

import pcbnew


def points(line):
    assert line.IsClosed() and line.ArcCount() == 0
    return [dict(x_nm=line.CPoint(i).x, y_nm=line.CPoint(i).y)
            for i in range(line.PointCount())]


def check(board, zone, expected):
    readers = {
        'name': zone.GetZoneName,
        'priority': zone.GetAssignedPriority,
        'clearance_nm': zone.GetLocalClearance,
        'min_thickness_nm': zone.GetMinThickness,
        'thermal_gap_nm': zone.GetThermalReliefGap,
        'thermal_spoke_width_nm': zone.GetThermalReliefSpokeWidth,
        'min_island_area_nm2': zone.GetMinIslandArea,
    }
    checked = {}
    for key, value in expected.items():
        if value is None:
            continue
        if key in readers:
            actual = readers[key]()
        elif key == 'net':
            value = value['name']
            actual = zone.GetNetname()
        elif key == 'layers':
            value = sorted(board.GetLayerID(name) for name in value)
            actual = sorted(zone.GetLayerSet().Seq())
        elif key == 'connection':
            value = {'solid': pcbnew.ZONE_CONNECTION_FULL,
                     'thermal': pcbnew.ZONE_CONNECTION_THERMAL,
                     'pth_thermal': pcbnew.ZONE_CONNECTION_THT_THERMAL}[value]
            actual = zone.GetPadConnection()
        elif key == 'island_removal':
            value = {'always': 0, 'never': 1, 'below_area': 2}[value]
            actual = zone.GetIslandRemovalMode()
        elif key == 'polygon':
            outline = zone.Outline()
            assert outline.OutlineCount() == 1
            actual = dict(exterior=dict(points=points(outline.COutline(0))),
                          holes=[dict(points=points(outline.CHole(0, i)))
                                 for i in range(outline.HoleCount(0))])
        else:
            raise AssertionError(f'unchecked intent field: {key}')
        assert actual == value, (key, actual, value)
        checked[key] = actual
    return checked


def main():
    output = Path(sys.argv[1]).resolve()
    result = json.loads((output / 'result.json').read_text())
    assert result['kicad_version'] == '10.0.7'
    records = []
    for record in result['stages']:
        index = record['stage']
        path = output / f'stage-{index}.kicad_pcb'
        before = path.read_bytes()
        board = pcbnew.LoadBoard(str(path))
        zones = {str(z.m_Uuid.AsString()): z for z in board.Zones()}
        request = json.loads((output / f'request-{index}.json').read_text())
        mutations = []
        for edit in request['zones']:
            if edit['kind'] == 'create':
                identifier = record['native_creation_ids'][edit['zone_id']]
                expected = edit['definition']
            else:
                identifier = edit['source_uuid']
                expected = edit.get('patch')
            if edit['kind'] == 'delete':
                assert identifier not in zones
                fields = None
            else:
                assert identifier in zones
                fields = check(board, zones[identifier], expected)
            mutations.append(dict(kind=edit['kind'], uuid=identifier, fields=fields))
        assert path.read_bytes() == before
        records.append(dict(file=path.name, sha256=hashlib.sha256(before).hexdigest(),
                            mutations=mutations))
    evidence = dict(records=records, exact_intent=True, read_only=True)
    (output / 'independent-saved-intent.json').write_text(json.dumps(evidence, indent=2) + '\n')
    print(json.dumps(dict(verified_saved_files=len(records), exact_intent=True, read_only=True)))


if __name__ == '__main__':
    main()
