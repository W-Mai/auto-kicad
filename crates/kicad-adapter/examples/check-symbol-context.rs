//! Verify symbol-library observations on an explicitly marked owned project copy.
use kicad_adapter::library_catalog::{LibraryCatalogPolicy, symbols::SymbolCatalog};
use kicad_schematic::{Limits, NetlistRunner, load_schematic_project};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: check-symbol-context OWNED_SCHEMATIC GLOBAL_TABLE OFFICIAL_SYMBOLS NEW_OUTPUT"
                .into(),
        );
    }
    let schematic = fs::canonicalize(&args[0])?;
    let root = schematic.parent().ok_or("missing parent")?;
    if fs::read_to_string(root.join(".auto-kicad-context-test-owned"))?
        != "symbol-context-acceptance\n"
    {
        return Err("owned-copy marker missing".into());
    }
    let output = PathBuf::from(&args[3]);
    fs::create_dir(&output)?;
    let official = fs::canonicalize(&args[2])?;
    let mut variables = BTreeMap::new();
    variables.insert(
        "KICAD10_SYMBOL_DIR".into(),
        official.to_str().ok_or("non-UTF8 path")?.into(),
    );
    let policy = LibraryCatalogPolicy::new(
        root,
        &PathBuf::from(&args[1]),
        Some(&official),
        &[],
        variables,
        vec![],
    )?;
    let catalog = SymbolCatalog::load(&policy)?;
    let project = load_schematic_project(root, &schematic, "10.0.7", 1, Limits::default())?;
    let names = project
        .files
        .iter()
        .flat_map(|f| &f.symbols)
        .map(|s| {
            s.lib_id
                .split_once(':')
                .map(|v| v.0.to_owned())
                .ok_or("unqualified symbol library")
        })
        .collect::<Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect::<Vec<_>>();
    let context = catalog.capture(&names)?;
    fs::write(
        output.join("context.json"),
        serde_json::to_vec_pretty(&context)?,
    )?;
    fs::write(
        output.join("tables.json"),
        serde_json::to_vec_pretty(catalog.table_evidence())?,
    )?;
    let runner = NetlistRunner::production()?;
    let before = runner.run_complete_erc(root, &schematic)?;
    fs::write(output.join("before-erc.json"), before.raw().get())?;
    catalog.verify(&context)?;
    let file = context
        .files()
        .iter()
        .filter_map(|f| f.path())
        .find(|p| p.starts_with(root) && p.is_file())
        .ok_or("no owned used symbol file")?;
    let original = fs::read(file)?;
    let mut changed = original.clone();
    changed.push(b'\n');
    fs::write(file, &changed)?;
    let changed_result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if catalog.verify(&context).is_ok() {
            return Err("changed symbol bytes accepted".into());
        }
        let erc = runner.run_complete_erc(root, &schematic)?;
        fs::write(output.join("changed-library-erc.json"), erc.raw().get())?;
        if !before.equivalent_to(&erc)? {
            return Err("whitespace-only library change altered native ERC".into());
        }
        Ok(())
    })();
    if fs::read(file)? != changed {
        return Err("owned file changed unexpectedly; refusing restore".into());
    }
    fs::write(file, &original)?;
    changed_result?;
    catalog.verify(&context)?;
    let table = root.join("sym-lib-table");
    let original_table = fs::read(&table)?;
    let mut changed_table = original_table.clone();
    changed_table.push(b'\n');
    fs::write(&table, &changed_table)?;
    let rejected = catalog.verify(&context).is_err();
    if fs::read(&table)? != changed_table {
        return Err("owned table changed unexpectedly; refusing restore".into());
    }
    fs::write(&table, &original_table)?;
    if !rejected {
        return Err("changed symbol table accepted".into());
    }
    catalog.verify(&context)?;
    let after = runner.run_complete_erc(root, &schematic)?;
    fs::write(output.join("after-erc.json"), after.raw().get())?;
    if !before.equivalent_to(&after)? {
        return Err("restored native ERC differs".into());
    }
    catalog.verify(&context)?;
    let result = serde_json::json!({"passed":true,"used_nicknames":names,"table_files":catalog.table_evidence().len(),"context_digest":context.digest(),"library_byte_drift_rejected":true,"table_byte_drift_rejected":true,"native_erc_unchanged_for_library_whitespace":true,"native_erc_restored":true,"baseline":before.summary()});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
