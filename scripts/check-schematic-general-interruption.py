#!/usr/bin/env python3
"""Interrupt an owned production MCP writer and recover through fresh stdio approval."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import threading
import time

from stdio_rpc import Rpc


def snapshot(root):
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in root.rglob("*") if path.is_file() and path.name != ".auto-kicad-writer.lock"}


def process_field(pid, field):
    return subprocess.run(["ps", "-p", str(pid), "-o", field + "="], text=True, capture_output=True, check=False).stdout.strip()


def descendants(pid):
    rows = subprocess.run(["ps", "-axo", "pid=,ppid="], text=True, capture_output=True, check=True).stdout
    parents = {int(parts[0]): int(parts[1]) for line in rows.splitlines() if len(parts := line.split()) == 2}
    owned = {pid}
    while True:
        expanded = owned | {child for child, parent in parents.items() if parent in owned}
        if expanded == owned:
            break
        owned = expanded
    return {child: process_field(child, "lstart") for child in owned - {pid}}


def settle_owned_children(children):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        alive = {pid: stamp for pid, stamp in children.items() if process_field(pid, "lstart") == stamp and process_field(pid, "stat") and not process_field(pid, "stat").startswith("Z")}
        if not alive:
            return
        time.sleep(0.05)
    for pid, stamp in children.items():
        if process_field(pid, "lstart") == stamp and not process_field(pid, "stat").startswith("Z"):
            os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if all(process_field(pid, "lstart") != stamp or not process_field(pid, "stat") or process_field(pid, "stat").startswith("Z") for pid, stamp in children.items()):
            return
        time.sleep(0.05)
    raise AssertionError("an owned native child did not stop")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "source", "config", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    binary, source, config = (path.resolve(strict=True) for path in (args.binary, args.source, args.config))
    project = source.parent
    for root in (project, config):
        assert (root / ".auto-kicad-audit-owned").read_text() == "schematic-audit-acceptance\n"
    args.output.mkdir()
    output = args.output.resolve(strict=True)
    assert not output.is_relative_to(project) and not output.is_relative_to(config)
    temporary, state = output / "runtime-tmp", output / "state"
    temporary.mkdir()
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(state), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE="1")
    before, configuration = snapshot(project), snapshot(config)
    transcript, launches = [], 0
    rpc = None

    def start():
        nonlocal rpc, launches
        launches += 1
        rpc = Rpc([str(binary)], env=env, log=output / f"server-{launches}.log")
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "schematic-interruption-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(name, parameters, ok=True):
        response = rpc.call("tools/call", {"name": name, "arguments": parameters}, timeout=600)
        assert not response.get("isError"), name
        value = response["structuredContent"]
        transcript.append({"tool": name, "request": parameters, "response": value})
        assert value["ok"] == ok, (name, value["data"].get("reason"))
        return value["data"]

    def observe():
        return call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}})

    def plan(observation, value, ok=True):
        unit = next(unit for unit in observation["units"] if unit["reference"].startswith("R"))
        return call("kicad_plan_schematic_change", {"project_root": str(project), "root_schematic": str(source), "source_run_id": "interrupted-stdio", "request_id": "plan-" + value,
            "general": {"schema_version": 1, "observation_id": observation["observation_id"], "observed_revision": observation["revision"], "edits": [{"file": unit["file"], "scope": {"kind": "file_all_instances"}, "action": {"kind": "set_property", "uuid": unit["symbol_uuid"], "name": "Value", "value": value}}]}}, ok=ok)

    try:
        start()
        baseline = observe()
        planned = plan(baseline, "AK_INTERRUPTED_STDIO_" + hashlib.sha256(baseline["observation_id"].encode()).hexdigest()[:16])
        identity = {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}
        preview = call("kicad_preview_schematic_change", identity)
        assert snapshot(project) == before
        completed, reply = threading.Event(), []

        def commit():
            try:
                reply.append(call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"])))
            except BaseException as error:
                reply.append({"connection_error": str(error)})
            finally:
                completed.set()

        worker = threading.Thread(target=commit)
        worker.start()
        deadline, record = time.monotonic() + 120, None
        while time.monotonic() < deadline and not completed.is_set():
            for path in state.rglob("record.json"):
                try:
                    current = json.loads(path.read_text())
                except (FileNotFoundError, json.JSONDecodeError):
                    continue
                if current.get("state") != "in_flight" or current.get("plan_hash") != planned["plan_hash"]:
                    continue
                if any(Path(file["original"]).is_file() and hashlib.sha256(Path(file["original"]).read_bytes()).hexdigest() == file["candidate_sha256"] for file in current.get("publication", {}).get("files", [])):
                    record = current
                    break
            if record:
                break
            time.sleep(0.001)
        assert record is not None, ("writer did not reach an interruptible published state", reply)
        children = descendants(rpc.process.pid)
        rpc.process.kill()
        rpc.process.wait(timeout=10)
        worker.join(timeout=10)
        assert completed.is_set()
        settle_owned_children(children)
        rpc.close()
        rpc = None
        journals = [json.loads(path.read_text()) for path in state.rglob("record.json")]
        record = next(entry for entry in journals if entry.get("attempt_id") == record["attempt_id"])
        assert record["state"] == "in_flight" and record["post_attempt"] is None
        (output / "interrupted-record.json").write_text(json.dumps(record, indent=2) + "\n")
        (output / "owned-child-processes.json").write_text(json.dumps(children, indent=2) + "\n")
        orphaned = [str(path.relative_to(temporary)) for path in temporary.iterdir()]
        (output / "interrupted-private-directories.json").write_text(json.dumps(orphaned, indent=2) + "\n")
        # Every entry is under this acceptance process's exclusive TMPDIR, and owned children have stopped.
        for path in temporary.iterdir():
            if path.is_dir() and not path.is_symlink():
                shutil.rmtree(path)
            else:
                path.unlink()
        start()
        call("kicad_commit_schematic_change", dict(identity, approval_hash=preview["approval_hash"]), ok=False)
        partial = observe()
        blocked = plan(partial, "AK_BLOCKED", ok=False)
        assert "recovery" in blocked["reason"]
        original = Path(record["publication"]["files"][0]["original"])
        candidate_bytes = original.read_bytes()
        original.write_bytes(candidate_bytes + b"\n")
        call("kicad_plan_recovery", {"project_dir": str(project), "attempt_id": record["attempt_id"], "action": "restore_backups"}, ok=False)
        assert original.read_bytes() == candidate_bytes + b"\n"
        original.write_bytes(candidate_bytes)
        rpc.close()
        rpc = None
        subprocess.run(["python3", str(Path(__file__).with_name("check-recovery.py")), str(binary), str(state), str(project), record["attempt_id"], str(output / "mcp-recovery.json")], env=env, check=True)
        assert snapshot(project) == before and snapshot(config) == configuration
        start()
        restored = observe()
        assert restored["connectivity"] == baseline["connectivity"] and restored["objects"] == baseline["objects"]
        subsequent = plan(restored, "AK_AFTER_RECOVERY_" + hashlib.sha256(restored["observation_id"].encode()).hexdigest()[:16])
        identity = {"plan_id": subsequent["plan_id"], "plan_hash": subsequent["plan_hash"]}
        approved = call("kicad_preview_schematic_change", identity)
        committed = call("kicad_commit_schematic_change", dict(identity, approval_hash=approved["approval_hash"]))
        assert committed["state"] == "committed" and snapshot(config) == configuration
    finally:
        if rpc:
            rpc.close()
        (output / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
    from importlib.util import spec_from_file_location, module_from_spec
    spec = spec_from_file_location("general_acceptance", Path(__file__).with_name("check-schematic-general.py"))
    module = module_from_spec(spec)
    spec.loader.exec_module(module)
    module.cleanup_native_cache(temporary)
    result = {"passed": True, "actual_stdio_writer_killed": True, "missing_terminal_and_post_evidence": True, "fresh_process_plan_blocked": True, "later_user_bytes_preserved": True,
              "actual_stdio_guarded_recovery": True, "exact_original_and_native_observation_restored": True, "subsequent_native_commit": True, "owned_children_stopped": True, "owned_private_directories_removed": True, "gui_launched": False}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
