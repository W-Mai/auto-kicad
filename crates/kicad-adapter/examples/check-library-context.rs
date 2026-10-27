//! Compare complete native DRC with a relocated effective footprint table.
use kicad_adapter::library_catalog::{FootprintCatalog, LibraryCatalogPolicy};
use kicad_checks::{DrcPolicy, run_pcb_drc_complete};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, env, fs, path::PathBuf, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: check-library-context SOURCE GLOBAL_TABLE OFFICIAL_ROOT NEW_OUTPUT".into(),
        );
    }
    let board = fs::canonicalize(&args[0])?;
    let project = board.parent().ok_or("missing project directory")?;
    let output = PathBuf::from(&args[3]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let private = output.join("private");
    fs::create_dir(&private)?;
    let policy = LibraryCatalogPolicy::new(
        project,
        &PathBuf::from(&args[1]),
        Some(&PathBuf::from(&args[2])),
        &[],
        BTreeMap::from([("KICAD10_FOOTPRINT_DIR".into(), args[2].clone())]),
        vec![],
    )?
    .with_native_settings(&PathBuf::from(&args[1]).with_file_name("kicad.json"))?;
    let catalog = FootprintCatalog::load(&policy)?;
    let table = catalog.relocated_table()?;
    fs::write(private.join("fp-lib-table"), &table)?;
    let inputs = ["kicad_pcb", "kicad_pro", "kicad_dru"]
        .into_iter()
        .map(|ext| {
            let path = board.with_extension(ext);
            let bytes = match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            Ok((path, bytes))
        })
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    for (path, bytes) in &inputs {
        if let Some(bytes) = bytes {
            fs::write(
                private.join(path.file_name().ok_or("missing input filename")?),
                bytes,
            )?;
        }
    }
    let original = run_pcb_drc_complete(
        &board,
        &DrcPolicy {
            allowed_root: project.into(),
            timeout: Duration::from_secs(60),
        },
    )
    .map_err(|e| format!("source DRC: {e:?}"))?;
    let copied = run_pcb_drc_complete(
        &private.join(board.file_name().unwrap()),
        &DrcPolicy {
            allowed_root: private.clone(),
            timeout: Duration::from_secs(60),
        },
    )
    .map_err(|e| format!("private DRC: {e:?}"))?;
    fs::write(output.join("source-drc.json"), original.raw().get())?;
    fs::write(output.join("private-drc.json"), copied.raw().get())?;
    if catalog.relocated_table()? != table {
        return Err("table context changed".into());
    }
    for (path, bytes) in &inputs {
        if fs::read(path).ok() != *bytes {
            return Err("source input changed".into());
        }
    }
    let equivalent = original.equivalent_to(&copied, &BTreeMap::new())?;
    let sha = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
    let record = json!({
        "passed":equivalent, "kicad_version":original.normalized().kicad_version,
        "source":board, "catalog_digest":catalog.digest(), "table_evidence":catalog.table_evidence(),
        "entry_count":catalog.entry_count(), "table_sha256":sha(table.as_bytes()),
        "inputs":inputs.iter().map(|(p,b)| json!({"path":p,"sha256":b.as_ref().map(|b|sha(b))})).collect::<Vec<_>>(),
        "source_unchanged":true, "violations":original.normalized().violations.len(),
        "unconnected":original.normalized().unconnected_items,
    });
    fs::write(
        output.join("acceptance.json"),
        serde_json::to_vec_pretty(&record)?,
    )?;
    println!("{record}");
    if !equivalent {
        return Err("complete DRC differs after library relocation".into());
    }
    Ok(())
}
