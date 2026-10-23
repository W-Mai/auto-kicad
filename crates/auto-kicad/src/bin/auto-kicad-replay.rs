//! Deterministic placement and simple routing playback in one owned PCB Editor.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_ipc::{CreateNetIdentity, CreateTrack};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
const HELPER: &str = include_str!("../../helpers/replay_helper.py");
const VIEW: &str = include_str!("../../helpers/replay.html");
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

static CANCELLED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupt(_: libc::c_int) {
    CANCELLED.store(true, Ordering::Relaxed);
}

fn check_cancelled() -> Result<()> {
    if CANCELLED.load(Ordering::Relaxed) {
        Err("Replay cancelled".into())
    } else {
        Ok(())
    }
}

fn pause(milliseconds: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(milliseconds);
    while Instant::now() < deadline {
        check_cancelled()?;
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        );
    }
    check_cancelled()
}

#[derive(Deserialize)]
struct Route {
    net: String,
    from: [String; 2],
    to: [String; 2],
    points: Vec<[i64; 2]>,
    width_nm: i64,
}

#[derive(Deserialize)]
struct RoutePlan {
    routes: Vec<Route>,
    geometry_sha256: String,
}

fn helper(mode: &str, paths: &[&Path], root: &Path) -> Result<()> {
    let log_path = root.join(format!("helper-{mode}.log"));
    let log = fs::File::create(&log_path)?;
    use std::os::unix::process::CommandExt;
    let mut child = Command::new(PYTHON)
        .process_group(0)
        .arg("-c")
        .arg(HELPER)
        .arg(mode)
        .args(paths)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(format!("{mode} failed; inspect {}", log_path.display()).into());
            }
            break;
        }
        if Instant::now() > deadline || check_cancelled().is_err() {
            // The helper has its own process group, including its DRC subprocess.
            unsafe {
                libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
            }
            child.wait()?;
            check_cancelled()?;
            return Err(format!("{mode} exceeded its time limit").into());
        }
        thread::sleep(Duration::from_millis(100));
    }
    check_cancelled()?;
    for line in fs::read_to_string(log_path)?
        .lines()
        .filter(|line| line.starts_with('{'))
    {
        println!("{line}");
    }
    Ok(())
}

fn reference_key(reference: &str) -> (u8, &str, u32) {
    let at = reference
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(reference.len());
    let (prefix, number) = reference.split_at(at);
    let priority = match prefix {
        "U" => 0,
        "J" => 1,
        "SW" => 2,
        "R" => 3,
        _ => 4,
    };
    (priority, prefix, number.parse().unwrap_or(0))
}

