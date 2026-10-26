//! Inject an unsaved layer-count drift into an acceptance-owned PCB copy.
use kicad_ipc::{
    ClientConfig, DocumentQuery, KiCadClient, KiCadTransport, NngTransport, OpenDocumentKind,
    base_types, board_commands,
    envelope::{ApiRequest, ApiRequestHeader},
    response_status, response_token,
};
use protobuf::{Message, MessageField, well_known_types::any::Any};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

type Error = Box<dyn std::error::Error>;
#[derive(Serialize, Deserialize)]
struct Evidence {
    board: PathBuf,
    token: String,
    original_copper_layers: u32,
    changed_copper_layers: u32,
    original_enabled: Vec<i32>,
    restored: bool,
}
fn command<M: Message>(
    transport: &mut NngTransport,
    token: &str,
    kind: &str,
    message: M,
) -> Result<board_commands::BoardEnabledLayersResponse, Error> {
    let reply = transport.request(ApiRequest {
        header: MessageField::some(ApiRequestHeader {
            client_name: "owned-stackup-drift".into(),
            kicad_token: token.into(),
            ..Default::default()
        }),
        message: MessageField::some(Any {
            type_url: format!("type.googleapis.com/kiapi.board.commands.{kind}"),
            value: message.write_to_bytes()?,
            ..Default::default()
        }),
        ..Default::default()
    })?;
    response_status(&reply)?;
    if response_token(&reply)?.as_str() != token {
        return Err("editor token changed".into());
    }
    let any = reply.message.as_ref().ok_or("missing layer response")?;
    if any.type_url != "type.googleapis.com/kiapi.board.commands.BoardEnabledLayersResponse" {
        return Err("unexpected layer response".into());
    }
    Ok(board_commands::BoardEnabledLayersResponse::parse_from_bytes(&any.value)?)
}
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: owned-stackup-drift change|restore ENDPOINT OWNED_BOARD EVIDENCE.json".into(),
        );
    }
    let action = &args[0];
    if !matches!(action.as_str(), "change" | "restore") {
        return Err("unknown probe action".into());
    }
    let board = fs::canonicalize(&args[2])?;
    let evidence = Path::new(&args[3]);
    let root = fs::canonicalize(evidence.parent().ok_or("missing evidence root")?)?;
    if !board.starts_with(root.join("work")) {
        return Err("probe requires an acceptance-owned working copy".into());
    }
    let original = fs::read(root.join("before/board.kicad_pcb"))?;
    if fs::read(&board)? != original {
        return Err("owned board bytes changed before drift probe".into());
    }
    let config = ClientConfig::new(&args[1], "owned-stackup-drift")?;
    let mut client = KiCadClient::new(NngTransport::connect(config.clone())?, config.clone());
    client.get_version()?;
    let docs = client.get_open_documents(DocumentQuery::Pcb)?;
    if docs.len() != 1
        || fs::canonicalize(
            docs[0]
                .project_path
                .as_deref()
                .ok_or("missing project path")?,
        )? != board.parent().ok_or("missing board parent")?
        || !matches!(&docs[0].kind, OpenDocumentKind::Pcb {board_filename} if Path::new(board_filename).file_name()==board.file_name())
    {
        return Err("probe editor does not exclusively own the expected copy".into());
    }
    let token = client
        .instance_token()
        .ok_or("missing editor token")?
        .as_str()
        .to_owned();
    let mut document = base_types::DocumentSpecifier {
        type_: base_types::DocumentType::DOCTYPE_PCB.into(),
        ..Default::default()
    };
    document.set_board_filename(
        board
            .file_name()
            .ok_or("missing filename")?
            .to_string_lossy()
            .into_owned(),
    );
    let mut transport = NngTransport::connect(config)?;
    let current = command(
        &mut transport,
        &token,
        "GetBoardEnabledLayers",
        board_commands::GetBoardEnabledLayers {
            board: MessageField::some(document.clone()),
            ..Default::default()
        },
    )?;
    if action == "change" {
        if current.copper_layer_count != 4 {
            return Err("drift fixture requires 4 copper layers".into());
        }
        let record = Evidence {
            board: board.clone(),
            token: token.clone(),
            original_copper_layers: 4,
            changed_copper_layers: 6,
            original_enabled: current.layers.iter().map(|l| l.value()).collect(),
            restored: false,
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(evidence)?;
        file.write_all(&serde_json::to_vec_pretty(&record)?)?;
        file.sync_all()?;
        // Only add empty copper layers; all original non-copper layers are retained.
        let changed = command(
            &mut transport,
            &token,
            "SetBoardEnabledLayers",
            board_commands::SetBoardEnabledLayers {
                board: MessageField::some(document),
                copper_layer_count: 6,
                layers: current.layers.clone(),
                ..Default::default()
            },
        )?;
        if changed.copper_layer_count != 6
            || current.layers.iter().any(|l| !changed.layers.contains(l))
        {
            return Err("layer addition did not preserve the complete original layer set".into());
        }
    } else {
        let mut record: Evidence = serde_json::from_slice(&fs::read(evidence)?)?;
        if record.board != board || record.token != token || record.restored {
            return Err("drift evidence does not bind this editor".into());
        }
        client.revert_board(document.board_filename())?;
        let restored = command(
            &mut transport,
            &token,
            "GetBoardEnabledLayers",
            board_commands::GetBoardEnabledLayers {
                board: MessageField::some(document),
                ..Default::default()
            },
        )?;
        if restored.copper_layer_count != record.original_copper_layers
            || restored
                .layers
                .iter()
                .map(|l| l.value())
                .collect::<Vec<_>>()
                != record.original_enabled
        {
            return Err("saved original layer set did not restore exactly".into());
        }
        record.restored = true;
        fs::write(evidence, serde_json::to_vec_pretty(&record)?)?;
    }
    if fs::read(board)? != original {
        return Err("unsaved stackup probe changed saved bytes".into());
    }
    Ok(())
}
