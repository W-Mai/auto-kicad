"""Verify read-only native rule evidence through production stdio MCP."""
import json
import os
from pathlib import Path
import sys

from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
project, custom = board.with_suffix(".kicad_pro"), board.with_suffix(".kicad_dru")
original = project.read_bytes()
original_board = board.read_bytes()
assert not custom.exists()
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / "state")), log=str(output / "stdio.log"))
records = {}


def inspect(label, **extra):
    response = rpc.call("tools/call", {"name": "kicad_inspect_project", "arguments": dict(endpoint=endpoint, **extra)})
    records[label] = response
    (output / "mcp.json").write_text(json.dumps(records, indent=2))
    assert not response.get("isError"), response
    value = response["structuredContent"]
    assert value["ok"], value
    return value["data"]


def rejected(label, arguments):
    try:
        result = rpc.call("tools/call", {"name": "kicad_inspect_project", "arguments": arguments})
    except RuntimeError as error:
        records[label] = {"rpc_error": str(error)}
    else:
        records[label] = result
        assert result.get("isError") or not result["structuredContent"]["ok"], result


try:
    records["initialize"] = rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "routing-rule-acceptance", "version": "1"}})
    rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = rpc.call("tools/list", {})["tools"]
    tool = next(tool for tool in tools if tool["name"] == "kicad_inspect_project")
    assert "routing_rules" in tool["inputSchema"]["properties"]
    base = inspect("baseline")
    nets = {track["net"]["name"]: track["net"]["net_id"] for track in base["pcb"]["snapshot"]["native_routing"]["tracks"]}
    assert set(nets) == {"GND", "VCC"}
    query = {"net_ids": list(nets.values())}
    observed = inspect("rules", routing_rules=query)["routing_rules"]
    assert observed["status"] == "observed", observed
    observation = observed["observation"]
    evidence = observation["evidence"]
    assert observation["nets"] == {value: key for key, value in nets.items()}
    assert observation["observed_revision"] == base["pcb"]["revision"]
    assert evidence["saved"]["constraints"] == dict(min_clearance_nm=100000, min_track_width_nm=200000, min_via_diameter_nm=500000, min_via_annular_width_nm=100000, min_through_hole_diameter_nm=200000)
    assert evidence["netclasses"]["VCC"]["preferred_track_width_nm"] == 400000
    assert evidence["netclasses"]["GND"]["preferred_track_width_nm"] == 250000
    assert inspect("repeat", routing_rules=query)["routing_rules"] == observed
    for label, ids in [("empty", []), ("duplicate", [nets["GND"]] * 2), ("too_many", [f"net-{i}" for i in range(65)]), ("oversized", ["x" * 4097]), ("nul", ["bad\0id"])]:
        rejected(label, dict(endpoint=endpoint, routing_rules={"net_ids": ids}))
    rejected("no_endpoint", dict(endpoint="", routing_rules=query))
    rejected("unknown_selector_field", dict(endpoint=endpoint, routing_rules=dict(query, path="/arbitrary")))
    unavailable = inspect("unknown_net", routing_rules={"net_ids": ["foreign-net-id"]})["routing_rules"]
    assert unavailable["status"] == "unavailable" and "observation" not in unavailable

    project.write_text('{"board":{},"board":{}}')
    invalid = inspect("duplicate_project_keys", routing_rules=query)["routing_rules"]
    assert invalid["status"] == "unavailable" and "duplicate" in invalid["reason"]
    project.unlink()
    absent = inspect("absent_project", routing_rules=query)["routing_rules"]
    assert absent["status"] == "observed", absent
    assert not absent["observation"]["evidence"]["saved"]["project"]["present"]
    assert all(v is None for v in absent["observation"]["evidence"]["saved"]["constraints"].values())
    project.write_bytes(original)
    custom.write_text("(version 1)\n")
    changed = inspect("custom_rule_present", routing_rules=query)["routing_rules"]
    assert changed["status"] == "observed" and changed["observation"]["evidence"]["saved"]["custom_rules"]["present"]
    assert changed["observation"]["evidence_digest"] != observation["evidence_digest"]
    custom.unlink()
    assert inspect("restored", routing_rules=query)["routing_rules"] == observed
    assert board.read_bytes() == original_board and project.read_bytes() == original
    (output / "result.json").write_text(json.dumps({"kicad_version": base["kicad_version"], "production_stdio": True, "net_count": 2, "exact_saved_minima": True, "preferred_width_distinct_from_minimum": True, "invalid_selections_rejected": True, "unknown_net_unavailable": True, "duplicate_json_unavailable": True, "missing_constraints_explicit": True, "custom_rule_digest_changed": True, "restored_evidence_equal": True, "source_unchanged": True}, indent=2))
finally:
    project.write_bytes(original)
    custom.unlink(missing_ok=True)
    (output / "mcp.json").write_text(json.dumps(records, indent=2))
    rpc.close()
