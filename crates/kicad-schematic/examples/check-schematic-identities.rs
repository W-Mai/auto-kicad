//! Compare source instance identities against a retained native XML export.
use kicad_schematic::{
    Limits, NetlistLimits, identities::SchematicIdentityIndex, load_schematic_project,
    parse_netlist_xml,
};
use std::{env, fs};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-schematic-identities SCHEMATIC NETLIST".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let project = load_schematic_project(
        source.parent().ok_or("no parent")?,
        &source,
        "10.0.7",
        1,
        Limits::default(),
    )?;
    let native = parse_netlist_xml(&fs::read(&args[1])?, NetlistLimits::default())?;
    let index = SchematicIdentityIndex::resolve(&project, &native)?;
    println!(
        "sheets={} units={} components={}",
        index.sheets().len(),
        index.units().len(),
        index.components().len()
    );
    println!("{index:#?}");
    Ok(())
}
