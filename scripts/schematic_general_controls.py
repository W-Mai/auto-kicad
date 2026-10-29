"""Authority, registry and shared-instance controls for production schematic MCP."""
import copy
import json

from schematic_general_cases import reviewed


def run_controls(rpc, call, observe, prepare, publish, release, project, source, snapshot, transcript, cases):
    baseline = snapshot(project)
    observations = [observe() for _ in range(4)]
    rejected = call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}}, ok=False)
    assert "limit" in rejected["reason"]
    for observation in observations:
        release(observation=observation)
    observation = observe()
    unit = next(unit for unit in observation["units"] if unit["reference"].startswith("R"))
    edit = {"file": unit["file"], "scope": {"kind": "file_all_instances"}, "action": {"kind": "set_property", "uuid": unit["symbol_uuid"], "name": "Value", "value": "AK_CONTROL"}}
    request = {"project_root": str(project), "root_schematic": str(source), "source_run_id": "general-controls", "request_id": "controls",
               "general": {"schema_version": 1, "observation_id": observation["observation_id"], "observed_revision": observation["revision"], "edits": [edit]}}
    for field, value in [("schema_version", 2), ("observed_revision", "wrong"), ("observation_id", observations[0]["observation_id"]), ("edits", [edit] * 129)]:
        negative = copy.deepcopy(request)
        negative["general"][field] = value
        call("kicad_plan_schematic_change", negative, ok=False)
    negative = copy.deepcopy(request)
    negative["root_schematic"] = str(source) + ".wrong"
    call("kicad_plan_schematic_change", negative, ok=False)
    for location, key in [("general", "symbol_root"), ("action", "catalog_digest"), ("action", "compatibility_evidence")]:
        negative = copy.deepcopy(request)
        target = negative["general"] if location == "general" else negative["general"]["edits"][0]["action"]
        target[key] = "/forged/server/authority"
        try:
            response = rpc.call("tools/call", {"name": "kicad_plan_schematic_change", "arguments": negative})
        except RuntimeError as error:
            response = json.loads(str(error))
            assert response["code"] == -32602 and "unknown field" in response["message"], response
        else:
            assert response.get("isError") and "unknown field" in json.dumps(response), response
        transcript.append({"tool": "kicad_plan_schematic_change", "request": negative, "schema_rejection": response})
    assert snapshot(project) == baseline
    point = {"x_nm": 900430000, "y_nm": 508000000}
    for action in [
        {"kind": "create_symbol", "key": "default-expectations-symbol", "lib_id": "Device:R", "reference": "R9900", "value": "1k", "unit": 1, "transform": {"position": point}},
        {"kind": "create_no_connect", "key": "default-expectations-marker", "position": point},
    ]:
        planned = prepare(observation, [{"file": source.name, "scope": {"kind": "file_all_instances"}, "action": action}])
        call("kicad_preview_schematic_change", {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}, ok=False)
        release(plan=planned)
        assert snapshot(project) == baseline
    release(observation=observation)
    cases.append({"name": "bounded-session-authority-controls", "observation_limit_and_release": True,
                  "schema_version_revision_path_rejected": True, "forged_authority_rejected": True,
                  "undeclared_pin_network_marker_erc_changes_rejected": True, "source_unchanged": True})
    # Rename one occurrence in a physical child reused by several sheet instances.
    observation = observe()
    target = next(unit for unit in observation["units"] if unit["reference"].startswith("R")
                  and sum(other["file"] == unit["file"] and other["symbol_uuid"] == unit["symbol_uuid"] for other in observation["units"]) > 1)
    old, new = target["reference"], "R9901"
    assert all(unit["reference"] != new for unit in observation["units"])
    release(observation=observation)

    def expected(plan):
        removed, added = [], []
        for net in plan["native_preview"]["connectivity_before"]["nets"]:
            name = net["name"].replace(old + "-", new + "-")
            if name != net["name"]:
                removed.append(net)
                added.append(dict(net, name=name))
        return reviewed(plan, {"removed_nets": removed, "added_nets": added})

    planned = publish("single-instance-reference", [{"file": target["file"], "scope": {"kind": "single_instance", "path": target["sheet_path"]},
                      "action": {"kind": "set_reference", "uuid": target["symbol_uuid"], "reference": new}}], expected)
    assert len(planned["changes"]) == 1 and planned["changes"][0]["instance_paths"] == [target["sheet_path"]]
    after = observe()
    expected_units = [dict(unit, reference=new) if unit == target else unit for unit in observation["units"]]
    assert after["units"] == expected_units
    release(observation=after)
    cases.append({"name": "shared-child-unrelated-instances-preserved", "only_selected_reference_changed": True})
