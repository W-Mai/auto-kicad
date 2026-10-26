//! Acceptance-only raw IPC relay with one explicitly armed response fault.
use kicad_ipc::{GET_VERSION_TYPE_URL, SAVE_DOCUMENT_TYPE_URL, envelope::ApiRequest};
use nng::{
    Message, Protocol, Socket,
    options::{Options, RecvMaxSize, RecvTimeout, SendTimeout},
};
use protobuf::Message as ProtoMessage;
use serde_json::json;
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: geometry-fault-relay UPSTREAM PROJECT NEW_CONTROL".into());
    }
    let project = fs::canonicalize(&args[1])?;
    let root = PathBuf::from(&args[2]);
    fs::create_dir(&root)?;
    let root = fs::canonicalize(root)?;
    let endpoint = format!("ipc:///tmp/auto-kicad-fault-{}.sock", std::process::id());
    let frontend = Socket::new(Protocol::Rep0)?;
    frontend.set_opt::<RecvMaxSize>(64 * 1024 * 1024)?;
    frontend.set_opt::<RecvTimeout>(Some(Duration::from_millis(200)))?;
    frontend.set_opt::<SendTimeout>(Some(Duration::from_secs(5)))?;
    frontend.listen(&endpoint)?;
    let backend = Socket::new(Protocol::Req0)?;
    backend.set_opt::<RecvMaxSize>(64 * 1024 * 1024)?;
    backend.set_opt::<RecvTimeout>(Some(Duration::from_secs(60)))?;
    backend.set_opt::<SendTimeout>(Some(Duration::from_secs(60)))?;
    backend.set_opt::<nng::options::protocol::reqrep::ResendTime>(None)?;
    backend.dial(&args[0])?;
    fs::write(
        root.join("ready.json"),
        serde_json::to_vec(&json!({"endpoint":endpoint,"pid":std::process::id()}))?,
    )?;
    let mut log = fs::File::create(root.join("commands.jsonl"))?;
    let deadline = Instant::now() + Duration::from_secs(1200);
    let mut faulted = false;
    let mut sequence = 0;
    while Instant::now() < deadline && !root.join("stop").exists() {
        let request = match frontend.recv() {
            Ok(request) => request,
            Err(nng::Error::TimedOut) => continue,
            Err(error) => return Err(error.into()),
        };
        let decoded = ApiRequest::parse_from_bytes(request.as_slice())?;
        let kind = decoded
            .message
            .as_ref()
            .ok_or("missing command")?
            .type_url
            .clone();
        let arm = fs::read_to_string(root.join("arm")).ok();
        let pending = project.join(".auto-kicad-geometry-pending").exists();
        sequence += 1;
        backend.send(request.clone()).map_err(|(_, error)| error)?;
        let reply = backend.recv()?;
        let corrupt = !faulted
            && pending
            && arm.as_deref() == Some("corrupt_saved_reply")
            && kind == SAVE_DOCUMENT_TYPE_URL;
        let hold = !faulted
            && pending
            && arm.as_deref() == Some("interrupt_before_write")
            && kind == GET_VERSION_TYPE_URL;
        writeln!(
            log,
            "{}",
            json!({"sequence":sequence,"command":kind,"pending":pending,"corrupt":corrupt,"hold":hold,"after_fault":faulted,"request_bytes":request.len(),"response_bytes":reply.len()})
        )?;
        log.flush()?;
        if corrupt || hold {
            kicad_ipc::response_status(&kicad_ipc::envelope::ApiResponse::parse_from_bytes(
                reply.as_slice(),
            )?)?;
            fs::write(root.join("fault-request.bin"), request.as_slice())?;
            fs::write(root.join("native-response.bin"), reply.as_slice())?;
            fs::write(
                root.join("fault.json"),
                serde_json::to_vec(
                    &json!({"sequence":sequence,"command":kind,"native_success":true,"mode":arm}),
                )?,
            )?;
            faulted = true;
        }
        if hold {
            while !root.join("release").exists()
                && !root.join("stop").exists()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            if root.join("stop").exists() {
                break;
            }
        }
        let response = if corrupt {
            Message::from(&[0xff_u8][..])
        } else {
            reply
        };
        // A deliberately terminated downstream client may no longer accept its held reply.
        if let Err((_, error)) = frontend.send(response)
            && !hold
        {
            return Err(error.into());
        }
    }
    if !root.join("stop").exists() {
        return Err("relay lifetime exceeded".into());
    }
    Ok(())
}
