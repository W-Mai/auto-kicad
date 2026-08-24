use std::env;
use std::fs;
use std::time::Instant;

use kicad_ipc::{
    BoardGraphicLayer, BoardGraphicSegment, ClientConfig, DocumentQuery, KiCadClient, NngTransport,
    OpenDocumentKind,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct SegmentPayload {
    scale_mm_per_px: f64,
    origin_mm: [f64; 2],
    segments_px: Vec<[i64; 4]>,
}

fn line(payload: &SegmentPayload, segment: [i64; 4]) -> BoardGraphicSegment {
    let scale_nm = payload.scale_mm_per_px * 1_000_000.0;
    let point = |x: i64, y: i64| {
        (
            (payload.origin_mm[0] * 1_000_000.0 + x as f64 * scale_nm).round() as i64,
            (payload.origin_mm[1] * 1_000_000.0 + y as f64 * scale_nm).round() as i64,
        )
    };
    let (start_x_nm, start_y_nm) = point(segment[0], segment[1]);
    let (end_x_nm, end_y_nm) = point(segment[2], segment[3]);
    BoardGraphicSegment {
        start_x_nm,
        start_y_nm,
        end_x_nm,
        end_y_nm,
        width_nm: 100_000,
        layer: BoardGraphicLayer::DwgsUser,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = env::var("KICAD_IPC_ENDPOINT")?;
    let source = env::var("SVG_SEGMENTS_JSON")?;
    let payload: SegmentPayload = serde_json::from_str(&fs::read_to_string(source)?)?;
    let config = ClientConfig::new(&endpoint, "auto-kicad-svg-rollback-smoke")?;
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
    let graphics: Vec<_> = payload
        .segments_px
        .iter()
        .copied()
        .map(|segment| line(&payload, segment))
        .collect();
    let commit = client.begin_commit()?;
    let started = Instant::now();
    let result = client.create_board_graphics(board_filename, &graphics);
    let create_ms = started.elapsed().as_millis();
    let rollback = client.drop_commit(&commit);
    result?;
    rollback?;
    println!(
        "{}",
        json!({
            "version": version.full_version,
            "board_filename": board_filename,
            "graphics": graphics.len(),
            "create_ms": create_ms,
            "saved": false,
            "rolled_back": true,
        })
    );
    Ok(())
}
