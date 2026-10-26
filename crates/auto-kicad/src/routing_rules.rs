//! Live board-bound rule inspection for explicit existing net identities.
use crate::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::{
    LiveBoardObservation,
    routing_rules::{RoutingRuleEvidence, observe_routing_rules, read_saved_routing_rules},
};
use kicad_ipc::{BoardNet, ClientConfig, KiCadClient, NngTransport};
use kicad_model::{RevisionDigest, StableId, board_net_id};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutingRulesRequest {
    /// Select 1 through 64 unique net IDs already observed on the connected board.
    #[schemars(length(min = 1, max = 64))]
    pub net_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RoutingRuleInspection {
    pub nets: BTreeMap<String, String>,
    pub evidence: RoutingRuleEvidence,
    #[serde(serialize_with = "serialize_digest")]
    pub evidence_digest: RevisionDigest,
    #[serde(serialize_with = "serialize_digest")]
    pub observed_revision: RevisionDigest,
}

fn serialize_digest<S: serde::Serializer>(
    digest: &RevisionDigest,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_str(digest)
}

pub(crate) fn validate_selection(ids: &[String]) -> Result<(), String> {
    if ids.is_empty()
        || ids.len() > 64
        || ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 4096 || id.contains('\0'))
        || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
    {
        return Err("routing rules require 1 through 64 bounded unique net IDs".into());
    }
    Ok(())
}

fn resolve_nets(
    board: &StableId,
    nets: &[BoardNet],
    ids: &[String],
) -> Result<BTreeMap<String, String>, String> {
    validate_selection(ids)?;
    let mut observed = BTreeMap::new();
    for net in nets.iter().filter(|net| !net.name.is_empty()) {
        let id = board_net_id(board, &net.name).to_string();
        if observed.insert(id, net.name.clone()).is_some() {
            return Err("duplicate native net identity".into());
        }
    }
    ids.iter()
        .map(|id| {
            observed
                .get(id)
                .map(|name| (id.clone(), name.clone()))
                .ok_or_else(|| "requested routing net ID is absent from the connected board".into())
        })
        .collect()
}

pub(crate) fn inspect(
    endpoint: &str,
    source: &LiveBoardObservation,
    ids: &[String],
) -> Result<RoutingRuleInspection, String> {
    validate_selection(ids)?;
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("observed board identity is missing")?;
    let config =
        ClientConfig::new(endpoint, "auto-kicad-routing-rules").map_err(|e| e.to_string())?;
    let mut ipc = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let version = ipc.get_version().map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&source.instance_token)
        || version.full_version != source.snapshot.kicad_version
    {
        return Err("editor identity changed before rule inspection".into());
    }
    let native_nets = ipc
        .get_board_nets(&source.board_filename)
        .map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&source.instance_token) {
        return Err("editor identity changed during net inspection".into());
    }
    let nets = resolve_nets(board, &native_nets, ids)?;
    let names = nets.values().cloned().collect::<Vec<_>>();
    let evidence = observe_routing_rules(&mut ipc, &source.board_path, &names)?;
    if source.snapshot.native_stackup.as_ref() != Some(&evidence.stackup) {
        return Err("stackup changed during rule inspection; inspect again".into());
    }
    let repeated = ipc
        .get_board_nets(&source.board_filename)
        .map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&source.instance_token)
        || resolve_nets(board, &repeated, ids)? != nets
    {
        return Err("net identities changed during rule inspection".into());
    }
    let fresh = IpcDiscoveryService.live_board_observation(endpoint, None)?;
    if fresh != *source || read_saved_routing_rules(&source.board_path)? != evidence.saved {
        return Err("source changed during rule inspection; save/reload and inspect again".into());
    }
    Ok(RoutingRuleInspection {
        nets,
        evidence_digest: evidence.digest()?,
        evidence,
        observed_revision: source.snapshot.revision().map_err(|e| e.to_string())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selections_are_bounded_unique_and_bound_to_existing_board_nets() {
        let board = StableId::new("board").unwrap();
        let foreign = StableId::new("foreign").unwrap();
        let nets = vec![BoardNet {
            name: "GND".into(),
            code: Some(1),
        }];
        let id = board_net_id(&board, "GND").to_string();
        assert_eq!(
            resolve_nets(&board, &nets, std::slice::from_ref(&id)).unwrap()[&id],
            "GND"
        );
        for ids in [
            vec![],
            vec![id.clone(), id.clone()],
            vec!["x".repeat(4097)],
            vec!["x\0y".into()],
            (0..65).map(|i| format!("id-{i}")).collect(),
        ] {
            assert!(validate_selection(&ids).is_err());
        }
        for id in [
            board_net_id(&foreign, "GND").to_string(),
            board_net_id(&board, "MISSING").to_string(),
        ] {
            assert!(resolve_nets(&board, &nets, &[id]).is_err());
        }
        assert!(resolve_nets(&board, &[nets[0].clone(), nets[0].clone()], &[id]).is_err());
        assert!(
            serde_json::from_value::<RoutingRulesRequest>(
                serde_json::json!({"net_ids":["net"],"project_path":"/arbitrary"})
            )
            .is_err()
        );
    }
}
