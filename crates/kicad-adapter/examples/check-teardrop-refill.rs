//! Verify complete generated refill content with one owned native editor and independent CLI.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::teardrop_refill::{TeardropRefillObservation, VerifiedTeardropRefill};
use kicad_checks::{DrcPolicy, refill::refill_closed_copy};
use kicad_ipc::ZoneFillAttempt;
use protobuf::Message;
use serde_json::json;
use std::{env, fs, path::PathBuf};
type Error = Box<dyn std::error::Error>;
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: check-teardrop-refill SOURCE NEW_OUTPUT EXPECTED_GENERATED".into());
    }
    let expected: usize = args[2].parse()?;
    let source = fs::canonicalize(&args[0])?;
    fs::create_dir(&args[1])?;
    let output = fs::canonicalize(PathBuf::from(&args[1]))?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|ext| {
        let p = source.with_extension(ext);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    fs::write(
        output.join("owned.json"),
        serde_json::to_vec_pretty(&json!({"pid":identity.pid}))?,
    )?;
    let result: Result<(), Error> = (|| {
        let mut client = editor.connect_owned()?;
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let protected = fs::read(copy.project_path())?;
        let mut before = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        let mut stages = Vec::new();
        for stage in 0..2 {
            client = match client.request_zone_refill(&identity.board_filename) {
                ZoneFillAttempt::Returned { client, .. } => client,
                _ => return Err("refill did not return a determinate native reply".into()),
            };
            let after = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
            if after.text().generated_count() != expected {
                return Err("unexpected generated count".into());
            }
            client.save_board(&identity.board_filename)?;
            let saved = fs::read_to_string(copy.board_path())?;
            let independent = refill_closed_copy(
                copy.board_path(),
                &DrcPolicy {
                    allowed_root: copy.root().to_path_buf(),
                    timeout: std::time::Duration::from_secs(60),
                },
            )
            .map_err(|e| format!("independent refill: {e:?}"))?;
            if independent.input_board() != saved {
                return Err("CLI input differs from actual saved bytes".into());
            }
            editor.reload_owned()?;
            let reloaded =
                TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
            fs::write(output.join(format!("stage-{stage}.kicad_pcb")), &saved)?;
            fs::write(
                output.join(format!("stage-{stage}-cli.kicad_pcb")),
                independent.output_board(),
            )?;
            fs::write(output.join(format!("stage-{stage}.kicad_pro")), &protected)?;
            for (name, observed) in [
                ("before", &before),
                ("after", &after),
                ("reload", &reloaded),
            ] {
                fs::write(
                    output.join(format!("stage-{stage}-{name}.kicad_pcb")),
                    observed.text().raw_text(),
                )?;
                for (id, zone) in observed.native().zones() {
                    fs::write(
                        output.join(format!("stage-{stage}-{name}-{id}.bin")),
                        zone.write_to_bytes()?,
                    )?;
                    fs::write(
                        output.join(format!("stage-{stage}-{name}-{id}.txt")),
                        protobuf::text_format::print_to_string(zone),
                    )?;
                }
            }
            let proof = VerifiedTeardropRefill::verify(&before, &after, &reloaded, &independent)?;
            let repeated =
                TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
            if repeated.native() != reloaded.native()
                || repeated.text().raw_text() != reloaded.text().raw_text()
            {
                return Err("post-verification native drift".into());
            }
            if fs::read(copy.project_path())? != protected
                || fs::read_to_string(copy.board_path())? != saved
            {
                return Err("protected project or saved board changed".into());
            }
            stages.push(json!({"stage":stage,"proof":proof,"cli":independent,"zones":after.native().zones().iter().map(|(id,z)|json!({"id":id,"protobuf":protobuf::text_format::print_to_string(z)})).collect::<Vec<_>>()}));
            before = reloaded;
        }
        fs::write(
            output.join("acceptance.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":true,"kicad_version":"10.0.7","pid":identity.pid,"expected_generated":expected,"stages":stages}),
            )?,
        )?;
        Ok(())
    })();
    let failure = result.as_ref().err().map(ToString::to_string);
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    let unchanged = originals.into_iter().all(|(p, b)| fs::read(p).ok() == b);
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"closed":true,"source_unchanged":unchanged,"passed":result.is_ok(),"failure":failure}),
        )?,
    )?;
    if !unchanged {
        return Err("original source changed".into());
    }
    result
}
