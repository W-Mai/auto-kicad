//! Stable complete native geometry inspection for explicit MCP authoring.
use crate::{DiscoveryService, IpcDiscoveryService, digest_bytes};
use kicad_adapter::{
    LiveBoardObservation, geometry::observe_context, routing_rules::read_saved_routing_rules,
};
use kicad_ipc::{ClientConfig, KiCadClient, NngTransport};
use serde_json::{Value, json};

pub(crate) fn inspect(endpoint: &str, source: &LiveBoardObservation) -> Result<Value, String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?;
    let config =
        ClientConfig::new(endpoint, "auto-kicad-geometry-inspection").map_err(|e| e.to_string())?;
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let version = client.get_version().map_err(|e| e.to_string())?;
    if client.instance_token() != Some(&source.instance_token)
        || version.full_version != source.snapshot.kicad_version
    {
        return Err("editor identity changed before geometry inspection".into());
    }
    let saved = read_saved_routing_rules(&source.board_path)?;
    let observed = observe_context(&mut client, &source.board_filename, board)?;
    let mut summary = observed.summary()?;
    let fresh = IpcDiscoveryService.live_board_observation(endpoint, None)?;
    if fresh != *source || read_saved_routing_rules(&source.board_path)? != saved {
        return Err("source or saved settings changed during geometry inspection".into());
    }
    observed.verify_current(&mut client, &source.board_filename)?;
    summary["pcb_revision"] = json!(
        source
            .snapshot
            .revision()
            .map_err(|e| e.to_string())?
            .to_string()
    );
    summary["saved_rules"] = serde_json::to_value(saved).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&summary).map_err(|e| e.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("complete geometry inspection exceeds 2 MiB".into());
    }
    summary["evidence_digest"] = json!(digest_bytes(&bytes).to_string());
    Ok(summary)
}
