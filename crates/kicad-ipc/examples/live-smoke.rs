use std::env;

use kicad_ipc::{
    ClientConfig, DocumentQuery, KiCadClient, NngTransport, OpenDocument, OpenDocumentKind,
};
use serde_json::{Value, json};

fn document_json(document: &OpenDocument) -> Value {
    let kind = match &document.kind {
        OpenDocumentKind::Pcb { board_filename } => json!({
            "type": "pcb",
            "board_filename": board_filename,
        }),
        OpenDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => json!({
            "type": "schematic",
            "filename": filename,
            "sheet_path": sheet_path,
            "sheet_kiids": sheet_kiids,
        }),
        OpenDocumentKind::Other => json!({ "type": "other" }),
    };
    json!({
        "project_name": document.project_name,
        "project_path": document.project_path,
        "kind": kind,
    })
}

fn documents_json(
    endpoint: &str,
    query: DocumentQuery,
) -> Result<Value, Box<dyn std::error::Error>> {
    let config = ClientConfig::new(endpoint, "auto-kicad-live-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    client.get_version()?;
    Ok(match client.get_open_documents(query) {
        Ok(documents) => Value::Array(documents.iter().map(document_json).collect()),
        Err(error) => json!({ "error": error.to_string(), "code": error.code() }),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint =
        env::var("KICAD_IPC_ENDPOINT").unwrap_or_else(|_| "ipc:///tmp/kicad/api.sock".to_owned());
    let version_config = ClientConfig::new(&endpoint, "auto-kicad-live-smoke")?;
    let version_transport = NngTransport::connect(version_config.clone())?;
    let mut version_client = KiCadClient::new(version_transport, version_config);
    let version = version_client.get_version()?;
    let pcb = documents_json(&endpoint, DocumentQuery::Pcb)?;
    let schematic = documents_json(&endpoint, DocumentQuery::Schematic)?;
    println!(
        "{}",
        json!({
            "version": {
                "major": version.major,
                "minor": version.minor,
                "patch": version.patch,
            },
            "documents": {
                "pcb": pcb,
                "schematic": schematic,
            },
        })
    );
    Ok(())
}
