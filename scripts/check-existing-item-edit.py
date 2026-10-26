"""Verify existing item edits through stdio MCP on an owned open KiCad board."""

import argparse
import copy
import json
import os
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
        assert value["ok"] == ok, {"tool": name, "data": value["data"]}
        return value["data"]

    def inspect():
        return call("kicad_inspect_project", {"endpoint": args.endpoint})["pcb"]

    def plan(pcb, changes, label, ok=True):
        return call("kicad_plan_change", {"endpoint": args.endpoint, "source_run_id": "existing-item-acceptance", "request_id": label, "edit_existing": {"observed_revision": pcb["revision"], "changes": changes}}, ok)

    def patch(pcb, kind, source, **fields):
        if kind == "graphic_segment":
            target = next(x for x in pcb["snapshot"]["board"]["graphics"] if x["graphic_id"] == source)
            target_id = target["graphic_id"]
        else:
            target = next(x for x in pcb["snapshot"]["native_routing"]["tracks" if kind == "track" else "vias"] if x["source_uuid"] == source)
            target_id = target["track_id" if kind == "track" else "via_id"]
        return dict(kind=kind, target_id=target_id, source_uuid=source, **fields)

    try:
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "existing-item-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        original = inspect()
        baseline = call("kicad_validate_project", {"endpoint": args.endpoint})
        source_bytes = args.board.read_bytes()
        protected = {args.board.with_suffix(ext): args.board.with_suffix(ext).read_bytes() if args.board.with_suffix(ext).exists() else None for ext in (".kicad_pro", ".kicad_sch", ".kicad_dru")}
        track = "40000000-0000-4000-8000-000000000002"
        via = "50000000-0000-4000-8000-000000000001"
        graphic = "30000000-0000-4000-8000-000000000005"
        change = patch(original, "track", track, width_nm=300000)
        for label, changes in [
            ("duplicate", [change, change]),
            ("wrong-identity", [dict(change, source_uuid=via)]),
            ("noop", [dict(change, width_nm=250000)]),
            ("invalid-width", [dict(change, width_nm=0)]),
            ("locked", [patch(original, "track", "40000000-0000-4000-8000-000000000003", width_nm=300000)]),
        ]:
            assert plan(original, changes, label, False).get("reason")
            assert args.board.read_bytes() == source_bytes
        stale = dict(original, revision="sha256:" + "0" * 64)
        assert plan(stale, [change], "stale", False)["reason"] == "stale_plan"
        changes = [dict(change,start_nm={"x":5100000,"y":5000000}), patch(original, "via", via, position_nm={"x":7600000,"y":5000000}, diameter_nm=700000, drill_nm=350000), patch(original, "graphic_segment", graphic, end_nm={"x":4500000,"y":12000000}, width_nm=200000)]
        planned = plan(original, changes, "edit-fields")
        parameters = {key: planned[key] for key in ("plan_id", "plan_hash")}
        preview = call("kicad_preview_change", parameters)
        assert preview["shadow_validation"]["normalized_snapshot_matches"] is True
        assert preview["shadow_validation"]["shadow_cleaned"] is True
        assert args.board.read_bytes() == source_bytes
        approval = dict(parameters, **{key: preview[key] for key in ("approval_hash", "validation_evidence_hash")})
        try:
            call("kicad_commit_change", dict(approval, approval_hash="wrong"))
            raise AssertionError("wrong approval accepted")
        except RuntimeError as error:
            assert json.loads(str(error))["message"] == "approval mismatch"
        assert args.board.read_bytes() == source_bytes
        committed = call("kicad_commit_change", approval)
        assert committed["state"] == "committed"
        after = inspect()
        expected = copy.deepcopy(original["snapshot"])
        next(t for t in expected["native_routing"]["tracks"] if t["source_uuid"] == track)["width_nm"] = 300000
        next(t for t in expected["native_routing"]["tracks"] if t["source_uuid"] == track)["geometry"]["Segment"]["start"]["x"] = 5100000
        via_state = next(v for v in expected["native_routing"]["vias"] if v["source_uuid"] == via)
        via_state["position"]["x"] = 7600000
        stack = via_state["pad_stack"]
        for shape in stack["copper_shapes"]:
            shape["size"] = {"x_nm":700000,"y_nm":700000}
        stack["primary_drill"]["size"] = {"x_nm":350000,"y_nm":350000}
        shape = next(g for g in expected["board"]["graphics"] if g["graphic_id"] == graphic)
        shape["width_nm"] = 200000
        shape["end"]["x"] = 4500000
        assert without_revisions(after["snapshot"]) == without_revisions(expected), {"expected":expected,"actual":after}
        saved = args.board.read_bytes()
        try:
            call("kicad_commit_change", approval)
            raise AssertionError("replayed approval accepted")
        except RuntimeError as error:
            assert json.loads(str(error))["code"] == -32602
        assert args.board.read_bytes() == saved
        assert plan(original, changes, "old-revision", False)["reason"] == "stale_plan"
        for label, change in [
            ("disconnect-pad", patch(after,"track",track,end_nm={"x":9000000,"y":5000000})),
            ("disconnect-via", patch(after,"via",via,position_nm={"x":8000000,"y":5000000})),
        ]:
            bad = plan(after,[change],label)
            rejected = call("kicad_preview_change",{key:bad[key] for key in ("plan_id","plan_hash")},False)
            assert rejected["approval_hash"] is None
            assert "disconnected" in rejected["shadow_validation"]["reason"], rejected
            assert rejected["shadow_validation"]["source_unchanged"] and rejected["shadow_validation"]["shadow_cleaned"]
            assert args.board.read_bytes() == saved
        final = call("kicad_validate_project", {"endpoint":args.endpoint,"baseline_id":baseline["baseline_id"]})
        assert not final["delta"]["introduced"]
        assert final["report"]["unconnected_items"] <= baseline["report"]["unconnected_items"]
        assert all((path.read_bytes() if path.exists() else None) == content for path,content in protected.items())
        args.evidence.write_text(json.dumps({"passed":True,"before":original["snapshot"],"after":inspect()["snapshot"],"baseline_drc":baseline["report"],"final_drc":final,"transcript":transcript},indent=2)+"\n")
    finally:
        rpc.close()
        if not args.evidence.exists():
            args.evidence.write_text(json.dumps({"passed":False,"transcript":transcript},indent=2)+"\n")


if __name__ == "__main__":
    main()
