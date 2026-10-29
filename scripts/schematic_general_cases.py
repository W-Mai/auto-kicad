"""Typed intent cases shared by the production stdio acceptance driver."""


def reviewed(plan, connectivity=None):
    native = plan["native_preview"]
    if connectivity is None:
        assert native["connectivity_before"] == native["connectivity_after"]
        connectivity = {}
    return {
        "connectivity": connectivity,
        "removed_markers": [marker for marker in plan["markers_before"] if marker not in plan["markers_after"]],
        "added_markers": [marker for marker in plan["markers_after"] if marker not in plan["markers_before"]],
        "erc": {"introduced": native["erc_delta"]["introduced"], "resolved": native["erc_delta"]["resolved"]},
    }


def run_cases(observe, publish, release):
    observation = observe()
    target = next(unit for unit in observation["units"] if unit["reference"] == "R2")
    file, uuid = target["file"], target["symbol_uuid"]
    release(observation=observation)
    scope = {"kind": "file_all_instances"}

    def apply(name, action, expected=None):
        return publish(name, [{"file": file, "scope": scope, "action": action}], expected)

    apply("custom-property", {"kind": "set_property", "uuid": uuid, "name": "AK_NOTE", "value": "中文\"\\line\nnext\tfield"})
    apply("flags", {"kind": "set_flags", "uuid": uuid, "flags": {"dnp": True, "on_board": False, "in_bom": False}})
    apply("flags-restore", {"kind": "set_flags", "uuid": uuid, "flags": {"dnp": False, "on_board": True, "in_bom": True}})

    def reference_expected(plan):
        removed, added = [], []
        for net in plan["native_preview"]["connectivity_before"]["nets"]:
            name = net["name"].replace("R2-", "R902-")
            if name != net["name"]:
                removed.append(net)
                added.append(dict(net, name=name))
        return reviewed(plan, {"removed_nets": removed, "added_nets": added})

    apply("reference", {"kind": "set_reference", "uuid": uuid, "reference": "R902"}, reference_expected)
    apply("footprint", {"kind": "assign_footprint", "uuid": uuid, "footprint": "Resistor_SMD:R_0603_1608Metric"})
    point = lambda x, y: {"x_nm": x, "y_nm": y}

    def symbol_expected(plan):
        symbol = plan["changes"][0]["uuid"]
        nets = [net for net in plan["native_preview"]["connectivity_after"]["nets"]
                if any(any(unit["symbol_uuid"] == symbol for unit in pin.get("units", [])) for pin in net["pins"])]
        assert len(nets) == 2 and {net["name"] for net in nets} == {"unconnected-(R900-Pad1)", "unconnected-(R900-Pad2)"}
        expected = reviewed(plan, {"added_nets": nets, "added_pins": [pin for net in nets for pin in net["pins"]]})
        assert len(expected["erc"]["introduced"]) == 2 and not expected["erc"]["resolved"]
        assert all("pin_not_connected" in diagnostic["diagnostic_json"] and "R900" in diagnostic["diagnostic_json"] for diagnostic in expected["erc"]["introduced"])
        return expected

    created = apply("symbol-create", {"kind": "create_symbol", "key": "mcp-resistor", "lib_id": "Device:R", "reference": "R900", "value": "1k", "unit": 1,
                                      "transform": {"position": point(609600000, 508000000)}}, symbol_expected)
    symbol = created["changes"][0]["uuid"]
    apply("symbol-transform", {"kind": "transform_symbol", "uuid": symbol,
                               "transform": {"position": point(621030000, 508000000), "rotation": "ccw90", "mirror": "x"}}, reviewed)
    for label_kind in [{"kind": "local"}, {"kind": "global", "shape": "input"}, {"kind": "hierarchical", "shape": "input"}]:
        kind = label_kind["kind"]
        label = {"label": label_kind, "text": "AK_MCP_" + kind.upper(), "position": point(650240000, 508000000)}
        created = apply("label-create-" + kind, {"kind": "create_label", "key": "label-" + kind, "label": label}, reviewed)
        label_uuid = created["changes"][0]["uuid"]
        apply("label-update-" + kind, {"kind": "update_label", "uuid": label_uuid, "label": dict(label, text=label["text"] + "_UPDATED")}, reviewed)
        apply("label-delete-" + kind, {"kind": "delete", "uuid": label_uuid, "object_kind": {"local": "local_label", "global": "global_label", "hierarchical": "hierarchical_label"}[kind]}, reviewed)
    created = apply("wire-create", {"kind": "create_wire", "key": "free-wire", "start": point(711200000, 508000000), "end": point(716280000, 508000000)}, reviewed)
    wire = created["changes"][0]["uuid"]
    apply("wire-update", {"kind": "update_wire", "uuid": wire, "start": point(711200000, 508000000), "end": point(717550000, 508000000)}, reviewed)
    apply("wire-delete", {"kind": "delete", "uuid": wire, "object_kind": "wire"}, reviewed)
    for kind, coordinate in [("junction", 762000000), ("no_connect", 787400000)]:
        created = apply("marker-create-" + kind, {"kind": "create_" + kind, "key": "marker-" + kind, "position": point(coordinate, 508000000)}, reviewed)
        marker = created["changes"][0]["uuid"]
        apply("marker-move-" + kind, {"kind": "move_marker", "uuid": marker, "object_kind": kind, "position": point(coordinate + 1270000, 508000000)}, reviewed)
        apply("marker-delete-" + kind, {"kind": "delete", "uuid": marker, "object_kind": kind}, reviewed)

    def deleted_expected(plan):
        nets = [net for net in plan["native_preview"]["connectivity_before"]["nets"]
                if any(any(unit["symbol_uuid"] == symbol for unit in pin.get("units", [])) for pin in net["pins"])]
        assert len(nets) == 2
        return reviewed(plan, {"removed_nets": nets, "removed_pins": [pin for net in nets for pin in net["pins"]]})

    apply("symbol-delete", {"kind": "delete", "uuid": symbol, "object_kind": "symbol"}, deleted_expected)
    observed = observe()
    contacts = [contact for row in observed["contacts"] for contact in row["contacts"] if contact["symbol_uuid"] == uuid]
    assert len(contacts) == 2
    selected = [row["pin"] for row in observed["contacts"] if any(contact["symbol_uuid"] == uuid for contact in row["contacts"])]
    removed = [net for net in observed["connectivity"]["nets"] if any(pin in selected for pin in net["pins"])]
    assert len(removed) == 2 and sum(net["name"] == "GND" for net in removed) == 1
    joined = {"name": "GND", "class": "Default", "pins": [pin for net in removed for pin in net["pins"]]}
    release(observation=observed)
    apply("independent-electrical-wire", {"kind": "create_wire", "key": "ground-merge", "start": contacts[0]["position"], "end": contacts[1]["position"]},
          lambda plan: reviewed(plan, {"removed_nets": removed, "added_nets": [joined]}))
