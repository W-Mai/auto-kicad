"""Verify existing-footprint edits through a real stdio server on an owned open PCB."""

import argparse
import copy
import json
import os
import re
from pathlib import Path

from stdio_rpc import Rpc


def without_revisions(value):
    if isinstance(value, dict):
        return {key: without_revisions(item) for key, item in value.items() if key not in {"observed_revision", "revision"}}
    if isinstance(value, list):
        return [without_revisions(item) for item in value]
    return value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("state", type=Path)
    parser.add_argument("endpoint")
    parser.add_argument("board", type=Path)
    parser.add_argument("evidence", type=Path)
    args = parser.parse_args()
    transcript = []
    rpc = Rpc([str(args.binary.resolve(strict=True))], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(args.state)))

    def call(name, parameters, ok=True):
        response = rpc.call("tools/call", {"name": name, "arguments": parameters})
        assert not response.get("isError"), response
        value = response["structuredContent"]
        transcript.append({"tool": name, "response": value})
        assert value["ok"] == ok, {"tool": name, "state": value["data"].get("state"), "reason": value["data"].get("reason"), "shadow_reason": value["data"].get("shadow_validation", {}).get("reason")}
        return value["data"]

    def inspect():
        result = call("kicad_inspect_project", {"endpoint": args.endpoint})
        assert result.get("pcb"), result
        return result["pcb"]

    def plan(pcb, reference, request_id, changes, ok=True):
        target = next(p for p in pcb["snapshot"]["native_footprints"]["placements"] if p["reference"] == reference)
        request = {"placement_id": target["placement_id"], "source_uuid": target["source_uuid"], "observed_revision": pcb["revision"], **changes}
        return call("kicad_plan_change", {"endpoint": args.endpoint, "request_id": request_id, "source_run_id": "native-footprint-acceptance", "move_footprint": request}, ok=ok)

    try:
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "footprint-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        names = {tool["name"] for tool in rpc.call("tools/list", {})["tools"]}
        assert {"kicad_inspect_project", "kicad_plan_change", "kicad_preview_change", "kicad_commit_change"} <= names
        original = inspect()
        baseline = call("kicad_validate_project", {"endpoint": args.endpoint})
        protected = {args.board.with_suffix(ext): args.board.with_suffix(ext).read_bytes() if args.board.with_suffix(ext).exists() else None for ext in (".kicad_pro", ".kicad_sch", ".kicad_dru")}
        source_bytes = args.board.read_bytes()
        unit_metadata = re.findall(rb'\(units\s+\(unit\s+\(name "A"\)\s+\(pins "1"\)\s*\)\s*\)', source_bytes)
        assert len(unit_metadata) == 1, "missing native unit metadata control"
        for reference, reason in [("J2", "target_locked"), ("J3", "connected_copper_requires_separate_edit"), ("J5", "footprint_transform_unavailable")]:
            for field in ({"x_nm": 6_000_000, "y_nm": 6_000_000}, {"orientation_mdeg": 90000}):
                rejection = plan(original, reference, f"reject-{reference}-{len(transcript)}", field, ok=False)
                assert reason in rejection["reason"], rejection
                if reference == "J5":
                    assert "units" in rejection["detail"], rejection
                if reference == "J3":
                    assert len(rejection["affected_ids"]) >= 2, rejection
                assert args.board.read_bytes() == source_bytes
        stale = dict(original, revision="sha256:" + "0" * 64)
        assert plan(stale, "J1", "stale", {"orientation_mdeg": 0}, ok=False)["reason"] == "stale_plan"

        for label, reference, changes in [("translate", "J1", {"x_nm": 6_000_000, "y_nm": 5_000_000}), ("rotate", "J1", {"orientation_mdeg": -270000}), ("back-transform", "J4", {"x_nm": 14_000_000, "y_nm": 10_000_000, "orientation_mdeg": 90000})]:
            before = inspect()
            source_bytes = args.board.read_bytes()
            planned = plan(before, reference, label, changes)
            parameters = {key: planned[key] for key in ("plan_id", "plan_hash")}
            preview = call("kicad_preview_change", parameters)
            assert preview["shadow_validation"]["normalized_snapshot_matches"] is True, preview
            assert args.board.read_bytes() == source_bytes, "preview modified source"
            approval = dict(parameters, **{key: preview[key] for key in ("approval_hash", "validation_evidence_hash")})
            try:
                call("kicad_commit_change", dict(approval, approval_hash="wrong"))
                raise AssertionError("wrong approval was accepted")
            except RuntimeError as error:
                rejection = json.loads(str(error))
                assert rejection["code"] == -32602 and rejection["message"] == "approval mismatch", rejection
                transcript.append({"tool": "kicad_commit_change", "wrong_approval_rejected": rejection})
            assert args.board.read_bytes() == source_bytes
            committed = call("kicad_commit_change", approval)
            assert committed["state"] == "committed", committed
            after = inspect()
            expected = copy.deepcopy(before["snapshot"])
            target = next(p for p in expected["native_footprints"]["placements"] if p["reference"] == reference)
            if "x_nm" in changes:
                target["position"]["x"] = changes["x_nm"]
                target["position"]["y"] = changes["y_nm"]
            if "orientation_mdeg" in changes:
                target["orientation_mdeg"] = changes["orientation_mdeg"] % 360000
            assert without_revisions(after["snapshot"]) == without_revisions(expected), {"expected": expected, "actual": after}
            assert after["revision"] != before["revision"]
            saved = args.board.read_bytes()
            assert all(saved.count(value) == 1 for value in unit_metadata), "unrelated native unit metadata changed"
            try:
                call("kicad_commit_change", approval)
                raise AssertionError("committed approval was reusable")
            except RuntimeError as error:
                rejection = json.loads(str(error))
                assert rejection["code"] == -32602, rejection
                assert rejection["message"] in {"source files changed since planning", "change already committed"}, rejection
                transcript.append({"tool": "kicad_commit_change", "replay_rejected": rejection})
            assert args.board.read_bytes() == saved, "replay changed saved PCB"
            assert plan(before, reference, f"{label}-old-revision", changes, ok=False)["reason"] == "stale_plan"
        before = inspect()
        saved = args.board.read_bytes()
        bad = plan(before, "J1", "foreign-net-collision", {"x_nm": 9_800_000, "y_nm": 5_500_000})
        preview = call("kicad_preview_change", {key: bad[key] for key in ("plan_id", "plan_hash")}, ok=False)
        assert preview["approval_hash"] is None
        assert "DRC" in preview["shadow_validation"]["reason"] or "connectivity" in preview["shadow_validation"]["reason"], preview
        assert preview["shadow_validation"]["source_unchanged"] and preview["shadow_validation"]["shadow_cleaned"], preview
        assert args.board.read_bytes() == saved
        assert inspect()["revision"] == before["revision"]
        final_drc = call("kicad_validate_project", {"endpoint": args.endpoint, "baseline_id": baseline["baseline_id"]})
        assert final_drc["delta"]["introduced"] == [], final_drc
        assert final_drc["report"]["unconnected_items"] <= baseline["report"]["unconnected_items"]
        assert all((path.read_bytes() if path.exists() else None) == content for path, content in protected.items())
        result = {"passed": True, "before": original["snapshot"], "after": inspect()["snapshot"], "baseline_drc": baseline["report"], "final_drc": final_drc, "transcript": transcript, "native_unit_move_rotation_rejected": True, "unrelated_native_units_preserved": True}
        args.evidence.write_text(json.dumps(result, indent=2) + "\n")
    finally:
        rpc.close()
        if not args.evidence.exists():
            args.evidence.write_text(json.dumps({"passed": False, "transcript": transcript}, indent=2) + "\n")


if __name__ == "__main__":
    main()
