"""Create one source-bound native footprint template inside an owned private directory."""
import hashlib
import json
import math
from pathlib import Path
import sys
import uuid
import wx

app = wx.App(False)
import pcbnew

MAX_SOURCE = 8 * 1024 * 1024
MAX_ITEMS = 100_000


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read(path, limit):
    require(path.is_file() and not path.is_symlink(), "template input must be a regular file")
    size = path.stat().st_size
    require(0 < size <= limit, "template input exceeds bound")
    value = path.read_bytes()
    require(len(value) == size, "template input changed during read")
    return value


def derived(seed, family, index):
    raw = bytearray(hashlib.sha256(f'auto-kicad-sync-template-v1:{seed}:{family}:{index}'.encode()).digest()[:16])
    raw[6] = (raw[6] & 15) | 128
    raw[8] = (raw[8] & 63) | 128
    return str(uuid.UUID(bytes=bytes(raw)))


def canonical_angle(value):
    require(math.isfinite(value) and abs(value) <= 36000, "invalid native pad angle")
    scaled = value * 1_000_000
    nearest = round(scaled)
    require(abs(scaled - nearest) <= 0.000001, "native pad angle exceeds exact microdegree precision")
    return nearest % 360_000_000


def pad_records(footprint):
    return {pad.m_Uuid.AsString(): dict(number=pad.GetNumber(),
            position_nm=[pad.GetPosition().x, pad.GetPosition().y],
            orientation_microdegrees=canonical_angle(pad.GetOrientationDegrees()),
            layers=[str(pcbnew.LayerName(layer)) for layer in pad.GetLayerSet().Seq()], property=int(pad.GetProperty()))
            for pad in footprint.Pads()}


def main():
    require(len(sys.argv) == 2, "expected one private request file")
    request_path = Path(sys.argv[1])
    root = request_path.parent
    raw = read(request_path, 128 * 1024)
    request = json.loads(raw)
    require(set(request) == {'identifier', 'source_sha256', 'source_size', 'seed', 'footprint_uuid', 'position_nm', 'orientation_mdeg', 'side'}, "invalid template request fields")
    require(pcbnew.GetBuildVersion() == '10.0.7', "unverified native template backend")
    identifier = request['identifier']
    require(set(identifier) == {'nickname', 'entry_name'}, "invalid template identifier")
    for value in identifier.values():
        require(isinstance(value, str) and value and not any(c in value for c in '/\\:'), "invalid template identifier component")
    source = root / 'source.pretty' / (identifier['entry_name'] + '.kicad_mod')
    source_bytes = read(source, MAX_SOURCE)
    require(len(source_bytes) == request['source_size'] and hashlib.sha256(source_bytes).hexdigest() == request['source_sha256'], "template source identity changed")
    seed = request['seed']
    require(isinstance(seed, str) and len(seed) == 64 and all(c in '0123456789abcdef' for c in seed), "invalid template allocation seed")
    root_uuid = str(uuid.UUID(request['footprint_uuid']))
    require(root_uuid == request['footprint_uuid'] and uuid.UUID(root_uuid).int != 0, "invalid template footprint UUID")
    require(isinstance(request['position_nm'], list) and len(request['position_nm']) == 2 and all(type(v) is int and abs(v) <= 2_000_000_000 for v in request['position_nm']), "invalid template position")
    require(type(request['orientation_mdeg']) is int and 0 <= request['orientation_mdeg'] < 360000, "invalid template orientation")
    require(request['side'] in ('front', 'back'), "invalid template side")
    footprint = pcbnew.FootprintLoad(str(source.parent), identifier['entry_name'], True)
    require(footprint is not None and footprint.GetLayer() == pcbnew.F_Cu, "template must load from a front-side source")
    require(str(footprint.GetFPID().GetLibItemName()) == identifier['entry_name'], "native template entry differs")
    require(not list(footprint.Groups()) and not list(footprint.Zones()) and not list(footprint.Points()), "template group/zone/point relationships require separate qualification")
    footprint.SetFPID(pcbnew.LIB_ID(identifier['nickname'], identifier['entry_name']))
    families = [('root', [footprint]), ('field', list(footprint.GetFields())),
                ('graphic', list(footprint.GraphicalItems())), ('pad', list(footprint.Pads()))]
    originals, allocated, receipt = set(), set(), []
    for family, items in families:
        for index, item in enumerate(items):
            original = item.m_Uuid.AsString()
            require(original not in originals, "native template object repeated")
            originals.add(original)
            value = root_uuid if family == 'root' else derived(seed, family, index)
            require(value not in allocated, "template child identity collision")
            allocated.add(value)
            item.SetUuid(pcbnew.KIID(value))
            require(item.m_Uuid.AsString() == value, "native template rejected allocated UUID")
            receipt.append(dict(uuid=value, family=family, index=index, kind=item.GetClass()))
            require(len(receipt) <= MAX_ITEMS, "template item count exceeds bound")
    board = pcbnew.BOARD()
    board.Add(footprint)
    before = root / 'template-before.kicad_pcb'
    after = root / 'template-after.kicad_pcb'
    require(not before.exists() and not after.exists(), "native template outputs already exist")
    before_pads = pad_records(footprint)
    require(pcbnew.SaveBoard(str(before), board), "native template baseline save failed")
    footprint.SetPosition(pcbnew.VECTOR2I(*request['position_nm']))
    if request['side'] == 'back':
        footprint.Flip(footprint.GetPosition(), False)
    footprint.SetOrientation(pcbnew.EDA_ANGLE(request['orientation_mdeg'] / 1000, pcbnew.DEGREES_T))
    placed_pads = pad_records(footprint)
    require(pcbnew.SaveBoard(str(after), board), "native template placement save failed")
    reopened = pcbnew.LoadBoard(str(after))
    matches = list(reopened.GetFootprints())
    require(len(matches) == 1 and matches[0].m_Uuid.AsString() == root_uuid, "native template reload identity differs")
    require(pad_records(matches[0]) == placed_pads, "native template pad properties/geometry changed after reload")
    require(read(source, MAX_SOURCE) == source_bytes and read(request_path, 128 * 1024) == raw, "native template inputs changed")
    print(json.dumps(dict(native_version='10.0.7', request_sha256=hashlib.sha256(raw).hexdigest(),
                          source_sha256=request['source_sha256'], before_sha256=hashlib.sha256(read(before, 128 * 1024 * 1024)).hexdigest(),
                          after_sha256=hashlib.sha256(read(after, 128 * 1024 * 1024)).hexdigest(), receipt=receipt,
                          before_pads=before_pads, placed_pads=placed_pads,
                          position_nm=[matches[0].GetPosition().x, matches[0].GetPosition().y],
                          orientation_degrees=matches[0].GetOrientationDegrees(), side=request['side']), ensure_ascii=False, allow_nan=False), flush=True)


if __name__ == '__main__':
    main()
