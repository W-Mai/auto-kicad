use kicad_adapter::{DiscoveryInput, DocumentObservation, discovery_snapshot};
use kicad_ipc::{ClientConfig, DocumentQuery, IpcError, KiCadClient, NngTransport};

fn observe(
    endpoint: &str,
    query: DocumentQuery,
) -> Result<DocumentObservation, Box<dyn std::error::Error>> {
    let config = ClientConfig::new(endpoint, "auto-kicad-adapter-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    client.get_version()?;
    Ok(match client.get_open_documents(query) {
        Ok(documents) => DocumentObservation::Documents(documents),
        Err(error) if error.code() == "unsupported_capability" => {
            DocumentObservation::Unsupported(error.to_string())
        }
        Err(error) => failed(error),
    })
}

fn failed(error: IpcError) -> DocumentObservation {
    DocumentObservation::Failed {
        code: error.code().to_owned(),
        message: error.to_string(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("KICAD_IPC_ENDPOINT")
        .unwrap_or_else(|_| "ipc:///tmp/kicad/api.sock".to_owned());
    let config = ClientConfig::new(&endpoint, "auto-kicad-adapter-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let version = client.get_version()?;
    let snapshot = discovery_snapshot(DiscoveryInput {
        endpoint: endpoint.clone(),
        version,
        schematic: observe(&endpoint, DocumentQuery::Schematic)?,
        pcb: observe(&endpoint, DocumentQuery::Pcb)?,
    })?;
    println!(
        "{}",
        serde_json::json!({
            "revision": snapshot.revision()?.to_string(),
            "snapshot": snapshot,
        })
    );
    Ok(())
}
