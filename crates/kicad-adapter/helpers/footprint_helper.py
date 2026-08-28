import json
import os
import sys

import wx

_app = wx.App(False)
import pcbnew

_io_manager = pcbnew.PCB_IO_MGR()

MAX_INPUT_BYTES = 64 * 1024
REQUIRED = {"board_path", "project_root", "source_path", "library_identifier", "target_id", "position", "reference", "value"}

def fail(message):
    raise RuntimeError(message)

def obj(value, name):
    if not isinstance(value, dict):
        fail(name + " must be an object")
    return value

def main():
    if sys.argv[1:] != ["--fixed-footprint-placement-v1"]:
        fail("unsupported arguments")
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        fail("request too large")
    request = obj(json.loads(raw), "request")
    if set(request) != REQUIRED:
        fail("request fields are not exact")
    paths = [request[key] for key in ("board_path", "project_root", "source_path")]
    if not all(isinstance(path, str) and os.path.isabs(path) for path in paths):
        fail("paths must be absolute")
    board_path, project_root, source_path = [os.path.realpath(path) for path in paths]
    try:
        if os.path.commonpath([board_path, project_root]) != project_root:
            fail("board outside project")
    except ValueError:
        fail("paths use different volumes")
    if not os.path.isdir(project_root):
        fail("project root missing")
    if not os.path.isfile(board_path) or not os.path.isfile(source_path):
        fail("input file missing")
    identifier = obj(request["library_identifier"], "library_identifier")
    if set(identifier) != {"nickname", "entry_name"}:
        fail("library identifier fields are not exact")
    position = obj(request["position"], "position")
    if set(position) != {"x", "y", "unit", "space"}:
        fail("position fields are not exact")
    if position["unit"] != "Nanometres" or position["space"] != "Board":
        fail("position units are invalid")
    if not all(isinstance(position[key], int) for key in ("x", "y")):
        fail("position coordinates must be integers")
    if not all(isinstance(request[key], str) and request[key] for key in ("target_id", "reference", "value")):
        fail("identity fields must be non-empty strings")
    if not hasattr(pcbnew.FOOTPRINT, "GetUUID"):
        fail("KiCad Python wrapper cannot expose footprint UUID")
    board = pcbnew.LoadBoard(board_path)
    source_name = os.path.basename(source_path)
    if not source_name.endswith(".kicad_mod"):
        fail("source is not a KiCad footprint")
    footprint = pcbnew.FootprintLoad(os.path.dirname(source_path), source_name[:-10], True)
    if footprint is None:
        fail("footprint load failed")
    loaded_id = footprint.GetFPID()
    if (str(loaded_id.GetLibNickname()) != identifier["nickname"]
            or str(loaded_id.GetLibItemName()) != identifier["entry_name"]):
        fail("loaded footprint library identity is not proven")
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
        if (str(lib_id.GetLibNickname()) == identifier["nickname"] and str(lib_id.GetLibItemName()) == identifier["entry_name"] and candidate.GetReference() == request["reference"] and candidate.GetValue() == request["value"] and pos.x == position["x"] and pos.y == position["y"]):
            matches.append(candidate)
    if len(matches) != 1:
        fail("footprint identity is not unique")
    candidate = matches[0]
    server_id = candidate.GetUUID().AsString()
    if not server_id:
        fail("server footprint UUID is empty")
    pad_numbers = sorted(str(pad.GetNumber()) for pad in candidate.Pads())
    if not pad_numbers or any(not number for number in pad_numbers):
        fail("pad identity is invalid")
    print(json.dumps({"postcondition": {"footprint_uuid": server_id, "library_identifier": identifier, "reference": candidate.GetReference(), "value": candidate.GetValue(), "position": position, "pad_count": len(pad_numbers), "pad_numbers": pad_numbers, "saved": True, "reopened": True}}, separators=(",", ":")))

if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("footprint helper: " + str(error), file=sys.stderr)
        sys.exit(1)
