//! Run native netlist/ERC using fixed process inputs and a prepared configuration copy.
use kicad_checks::erc::CompleteErcReport;
use kicad_schematic::{
    Limits, NetlistLimits, NetlistRunner, identities::SchematicIdentityIndex,
    load_schematic_project, parse_netlist_xml,
};
use std::{env, fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("usage: check-schematic-execution SCHEMATIC CONFIG_HOME BASELINE_XML BASELINE_ERC NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let root = source.parent().ok_or("missing source parent")?;
    let config = fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[4]);
    fs::create_dir(&output)?;
    let runner = NetlistRunner::production()?.freeze_context(Some(&config))?;
    let context = runner.execution_context().ok_or("missing context")?;
    fs::write(
        output.join("execution.txt"),
        format!(
            "{context:#?}\nenvironment_digest={}\nexecutable_digest={}\n",
            context.environment_digest(),
            context.executable_digest()
        ),
    )?;
    let project = load_schematic_project(root, &source, "10.0.7", 1, Limits::default())?;
    let baseline = parse_netlist_xml(&fs::read(&args[2])?, NetlistLimits::default())?;
    let netlist = runner.run(root, &source)?;
    if baseline != netlist.document {
        return Err("fixed-context native netlist differs from baseline".into());
    }
    let index = SchematicIdentityIndex::resolve(&project, &netlist.document)?;
    let erc = runner.run_complete_erc(root, &source)?;
    fs::write(output.join("erc.json"), erc.raw().get())?;
    let expected = CompleteErcReport::parse(&fs::read_to_string(&args[3])?)?;
    if !expected.equivalent_to(&erc)? {
        return Err("fixed-context native ERC differs from baseline".into());
    }
    context.verify()?;
    let after = load_schematic_project(root, &source, "10.0.7", 1, Limits::default())?;
    if after != project {
        return Err("schematic inputs changed".into());
    }
    let summary = format!(
        "passed=true\nsheets={}\nunits={}\ncomponents={}\nnets={}\nerc={:?}\n",
        index.sheets().len(),
        index.units().len(),
        index.components().len(),
        netlist.document.nets.len(),
        erc.summary()
    );
    fs::write(output.join("result.txt"), &summary)?;
    print!("{summary}");
    Ok(())
}
