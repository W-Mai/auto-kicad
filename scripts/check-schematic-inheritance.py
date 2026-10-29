#!/usr/bin/env python3
"""Qualify inherited symbol creation with actual stdio, native fields/pins/ERC and SVG geometry."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import xml.etree.ElementTree as ET

from schematic_general_cases import reviewed
from stdio_rpc import Rpc


def snapshot(root):
    result = {}
    for path in root.rglob("*"):
        assert not path.is_symlink(), path
        if path.is_file() and path.name != ".auto-kicad-writer.lock":
            result[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
    return result


def children(raw):
    depth, quoted, escaped, start = 0, False, False, None
    for index, char in enumerate(raw):
        if quoted:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char == "(":
            if depth == 1:
                start = index
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 1 and start is not None:
                yield raw[start:index + 1]
                start = None
    assert depth == 0 and not quoted


def svg_digest(raw, pin_texts=None):
    root = ET.fromstring(raw)
    title = root.find("{http://www.w3.org/2000/svg}title")
    title.text, count = re.subn(r"date \d{4}-\d\d-\d\dT\d\d:\d\d:\d\d", "date NATIVE_TIMESTAMP", title.text)
    assert count == 1
    if pin_texts is not None:
        for group in list(root):
            removed = False
            items = list(group)
            for index, item in enumerate(items):
                if item.tag == "{http://www.w3.org/2000/svg}text" and item.text not in pin_texts:
                    assert index + 1 < len(items) and items[index + 1].attrib.get("class") == "stroked-text"
                    group.remove(item)
                    group.remove(items[index + 1])
                    removed = True
            if removed and not list(group):
                root.remove(group)
    for item in root.iter():
        if item.text and item.text.isspace():
            item.text = None
        if item.tail and item.tail.isspace():
            item.tail = None
    return hashlib.sha256(ET.canonicalize(ET.tostring(root, encoding="unicode")).encode()).hexdigest()


def native_svg(cli, source, identifier, original_library, config, output):
    nickname, entry = identifier.split(":")
    cache = next(node for node in children(source.read_text()) if re.match(r"\(lib_symbols\b", node))
    definition = next(node for node in children(cache) if re.match(r"\(symbol\s+" + re.escape(json.dumps(identifier)) + r"\s", node))
    assert "(extends " not in definition
    # These fixtures have no authored graphical text; pin text remains part of the geometry check.
    assert not re.search(r"\((?:text|text_box)\s", definition)
    pin_texts = {json.loads(match) for match in re.findall(r'\((?:name|number)\s+("(?:\\.|[^"\\])*")', definition)}
    definition = re.sub(r'^(\(symbol\s+)"[^"]+"', lambda match: match[1] + json.dumps(entry), definition, count=1)
    own_library = output / (nickname + ".kicad_sym")
    own_library.write_text('(kicad_symbol_lib (version 20251024) (generator "auto-kicad-acceptance")\n' + definition + "\n)\n")
    rendering = {}
    env = dict(os.environ, KICAD_CONFIG_HOME=str(config))
    for name, library in [("original", original_library), ("embedded", own_library)]:
        directory = output / name
        directory.mkdir()
        completed = subprocess.run([str(cli), "sym", "export", "svg", "--symbol", entry, "--include-hidden-pins", "--include-hidden-fields", "-o", str(directory), str(library)], env=env, capture_output=True, timeout=120)
        (output / (name + ".stdout")).write_bytes(completed.stdout)
        (output / (name + ".stderr")).write_bytes(completed.stderr)
        assert completed.returncode == 0, (name, completed.stderr)
        rendering[name] = {path.name: {"raw_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                                     "timestamp_normalized_sha256": svg_digest(path.read_bytes()),
                                     "geometry_sha256": svg_digest(path.read_bytes(), pin_texts)} for path in directory.glob("*.svg")}
        assert rendering[name], (name, completed.stdout)
    assert rendering["original"].keys() == rendering["embedded"].keys()
    for name in rendering["original"]:
        assert rendering["original"][name]["geometry_sha256"] == rendering["embedded"][name]["geometry_sha256"], rendering
    rendering["all_fields_svg_equal"] = all(rendering["original"][name]["timestamp_normalized_sha256"] == rendering["embedded"][name]["timestamp_normalized_sha256"] for name in rendering["original"])
    return rendering


def native_field_control(cli, source, config, output):
    project = output / "project"
    shutil.copytree(source.parent, project)
    own_source = project / source.name
    saved = own_source.read_bytes()
    cache = next(node for node in children(saved.decode()) if re.match(r"\(lib_symbols\b", node))
    definition = next(node for node in children(cache) if node.startswith('(symbol "Inherited_A:Leaf"'))
    changed = definition.replace('(property "BaseNote" "Inherited_A"', '(property "BaseNote" "WRONG_NATIVE_FIELD"')
    assert changed != definition and saved.decode().count(definition) == 1
    env = dict(os.environ, KICAD_CONFIG_HOME=str(config))
    diagnostics = {}
    for name, data in [("baseline", saved), ("wrong-field", saved.decode().replace(definition, changed).encode()), ("restored", saved)]:
        own_source.write_bytes(data)
        report = output / (name + ".json")
        completed = subprocess.run([str(cli), "sch", "erc", "--format", "json", "--severity-all", "-o", str(report), str(own_source)], env=env, capture_output=True, timeout=120)
        (output / (name + ".stdout")).write_bytes(completed.stdout)
        (output / (name + ".stderr")).write_bytes(completed.stderr)
        assert completed.returncode == 0 and report.is_file(), completed.stderr
        parsed = json.loads(report.read_text())
        diagnostics[name] = sorted(json.dumps(row, sort_keys=True) for sheet in parsed["sheets"] for row in sheet["violations"])
    assert diagnostics["restored"] == diagnostics["baseline"]
    introduced = list(diagnostics["wrong-field"])
    for item in diagnostics["baseline"]:
        introduced.remove(item)
    assert len(introduced) == 1 and json.loads(introduced[0])["type"] == "lib_symbol_mismatch" and "U9901" in introduced[0]
    assert own_source.read_bytes() == saved
    return {"one_wrong_inherited_field_detected_by_native_comparator": True, "complete_baseline_restored": True}


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
    temporary = output / "runtime-tmp"
    temporary.mkdir()
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(output / "state"), KICAD_CONFIG_HOME=str(config), TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE="1")
    before, configuration = snapshot(project), snapshot(config)
    transcript, cases = [], []
    rpc = Rpc([str(binary)], env=env, log=output / "server.log")

    def call(name, parameters, ok=True):
        response = rpc.call("tools/call", {"name": name, "arguments": parameters}, timeout=600)
        assert not response.get("isError"), response
        value = response["structuredContent"]
        transcript.append({"tool": name, "request": parameters, "response": value})
        assert value["ok"] == ok, value
        return value["data"]

    def observe():
        return call("kicad_schematic_audit", {"observe": {"project_root": str(project), "root_schematic": str(source)}})

    def plan(identifier, reference, unit, x):
        observation = observe()
        return call("kicad_plan_schematic_change", {"project_root": str(project), "root_schematic": str(source), "source_run_id": "inherited-symbol-acceptance", "request_id": reference,
                    "general": {"schema_version": 1, "observation_id": observation["observation_id"], "observed_revision": observation["revision"],
                                "edits": [{"file": source.name, "scope": {"kind": "file_all_instances"}, "action": {"kind": "create_symbol", "key": reference,
                                           "lib_id": identifier, "reference": reference, "value": reference, "unit": unit,
                                           "transform": {"position": {"x_nm": x, "y_nm": 508000000}}}}]}})

    def approve(planned, reference, numbers, missing_unit=None):
        identity = {"plan_id": planned["plan_id"], "plan_hash": planned["plan_hash"]}
        call("kicad_preview_schematic_change", identity, ok=False)
        uuid = planned["changes"][0]["uuid"]
        nets = [net for net in planned["native_preview"]["connectivity_after"]["nets"]
                if any(any(unit["symbol_uuid"] == uuid for unit in pin.get("units", [])) for pin in net["pins"])]
        assert len(nets) == len(numbers) and all(len(net["pins"]) == 1 and reference in net["name"] for net in nets)
        pins = [pin for net in nets for pin in net["pins"]]
        assert {pin["number"] for pin in pins} == numbers
        expected = reviewed(planned, {"added_nets": nets, "added_pins": pins})
        assert not expected["erc"]["resolved"] and len(expected["erc"]["introduced"]) == len(numbers) + bool(missing_unit)
        for item in expected["erc"]["introduced"]:
            diagnostic = json.loads(item["diagnostic_json"])
            if diagnostic["type"] == "missing_unit":
                assert missing_unit and diagnostic["description"] == f"Symbol {reference} has unplaced units [ {missing_unit} ]"
            else:
                assert diagnostic["type"] == "pin_not_connected" and reference in item["diagnostic_json"]
        return identity, call("kicad_preview_schematic_change", dict(identity, expectations=expected))

    try:
        rpc.call("initialize", {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "inherited-symbol-acceptance", "version": "1"}})
        rpc.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        planned = plan("Inherited_A:Leaf", "U9900", 1, 609600000)
        identity, approval = approve(planned, "U9900", {"1", "2", "3"}, "B")
        library = project / "Inherited_A.kicad_sym"
        saved = library.read_bytes()
        library.write_bytes(saved.replace(b'"BaseNote"', b'"ChangedAncestorNote"'))
        assert library.read_bytes() != saved
        call("kicad_commit_schematic_change", dict(identity, approval_hash=approval["approval_hash"]), ok=False)
        library.write_bytes(saved)
        call("kicad_commit_schematic_change", dict(identity, approval_hash=approval["approval_hash"]), ok=False)
        assert snapshot(project) == before and snapshot(config) == configuration
        call("kicad_schematic_audit", {"release": {"plan_ids": [planned["plan_id"]]}})
        cases.append({"name": "raw-ancestor-drift", "permanent_approval_rejection": True, "source_unchanged": True})
        for index, (identifier, reference, unit, numbers) in enumerate([
            ("Device:Filter_EMI_C", "C9901", 1, {"1", "2", "3"}),
            ("Inherited_A:Leaf", "U9901", 1, {"1", "2", "3"}),
            ("Inherited_B:Leaf", "U9902", 2, {"3", "4", "5"}),
            ("Device:R", "R9903", 1, {"1", "2"}),
        ]):
            baseline = snapshot(project)
            planned = plan(identifier, reference, unit, 609600000 + index * 50800000)
            identity, approval = approve(planned, reference, numbers, {"Inherited_A:Leaf": "B", "Inherited_B:Leaf": "A"}.get(identifier))
            assert snapshot(project) == baseline and snapshot(config) == configuration
            committed = call("kicad_commit_schematic_change", dict(identity, approval_hash=approval["approval_hash"]))
            assert committed["state"] == "committed"
            after = snapshot(project)
            assert set(after) == set(baseline) and {path for path in after if after[path] != baseline[path]} == {source.name}
            observed = observe()
            assert any(row["reference"] == reference and row["unit"] == unit for row in observed["units"])
            if identifier.startswith("Inherited_"):
                uuid = planned["changes"][0]["uuid"]
                contacts = {row["pin"]["number"]: contact for row in observed["contacts"] for contact in row["contacts"] if contact["symbol_uuid"] == uuid}
                x = 609600000 + index * 50800000
                expected_positions = {str(number): {"x_nm": x + offset, "y_nm": 508000000} for number, offset in [(1, -5080000), (2, 5080000), (4, -5080000), (5, 5080000)]}
                expected_positions["3"] = {"x_nm": x, "y_nm": 502920000}
                assert set(contacts) == numbers
                assert all(contact["electrical_type"] == "passive" and contact["position"] == expected_positions[number] for number, contact in contacts.items())
            call("kicad_schematic_audit", {"release": {"observation_ids": [observed["observation_id"]], "plan_ids": [planned["plan_id"]]}})
            call("kicad_commit_schematic_change", dict(identity, approval_hash=approval["approval_hash"]), ok=False)
            cases.append({"name": identifier, "reference": reference, "unit": unit, "pin_numbers": sorted(numbers), "native_committed": True})
        assert snapshot(config) == configuration
    finally:
        rpc.close()
        (output / "transcript.json").write_text(json.dumps(transcript, indent=2) + "\n")
    for directory in [temporary / "org.kicad.kicad/instances", temporary / "org.kicad.kicad"]:
        if directory.is_dir() and not list(directory.iterdir()):
            directory.rmdir()
    assert not list(temporary.iterdir())
    render_config = output / "render-config"
    shutil.copytree(config, render_config)
    cli = Path("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli")
    rendering = {}
    for identifier in ("Device:Filter_EMI_C", "Inherited_A:Leaf", "Inherited_B:Leaf", "Device:R"):
        nickname = identifier.split(":")[0]
        library = (Path("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols") if nickname == "Device" else project) / (nickname + ".kicad_sym")
        destination = output / ("svg-" + identifier.replace(":", "-"))
        destination.mkdir()
        rendering[identifier] = native_svg(cli, source, identifier, library, render_config, destination)
    assert snapshot(config) == configuration
    field_control = native_field_control(cli, source, render_config, output / "field-control")
    assert snapshot(config) == configuration
    result = {"passed": True, "production_stdio": True, "native_version": "10.0.7", "cases": cases,
              "native_svg_geometry_and_bounds": rendering, "native_field_comparison_control": field_control,
              "private_snapshots_removed": True, "configuration_unchanged": True, "gui_launched": False}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
