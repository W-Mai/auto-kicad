#!/usr/bin/env python3
"""Qualify bounded schematic editing against the production stdio executable on owned copies."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

from stdio_rpc import Rpc


def cleanup_native_cache(temporary):
    instances = temporary / "org.kicad.kicad/instances"
    if instances.is_dir() and not list(instances.iterdir()):
        instances.rmdir()
    cache = temporary / "org.kicad.kicad"
    if cache.is_dir() and not list(cache.iterdir()):
        cache.rmdir()
    assert not list(temporary.iterdir()), list(temporary.iterdir())


def snapshot(root):
    result = {}
    for path in root.rglob("*"):
        assert not path.is_symlink(), path
        if path.is_file() and path.name != ".auto-kicad-writer.lock":
            result[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("source", type=Path)
    parser.add_argument("config", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--extended", action="store_true")
    parser.add_argument("--controls", action="store_true")
    args = parser.parse_args()
    binary, source, config = (path.resolve(strict=True) for path in (args.binary, args.source, args.config))
    project = source.parent
    for root in (project, config):
        assert (root / ".auto-kicad-audit-owned").read_text() == "schematic-audit-acceptance\n"
    args.output.mkdir()
    output = args.output.resolve(strict=True)
    assert not output.is_relative_to(project) and not output.is_relative_to(config)
    temporary = output / "runtime-tmp"
    temporary.mkdir()
    state = output / "state"
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(state), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE="1")
    before, configuration = snapshot(project), snapshot(config)
    transcript = []
    cases = []
    rpc = Rpc([str(binary)], env=env, log=output / "server.log")

    def call(name, parameters, ok=True):
        response = rpc.call("tools/call", {"name": name, "arguments": parameters}, timeout=600)
        assert not response.get("isError"), response
        envelope = response["structuredContent"]
        transcript.append({"tool": name, "request": parameters, "response": envelope})
        assert envelope["ok"] == ok, envelope
        return envelope["data"]

    def unchanged():
        assert snapshot(project) == before
        assert snapshot(config) == configuration

    def observe():
        baseline = snapshot(project)
        value = call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}})
        assert snapshot(project) == baseline and snapshot(config) == configuration
        return value

    def release(observation=None, plan=None):
        call("kicad_schematic_audit", {"release": {"observation_ids": [observation["observation_id"]] if observation else [], "plan_ids": [plan["plan_id"]] if plan else []}})

    def prepare(observation, edits):
        baseline = snapshot(project)
        value = call("kicad_plan_schematic_change", {"project_root": str(project), "root_schematic": str(source), "source_run_id": "general-stdio", "request_id": "case-" + str(len(transcript)), "general": {"schema_version": 1, "observation_id": observation["observation_id"], "observed_revision": observation["revision"], "edits": edits}})
        assert snapshot(project) == baseline and snapshot(config) == configuration
        return value

    def publish(name, edits, expected=None):
        observed = observe()
        planned = prepare(observed, edits)
        baseline = snapshot(project)
        identity = {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}
        expectations = expected(planned) if expected else None
        declares_changes = expectations and (any(expectations["connectivity"].values())
            or any(expectations[field] for field in ("removed_markers", "added_markers"))
            or any(expectations["erc"].values()))
        if declares_changes:
            call("kicad_preview_schematic_change", identity, ok=False)
            assert snapshot(project) == baseline and snapshot(config) == configuration
        preview = call("kicad_preview_schematic_change", dict(identity, **({"expectations": expectations} if expectations is not None else {})))
        assert snapshot(project) == baseline
        committed = call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]))
        assert committed["state"] == "committed"
        changed = snapshot(project)
        assert set(changed) == set(baseline)
        assert {path for path in baseline if baseline[path] != changed[path]} == {change["file"] for change in planned["changes"]}
        assert snapshot(config) == configuration
        call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]), ok=False)
        release(plan=planned)
        cases.append({"name": name, "physical_files": len({change["file"] for change in planned["changes"]}), "affected_instances": sum(len(change["instance_paths"]) for change in planned["changes"])})
        return planned

    try:
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "general-schematic-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        tools = {tool["name"]: tool for tool in rpc.call("tools/list", {})["tools"]}
        assert {"kicad_schematic_audit", "kicad_plan_schematic_change", "kicad_preview_schematic_change", "kicad_commit_schematic_change"} <= tools.keys()
        assert "general" in tools["kicad_plan_schematic_change"]["inputSchema"]["properties"]
        observation = call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}})
        unchanged()
        targets = {}
        for unit in observation["units"]:
            if unit["reference"].startswith("R"):
                targets.setdefault(unit["file"], unit)
        assert targets
        salt = hashlib.sha256(str(output).encode()).hexdigest()[:8]
        edits = [{"file": file, "scope": {"kind": "file_all_instances"}, "action": {"kind": "set_property", "uuid": unit["symbol_uuid"], "name": "Value", "value": f"AK_MCP_{salt}_{index}"}} for index, (file, unit) in enumerate(sorted(targets.items()))]
        for path in [source, config / "10.0/kicad_common.json", *sorted(project.glob("*.kicad_sym"))]:
            negative = prepare(observation, edits)
            identity = {"plan_id": negative["plan_id"], "plan_hash": negative["plan_hash"]}
            approved = call("kicad_preview_schematic_change", identity)
            saved = path.read_bytes()
            path.write_bytes(saved + b"\n")
            rejected = call("kicad_commit_schematic_change", dict(identity, approval_hash=approved["approval_hash"]), ok=False)
            path.write_bytes(saved)
            assert rejected["state"] == "rejected"
            call("kicad_commit_schematic_change", dict(identity, approval_hash=approved["approval_hash"]), ok=False)
            unchanged()
            release(plan=negative)
            cases.append({"name": "stale-input", "input": str(path), "rejected_before_write": True})
            observation = observe()
        planned = call("kicad_plan_schematic_change", {"project_root": str(project), "root_schematic": str(source), "source_run_id": "general-stdio", "request_id": "properties", "general": {"schema_version": 1, "observation_id": observation["observation_id"], "observed_revision": observation["revision"], "edits": edits}})
        unchanged()
        identity = {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}
        preview = call("kicad_preview_schematic_change", identity)
        unchanged()
        rpc.close()
        cleanup_native_cache(temporary)
        rpc = Rpc([str(binary)], env=env, log=output / "server-restarted.log")
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "general-schematic-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]), ok=False)
        unchanged()
        cases.append({"name": "restart-approval", "rejected_before_write": True})
        observation = observe()
        planned = prepare(observation, edits)
        identity = {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}
        preview = call("kicad_preview_schematic_change", identity)
        call("kicad_commit_schematic_change", dict(identity, approval_hash="wrong"), ok=False)
        unchanged()
        committed = call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]))
        assert committed["state"] == "committed"
        assert snapshot(config) == configuration
        changed = snapshot(project)
        assert {name for name in before if changed[name] != before[name]} == set(targets)
        assert set(changed) == set(before)
        call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]), ok=False)
        assert snapshot(project) == changed
        after = call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}})
        assert after["connectivity"] == observation["connectivity"]
        assert len(after["units"]) == len(observation["units"])
        history = call("kicad_inspect_project", {"endpoint": "", "project_dir": str(project)})
        assert history["execution_history"][0]["state"] == "committed"
        record = history["execution_history"][0]
        assert len(record["publication"]["files"]) == len(targets)
        for artifact in record["evidence"]["artifacts"]:
            saved = Path(artifact["path"]).read_bytes()
            assert hashlib.sha256(saved).hexdigest() == artifact["sha256"]
        cases.append({"name": "properties", "physical_files": len(targets), "native_publication_committed": True})
        release(observation=after, plan=planned)
        if args.controls:
            from schematic_general_controls import run_controls
            run_controls(rpc, call, observe, prepare, publish, release, project, source, snapshot, transcript, cases)
        if args.extended:
            from schematic_general_cases import run_cases
            run_cases(observe, publish, release)
        (output / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
    finally:
        rpc.close()
        (output / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
    cleanup_native_cache(temporary)
    (output / "result.json").write_text(json.dumps({"passed": True, "production_stdio": True, "cases": cases, "extended": args.extended, "private_snapshots_removed": True, "configuration_unchanged": True, "wrong_approval_restart_and_replay_rejected": True, "gui_launched": False}, indent=2) + "\n")
    print((output / "result.json").read_text())


if __name__ == "__main__":
    main()
