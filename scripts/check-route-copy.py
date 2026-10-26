"""Accept a supplied explicit path on a populated copy, including unsaved stackup drift."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary, probe, profile_arg = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
profile = json.loads(Path(profile_arg).read_text())
records = []
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / "state")), log=str(output / "stdio.log"))

def call(name, arguments, reject=False):
    try:
        raw = rpc.call("tools/call", {"name":name, "arguments":arguments})
    except RuntimeError as error:
        raw = {"rpc_error":str(error)}
    records.append(dict(name=name, arguments=arguments, response=raw))
    (output / "mcp.json").write_text(json.dumps(records, indent=2))
    error = raw.get("isError") or "rpc_error" in raw or not raw.get("structuredContent", {}).get("ok")
    assert bool(error) == reject, {"tool":name, "expected_rejection":reject, "error":raw.get("rpc_error"), "data":{k:v for k,v in raw.get("structuredContent", {}).get("data", {}).items() if k in ("state", "reason")}, "shadow_reason":raw.get("structuredContent", {}).get("data", {}).get("shadow_validation", {}).get("reason")}
    return raw if reject else raw["structuredContent"]["data"]

def params(plan):
    return {key:plan[key] for key in ("plan_id", "plan_hash")}

def plan(route, label):
    return call("kicad_plan_change", dict(endpoint=endpoint, source_run_id="populated-route", request_id=label, route=route))

def canonical(value):
    if isinstance(value, dict): return {k:canonical(v) for k,v in value.items() if k not in ("revision", "observed_revision")}
    if isinstance(value, list): return [canonical(v) for v in value]
    return value

def drift(action):
    subprocess.run([probe, action, endpoint, str(board), str(output / "stackup-drift.json")], check=True)

try:
    rpc.call("initialize", dict(protocolVersion="2025-03-26", capabilities={}, clientInfo=dict(name="populated-route-acceptance",version="1")))
    rpc.send(dict(jsonrpc="2.0",method="notifications/initialized"))
    before = call("kicad_inspect_project", dict(endpoint=endpoint))["pcb"]
    snapshot = before["snapshot"]
    def anchor(selector):
        fps = [f for f in snapshot["native_footprints"]["placements"] if f["reference"] == selector["reference"]]
        assert len(fps) == 1, selector
        pads = [p for p in fps[0]["pads"] if p["number"] == selector["pad"]]
        assert len(pads) == 1 and pads[0]["net"]["name"] == profile["net"], selector
        return dict(kind="pad", pad_id=pads[0]["pad_id"], footprint_placement_id=fps[0]["placement_id"]), pads[0]["net"]
    start, net = anchor(profile["start"])
    end, end_net = anchor(profile["end"])
    assert net == end_net
    route = dict(route_id="populated-sim-power", observed_revision=before["revision"], start=start, end=end, net=net, legs=profile["legs"], vias=profile["vias"])
    (output / "route.json").write_text(json.dumps(route,indent=2))
    baseline = call("kicad_validate_project", dict(endpoint=endpoint))
    saved = board.read_bytes()
    protected = {board.with_suffix(ext):board.with_suffix(ext).read_bytes() if board.with_suffix(ext).exists() else None for ext in (".kicad_pro", ".kicad_sch", ".kicad_dru")}
    planned = plan(route, "stackup-drift-plan")
    preview = call("kicad_preview_change", params(planned))
    approval = dict(params(planned), **{k:preview[k] for k in ("approval_hash","validation_evidence_hash")})
    assert board.read_bytes() == saved
    try:
        drift("change")
        changed = call("kicad_inspect_project", dict(endpoint=endpoint))["pcb"]
        assert changed["snapshot"]["native_stackup"] != snapshot["native_stackup"]
        assert changed["revision"] != before["revision"]
        assert board.read_bytes() == saved, "drift must be live and unsaved"
        call("kicad_commit_change", approval, reject=True)
    finally:
        drift("restore")
    restored = call("kicad_inspect_project", dict(endpoint=endpoint))["pcb"]
    assert restored == before, "live stackup restoration changed source observation"
    call("kicad_commit_change", approval, reject=True)
    assert board.read_bytes() == saved
    planned = plan(route, "accepted-route-plan")
    preview = call("kicad_preview_change", params(planned))
    assert preview["shadow_validation"]["source_unchanged"] and preview["shadow_validation"]["shadow_cleaned"]
    assert board.read_bytes() == saved
    approval = dict(params(planned), **{k:preview[k] for k in ("approval_hash","validation_evidence_hash")})
    committed = call("kicad_commit_change", approval)
    assert committed["state"] == "committed"
    after = call("kicad_inspect_project", dict(endpoint=endpoint))["pcb"]
    expected = copy.deepcopy(after["snapshot"])
    old = snapshot["native_routing"]
    created = {}
    for key in ("tracks", "vias"):
        ids = {o["source_uuid"] for o in old[key]}
        created[key] = [o for o in expected["native_routing"][key] if o["source_uuid"] not in ids]
        expected["native_routing"][key] = [o for o in expected["native_routing"][key] if o["source_uuid"] in ids]
    assert len(created["tracks"]) == sum(len(leg["points"])-1 for leg in route["legs"])
    assert len(created["vias"]) == len(route["vias"])
    assert canonical(expected) == canonical(snapshot), "unrelated normalized board content changed"
    final = call("kicad_validate_project", dict(endpoint=endpoint, baseline_id=baseline["baseline_id"]))
    assert not final["delta"]["introduced"]
    assert final["report"]["unconnected_items"] < baseline["report"]["unconnected_items"], "route must close a real missing connection"
    assert all((p.read_bytes() if p.exists() else None)==v for p,v in protected.items())
    result = dict(passed=True, profile=profile, baseline=baseline, final=final, created=created, stackup_drift_rejected=True, unsaved_drift_left_source_bytes_unchanged=True, restored_approval_rejected=True, all_unrelated_normalized_content_unchanged=True, protected_files_unchanged=True, source_before_sha256=hashlib.sha256(saved).hexdigest(), source_after_sha256=hashlib.sha256(board.read_bytes()).hexdigest(), footprints=len(snapshot["native_footprints"]["placements"]), pads=sum(len(f["pads"]) for f in snapshot["native_footprints"]["placements"]))
    (output / "result.json").write_text(json.dumps(result,indent=2))
finally:
    rpc.close()
