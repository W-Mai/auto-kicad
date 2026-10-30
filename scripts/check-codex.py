#!/usr/bin/env python3
"""Verify Codex can initialize the registered auto-kicad MCP and list its tools."""

import argparse
import json
from pathlib import Path
from stdio_rpc import Rpc

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
rpc = Rpc(["codex", "app-server", "--stdio"], log=str(args.output.with_suffix(".log")))
try:
    rpc.call(
        "initialize",
        {
            "clientInfo": {"name": "auto_kicad_acceptance", "version": "1.0"},
            "capabilities": {"experimentalApi": True, "requestAttestation": False},
        },
    )
    rpc.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
    result = rpc.call(
        "mcpServerStatus/list",
        {"serverName": "auto-kicad", "detail": "toolsAndAuthOnly"},
    )
    servers = [s for s in result["data"] if s["name"] == "auto-kicad"]
    assert len(servers) == 1, result
    server = servers[0]
    assert not server["toolsError"], server["toolsError"]
    required = {
        "kicad_capabilities",
        "kicad_inspect_project",
        "kicad_plan_change",
        "kicad_preview_change",
        "kicad_commit_change",
        "kicad_validate_project",
        "kicad_schematic_audit",
        "kicad_plan_schematic_change",
        "kicad_preview_schematic_change",
        "kicad_commit_schematic_change",
        "kicad_plan_recovery",
        "kicad_preview_recovery",
        "kicad_commit_recovery",
    }
    assert required <= set(server["tools"]), sorted(server["tools"])
    args.output.write_text(json.dumps(server, indent=2))
    print("Codex MCP handshake and tool discovery passed:", sorted(server["tools"]))
finally:
    rpc.close()
