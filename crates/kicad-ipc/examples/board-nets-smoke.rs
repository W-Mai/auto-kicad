use std::env;

use kicad_ipc::{ClientConfig, DocumentQuery, KiCadClient, NngTransport};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("KICAD_IPC_ENDPOINT")
        .unwrap_or_else(|_| "ipc:///tmp/kicad/api-82751.sock".to_owned());
    let config = ClientConfig::new(&endpoint, "auto-kicad-board-nets-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let version = client.get_version()?;
    let documents = client.get_open_documents(DocumentQuery::Pcb)?;
    let board_filename = documents
        .first()
        .and_then(|document| match &document.kind {
            kicad_ipc::OpenDocumentKind::Pcb { board_filename } => Some(board_filename.as_str()),
            _ => None,
        })
        .ok_or("PCB document not returned")?;
    let nets = client.get_board_nets(board_filename)?;
    println!(
        "{}",
        json!({
            "version": version.full_version,
            "board_filename": board_filename,
            "nets": nets.iter().map(|net| json!({
                "code": net.code,
                "name": net.name,
            })).collect::<Vec<_>>(),
        })
    );
    Ok(())
}
