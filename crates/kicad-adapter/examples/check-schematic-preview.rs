//! Validate complete native edit previews on explicitly owned project/configuration copies.
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_schematic::editing::{
    EditAction, EditScope, Label, LabelKind, NewObject, SchematicEdit, SymbolCreation, SymbolFlags,
    SymbolInstancePath, SymbolProperty,
};
use kicad_schematic::geometry::{Mirror, Point, QuarterTurn, SymbolTransform};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut stack = vec![root.to_owned()];
    let mut result = BTreeMap::new();
    let mut total = 0usize;
    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err("symlink in owned input".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                stack.push(entry?.path());
            }
        } else if metadata.is_file() {
            if metadata.len() > 32 * 1024 * 1024 {
                return Err("input file exceeds bound".into());
            }
            let bytes = fs::read(&path)?;
            total += bytes.len();
            if total > 128 * 1024 * 1024 || result.len() > 8192 {
                return Err("input exceeds bound".into());
            }
            result.insert(
                path.strip_prefix(root)?.to_owned(),
                hex::encode(Sha256::digest(&bytes)),
            );
        } else {
            return Err("special file in owned input".into());
        }
    }
    Ok(result)
}

fn main() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: check-schematic-preview OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT REFERENCE"
                .into(),
        );
    }
    let source = fs::canonicalize(&args[0])?;
    let project = source.parent().ok_or("missing project parent")?;
    let config = fs::canonicalize(&args[1])?;
    for dir in [project, &config] {
        if fs::read_to_string(dir.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("missing owned-copy marker".into());
        }
    }
    let output = PathBuf::from(&args[2]);
    if output.starts_with(project) || output.starts_with(&config) {
        return Err("output overlaps input".into());
    }
    fs::create_dir(&output)?;
    let reference = args[3].to_str().ok_or("reference is not UTF-8")?;
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    let policy = SchematicAuditPolicy::new(
        project,
        &source,
        &config,
        &shared.join("symbols"),
        &shared.join("footprints"),
        &[],
    )?;
    let original = snapshot(project)?;
    let configuration = snapshot(&config)?;
    let audit = SchematicAudit::prepare(&policy)?;
    let audit_errors = audit.erc().summary().errors;
    let audit_warnings = audit.erc().summary().warnings;
    let baseline_root = audit.private_root().to_owned();
    let target = audit
        .identities()
        .units()
        .iter()
        .find(|unit| unit.reference == reference)
        .ok_or("reference missing")?;
    let edit = SchematicEdit {
        file: target.file.clone(),
        scope: EditScope::FileAllInstances,
        action: EditAction::SetProperty {
            uuid: target.source_uuid.clone(),
            property: SymbolProperty::Value,
            value: "preview \"中文\"\\candidate".into(),
        },
    };
    let preview = audit.preview_edits(&[edit])?;
    let affected_instances = preview.candidate().changes()[0].instance_paths.len();
    preview.verify()?;
    if preview.connectivity_before() != preview.connectivity_after() {
        return Err("property preview changed connectivity".into());
    }
    if !preview.erc_delta().introduced.is_empty()
        || !preview.erc_delta().resolved.is_empty()
        || preview.erc_delta().metadata_changed
        || preview.erc_delta().sheet_headers_changed
    {
        return Err("property preview changed complete ERC".into());
    }
    fs::write(
        output.join("property.json"),
        serde_json::to_vec_pretty(preview.evidence())?,
    )?;
    let property_root = preview.observed().private_root().to_owned();
    let candidate_source = preview.observed().evidence()?["source"]["files"]
        .as_object()
        .ok_or("candidate source evidence missing")?
        .keys()
        .find(|path| path.ends_with(source.file_name().unwrap().to_str().unwrap()))
        .ok_or("candidate root path missing")?
        .clone();
    let outer_root = PathBuf::from(&candidate_source)
        .parent()
        .ok_or("candidate parent missing")?
        .parent()
        .ok_or("candidate owner missing")?
        .to_owned();
    let drift_source = project.join(&target.file);
    let pinned = fs::read(&drift_source)?;
    let mut changed = pinned.clone();
    changed.extend_from_slice(b"\n");
    fs::write(&drift_source, changed)?;
    let detected = preview.verify().is_err();
    fs::write(&drift_source, &pinned)?;
    if !detected || preview.verify().is_ok() {
        return Err("source drift did not permanently invalidate preview".into());
    }
    drop(preview);
    if property_root.exists() || outer_root.exists() {
        return Err("property snapshot cleanup failed".into());
    }
    // Validate a catalog-backed symbol on the same exact source and configuration evidence.
    let definition = audit.symbol_definition("Device:R")?;
    let root_file = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid schematic basename")?
        .to_owned();
    let paths = audit
        .identities()
        .sheets()
        .iter()
        .filter(|sheet| sheet.file == root_file)
        .map(|sheet| SymbolInstancePath {
            path: sheet.path.clone(),
        })
        .collect::<Vec<_>>();
    let create = SchematicEdit {
        file: root_file,
        scope: EditScope::FileAllInstances,
        action: EditAction::CreateSymbol {
            key: "native-preview-resistor".into(),
            symbol: SymbolCreation {
                evidence: definition,
                transform: SymbolTransform {
                    origin: Point::from_nm(157_480_000, 85_090_000)?,
                    rotation: QuarterTurn::Zero,
                    mirror: Mirror::None,
                },
                unit: 1,
                body_style: None,
                reference: "R900".into(),
                value: "1k".into(),
                footprint: None,
                flags: SymbolFlags::default(),
                instance_paths: paths,
            },
        },
    };
    let mut forged = create.clone();
    if let EditAction::CreateSymbol { symbol, .. } = &mut forged.action {
        symbol.evidence.catalog_digest = "0".repeat(64);
    }
    if audit.preview_edits(&[forged]).is_ok() {
        return Err("forged catalog evidence accepted".into());
    }
    let created = audit.preview_edits(&[create])?;
    created.verify()?;
    if !created
        .observed()
        .netlist()
        .components
        .iter()
        .any(|component| component.reference == "R900")
    {
        return Err("created symbol missing from native netlist".into());
    }
    fs::write(
        output.join("creation.json"),
        serde_json::to_vec_pretty(created.evidence())?,
    )?;
    let created_root = created.observed().private_root().to_owned();
    let config_file = config.join("10.0/kicad_common.json");
    let config_bytes = fs::read(&config_file)?;
    let mut changed_config = config_bytes.clone();
    changed_config.extend_from_slice(b"\n");
    fs::write(&config_file, changed_config)?;
    let config_drift = created.verify().is_err();
    fs::write(&config_file, config_bytes)?;
    if !config_drift || created.verify().is_ok() {
        return Err("configuration drift did not permanently invalidate preview".into());
    }
    drop(created);
    let diagnostic = audit.preview_edits(&[SchematicEdit {
        file: source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid basename")?
            .into(),
        scope: EditScope::FileAllInstances,
        action: EditAction::Create {
            key: "floating-label".into(),
            object: NewObject::Label(Label {
                kind: LabelKind::Local,
                text: "AK_PREVIEW_FLOATING".into(),
                position: Point::from_nm(600_000_000, 500_000_000)?,
                rotation: QuarterTurn::Zero,
            }),
        },
    }])?;
    if diagnostic.erc_delta().introduced.is_empty() {
        return Err("complete preview omitted introduced native ERC diagnostic".into());
    }
    diagnostic.verify()?;
    fs::write(
        output.join("introduced-erc.json"),
        serde_json::to_vec_pretty(diagnostic.evidence())?,
    )?;
    let introduced_diagnostics = diagnostic.erc_delta().introduced.len();
    let diagnostic_root = diagnostic.observed().private_root().to_owned();
    let owned_library = fs::read_dir(project)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|extension| extension.to_str()) == Some("kicad_sym"))
        .ok_or("owned profile needs a project symbol library")?;
    let library_bytes = fs::read(&owned_library)?;
    let mut changed_library = library_bytes.clone();
    changed_library.extend_from_slice(b"\n");
    fs::write(&owned_library, changed_library)?;
    let library_drift = diagnostic.verify().is_err();
    fs::write(&owned_library, library_bytes)?;
    if !library_drift || diagnostic.verify().is_ok() {
        return Err("library drift did not permanently invalidate preview".into());
    }
    drop(diagnostic);
    audit.verify()?;
    drop(audit);
    if created_root.exists() || baseline_root.exists() || diagnostic_root.exists() {
        return Err("native audit cleanup failed".into());
    }
    if snapshot(project)? != original || snapshot(&config)? != configuration {
        return Err("original inputs changed".into());
    }
    let result = serde_json::json!({
        "passed": true, "native_version": "10.0.7", "gui_launched": false,
        "cases": ["property", "catalog_symbol", "forged_catalog_rejected", "source_drift_latched", "configuration_drift_latched", "library_drift_latched", "introduced_erc_retained"],
        "introduced_diagnostics": introduced_diagnostics,
        "complete_erc_retained": true, "complete_source_pin_connectivity": true,
        "affected_instances": affected_instances,
        "baseline_errors": audit_errors,
        "baseline_warnings": audit_warnings,
        "source_unchanged": true, "configuration_unchanged": true, "private_directories_removed": true,
        "public_mcp_publication": false,
    });
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
