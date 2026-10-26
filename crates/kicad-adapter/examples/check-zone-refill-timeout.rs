//! Verify an actual accepted native fill request can time out without retransmission or later writes.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::{LiveBoardText, observe_live_geometry};
use kicad_ipc::{
    ClientConfig, IpcError, KiCadClient, KiCadTransport, NngTransport, ZoneFillAttempt,
    envelope::{ApiRequest, ApiResponse},
};
use nng::{
    Message as WireMessage, Protocol, Socket,
    options::{Options, RecvMaxSize, RecvTimeout, SendTimeout, protocol::reqrep::ResendTime},
};
use protobuf::Message;
use serde_json::json;
use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error>;
#[derive(Default)]
struct Dispatch {
    count: usize,
    sent: bool,
    timed_out: bool,
}
struct ShortActionTransport {
    normal: NngTransport,
    endpoint: String,
    observed: Arc<Mutex<Dispatch>>,
}
impl KiCadTransport for ShortActionTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        if request
            .message
            .as_ref()
            .is_none_or(|m| m.type_url != "type.googleapis.com/kiapi.common.commands.RunAction")
        {
            return self.normal.request(request);
        }
        self.observed.lock().unwrap().count += 1;
        let socket = Socket::new(Protocol::Req0)?;
        socket.set_opt::<ResendTime>(None)?;
        socket.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;
        socket.set_opt::<RecvTimeout>(Some(Duration::from_millis(1)))?;
        socket.set_opt::<RecvMaxSize>(64 * 1024 * 1024)?;
        socket.dial(&self.endpoint)?;
        let data = request
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?;
        socket
            .send(WireMessage::from(&data))
            .map_err(|(_, e)| IpcError::Transport(e))?;
        self.observed.lock().unwrap().sent = true;
        match socket.recv() {
            Ok(reply) => {
                ApiResponse::parse_from_bytes(&reply).map_err(|_| IpcError::MalformedEnvelope)
            }
            Err(error) => {
                self.observed.lock().unwrap().timed_out = error == nng::Error::TimedOut;
                Err(IpcError::Transport(error))
            }
        }
    }
}
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-zone-refill-timeout SOURCE NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_sch", "kicad_dru"].map(|ext| {
        let p = source.with_extension(ext);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let status=Command::new("/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-refill-definitions.py"))
        .arg("prepare").arg(copy.board_path()).arg(&source).arg(&output).status()?;
    if !status.success() {
        editor.cleanup()?;
        return Err("preparation failed".into());
    }
    let identity = editor.launch(&copy)?;
    let result: Result<(), Error> = (|| {
        let mut config = ClientConfig::new(&identity.endpoint, "owned-fill-timeout")?;
        config.timeout = Duration::from_secs(60);
        let sent = Arc::new(Mutex::new(Dispatch::default()));
        let transport = ShortActionTransport {
            normal: NngTransport::connect(config.clone())?,
            endpoint: identity.endpoint.clone(),
            observed: sent.clone(),
        };
        let mut client = KiCadClient::new(transport, config);
        if client.get_version()?.full_version != "10.0.7" {
            return Err("wrong version".into());
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let before = observe_live_geometry(&mut client, &identity.board_filename)?;
        if before.native.zones().values().any(|z| z.filled) {
            return Err("fixture already filled".into());
        }
        let saved = fs::read(copy.board_path())?;
        let project = fs::read(copy.project_path())?;
        let commit = client.begin_commit()?;
        client = match client.request_zone_refill(&identity.board_filename) {
            ZoneFillAttempt::NotDispatched { client, .. } => client,
            _ => return Err("refill allowed an active native transaction".into()),
        };
        if sent.lock().unwrap().count != 0 {
            return Err("transaction guard dispatched an action".into());
        }
        client.drop_commit(&commit)?;
        let started = Instant::now();
        let mut uncertain = match client.request_zone_refill(&identity.board_filename) {
            ZoneFillAttempt::Uncertain(uncertain) => uncertain,
            ZoneFillAttempt::NotDispatched { reason, .. } => {
                return Err(format!("request not sent: {reason}").into());
            }
            ZoneFillAttempt::Returned { .. } => return Err("action did not time out".into()),
        };
        let timeout_millis = started.elapsed().as_millis();
        {
            let sent = sent.lock().unwrap();
            if sent.count != 1 || !sent.sent || !sent.timed_out {
                return Err("timeout did not follow one accepted send".into());
            }
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut reads = 0;
        let after = loop {
            if Instant::now() >= deadline {
                return Err("read-only observation deadline expired".into());
            }
            reads += 1;
            if let Ok(native) = uncertain.observe_geometry()
                && native.zones().values().any(|z| z.filled)
            {
                break native;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let text = uncertain.observe_live_text()?;
        let after_text = LiveBoardText::parse(&text)?;
        if before.native.zone_definition_bytes()? != after.zone_definition_bytes()?
            || before.text.refill_definition_digest()? != after_text.refill_definition_digest()?
        {
            return Err("timeout fill changed definitions".into());
        }
        if saved != fs::read(copy.board_path())? || project != fs::read(copy.project_path())? {
            return Err("uncertain path wrote saved files".into());
        }
        if sent.lock().unwrap().count != 1 {
            return Err("action retried".into());
        }
        fs::write(output.join("observed-after-timeout.kicad_pcb"), text)?;
        fs::write(output.join("before-saved.kicad_pcb"), saved)?;
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":"10.0.7","pid":identity.pid,"action_sent":true,"request_count":1,"receive_timeout_ms":1,"outcome":"uncertain","reason":uncertain.reason(),"timeout_elapsed_ms":timeout_millis,"read_only_polls":reads,"eventual_filled_zones":after.zones().values().filter(|z|z.filled).count(),"complete_definitions_preserved":true,"no_subsequent_save_reload_rollback_or_retry":true,"saved_files_unchanged":true,"active_transaction_dispatch_refused":true,"scope":"actual transport timeout after accepted native request; not GUI cancellation or completed source recovery"}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (path, before) in originals {
        if fs::read(path).ok() != before {
            return Err("source changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_pid":identity.pid,"closed":true,"source_unchanged":true}),
        )?,
    )?;
    if let Err(e) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"error":e.to_string()}))?,
        )?;
    }
    fs::remove_dir(output.join("work"))?;
    result
}
