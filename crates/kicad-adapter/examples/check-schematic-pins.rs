//! Check native component-pin evidence on an explicitly owned source/configuration copy.
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_schematic::{Limits, load_schematic_project, pins::SchematicPinIndex};
use std::{collections::BTreeMap, env, fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 && !(args.len() == 4 && args[3] == "--connectivity") {
        return Err(
            "usage: check-schematic-pins OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT [--connectivity]"
                .into(),
        );
    }
    let source = fs::canonicalize(&args[0])?;
    let root = source.parent().ok_or("missing source parent")?;
    let config = fs::canonicalize(&args[1])?;
    for directory in [root, &config] {
        if fs::read_to_string(directory.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("missing owned-copy marker".into());
        }
    }
    let output = Path::new(&args[2]);
    fs::create_dir(output)?;
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    let policy = SchematicAuditPolicy::new(
        root,
        &source,
        &config,
        &fs::canonicalize(shared.join("symbols"))?,
        &fs::canonicalize(shared.join("footprints"))?,
        &[],
    )?;
    let audit = SchematicAudit::prepare(&policy)?;
    fs::write(
        output.join("evidence.json"),
        serde_json::to_vec_pretty(&audit.evidence()?)?,
    )?;
    fs::write(output.join("pins.txt"), format!("{:#?}", audit.pins()))?;
    fs::write(
        output.join("connectivity.txt"),
        format!("{:#?}", audit.pins().connectivity()),
    )?;
    fs::write(output.join("erc.json"), audit.erc().raw().get())?;
    let parsed = load_schematic_project(root, &source, "10.0.7", 1, Limits::default())?;
    let bytes = parsed
        .files
        .iter()
        .map(|f| {
            Ok((
                f.artifact.relative_path.clone(),
                fs::read(root.join(&f.artifact.relative_path))?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, std::io::Error>>()?;
    let sources = bytes
        .iter()
        .map(|(name, data)| (name.clone(), data.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let contact = |reference: &str,
                   number: &str,
                   occurrence: &kicad_schematic::pins::PinOccurrence,
                   exported: bool| {
        serde_json::json!({"reference":reference,"number":number,"file":occurrence.source.file,"sheet_path":occurrence.source.sheet_path,"symbol_uuid":occurrence.source.symbol_uuid,"graphical_number":occurrence.graphical_number,"x_nm":occurrence.position.x.nm(),"y_nm":occurrence.position.y.nm(),"library_x_nm":occurrence.library_position.x.nm(),"library_y_nm":occurrence.library_position.y.nm(),"exported":exported})
    };
    let mut contacts = Vec::new();
    for pin in audit.pins().pins() {
        for occurrence in &pin.occurrences {
            contacts.push(contact(&pin.reference, &pin.key.number, occurrence, true));
        }
    }
    for pin in audit.pins().unexported() {
        contacts.push(contact(&pin.reference, &pin.number, &pin.occurrence, false));
    }
    fs::write(
        output.join("contacts.json"),
        serde_json::to_vec_pretty(&contacts)?,
    )?;
    let mut connectivity_summary = serde_json::Value::Null;
    if args.len() == 4 {
        let observed = audit.observe_pin_connectivity()?;
        fs::write(
            output.join("complete-connectivity.txt"),
            format!("{:#?}", observed.connectivity()),
        )?;
        fs::write(
            output.join("probe-netlist.txt"),
            format!("{:#?}", observed.native()),
        )?;
        fs::write(
            output.join("probe-evidence.json"),
            serde_json::to_vec_pretty(observed.evidence())?,
        )?;
        let plan =
            kicad_schematic::probes::PinProbePlan::prepare(&parsed, audit.netlist(), &sources)?;
        let original_refs = audit
            .netlist()
            .components
            .iter()
            .map(|c| c.reference.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let probe = observed
            .native()
            .components
            .iter()
            .find(|c| !original_refs.contains(c.reference.as_str()));
        let mut rejected = Vec::new();
        if let Some(probe) = probe {
            for mode in [
                "missing_probe",
                "duplicate_probe",
                "changed_original_network",
            ] {
                let mut bad = observed.native().clone();
                let net = bad
                    .nets
                    .iter_mut()
                    .find(|n| n.nodes.iter().any(|p| p.reference == probe.reference))
                    .ok_or("probe node absent")?;
                let index = net
                    .nodes
                    .iter()
                    .position(|p| p.reference == probe.reference)
                    .ok_or("probe node absent")?;
                match mode {
                    "missing_probe" => {
                        net.nodes.remove(index);
                    }
                    "duplicate_probe" => net.nodes.push(net.nodes[index].clone()),
                    "changed_original_network" => {
                        let original = bad
                            .nets
                            .iter_mut()
                            .find(|n| {
                                n.nodes
                                    .iter()
                                    .any(|p| original_refs.contains(p.reference.as_str()))
                            })
                            .ok_or("original network absent")?;
                        original.name.push_str("-changed");
                    }
                    _ => unreachable!(),
                }
                if plan.validate(&bad).is_ok() {
                    return Err(format!("accepted {mode}").into());
                }
                rejected.push(mode);
            }
        }
        connectivity_summary = serde_json::json!({"nets":observed.connectivity().nets().len(),"connected_pins":observed.connectivity().nets().iter().map(|n|n.pins.len()).sum::<usize>(),"isolated_no_connect":observed.connectivity().isolated_no_connect().len(),"negative_controls":rejected,"evidence":observed.evidence()});
    }
    let original = audit.netlist();
    let populated = original
        .nets
        .iter()
        .position(|n| !n.nodes.is_empty())
        .ok_or("no pin nodes")?;
    let mut controls = Vec::new();
    for mode in [
        "missing_node",
        "duplicate_membership",
        "wrong_function",
        "wrong_unit_pins",
    ] {
        let mut bad = original.clone();
        match mode {
            "missing_node" => {
                bad.nets[populated].nodes.remove(0);
            }
            "duplicate_membership" => {
                let node = bad.nets[populated].nodes[0].clone();
                bad.nets[populated].nodes.push(node);
            }
            "wrong_function" => {
                bad.nets[populated].nodes[0]
                    .pin_function
                    .push_str("_unexpected");
            }
            "wrong_unit_pins" => {
                bad.components[0].units.as_mut().ok_or("missing units")?[0]
                    .pins
                    .push("unexpected".into());
            }
            _ => unreachable!(),
        }
        if SchematicPinIndex::resolve(&parsed, &bad, &sources).is_ok() {
            return Err(format!("accepted {mode}").into());
        }
        controls.push(mode);
    }
    audit.verify()?;
    let result = serde_json::json!({"passed":true,"components":audit.identities().components().len(),"pins":audit.pins().pins().len(),"unexported_source_pins":audit.pins().unexported().len(),"explicit_no_connect":audit.pins().pins().iter().filter(|p|p.net.explicit_no_connect).count(),"common_pin_groups":audit.pins().pins().iter().filter(|p|p.occurrences.len()>1).count(),"nets":audit.netlist().nets.len(),"negative_controls":controls,"complete_connectivity":connectivity_summary});
    let private = audit.private_root().to_owned();
    drop(audit);
    if private.exists() {
        return Err("private snapshot not cleaned".into());
    }
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
