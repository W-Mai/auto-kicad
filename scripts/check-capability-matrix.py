#!/usr/bin/env python3
"""Validate the checked-in capability matrix against the MCP router."""

import json
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
MATRIX = ROOT / "schemas/capability-matrix.json"
SERVER = ROOT / "crates/auto-kicad/src/lib.rs"
ALLOWED_STATUS = {"verified", "unavailable", "unsupported", "deferred"}
ALLOWED_MODE = {"read", "write"}


def fail(message):
    raise SystemExit(f"capability matrix: {message}")


def main():
    matrix = json.loads(MATRIX.read_text())
    if matrix.get("schema_version") != "1":
        fail("schema_version must be 1")
    advertised = matrix.get("advertised_tools")
    if not isinstance(advertised, dict):
        fail("advertised_tools must be an object")
    always = advertised.get("always")
    conditional = advertised.get("conditional")
    if not isinstance(always, list) or not isinstance(conditional, list):
        fail("advertised_tools.always and conditional must be arrays")
    listed_tools = always + conditional
    if len(set(listed_tools)) != len(listed_tools) or any(
        not isinstance(tool, str) or not tool.startswith("kicad_") for tool in listed_tools
    ):
        fail("tool names must be unique kicad_* strings")
    source_tools = re.findall(r'name = "(kicad_[a-z0-9_]+)"', SERVER.read_text())
    if sorted(source_tools) != sorted(listed_tools):
        fail(f"router tools differ: source={sorted(source_tools)} matrix={sorted(listed_tools)}")

    capabilities = matrix.get("capabilities")
    if not isinstance(capabilities, list) or not capabilities:
        fail("capabilities must be a non-empty array")
    ids = set()
    for capability in capabilities:
        if not isinstance(capability, dict):
            fail("each capability must be an object")
        identifier = capability.get("id")
        if not isinstance(identifier, str) or not identifier or identifier in ids:
            fail(f"invalid or duplicate capability id: {identifier!r}")
        ids.add(identifier)
        if capability.get("domain") not in {"base", "project", "pcb", "schematic", "integration", "release"}:
            fail(f"invalid domain for {identifier}")
        if capability.get("mode") not in ALLOWED_MODE:
            fail(f"invalid mode for {identifier}")
        status = capability.get("status")
        if status not in ALLOWED_STATUS:
            fail(f"invalid status for {identifier}")
        tools = capability.get("mcp_tools")
        if not isinstance(tools, list) or any(tool not in listed_tools for tool in tools):
            fail(f"invalid mcp_tools for {identifier}")
        constraints = capability.get("constraints")
        if not isinstance(constraints, list) or not constraints or any(not item for item in constraints):
            fail(f"constraints must be non-empty for {identifier}")
        evidence = capability.get("evidence")
        if status == "verified":
            if not isinstance(evidence, dict):
                fail(f"verified capability lacks evidence: {identifier}")
            for key in ("fixture", "tested_versions", "command", "record"):
                if not evidence.get(key):
                    fail(f"verified capability lacks evidence.{key}: {identifier}")
            record = ROOT / evidence["record"]
            if not record.is_file():
                fail(f"evidence record does not exist for {identifier}: {record}")
            if not record.resolve().is_relative_to(ROOT):
                fail(f"evidence record escapes repository: {record}")
            if subprocess.run(["git", "check-ignore", "-q", str(record)], cwd=ROOT).returncode == 0:
                fail(f"evidence record is ignored and unavailable in clean checkouts: {record}")
        elif evidence is not None:
            fail(f"non-verified capability must use null evidence: {identifier}")
    print(f"Capability matrix valid: {len(capabilities)} capabilities, {len(listed_tools)} router tools")


if __name__ == "__main__":
    main()
