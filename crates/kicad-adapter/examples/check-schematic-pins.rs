//! Check native component-pin evidence on an explicitly owned source/configuration copy.
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_schematic::{Limits, load_schematic_project, pins::SchematicPinIndex};
use std::{collections::BTreeMap, env, fs, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: check-schematic-pins OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT".into());
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
    let result = serde_json::json!({"passed":true,"components":audit.identities().components().len(),"pins":audit.pins().pins().len(),"unexported_source_pins":audit.pins().unexported().len(),"explicit_no_connect":audit.pins().pins().iter().filter(|p|p.net.explicit_no_connect).count(),"common_pin_groups":audit.pins().pins().iter().filter(|p|p.occurrences.len()>1).count(),"nets":audit.netlist().nets.len(),"negative_controls":controls});
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
