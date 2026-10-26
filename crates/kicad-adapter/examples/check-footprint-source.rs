//! Resolve exact local footprint sources without launching or modifying KiCad.

use kicad_adapter::library_catalog::{FootprintCatalog, LibraryCatalogPolicy};
use kicad_model::FootprintLibraryIdentifier;
use serde_json::json;
use std::{collections::BTreeMap, env, path::Path};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.len() < 5 {
        return Err("usage: check-footprint-source PROJECT GLOBAL_TABLE OFFICIAL_ROOT MAJOR NICKNAME:ENTRY ...".into());
    }
    let major: u16 = args[3].parse()?;
    if major == 0 {
        return Err("KiCad major version must be positive".into());
    }
    let variables = BTreeMap::from([(format!("KICAD{major}_FOOTPRINT_DIR"), args[2].clone())]);
    let policy = LibraryCatalogPolicy::new(
        Path::new(&args[0]),
        Path::new(&args[1]),
        Some(Path::new(&args[2])),
        &[],
        variables,
        vec![],
    )?;
    let catalog = FootprintCatalog::load(&policy)?;
    let mut resolutions = Vec::new();
    for value in &args[4..] {
        let (nickname, entry_name) = value.split_once(':').ok_or("expected NICKNAME:ENTRY")?;
        resolutions.push(catalog.resolve(&FootprintLibraryIdentifier {
            nickname: nickname.into(),
            entry_name: entry_name.into(),
        })?);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"catalog_digest": catalog.digest(), "entries":catalog.entry_count(), "tables":catalog.table_evidence(), "resolutions":resolutions, "read_only":true})
        )?
    );
    Ok(())
}
