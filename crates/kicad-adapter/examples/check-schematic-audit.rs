//! Verify private native schematic observations and input-drift rejection on owned copies.
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_checks::erc::CompleteErcReport;
use kicad_schematic::{NetlistLimits, parse_netlist_xml};
use std::{env, fs, path::Path};

fn drift(
    audit: &SchematicAudit,
    path: &Path,
    changed: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let original = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    fs::write(path, changed)?;
    let rejected = audit.verify().is_err();
    if fs::read(path)? != changed {
        return Err("concurrent change; refusing restore".into());
    }
    if let Some(bytes) = original {
        fs::write(path, bytes)?;
    } else {
        fs::remove_file(path)?;
    }
    if !rejected {
        return Err(format!("drift accepted: {}", path.display()).into());
    }
    audit.verify()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: check-schematic-audit OWNED_SCHEMATIC OWNED_CONFIG BASELINE_XML BASELINE_ERC NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let root = source.parent().ok_or("missing parent")?;
    let config = fs::canonicalize(&args[1])?;
    for directory in [root, &config] {
        if fs::read_to_string(directory.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("owned-copy marker missing".into());
        }
    }
    let output = Path::new(&args[4]);
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
    fs::write(output.join("erc.json"), audit.erc().raw().get())?;
    let expected = parse_netlist_xml(&fs::read(&args[2])?, NetlistLimits::default())?;
    if audit.netlist() != &expected {
        return Err("private native netlist differs from source baseline".into());
    }
    let baseline = CompleteErcReport::parse(&fs::read_to_string(&args[3])?)?;
    if !audit.erc().equivalent_to(&baseline)? {
        return Err("private native ERC differs from source baseline".into());
    }
    let mut controls = Vec::new();
    for path in [
        source.clone(),
        source.with_extension("kicad_pro"),
        root.join("sym-lib-table"),
        root.join("fp-lib-table"),
        config.join("10.0/kicad_common.json"),
    ] {
        let mut changed = fs::read(&path)?;
        changed.push(b'\n');
        drift(&audit, &path, &changed)?;
        controls.push(path.display().to_string());
    }
    let evidence = audit.evidence()?;
    for kind in ["symbols", "footprints"] {
        let files = evidence[kind]["files"]
            .as_array()
            .ok_or("missing resource files")?;
        let source_path = files
            .iter()
            .filter_map(|v| v["path"].as_str())
            .map(Path::new)
            .find(|p| p.starts_with(root) && p.is_file());
        if let Some(path) = source_path {
            let mut changed = fs::read(path)?;
            changed.push(b'\n');
            drift(&audit, path, &changed)?;
            controls.push(path.display().to_string());
        }
        let private_files = evidence["private_inputs"]["files"]
            .as_object()
            .ok_or("missing private inputs")?;
        let fragment = format!("/resources/{kind}/");
        let path = private_files
            .keys()
            .find(|p| p.contains(&fragment))
            .ok_or("missing private resource copy")?;
        let mut changed = fs::read(path)?;
        changed.push(b'\n');
        drift(&audit, Path::new(path), &changed)?;
        controls.push(path.clone());
    }
    for path in [
        source.with_extension("kicad_prl"),
        config.join("10.0/audit-added.json"),
        audit.private_root().join("project/audit-added.kicad_sch"),
        audit.private_root().join("config/10.0/audit-added.json"),
    ] {
        drift(&audit, &path, b"{}")?;
        controls.push(path.display().to_string());
    }
    audit.verify()?;
    let private_root = audit.private_root().to_owned();
    let result = serde_json::json!({"passed":true,"components":audit.identities().components().len(),"units":audit.identities().units().len(),"sheets":audit.identities().sheets().len(),"nets":audit.netlist().nets.len(),"erc":audit.erc().summary(),"drift_controls":controls});
    drop(audit);
    if private_root.exists() {
        return Err("private native snapshot was not cleaned".into());
    }
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
