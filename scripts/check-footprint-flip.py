"""Verify explicit-side footprint flips through real stdio MCP and KiCad."""
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
    for name in ("binary", "state", "endpoint", "board", "evidence"):
        parser.add_argument(name)
    args = parser.parse_args()
    board = Path(args.board)
    transcript = []
    rpc = Rpc([args.binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=args.state))
    def call(name, parameters, ok=True):
        response = rpc.call("tools/call", {"name":name, "arguments":parameters})
        assert not response.get("isError"), response
        value = response["structuredContent"]
        transcript.append({"tool":name, "response":value})
        assert value["ok"] == ok, value
        return value["data"]
    def inspect():
        return call("kicad_inspect_project", {"endpoint":args.endpoint})["pcb"]
    def target(pcb, reference):
        return next(f for f in pcb["snapshot"]["native_footprints"]["placements"] if f["reference"] == reference)
    def plan(pcb, reference, axis, label, ok=True, **overrides):
        fp = target(pcb, reference)
        request = dict(observed_revision=pcb["revision"], placement_id=fp["placement_id"], source_uuid=fp["source_uuid"], target_side="back" if fp["side"] == "Front" else "front", axis=axis)
        request.update(overrides)
        return call("kicad_plan_change", dict(endpoint=args.endpoint, source_run_id="flip-acceptance", request_id=label, flip_footprint=request), ok)
    def expected(pcb, reference, axis):
        snapshot = copy.deepcopy(pcb["snapshot"])
        fp = target(dict(snapshot=snapshot), reference)
        fp["side"] = "Back" if fp["side"] == "Front" else "Front"
        fp["orientation_mdeg"] = ((0 if axis == "top_bottom" else 180000) - fp["orientation_mdeg"]) % 360000
        layers = {"FrontCopper":"BackCopper", "BackCopper":"FrontCopper", "FrontMask":"BackMask", "BackMask":"FrontMask", "FrontPaste":"BackPaste", "BackPaste":"FrontPaste"}
        order = ["FrontCopper", "BackCopper", "FrontMask", "BackMask", "FrontPaste", "BackPaste"]
        for pad in fp["pads"]:
            pad["orientation_mdeg"] = -pad["orientation_mdeg"] % 360000
            pad["layers"] = sorted([layers[l] for l in pad["layers"]], key=order.index)
            for shape in pad["copper_shapes"]:
                shape["offset"]["y_nm"] *= -1
                shape["trapezoid_delta"]["y_nm"] *= -1
                for side in ("left", "right"):
                    a, b = "chamfer_top_"+side, "chamfer_bottom_"+side
                    shape[a], shape[b] = shape[b], shape[a]
        return snapshot
    try:
        rpc.call("initialize", {"protocolVersion":"2025-03-26", "capabilities":{}, "clientInfo":{"name":"flip-acceptance", "version":"1"}})
        rpc.send({"jsonrpc":"2.0", "method":"notifications/initialized"})
        original = inspect()
        baseline = call("kicad_validate_project", {"endpoint":args.endpoint})
        protected = {board.with_suffix(ext):board.with_suffix(ext).read_bytes() if board.with_suffix(ext).exists() else None for ext in (".kicad_pro", ".kicad_sch", ".kicad_dru")}
        saved = board.read_bytes()
        for label, ref, overrides in [
            ("locked", "J2", {}), ("connected", "J3", {}),
            ("noop", "J1", {"target_side":"front"}),
            ("wrong-identity", "J1", {"source_uuid":target(original,"J4")["source_uuid"]}),
            ("stale", "J1", {"observed_revision":"sha256:"+"0"*64}),
        ]:
            rejected = plan(original, ref, "top_bottom", label, False, **overrides)
            assert {"locked":"target_locked", "connected":"connected_copper", "noop":"already on target side", "wrong-identity":"scope", "stale":"stale_plan"}[label] in rejected.get("reason", ""), rejected
            assert board.read_bytes() == saved
        current = original
        for ref in ("J1", "J4"):
            for axis in ("top_bottom", "left_right"):
                roundtrip = current
                for step in range(2):
                    label = f"{ref}-{axis}-{step}"
                    planned = plan(current, ref, axis, label)
                    params = {key:planned[key] for key in ("plan_id", "plan_hash")}
                    preview = call("kicad_preview_change", params)
                    assert preview["shadow_validation"]["normalized_snapshot_matches"]
                    assert preview["shadow_validation"]["shadow_cleaned"]
                    assert board.read_bytes() == saved
                    approval = dict(params, **{key:preview[key] for key in ("approval_hash", "validation_evidence_hash")})
                    try:
                        call("kicad_commit_change", dict(approval, approval_hash="wrong"))
                        raise AssertionError("wrong approval accepted")
                    except RuntimeError as error:
                        assert json.loads(str(error))["message"] == "approval mismatch"
                    assert board.read_bytes() == saved
                    assert call("kicad_commit_change", approval)["state"] == "committed"
                    after = inspect()
                    assert without_revisions(after["snapshot"]) == without_revisions(expected(current,ref,axis)), {"expected":expected(current,ref,axis),"actual":after}
                    saved = board.read_bytes()
                    try:
                        call("kicad_commit_change", approval)
                        raise AssertionError("replay accepted")
                    except RuntimeError as error:
                        assert json.loads(str(error))["code"] == -32602
                    assert board.read_bytes() == saved
                    assert plan(current,ref,axis,label+"-old",False)["reason"] == "stale_plan"
                    current = after
                assert without_revisions(current["snapshot"]) == without_revisions(roundtrip["snapshot"])
        bad = plan(current, "J5", "top_bottom", "foreign-net-collision")
        rejected = call("kicad_preview_change", {key:bad[key] for key in ("plan_id", "plan_hash")}, False)
        assert rejected["approval_hash"] is None
        assert "DRC" in rejected["shadow_validation"]["reason"], rejected
        assert rejected["shadow_validation"]["source_unchanged"] and rejected["shadow_validation"]["shadow_cleaned"]
        assert board.read_bytes() == saved
        assert inspect()["revision"] == current["revision"]
        final = call("kicad_validate_project", {"endpoint":args.endpoint,"baseline_id":baseline["baseline_id"]})
        assert not final["delta"]["introduced"]
        assert final["report"]["unconnected_items"] <= baseline["report"]["unconnected_items"]
        assert all((p.read_bytes() if p.exists() else None) == v for p,v in protected.items())
        Path(args.evidence).write_text(json.dumps(dict(passed=True, before=original["snapshot"], after=current["snapshot"], baseline_drc=baseline["report"], final_drc=final, transcript=transcript),indent=2)+"\n")
    finally:
        rpc.close()
        if not Path(args.evidence).exists():
            Path(args.evidence).write_text(json.dumps(dict(passed=False, transcript=transcript),indent=2)+"\n")

if __name__ == "__main__":
    main()
