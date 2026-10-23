import hashlib
import json
import os
from pathlib import Path
import sys

from stdio_rpc import Rpc

endpoint, board_arg, evidence_arg, binary = sys.argv[1:]
board = Path(board_arg).resolve(strict=True)
evidence = Path(evidence_arg)
env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(evidence / "state"))
rpc = Rpc([binary], env=env, log=str(evidence / "stdio.log"))
records = {}


def call(name, args):
    response = rpc.call("tools/call", {"name": name, "arguments": args})
    if response.get("isError"):
        raise RuntimeError(str(response))
    value = response["structuredContent"]
    if not value["ok"]:
        raise RuntimeError(str(value))
    records[name] = value
    (evidence / "mcp.json").write_text(json.dumps(records, indent=2))
    return value["data"]


try:
    records["initialize"] = rpc.call(
        "initialize",
        {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "auto-kicad-copy-acceptance", "version": "1"},
        },
    )
    rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = rpc.call("tools/list", {})["tools"]
    capabilities = call("kicad_capabilities", {})
    assert sorted(t["name"] for t in tools) == sorted(capabilities["advertised_tools"])
    inspected = call(
        "kicad_inspect_project",
        {"endpoint": endpoint, "project_dir": str(board.parent)},
    )
    baseline = call("kicad_validate_project", {"endpoint": endpoint})
    assert "baseline_id" in baseline, baseline
    original = board.read_bytes()

    def xy(x, y):
        return {"x": x, "y": y, "unit": "Nanometres", "space": "Board"}

    planned = call(
        "kicad_plan_change",
        {
            "endpoint": endpoint,
            "source_run_id": "pawchrona-real-copy",
            "request_id": "pawchrona-marker",
            "segments": [
                {
                    "graphic_id": "acceptance-marker",
                    "start": xy(80000000, 78000000),
                    "end": xy(83000000, 78000000),
                    "width_nm": 100000,
                    "layer": "DwgsUser",
                }
            ],
        },
    )
    assert board.read_bytes() == original
    preview = call(
        "kicad_preview_change",
        {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]},
    )
    assert preview["state"] == "awaiting_approval", preview
    for field in [
        "reopened",
        "normalized_snapshot_matches",
        "source_unchanged",
        "shadow_cleaned",
    ]:
        assert preview["shadow_validation"][field] is True, field
    assert board.read_bytes() == original
    approval = {
        key: preview[key] for key in ["approval_hash", "validation_evidence_hash"]
    }
    approval.update({key: planned[key] for key in ["plan_id", "plan_hash"]})
    rejected = False
    try:
        call("kicad_commit_change", dict(approval, approval_hash="invalid"))
    except RuntimeError:
        rejected = True
    assert rejected and board.read_bytes() == original
    committed = call("kicad_commit_change", approval)
    assert committed["state"] == "committed", committed
    saved = board.read_bytes()
    assert saved != original
    rejected = False
    try:
        call("kicad_commit_change", approval)
    except RuntimeError:
        rejected = True
    assert rejected and board.read_bytes() == saved
    checked = call(
        "kicad_validate_project",
        {"endpoint": endpoint, "baseline_id": baseline["baseline_id"]},
    )
    assert checked["delta"]["introduced"] == [], checked
    assert (
        checked["report"]["unconnected_items"]
        == baseline["report"]["unconnected_items"]
    )
    history = call(
        "kicad_inspect_project",
        {"endpoint": endpoint, "project_dir": str(board.parent)},
    )["execution_history"]
    committed_history = [r for r in history if r["state"] == "committed"]
    assert len(committed_history) == 1, history
    backups = committed_history[0]["backups"]
    assert any(Path(b["saved"]).read_bytes() == original for b in backups)
    for b in backups:
        assert (
            "sha256:" + hashlib.sha256(Path(b["saved"]).read_bytes()).hexdigest()
            == b["sha256"]
        )
    records["acceptance"] = {
        "passed": True,
        "wrong_approval_rejected": True,
        "replay_rejected": True,
        "source_unchanged_during_preview": True,
        "backup_verified": True,
        "baseline_drc": baseline,
        "final_drc": checked,
        "commit": committed,
    }
    (evidence / "mcp.json").write_text(json.dumps(records, indent=2))
    print(
        "Installed stdio MCP: inspect, plan, isolated preview, approval, commit, replay rejection, DRC and backups passed",
        flush=True,
    )
finally:
    rpc.close()
