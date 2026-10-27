//! Replay retained native diagnostic evidence without regenerating a favorable DRC report.
use kicad_checks::{
    CompleteDrcReport, DrcPolicy,
    connectivity::{ClosedConnectivityEvidence, check_saved_connectivity},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
type Error = Box<dyn std::error::Error>;

fn read(path: &Path) -> Result<Value, Error> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn native(board: &Path) -> Result<ClosedConnectivityEvidence, Error> {
    check_saved_connectivity(
        board,
        &DrcPolicy {
            allowed_root: board.parent().ok_or("missing parent")?.to_owned(),
            timeout: Duration::from_secs(60),
        },
    )
    .map_err(|error| format!("{error:?}").into())
}
fn item(proof: &ClosedConnectivityEvidence, id: &str) -> Value {
    let endpoint = proof.endpoint(id).unwrap();
    let [x, y] = endpoint.position_nm();
    json!({"uuid":id,"description":"native test endpoint","pos":{"x":x as f64/1e6,"y":y as f64/1e6}})
}
fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "usage: check-connectivity-drc RETAINED_PAIR_ROOT ISLAND_BOARD NEW_OUTPUT [CONTROL_ROOT]".into(),
        );
    }
    let root = PathBuf::from(&args[0]);
    let island = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let left_result = read(&root.join("preview/result.json"))?;
    let right_result = read(&root.join("source/result.json"))?;
    for field in ["before", "after", "intent_digest"] {
        assert_eq!(
            left_result["candidate"][field],
            right_result["candidate"][field]
        );
    }
    let index: BTreeMap<_, _> = right_result["candidate"]["creations"]
        .as_array()
        .ok_or("missing creations")?
        .iter()
        .map(|item| {
            (
                (
                    item["kind"].as_str().unwrap(),
                    item["logical_id"].as_str().unwrap(),
                ),
                item["native_uuid"].as_str().unwrap(),
            )
        })
        .collect();
    let mapping: BTreeMap<String, String> = left_result["candidate"]["creations"]
        .as_array()
        .ok_or("missing creations")?
        .iter()
        .map(|item| {
            (
                item["native_uuid"].as_str().unwrap().into(),
                index[&(
                    item["kind"].as_str().unwrap(),
                    item["logical_id"].as_str().unwrap(),
                )]
                    .into(),
            )
        })
        .collect();
    assert_eq!(mapping.len(), 9);
    let left = CompleteDrcReport::parse(
        left_result["after"]["raw_json"]
            .as_str()
            .ok_or("missing raw report")?,
    )?;
    let right = CompleteDrcReport::parse(
        right_result["after"]["raw_json"]
            .as_str()
            .ok_or("missing raw report")?,
    )?;
    let left_proof = native(&root.join("preview/after.kicad_pcb"))?;
    let right_proof = native(&root.join("source/after.kicad_pcb"))?;
    assert!(!left.equivalent_to(&right, &mapping)?);
    assert!(left.equivalent_with_closed_connectivity(
        &right,
        &mapping,
        &left_proof,
        &right_proof
    )?);
    let base: Value = serde_json::from_str(left.raw().get())?;
    let canonical = CompleteDrcReport::parse(&serde_json::to_string(&base)?)?;
    let mut controls = Vec::new();
    {
        let mut reject = |name: &str, changed: Value| -> Result<(), Error> {
            let text = serde_json::to_string_pretty(&changed)?;
            let report = CompleteDrcReport::parse(&text)?;
            let result = canonical.equivalent_with_closed_connectivity(
                &report,
                &BTreeMap::new(),
                &left_proof,
                &left_proof,
            );
            assert!(
                !matches!(result, Ok(true)),
                "accepted negative case: {name}"
            );
            fs::write(output.join(format!("negative-{name}.json")), text)?;
            controls.push(json!({"name":name,"result":format!("{result:?}")}));
            Ok(())
        };
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][0]["description"] =
            json!("changed same UUID label");
        reject("same-id-label", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][0]["pos"]["x"] = json!(20.000001);
        reject("wrong-native-position", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][0]["pos"]["x"] = json!(20.0000001);
        reject("fractional-nanometres", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][0]["uuid"] =
            json!("ffffffff-ffff-ffff-ffff-ffffffffffff");
        reject("foreign-id", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][1] =
            changed["unconnected_items"][0]["items"][0].clone();
        reject("duplicate-endpoint", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"]
            .as_array_mut()
            .unwrap()
            .pop();
        reject("missing-endpoint", changed)?;
        let mut changed = base.clone();
        let extra = changed["unconnected_items"][0]["items"][0].clone();
        changed["unconnected_items"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        reject("extra-endpoint", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"].as_array_mut().unwrap().pop();
        reject("missing-diagnostic", changed)?;
        let mut changed = base.clone();
        let extra = changed["unconnected_items"][0].clone();
        changed["unconnected_items"]
            .as_array_mut()
            .unwrap()
            .push(extra);
        reject("extra-diagnostic", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["type"] = json!("clearance");
        reject("wrong-rule", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["severity"] = json!("warning");
        reject("changed-severity", changed)?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["description"] = json!("different diagnostic label");
        reject("diagnostic-label", changed)?;
        let mut changed = base.clone();
        changed["coordinate_units"] = json!("in");
        reject("wrong-units", changed)?;
        let mut changed = base.clone();
        changed["kicad_version"] = json!("10.0.6");
        reject("wrong-version", changed)?;
        for (name, pointer) in [
            ("unknown-root", ""),
            ("unknown-diagnostic", "/unconnected_items/0"),
            ("unknown-endpoint", "/unconnected_items/0/items/0"),
            ("unknown-position", "/unconnected_items/0/items/0/pos"),
        ] {
            let mut changed = base.clone();
            changed
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("future".into(), json!(1));
            reject(name, changed)?;
        }
        let foreign = left_proof
            .endpoints()
            .iter()
            .find(|endpoint| endpoint.net() == "VCC")
            .ok_or("missing foreign net control")?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][1] = item(&left_proof, foreign.uuid());
        reject("different-net", changed)?;
        let id = base["unconnected_items"][0]["items"][0]["uuid"]
            .as_str()
            .unwrap();
        let isolated = left_proof
            .endpoints()
            .iter()
            .find(|endpoint| {
                endpoint.net() == "GND"
                    && endpoint.uuid() != id
                    && left_proof.component(endpoint.uuid()).unwrap().len() == 1
            })
            .ok_or("missing isolated control")?;
        let mut changed = base.clone();
        changed["unconnected_items"][0]["items"][1] = item(&left_proof, isolated.uuid());
        reject("different-component", changed)?;
        for section in ["violations", "schematic_parity"] {
            let mut changed = base.clone();
            let mut diagnostic = base["unconnected_items"][0].clone();
            diagnostic["type"] = json!("clearance");
            changed[section].as_array_mut().unwrap().push(diagnostic);
            reject(section, changed)?;
        }
        let mut changed = base.clone();
        changed["ignored_checks"] = json!(["clearance"]);
        reject("ignored-checks-metadata", changed)?;
    }
    // Preserve raw unknown number tokens even when they represent the same numeric value.
    for pointer in [
        "",
        "/unconnected_items/0",
        "/unconnected_items/0/items/0",
        "/unconnected_items/0/items/0/pos",
    ] {
        let mut extended = base.clone();
        extended
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("raw_number_control".into(), json!(1));
        let text = serde_json::to_string(&extended)?;
        let changed = text.replace("\"raw_number_control\":1", "\"raw_number_control\":1.0");
        assert_ne!(text, changed);
        assert!(
            !CompleteDrcReport::parse(&text)?.equivalent_with_closed_connectivity(
                &CompleteDrcReport::parse(&changed)?,
                &BTreeMap::new(),
                &left_proof,
                &left_proof
            )?
        );
        controls.push(json!({"name":"exact-unknown-number","pointer":pointer,"rejected":true}));
    }
    let route_id = base["unconnected_items"][0]["items"][1]["uuid"]
        .as_str()
        .unwrap();
    let alternate_id = left_proof
        .component(route_id)
        .unwrap()
        .iter()
        .find(|id| id.as_str() != route_id)
        .unwrap();
    for field in ["description", "unknown_control"] {
        let mut original = base.clone();
        original["unconnected_items"] = json!([
            base["unconnected_items"][0].clone(),
            base["unconnected_items"][0].clone()
        ]);
        original["unconnected_items"][0]["items"][1] = item(&left_proof, route_id);
        original["unconnected_items"][1]["items"][1] = item(&left_proof, alternate_id);
        original["unconnected_items"][0]["items"][1][field] = json!("first");
        original["unconnected_items"][1]["items"][1][field] = json!("second");
        let mut changed = original.clone();
        changed["unconnected_items"][0]["items"][1][field] = json!("second");
        changed["unconnected_items"][1]["items"][1][field] = json!("first");
        let a = CompleteDrcReport::parse(&serde_json::to_string(&original)?)?;
        let b = CompleteDrcReport::parse(&serde_json::to_string(&changed)?)?;
        assert!(!a.equivalent_with_closed_connectivity(
            &b,
            &BTreeMap::new(),
            &left_proof,
            &left_proof
        )?);
        controls.push(json!({"name":"shared-identity-fields-cannot-be-permuted", "field":field,"rejected":true}));
    }
    let mut contradictory = base.clone();
    contradictory["unconnected_items"] = json!([
        base["unconnected_items"][0].clone(),
        base["unconnected_items"][0].clone()
    ]);
    contradictory["unconnected_items"][1]["items"][1]["description"] =
        json!("same UUID with conflicting label");
    let contradictory = CompleteDrcReport::parse(&serde_json::to_string(&contradictory)?)?;
    assert!(
        contradictory
            .equivalent_with_closed_connectivity(
                &contradictory,
                &BTreeMap::new(),
                &left_proof,
                &left_proof
            )
            .is_err()
    );
    controls.push(json!({"name":"contradictory-identity-within-one-report","rejected":true}));
    let island_proof = native(&island)?;
    assert_eq!(
        island_proof
            .components()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 2]
    );
    let mut island_report = base.clone();
    island_report["violations"] = json!([]);
    island_report["schematic_parity"] = json!([]);
    island_report["unconnected_items"] = json!([{"type":"unconnected_items","severity":"error","description":"Missing connection between items",
        "items":[item(&island_proof,&island_proof.components()[0][0]),item(&island_proof,&island_proof.components()[1][0])]}]);
    let island_report = CompleteDrcReport::parse(&serde_json::to_string(&island_report)?)?;
    assert!(island_report.equivalent_with_closed_connectivity(
        &island_report,
        &BTreeMap::new(),
        &island_proof,
        &island_proof
    )?);
    let mut bad_island: Value = serde_json::from_str(island_report.raw().get())?;
    bad_island["unconnected_items"][0]["items"][1] =
        item(&island_proof, &island_proof.components()[0][1]);
    assert!(
        island_report
            .equivalent_with_closed_connectivity(
                &CompleteDrcReport::parse(&serde_json::to_string(&bad_island)?)?,
                &BTreeMap::new(),
                &island_proof,
                &island_proof
            )
            .is_err()
    );
    controls.push(json!({"name":"disconnected-islands-sharing-one-zone","distinct_groups_preserved":true,"connected-pair-diagnostic-rejected":true}));
    if let Some(control_root) = args.get(3) {
        let control_root = PathBuf::from(control_root);
        let split = native(&control_root.join("split/board.kicad_pcb"))?;
        assert_eq!(split.endpoints().len(), left_proof.endpoints().len());
        for endpoint in left_proof.endpoints() {
            let other = split
                .endpoint(endpoint.uuid())
                .ok_or("split lost endpoint")?;
            assert_eq!(
                (endpoint.kind(), endpoint.net(), endpoint.position_nm()),
                (other.kind(), other.net(), other.position_nm())
            );
        }
        let result = canonical.equivalent_with_closed_connectivity(
            &canonical,
            &BTreeMap::new(),
            &left_proof,
            &split,
        );
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("component membership"))
        );
        controls.push(json!({"name":"native-component-split-with-unchanged-endpoint-records", "result":format!("{result:?}")}));
        let netless = native(&control_root.join("netless/board.kicad_pcb"))?;
        let a = netless
            .endpoints()
            .iter()
            .find(|item| item.net().is_empty())
            .ok_or("missing netless endpoint")?;
        let b = netless
            .endpoints()
            .iter()
            .find(|item| !item.net().is_empty())
            .ok_or("missing named endpoint")?;
        let mut report = base.clone();
        report["unconnected_items"] = json!([{"type":"unconnected_items","description":"Missing connection between items","severity":"error",
            "items":[item(&netless,a.uuid()),item(&netless,b.uuid())]}]);
        let report = CompleteDrcReport::parse(&serde_json::to_string(&report)?)?;
        let result = report.equivalent_with_closed_connectivity(
            &report,
            &BTreeMap::new(),
            &netless,
            &netless,
        );
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("netless"))
        );
        controls
            .push(json!({"name":"netless-diagnostic-endpoint", "result":format!("{result:?}")}));
        fs::write(
            output.join("split-connectivity.json"),
            serde_json::to_vec_pretty(&split)?,
        )?;
        fs::write(
            output.join("netless-connectivity.json"),
            serde_json::to_vec_pretty(&netless)?,
        )?;
    }
    let mut incomplete_mapping = mapping.clone();
    let missing = mapping
        .keys()
        .find(|key| left_proof.endpoint(key).is_some())
        .unwrap();
    incomplete_mapping.remove(missing);
    assert!(
        left.equivalent_with_closed_connectivity(
            &right,
            &incomplete_mapping,
            &left_proof,
            &right_proof
        )
        .is_err()
    );
    let mut duplicate_mapping = mapping.clone();
    let keys = mapping.keys().take(2).cloned().collect::<Vec<_>>();
    duplicate_mapping.insert(keys[1].clone(), mapping[&keys[0]].clone());
    assert!(
        left.equivalent_with_closed_connectivity(
            &right,
            &duplicate_mapping,
            &left_proof,
            &right_proof
        )
        .is_err()
    );
    controls.push(json!({"name":"incomplete-receipt-map", "rejected":true}));
    controls.push(json!({"name":"duplicate-receipt-target", "rejected":true}));
    let result = json!({"strict_retained_pair_rejected":true,"component_retained_pair_equal":true,"identity_map":mapping,
        "left_connectivity":left_proof,"right_connectivity":right_proof,"island_connectivity":island_proof,"negative_controls":controls,
        "scope":"closed-native semantic replay of original reports; no regenerated DRC, no production MCP approval"});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!(
        "retained native pair accepted with {} negative controls; strict comparison still rejects",
        controls.len()
    );
    Ok(())
}
