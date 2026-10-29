"""Verify guarded recovery through the actual stdio executable on a closed test project."""

import argparse
import hashlib
import json
import os
from pathlib import Path

from stdio_rpc import Rpc


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("state", type=Path)
    parser.add_argument("project", type=Path)
    parser.add_argument("attempt_id")
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    project = args.project.resolve(strict=True)
    state = args.state.resolve(strict=True)
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(state))
    transcript = []

    def start():
        rpc = Rpc([str(binary)], env=env)
        try:
            rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "recovery-acceptance", "version": "1"}})
            rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            names = {tool["name"] for tool in rpc.call("tools/list", {})["tools"]}
            assert {"kicad_plan_recovery", "kicad_preview_recovery", "kicad_commit_recovery"} <= names
            return rpc
        except BaseException:
            rpc.close()
            raise

    def call(rpc, name, parameters, ok=True):
        response = rpc.call("tools/call", {"name": name, "arguments": parameters})
        assert not response.get("isError"), response
        value = response["structuredContent"]
        transcript.append({"tool": name, "response": value})
        assert value["ok"] == ok, value
        return value["data"]

    def prepare(rpc):
        plan = call(rpc, "kicad_plan_recovery", {"project_dir": str(project), "attempt_id": args.attempt_id, "action": "restore_backups"})
        parameters = {"project_dir": str(project), "recovery_id": plan["recovery_id"], "recovery_plan_hash": plan["recovery_plan_hash"]}
        preview = call(rpc, "kicad_preview_recovery", parameters)
        return plan, dict(parameters, approval_hash=preview["approval_hash"])

    rpc = start()
    try:
        history_before = call(rpc, "kicad_inspect_project", {"endpoint": "", "project_dir": str(project)})
        assert history_before["observation"] == "durable_history_only"
        plan, approval = prepare(rpc)
        before = {f["original"]: Path(f["original"]).read_bytes() for f in plan["files"]}
        created = {
            source["original"]: Path(source["original"]).read_bytes()
            if Path(source["original"]).exists() else None
            for source in plan.get("created_sources", [])
        }
        if created:
            assert plan["schema_version"] in (3, 4)
            assert plan["created_sources_restore_to"] == {"kind": "absent"}
        staging = {
            source["original"]: Path(source["original"]).read_bytes()
            if Path(source["original"]).exists() else None
            for source in plan.get("staging_files", [])
        }
        if staging:
            assert plan["schema_version"] == 4
            assert plan["staging_files_restore_to"] == {"kind": "absent"}
        call(rpc, "kicad_commit_recovery", dict(approval, approval_hash="wrong"), ok=False)
        assert all(Path(p).read_bytes() == data for p, data in before.items())
        assert all((Path(p).read_bytes() if Path(p).exists() else None) == data for p, data in created.items())
        assert all((Path(p).read_bytes() if Path(p).exists() else None) == data for p, data in staging.items())
    finally:
        rpc.close()
    rpc = start()
    try:
        stale = call(rpc, "kicad_commit_recovery", approval, ok=False)
        assert stale["state"] == "rejected"
        assert all(Path(p).read_bytes() == data for p, data in before.items())
        assert all((Path(p).read_bytes() if Path(p).exists() else None) == data for p, data in created.items())
        assert all((Path(p).read_bytes() if Path(p).exists() else None) == data for p, data in staging.items())
        plan, approval = prepare(rpc)
        restored = call(rpc, "kicad_commit_recovery", approval)
        assert restored["state"] == "restored", restored
        for file in plan["files"]:
            saved = Path(file["original"]).read_bytes()
            assert saved == Path(file["backup"]).read_bytes()
            assert "sha256:" + hashlib.sha256(saved).hexdigest() == file["backup_sha256"]
        for source in plan.get("created_sources", []):
            assert not Path(source["original"]).exists(), source
        for source in plan.get("staging_files", []):
            assert not Path(source["original"]).exists(), source
        replay = call(rpc, "kicad_commit_recovery", approval, ok=False)
        assert replay["state"] == "rejected"
        history_after = call(rpc, "kicad_inspect_project", {"endpoint": "", "project_dir": str(project)})
        assert history_after["execution_history"] == history_before["execution_history"]
        assert history_after["recovery_history"][0]["state"] == "restored"
        args.evidence.write_text(json.dumps({"passed": True, "restored": restored, "restart_rejected": True, "replay_rejected": True, "new_schematic_sources_restored_to_absence": sorted(created), "publication_staging_removed": sorted(staging), "transcript": transcript}, indent=2) + "\n")
    finally:
        rpc.close()


if __name__ == "__main__":
    main()