fn main() -> Result<()> {
    // Defer cancellation to normal control flow so owned editor cleanup runs.
    unsafe {
        libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, interrupt as *const () as libc::sighandler_t);
    }
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|s| s == "--help") {
        println!(
            "auto-kicad-replay SOURCE.kicad_pcb NEW_OUTPUT_DIR [DELAY_MS=250]\nRebuild an owned copy one component at a time, then play DRC-checked simple routes.\nInput must be an unrouted front-SMT PCB with matching project settings.\nOriginal files are preserved; the one owned editor closes at the end."
        );
        return Ok(());
    }
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: auto-kicad-replay SOURCE.kicad_pcb NEW_OUTPUT_DIR [DELAY_MS=250]".into(),
        );
    }
    let source = fs::canonicalize(&args[0])?;
    let delay_ms: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(250);
    if delay_ms > 5000 {
        return Err("delay must be 0..=5000 milliseconds".into());
    }
    let original = fs::read(&source)?;
    let original_project = fs::read(source.with_extension("kicad_pro"))?;
    let root = PathBuf::from(&args[1]);
    fs::create_dir(&root)?;
    let root = fs::canonicalize(root)?;
    let prepared = root.join("prepared");
    helper("prepare", &[&source, &prepared], &root)?;
    let mut launcher = OwnedPcbnewLauncher::new(&root.join("work"))?;
    let copy = launcher.create_shadow(&prepared.join("board.kicad_pcb"))?;
    let identity = launcher.launch(&copy)?;
    let mut events = fs::File::create(root.join("events.jsonl"))?;
    writeln!(
        events,
        "{}",
        json!({"kind":"start","source":source,"source_sha256":hex::encode(Sha256::digest(&original)),"editor_pid":identity.pid,"delay_ms":delay_ms})
    )?;
    let result: Result<()> = (|| {
        let mut client = launcher.connect_owned()?;
        let mut placements = client.get_footprint_instances(&identity.board_filename)?;
        placements.sort_by(|a, b| reference_key(&a.reference).cmp(&reference_key(&b.reference)));
        let templates = client
            .capture_footprint_instance_templates(&identity.board_filename)?
            .into_iter()
            .map(|t| (t.metadata().source_uuid.clone(), t))
            .collect::<BTreeMap<_, _>>();
        drop(client);
        fs::copy(prepared.join("empty.kicad_pcb"), copy.board_path())?;
        launcher.reload_owned()?;
        let mut client = launcher.connect_owned()?;
        if !client
            .get_footprint_instances(&identity.board_filename)?
            .is_empty()
        {
            return Err("replay did not start from an empty board".into());
        }
        println!("Replay started in editor PID {}", identity.pid);
        for (index, placement) in placements.iter().enumerate() {
            check_cancelled()?;
            let template = templates
                .get(&placement.source_uuid)
                .ok_or("missing captured template")?;
            let commit = client.begin_commit()?;
            let receipt = match client.create_footprint_instance(
                &identity.board_filename,
                template,
                placement.position_x_nm,
                placement.position_y_nm,
                &placement.reference,
                &placement.value,
            ) {
                Ok(receipt) => receipt,
                Err(error) => {
                    client.drop_commit(&commit)?;
                    return Err(error.into());
                }
            };
            client.commit_changes(&commit, &format!("Replay place {}", placement.reference))?;
            if client
                .get_footprint_instances(&identity.board_filename)?
                .len()
                != index + 1
            {
                return Err("placement count differs from replay step".into());
            }
            writeln!(
                events,
                "{}",
                json!({"kind":"place","step":index+1,"reference":placement.reference,"value":placement.value,"position_nm":[placement.position_x_nm,placement.position_y_nm],"native_uuid":receipt.source_uuid})
            )?;
            events.flush()?;
            println!(
                "Place {}/{}: {}",
                index + 1,
                placements.len(),
                placement.reference
            );
            pause(delay_ms)?;
        }
        client.save_board(&identity.board_filename)?;
        drop(client);
        helper("plan", &[copy.board_path(), &root], &root)?;
        let plan: RoutePlan = serde_json::from_slice(&fs::read(root.join("route-plan.json"))?)?;
        let mut client = launcher.connect_owned()?;
        let mut segment_count = 0;
        for (index, route) in plan.routes.iter().enumerate() {
            check_cancelled()?;
            let commit = client.begin_commit()?;
            for points in route.points.windows(2) {
                let track = CreateTrack {
                    start_x_nm: points[0][0],
                    start_y_nm: points[0][1],
                    end_x_nm: points[1][0],
                    end_y_nm: points[1][1],
                    width_nm: route.width_nm,
                    net: Some(CreateNetIdentity {
                        name: route.net.clone(),
                    }),
                };
                if let Err(error) = client.create_track(&identity.board_filename, &track) {
                    client.drop_commit(&commit)?;
                    return Err(error.into());
                }
            }
            client.commit_changes(&commit, &format!("Replay route {}", route.net))?;
            segment_count += route.points.len() - 1;
            writeln!(
                events,
                "{}",
                json!({"kind":"route","step":index+1,"net":route.net,"from":route.from,"to":route.to,"points_nm":route.points,"width_nm":route.width_nm})
            )?;
            events.flush()?;
            println!(
                "Route {}/{}: {} {}.{} -> {}.{}",
                index + 1,
                plan.routes.len(),
                route.net,
                route.from[0],
                route.from[1],
                route.to[0],
                route.to[1]
            );
            pause(delay_ms)?;
        }
        client.save_board(&identity.board_filename)?;
        drop(client);
        let saved = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        launcher.reload_owned()?;
        let reopened = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        if saved != reopened || launcher.current_identity()?.pid != identity.pid {
            return Err("reloaded snapshot or editor PID changed".into());
        }
        if reopened
            .snapshot
            .native_routing
            .as_ref()
            .ok_or("missing routing")?
            .tracks
            .len()
            != segment_count
        {
            return Err("native segment count differs from validated route plan".into());
        }
        // Retain the actual result even if a final audit rejects it.
        fs::create_dir(root.join("result"))?;
        fs::copy(copy.board_path(), root.join("result/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), root.join("result/board.kicad_pro"))?;
        helper("audit", &[copy.board_path(), &prepared, &root], &root)?;
        if fs::read(&source)? != original
            || fs::read(source.with_extension("kicad_pro"))? != original_project
        {
            return Err("original source files changed".into());
        }
        writeln!(
            events,
            "{}",
            json!({"kind":"complete","components":placements.len(),"routes":plan.routes.len(),"segments":segment_count,"geometry_sha256":plan.geometry_sha256,"same_editor_pid":identity.pid,"saved_reload_matches":true})
        )?;
        let input: serde_json::Value =
            serde_json::from_slice(&fs::read(prepared.join("source.json"))?)?;
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("summary.json"))?)?;
        let steps = fs::read_to_string(root.join("events.jsonl"))?
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let data =
            json!({"summary":summary,"bounds":input["bounds"],"pads":input["pads"],"events":steps});
        fs::write(
            root.join("replay.html"),
            VIEW.replace(
                "__REPLAY_DATA__",
                &serde_json::to_string(&data)?.replace('<', "\\u003c"),
            ),
        )?;
        println!(
            "Replay complete; saved {}",
            root.join("result/board.kicad_pcb").display()
        );
        pause(1500)?;
        Ok(())
    })();
    if let Err(error) = &result {
        writeln!(
            events,
            "{}",
            json!({"kind":"failed","error":error.to_string()})
        )?;
    }
    launcher.close_owned(identity.pid)?;
    launcher.cleanup()?;
    fs::remove_dir(root.join("work"))?;
    result
}
