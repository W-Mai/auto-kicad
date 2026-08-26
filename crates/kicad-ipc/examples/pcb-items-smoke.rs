use kicad_ipc::{
    ClientConfig, DocumentQuery, KiCadClient, NngTransport, OpenDocumentKind, PcbItemKind,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("KICAD_IPC_ENDPOINT")?;
    let config = ClientConfig::new(endpoint, "pcb-items-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let documents = client.get_open_documents(DocumentQuery::Pcb)?;
    let board_filename = documents
        .iter()
        .find_map(|document| match &document.kind {
            OpenDocumentKind::Pcb { board_filename } => Some(board_filename.as_str()),
            _ => None,
        })
        .ok_or("no open PCB document")?;
    let diagnostics = client.get_pcb_item_diagnostics(board_filename, &PcbItemKind::all())?;
    let items = diagnostics
        .iter()
        .map(|item| {
            json!({
                "requested_kind": item.requested_kind.name(),
                "type_url": item.type_url,
                "byte_length": item.byte_length,
                "decoded_kind": item.decoded_kind,
                "stable_id": item.stable_id,
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        json!({ "board_filename": board_filename, "items": items })
    );
    Ok(())
}
