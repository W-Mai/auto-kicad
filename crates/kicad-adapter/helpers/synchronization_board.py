"""Read a private native board and retain its complete native saved serialization."""
import hashlib
import json
from pathlib import Path
import sys
import wx

app = wx.App(False)
import pcbnew

MAX_BYTES = 128 * 1024 * 1024
MAX_RECORDS = 100_000


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path):
    require(path.is_file() and not path.is_symlink(), "native snapshot must be a regular file")
    size = path.stat().st_size
    require(0 < size <= MAX_BYTES, "native snapshot exceeds bound")
    data = path.read_bytes()
    require(len(data) == size, "native snapshot changed while reading")
    return data


def main():
    require(len(sys.argv) == 3, "expected private snapshot and private serialization paths")
    board_path, saved_path = map(Path, sys.argv[1:])
    project_path = board_path.with_suffix('.kicad_pro')
    require(board_path.parent == saved_path.parent and not saved_path.exists(), "invalid native output location")
    before, project = read(board_path), read(project_path)
    require(pcbnew.GetBuildVersion() == '10.0.7', "unverified native board backend")
    board = pcbnew.LoadBoard(str(board_path))
    require(board is not None, "native board load failed")
    footprints, pads, copper = [], [], []
    seen = set()

    def identifier(item):
        value = item.m_Uuid.AsString()
        require(len(value) == 36 and value not in seen, "duplicate native board identity")
        seen.add(value)
        require(len(seen) <= MAX_RECORDS, "native item count exceeds bound")
        return value

    for footprint in board.GetFootprints():
        uuid = identifier(footprint)
        library_id = footprint.GetFPID()
        library = str(library_id.GetLibNickname()) + chr(58) + str(library_id.GetLibItemName())
        footprints.append(dict(uuid=uuid, reference=footprint.GetReference(), value=footprint.GetValue(),
                               library=library, path=footprint.GetPath().AsString(),
                               attributes=footprint.GetAttributes(), locked=footprint.IsLocked(),
                               position_nm=[footprint.GetPosition().x, footprint.GetPosition().y],
                               orientation_degrees=footprint.GetOrientationDegrees(), layer=footprint.GetLayer()))
        for pad in footprint.Pads():
            pads.append(dict(uuid=identifier(pad), footprint=uuid, number=pad.GetNumber(), net=pad.GetNetname(),
                             position_nm=[pad.GetPosition().x, pad.GetPosition().y], property=pad.GetProperty(),
                             pin_function=pad.GetPinFunction(), pin_type=pad.GetPinType()))
    copper_items = [*board.GetTracks(), *[item for item in board.GetDrawings() if item.GetClass() == 'PCB_SHAPE' and item.IsOnCopperLayer()]]
    for item in copper_items:
        copper.append(dict(uuid=identifier(item), kind=item.GetClass(), net=item.GetNetname(),
                           locked=item.IsLocked(), position_nm=[item.GetPosition().x, item.GetPosition().y]))
    settings = board.GetDesignSettings().m_NetSettings
    configured_classes = sorted(board.GetAllNetClasses().items(), key=lambda pair: str(pair[0]))
    require(len(configured_classes) <= 512, "native netclass count exceeds bound")
    names = sorted(str(name) for name in board.GetNetsByName().keys() if str(name))
    require(len(names) <= MAX_RECORDS, "native net count exceeds bound")
    netclasses = {}
    def class_identity(value):
        return None if value is None else dict(name=str(value.GetName()), priority=value.GetPriority())
    for name in names:
        effective = settings.GetEffectiveNetClass(name)
        require(effective is not None, "missing effective native netclass")
        fields = {}
        for key in ['Clearance', 'TrackWidth', 'ViaDiameter', 'ViaDrill', 'uViaDiameter',
                    'uViaDrill', 'DiffPairWidth', 'DiffPairGap', 'DiffPairViaGap']:
            fields[key] = dict(defined=bool(getattr(effective, 'Has' + key)()),
                               value_nm=getattr(effective, 'Get' + key)(),
                               parent=class_identity(getattr(effective, 'Get' + key + 'Parent')()))
        netclasses[name] = dict(name=str(effective.GetName()), priority=effective.GetPriority(),
                                description=str(effective.GetDescription()),
                                tuning_profile=dict(defined=bool(effective.HasTuningProfile()), value=str(effective.GetTuningProfile()),
                                                    parent=class_identity(effective.GetTuningProfileParent())),
                                constituents=[class_identity(value) for key, value in configured_classes if effective.ContainsNetclassWithName(key)], fields=fields)
    require(pcbnew.SaveBoard(str(saved_path), board), "native board serialization failed")
    saved = read(saved_path)
    require(read(board_path) == before and read(project_path) == project, "native input bytes changed")
    result = dict(native_version='10.0.7', board_sha256=hashlib.sha256(before).hexdigest(),
                  project_sha256=hashlib.sha256(project).hexdigest(), native_saved_sha256=hashlib.sha256(saved).hexdigest(),
                  netclasses=netclasses, footprints=sorted(footprints, key=lambda f: f['uuid']), pads=sorted(pads, key=lambda p: p['uuid']),
                  copper=sorted(copper, key=lambda c: c['uuid']))
    output = json.dumps(result, ensure_ascii=False, separators=(',', ':'), allow_nan=False)
    require(len(output.encode('utf-8')) <= 64 * 1024 * 1024, "native board report exceeds bound")
    print(output, flush=True)


if __name__ == '__main__':
    main()
