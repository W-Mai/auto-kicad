use kicad_ipc::{ClientConfig, DocumentQuery, KiCadClient, NngTransport, OpenDocumentKind};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("KICAD_IPC_ENDPOINT")?;
    let config = ClientConfig::new(endpoint, "board-graphics-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let version = client.get_version()?;
    let documents = client.get_open_documents(DocumentQuery::Pcb)?;
    let board_filename = documents
        .iter()
        .find_map(|document| match &document.kind {
            OpenDocumentKind::Pcb { board_filename } => Some(board_filename.as_str()),
            _ => None,
        })
        .ok_or("no open PCB document")?;
    let graphics = client.get_board_graphics(board_filename)?;
    println!(
        "{}",
        json!({
            "board_filename": board_filename,
            "graphics": graphics.len(),
            "version": version.full_version,
        })
    );
    Ok(())
}
