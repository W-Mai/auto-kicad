import hashlib
import json
import os
import sys

import wx

_app = wx.App(False)
import pcbnew

MAX_INPUT_BYTES = 64 * 1024
REQUIRED = {
    "board_path",
    "project_root",
    "source_registry_root",
    "source_path",
    "library_identifier",
    "target_id",
    "position",
    "reference",
    "value",
    "source_digest",
    "source_size",
}


def fail(message):
    raise RuntimeError(message)


def obj(value, name):
    if not isinstance(value, dict):
        fail(name + " must be an object")
    return value


def confined_file(path, root, name):
    if not isinstance(path, str) or not os.path.isabs(path):
        fail(name + " must be an absolute path")
    canonical_root = os.path.realpath(root)
    if os.path.islink(path):
        fail(name + " must not be a symlink")
    canonical_path = os.path.realpath(path)
    try:
        confined = os.path.commonpath([canonical_path, canonical_root]) == canonical_root
    except ValueError:
        confined = False
    if not confined or not os.path.isfile(canonical_path):
        fail(name + " is outside its server-owned root or missing")
    return canonical_path


def source_digest(path):
    with open(path, "rb") as source:
        data = source.read()
    return len(data), hashlib.sha256(data).hexdigest()


def main():
    if sys.argv[1:] != ["--fixed-footprint-placement-v2"]:
        fail("unsupported arguments")
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        fail("request too large")
    request = obj(json.loads(raw), "request")
    if set(request) != REQUIRED:
        fail("request fields are not exact")

    shadow_root = os.path.realpath(request["project_root"])
    if not os.path.isdir(shadow_root):
        fail("project root missing")
    board_path = confined_file(request["board_path"], shadow_root, "board_path")
    source_root = os.path.realpath(request["source_registry_root"])
    if not os.path.isdir(source_root):
        fail("source registry root missing")
    source_path = confined_file(request["source_path"], source_root, "source_path")
    if not source_path.endswith(".kicad_mod"):
        fail("source is not a KiCad footprint")

    identifier = obj(request["library_identifier"], "library_identifier")
    if set(identifier) != {"nickname", "entry_name"}:
        fail("library identifier fields are not exact")
    source_entry_name = os.path.basename(source_path)[:-10]
    source_nickname = os.path.basename(os.path.dirname(source_path))
    if (source_entry_name != identifier["entry_name"]
            or source_nickname != identifier["nickname"] + ".pretty"):
        fail("source path does not match library identity")

    size, digest = source_digest(source_path)
    if request["source_size"] != size or request["source_digest"] != digest:
        fail("allowlisted footprint source changed")

    position = obj(request["position"], "position")
    if set(position) != {"x", "y", "unit", "space"}:
        fail("position fields are not exact")
    if position["unit"] != "Nanometres" or position["space"] != "Board":
        fail("position units are invalid")
    if not all(isinstance(position[key], int) for key in ("x", "y")):
        fail("position coordinates must be integers")
    if not all(isinstance(request[key], str) and request[key] for key in ("target_id", "reference", "value")):
        fail("identity fields must be non-empty strings")
    if not all(isinstance(identifier[key], str) and identifier[key] for key in ("nickname", "entry_name")):
        fail("library identity fields must be non-empty strings")

    board = pcbnew.LoadBoard(board_path)
    source_name = os.path.basename(source_path)
    footprint = pcbnew.FootprintLoad(os.path.dirname(source_path), source_name[:-10], True)
    if footprint is None:
        fail("footprint load failed")
    loaded_id = footprint.GetFPID()
    if str(loaded_id.GetLibItemName()) != identifier["entry_name"]:
        fail("loaded footprint library entry is not proven")
    footprint.SetFPID(pcbnew.LIB_ID(identifier["nickname"], identifier["entry_name"]))
    footprint.SetReference(request["reference"])
    footprint.SetValue(request["value"])
    footprint.SetPosition(pcbnew.VECTOR2I(position["x"], position["y"]))
    board.Add(footprint)
    pcbnew.SaveBoard(board_path, board)

    reopened = pcbnew.LoadBoard(board_path)
    matches = []
    for candidate in reopened.GetFootprints():
        lib_id = candidate.GetFPID()
        pos = candidate.GetPosition()
        if (str(lib_id.GetLibNickname()) == identifier["nickname"]
                and str(lib_id.GetLibItemName()) == identifier["entry_name"]
                and candidate.GetReference() == request["reference"]
                and candidate.GetValue() == request["value"]
                and pos.x == position["x"]
                and pos.y == position["y"]):
            matches.append(candidate)
    if len(matches) != 1:
        fail("footprint identity is not unique")
    candidate = matches[0]
    pad_numbers = sorted(str(pad.GetNumber()) for pad in candidate.Pads())
    if not pad_numbers or any(not number for number in pad_numbers):
        fail("pad identity is invalid")
    print(json.dumps({"postcondition": {
        "library_identifier": identifier,
        "reference": candidate.GetReference(),
        "value": candidate.GetValue(),
        "position": position,
        "pad_count": len(pad_numbers),
        "pad_numbers": pad_numbers,
        "saved": True,
        "reopened": True,
        "source_digest": digest,
        "source_size": size,
    }}, separators=(",", ":")))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("footprint helper: " + str(error), file=sys.stderr)
        sys.exit(1)
