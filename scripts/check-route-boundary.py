"""Accept the full additive route/graphics bound through production stdio MCP."""
import copy
import json
import os
from pathlib import Path
import sys
from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / "state")), log=str(output / "stdio.log"))
records = []


def call(name, arguments, reject=False):
    try:
        response = rpc.call("tools/call", {"name": name, "arguments": arguments})
    except RuntimeError as error:
        response = {"rpc_error": str(error)}
    records.append(dict(name=name, arguments=arguments, response=response))
    (output / "mcp.json").write_text(json.dumps(records, indent=2))
    error = "rpc_error" in response or response.get("isError") or not response.get("structuredContent", {}).get("ok")
    assert bool(error) == reject, response
    return response if reject else response["structuredContent"]["data"]


def canonical(value):
    if isinstance(value, dict):
        return {k: canonical(v) for k, v in sorted(value.items()) if k not in ("revision", "observed_revision")}
    if isinstance(value, list):
        return sorted((canonical(v) for v in value), key=lambda v: json.dumps(v, sort_keys=True))
    return value


try:
    rpc.call("initialize", dict(protocolVersion="2025-03-26", capabilities={}, clientInfo=dict(name="route-boundary-acceptance", version="1")))
    rpc.send(dict(jsonrpc="2.0", method="notifications/initialized"))
    observed = call("kicad_inspect_project", dict(endpoint=endpoint))
    pcb = observed["pcb"]
    snapshot = pcb["snapshot"]
    baseline = call("kicad_validate_project", dict(endpoint=endpoint))
    saved = board.read_bytes()
    protected = {board.with_suffix(ext): board.with_suffix(ext).read_bytes() if board.with_suffix(ext).exists() else None for ext in (".kicad_pro", ".kicad_sch", ".kicad_dru")}
    net = next(t["net"] for t in snapshot["native_routing"]["tracks"] if t["net"]["name"] == "GND")
    net = {k: net[k] for k in ("net_id", "name")}
    footprints = {f["reference"]: f for f in snapshot["native_footprints"]["placements"]}

    def pad(ref):
        fp = footprints[ref]
        return dict(kind="pad", pad_id=fp["pads"][0]["pad_id"], footprint_placement_id=fp["placement_id"])

    def route(label, start, end, y):
        return dict(route=dict(route_id=label, observed_revision=pcb["revision"], net=net, start=start, end=end, vias=[], legs=[dict(layer="F.Cu", width_nm=250000, points=[dict(x_nm=20000000 + i * 156250, y_nm=y) for i in range(129)])]))

    def coord(x, y):
        return dict(x=x, y=y, unit="Nanometres", space="Board")

    batch = [
        route("via-maximum", dict(kind="via", source_uuid="50000000-0000-4000-8000-000000000001"), dict(kind="via", source_uuid="50000000-0000-4000-8000-000000000002"), 20000000),
        route("pad-maximum", pad("TP2"), pad("TP3"), 40000000),
        dict(outline=dict(top_left_x_nm=80000000, top_left_y_nm=60000000, bottom_right_x_nm=90000000, bottom_right_y_nm=70000000, width_nm=50000)),
        dict(segments=[dict(graphic_id=f"drawing-{i}", start=coord(5000000 + (i % 24) * 350000, 60000000 + (i // 24) * 2000000), end=coord(5200000 + (i % 24) * 350000, 60000000 + (i // 24) * 2000000), width_nm=100000, layer="DwgsUser") for i in range(96)]),
    ]
    request = dict(endpoint=endpoint, source_run_id="boundary-acceptance", request_id="boundary-plan", batch=batch)
    # The approved maximum must remain distinct from an excessive copper batch.
    excessive = copy.deepcopy(request)
    extra = copy.deepcopy(batch[1])
    extra["route"]["route_id"] = "one-too-many"
    extra["route"]["legs"][0]["points"] = [extra["route"]["legs"][0]["points"][0], extra["route"]["legs"][0]["points"][-1]]
    excessive["batch"].append(extra)
    excessive["request_id"] = "excessive-plan"
    rejected = call("kicad_plan_change", excessive, reject=True)
    assert "batch copper count exceeds 256" in json.dumps(rejected), rejected
    assert board.read_bytes() == saved
    planned = call("kicad_plan_change", request)
    params = dict(plan_id=planned["plan_id"], plan_hash=planned["plan_hash"], request_id="boundary-preview")
    preview = call("kicad_preview_change", params)
    assert preview["state"] == "awaiting_approval", preview
    validation = preview["shadow_validation"]
    assert all(validation[k] for k in ("source_unchanged", "shadow_cleaned", "normalized_snapshot_matches"))
    assert board.read_bytes() == saved
    approval = dict(params, request_id="boundary-commit", **{k: preview[k] for k in ("approval_hash", "validation_evidence_hash")})
    committed = call("kicad_commit_change", approval)
    assert committed["state"] == "committed", committed
    after = call("kicad_inspect_project", dict(endpoint=endpoint))["pcb"]["snapshot"]
    filtered = copy.deepcopy(after)
    old_tracks = {t["source_uuid"] for t in snapshot["native_routing"]["tracks"]}
    created_tracks = [t for t in after["native_routing"]["tracks"] if t["source_uuid"] not in old_tracks]
    assert len(created_tracks) == 256
    filtered["native_routing"]["tracks"] = [t for t in filtered["native_routing"]["tracks"] if t["source_uuid"] in old_tracks]
    old_graphics = {g["graphic_id"] for g in snapshot["board"]["graphics"]}
    added_graphics = [g for g in after["board"]["graphics"] if g["graphic_id"] not in old_graphics]
    assert len(added_graphics) == 100
    assert sum(g["layer"] == "EdgeCuts" for g in added_graphics) == 4
    filtered["board"]["graphics"] = [g for g in filtered["board"]["graphics"] if g["graphic_id"] in old_graphics]
    assert canonical(filtered) == canonical(snapshot), "unrelated normalized state changed"
    final = call("kicad_validate_project", dict(endpoint=endpoint, baseline_id=baseline["baseline_id"]))
    assert not final["delta"]["introduced"], final
    assert final["report"]["unconnected_items"] < baseline["report"]["unconnected_items"]
    assert all((p.read_bytes() if p.exists() else None) == v for p, v in protected.items())
    (output / "result.json").write_text(json.dumps(dict(passed=True, production_stdio=True, kicad_version=observed["kicad_version"], tracks_added=256, graphics_added=100, edge_cuts_added=4, excessive_copper_rejected=True, unrelated_normalized_state_preserved=True, protected_files_preserved=True, baseline=baseline, final=final, validation=validation), indent=2))
finally:
    rpc.close()
