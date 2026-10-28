//! Capture complete native ERC and compare it with a retained report on an owned project copy.
use kicad_checks::erc::CompleteErcReport;
use kicad_schematic::NetlistRunner;
use std::{env, fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: check-schematic-erc SCHEMATIC NEW_OUTPUT [BASELINE_JSON]".into());
    }
    let schematic = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let report = NetlistRunner::production()?
        .run_complete_erc(schematic.parent().ok_or("missing parent")?, &schematic)?;
    fs::write(output.join("report.json"), report.raw().get())?;
    fs::write(
        output.join("summary.txt"),
        format!("{:#?}\n", report.summary()),
    )?;
    println!("{:?}", report.summary());
    if let Some(baseline) = args.get(2) {
        let baseline = CompleteErcReport::parse(&fs::read_to_string(baseline)?)?;
        let delta = baseline.compare(&report)?;
        let equivalent = baseline.equivalent_to(&report)?;
        fs::write(
            output.join("delta.txt"),
            format!("equivalent={equivalent}\n{delta:#?}\n"),
        )?;
        println!(
            "equivalent={equivalent} introduced={} resolved={} unchanged={}",
            delta.introduced.len(),
            delta.resolved.len(),
            delta.unchanged
        );
        if !equivalent {
            return Err("complete native ERC differs from retained baseline".into());
        }
    }
    Ok(())
}
