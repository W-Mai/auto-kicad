use std::env;

use kicad_ipc::{
    BoardGraphicLayer, BoardGraphicSegment, ClientConfig, DocumentQuery, KiCadClient, NngTransport,
    OpenDocumentKind,
};
use serde_json::json;

fn line(x1: i64, y1: i64, x2: i64, y2: i64) -> BoardGraphicSegment {
    BoardGraphicSegment {
        start_x_nm: x1,
        start_y_nm: y1,
        end_x_nm: x2,
        end_y_nm: y2,
        width_nm: 250_000,
        layer: BoardGraphicLayer::FSilkS,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("KICAD_IPC_ENDPOINT")
        .unwrap_or_else(|_| "ipc:///tmp/kicad/api-98901.sock".to_owned());
    let config = ClientConfig::new(&endpoint, "auto-kicad-graphics-rollback-smoke")?;
    let transport = NngTransport::connect(config.clone())?;
    let mut client = KiCadClient::new(transport, config);
    let version = client.get_version()?;
    let documents = client.get_open_documents(DocumentQuery::Pcb)?;
    let board_filename = documents
        .first()
        .and_then(|document| match &document.kind {
            OpenDocumentKind::Pcb { board_filename } => Some(board_filename.as_str()),
            _ => None,
        })
        .ok_or("PCB document not returned")?;
    let graphics = vec![
        line(100_000_000, 100_000_000, 105_000_000, 100_000_000),
        line(105_000_000, 100_000_000, 110_000_000, 100_000_000),
        line(110_000_000, 100_000_000, 115_000_000, 100_000_000),
        line(115_000_000, 100_000_000, 120_000_000, 100_000_000),
        line(120_000_000, 100_000_000, 125_000_000, 100_000_000),
    ];
    let commit = client.begin_commit()?;
    let result = client.create_board_graphics(board_filename, &graphics);
    let rollback = client.drop_commit(&commit);
    result?;
    rollback?;
    println!(
        "{}",
        json!({
            "version": version.full_version,
            "board_filename": board_filename,
            "graphics": graphics.len(),
            "saved": false,
            "rolled_back": true,
        })
    );
    Ok(())
}
