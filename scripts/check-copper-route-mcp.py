"""Verify explicit copper routes, rule drift and approval invalidation over stdio MCP."""
import copy
import json
import os
from pathlib import Path
import sys
from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
custom = board.with_suffix(".kicad_dru")
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / "state")), log=str(output / "stdio.log"))
records = []

def call(name, arguments, reject=False):
    try:
        raw = rpc.call("tools/call", {"name": name, "arguments": arguments})
    except RuntimeError as error:
        raw = {"rpc_error": str(error)}
    records.append({"name": name, "arguments": arguments, "response": raw})
    (output / "mcp.json").write_text(json.dumps(records, indent=2))
    error = raw.get("isError") or "rpc_error" in raw or not raw.get("structuredContent", {}).get("ok")
    if reject:
        assert error, raw
        return raw
    assert not error, raw
    return raw["structuredContent"]["data"]

def request(routes, label):
    return dict(endpoint=endpoint, source_run_id="route-acceptance", request_id=label, batch=[{"route": route} for route in routes])

def params(plan, label):
    return dict(plan_id=plan["plan_id"], plan_hash=plan["plan_hash"], request_id=label)

try:
    rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "copper-route-acceptance", "version": "1"}})
    rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = rpc.call("tools/list", {})["tools"]
    assert "route" in next(t for t in tools if t["name"] == "kicad_plan_change")["inputSchema"]["properties"]
    base = call("kicad_inspect_project", dict(endpoint=endpoint))
    snap = base["pcb"]["snapshot"]
    net = next(t["net"] for t in snap["native_routing"]["tracks"] if t["net"]["name"] == "GND")
    net = {"net_id": net["net_id"], "name": net["name"]}
    def point(x,y): return dict(x_nm=x*1000000, y_nm=y*1000000)
    def anchor(n): return dict(kind="via", source_uuid=f"50000000-0000-4000-8000-{n:012}")
    def leg(layer, points): return dict(layer=layer, width_nm=250000, points=[point(*p) for p in points])
    def via(name,x,y): return dict(via_id=name, position=point(x,y), pad_diameter_nm=600000, drill_diameter_nm=300000)
    footprints = {f["reference"]: f for f in snap["native_footprints"]["placements"]}
    fp = footprints["TP1"]
    def pad_anchor(ref):
        f = footprints[ref]
        return dict(kind="pad", pad_id=f["pads"][0]["pad_id"], footprint_placement_id=f["placement_id"])
    power = next(t["net"] for t in snap["native_routing"]["tracks"] if t["net"]["name"] == "VCC")
    power = {"net_id": power["net_id"], "name": power["name"]}
    common = dict(observed_revision=base["pcb"]["revision"], net=net)
    routes = [
        dict(common, route_id="path", start=anchor(1), end=anchor(2), legs=[leg("F.Cu",[(20,20),(20,25),(25,25)]),leg("In1.Cu",[(25,25),(35,25)]),leg("F.Cu",[(35,25),(40,25),(40,20)])], vias=[via("transition-a",25,25),via("transition-b",35,25)]),
        dict(common, route_id="join-track", start=dict(kind="track_end",source_uuid="40000000-0000-4000-8000-000000000001",end="end"), end=anchor(1), legs=[leg("F.Cu",[(15,10),(20,10),(20,20)])], vias=[]),
        dict(common, route_id="join-pad", start=dict(kind="pad",pad_id=fp["pads"][0]["pad_id"],footprint_placement_id=fp["placement_id"]), end=anchor(2), legs=[leg("B.Cu",[(50,20),(45,20),(40,20)])], vias=[]),
    ]
    routes += [
        dict(common, route_id="front-pads", start=pad_anchor("TP2"), end=pad_anchor("TP3"), legs=[leg("F.Cu",[(20,40),(20,45),(25,45)]),leg("In1.Cu",[(25,45),(35,45)]),leg("F.Cu",[(35,45),(40,45),(40,40)])], vias=[via("front-transition-a",25,45),via("front-transition-b",35,45)]),
        dict(common, net=power, route_id="power", start=dict(kind="track_end",source_uuid="40000000-0000-4000-8000-000000000002",end="end"), end=anchor(3), legs=[leg("In1.Cu",[(35,10),(45,10)])], vias=[]),
    ]
    original = board.read_bytes()
    for label in ["missing-transition", "disabled-layer", "wrong-net", "stale", "below-minimum"]:
        invalid = copy.deepcopy(routes)
        if label == "missing-transition": invalid[0]["vias"].pop()
        if label == "disabled-layer": invalid[0]["legs"][1]["layer"] = "In3.Cu"
        if label == "wrong-net": invalid[0]["net"]["net_id"] = "foreign-net"
        if label == "stale": invalid[0]["observed_revision"] = "sha256:" + "0"*64
        if label == "below-minimum": invalid[0]["legs"][0]["width_nm"] = 100000
        call("kicad_plan_change", request(invalid,label), reject=True)
        assert board.read_bytes() == original

    project = board.with_suffix(".kicad_pro")
    original_project = project.read_bytes()
    for drift in ["custom", "minimum", "netclass"]:
        planned = call("kicad_plan_change", request(routes,f"{drift}-drift-plan"))
        assert planned["routing_evidence"]["rules"]["saved"]["constraints"]["min_track_width_nm"] == 200000
        preview = call("kicad_preview_change", params(planned,f"{drift}-drift-preview"))
        assert preview["state"] == "awaiting_approval", preview
        assert board.read_bytes() == original
        approval = dict(params(planned,f"{drift}-drift-commit"), **{key:preview[key] for key in ["approval_hash","validation_evidence_hash"]})
        assert not custom.exists()
        try:
            if drift == "custom":
                custom.write_text("(version 1)\n")
            else:
                changed = json.loads(original_project)
                if drift == "minimum": changed["board"]["design_settings"]["rules"]["min_track_width"] = 0.24
                else: changed["net_settings"]["classes"][0]["track_width"] = 0.35
                project.write_text(json.dumps(changed))
            call("kicad_commit_change", approval, reject=True)
        finally:
            if custom.exists(): custom.unlink()
            project.write_bytes(original_project)
        call("kicad_commit_change", approval, reject=True)
        assert board.read_bytes() == original

    custom.write_text("(version 1)\n(rule \"route-width\" (condition \"A.NetName == 'GND'\") (constraint track_width (min 0.3mm)))\n")
    try:
        planned = call("kicad_plan_change", request(routes,"custom-drc-plan"))
        rejected = call("kicad_preview_change", params(planned,"custom-drc-preview"), reject=True)["structuredContent"]["data"]
        assert rejected["state"] == "rejected", rejected
        drc_rejection = rejected["shadow_validation"]
        assert drc_rejection["delta"]["introduced"], drc_rejection
        assert any(item["rule_type"] == "track_width" for item in drc_rejection["delta"]["introduced"]), drc_rejection
        assert drc_rejection["source_unchanged"] and drc_rejection["shadow_cleaned"]
        assert not rejected["approval_hash"]
        assert board.read_bytes() == original
    finally:
        custom.unlink()

    planned = call("kicad_plan_change", request(routes,"valid-plan"))
    preview = call("kicad_preview_change", params(planned,"valid-preview"))
    assert preview["state"] == "awaiting_approval", preview
    assert board.read_bytes() == original
    approval = dict(params(planned,"valid-commit"), **{key:preview[key] for key in ["approval_hash","validation_evidence_hash"]})
    call("kicad_commit_change",dict(approval,approval_hash="invalid"),reject=True)
    committed = call("kicad_commit_change",approval)
    assert committed["state"] == "committed", committed
    call("kicad_commit_change",approval,reject=True)
    final = call("kicad_inspect_project",dict(endpoint=endpoint))
    routing = final["pcb"]["snapshot"]["native_routing"]
    assert len(routing["tracks"]) == len(snap["native_routing"]["tracks"])+15
    assert len(routing["vias"]) == len(snap["native_routing"]["vias"])+4
    validation = preview["shadow_validation"]
    assert validation["normalized_snapshot_matches"] and validation["source_unchanged"] and validation["shadow_cleaned"]
    track = dict(source_uuid="selected-back-track", layer="B.Cu", start_x_nm=50000000, start_y_nm=20000000, end_x_nm=60000000, end_y_nm=20000000, width_nm=250000,
        start_pad={k:v for k,v in pad_anchor("TP1").items() if k!="kind"}, end_pad={k:v for k,v in pad_anchor("TP4").items() if k!="kind"}, net=net)
    for layer in ["In3.Cu", "Dwgs.User", "F.Cu"]:
        call("kicad_plan_change", dict(endpoint=endpoint, source_run_id="route-acceptance", request_id=f"bad-track-{layer}", track=dict(track,layer=layer)), reject=True)
    planned = call("kicad_plan_change", dict(endpoint=endpoint, source_run_id="route-acceptance", request_id="back-track-plan", track=track))
    track_preview = call("kicad_preview_change", params(planned,"back-track-preview"))
    track_committed = call("kicad_commit_change", dict(params(planned,"back-track-commit"), **{key:track_preview[key] for key in ["approval_hash","validation_evidence_hash"]}))
    assert track_committed["state"] == "committed"
    final = call("kicad_inspect_project",dict(endpoint=endpoint))
    tracks = final["pcb"]["snapshot"]["native_routing"]["tracks"]
    old_ids = {t["source_uuid"] for t in routing["tracks"]}
    selected = [t for t in tracks if t["source_uuid"] not in old_ids]
    assert len(selected) == 1 and selected[0]["layer"] == "BackCopper", selected
    (output/"result.json").write_text(json.dumps(dict(production_stdio=True,kicad_version=base["kicad_version"],tracks_added=16,vias_added=4,front_smd_inner_transition=True,non_default_netclass=True,explicit_width_differs_from_preference=True,selected_back_track=True,custom_rule_drc_rejection=drc_rejection,drift_cases=["custom_rule_presence","saved_minimum","saved_netclass"],anchor_kinds=["pad","track_end","via"],layers=["F.Cu","In1.Cu","B.Cu"],invalid_paths_rejected=True,rule_drift_invalidates_approval=True,restored_rules_cannot_replay_approval=True,invalid_approval_rejected=True,replay_rejected=True,validation=validation,scope="synthetic route MCP acceptance; populated-copy gate remains open"),indent=2))
finally:
    if custom.exists(): custom.unlink()
    (output/"mcp.json").write_text(json.dumps(records,indent=2))
    rpc.close()
