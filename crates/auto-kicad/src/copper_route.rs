//! Native evidence and verified execution for explicit copper routes.
use super::*;
use kicad_adapter::routing_rules::{RoutingRuleEvidence, SavedRoutingConstraints};
use kicad_model::{CopperRouteIntent, RoutingAnchor};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RouteEvidence {
    pub rules: RoutingRuleEvidence,
    pub native_digest: RevisionDigest,
}

pub(super) fn contains_route(operations: &[Operation]) -> bool {
    operations
        .iter()
        .any(|op| matches!(op.kind, OperationKind::AddCopperRoute { .. }))
}

fn net_ids(kinds: &[OperationKind]) -> Result<Vec<String>, String> {
    let mut nets = BTreeSet::new();
    for kind in kinds {
        let net = match kind {
            OperationKind::AddCopperRoute { intent, .. } => Some(&intent.net),
            OperationKind::AddStraightTrack { intent, .. } => intent.net.as_ref(),
            OperationKind::AddThroughVia { intent, .. } => intent.net.as_ref(),
            _ => continue,
        }
        .ok_or("route batch copper requires an explicit net")?;
        nets.insert(net.net_id.to_string());
    }
    let ids = nets.into_iter().collect::<Vec<_>>();
    routing_rules::validate_selection(&ids)?;
    Ok(ids)
}

fn minimums(kinds: &[OperationKind], limits: &SavedRoutingConstraints) -> Result<(), String> {
    let track = |width| {
        if limits.min_track_width_nm.is_some_and(|min| width < min) {
            Err("track width is below the saved global minimum".to_owned())
        } else {
            Ok(())
        }
    };
    let via = |diameter: i64, drill: i64| {
        if limits.min_via_diameter_nm.is_some_and(|min| diameter < min)
            || limits
                .min_through_hole_diameter_nm
                .is_some_and(|min| drill < min)
            || limits
                .min_via_annular_width_nm
                .is_some_and(|min| i128::from(diameter) - i128::from(drill) < i128::from(min) * 2)
        {
            Err("via dimensions are below the saved global minimum".to_owned())
        } else {
            Ok(())
        }
    };
    for kind in kinds {
        match kind {
            OperationKind::AddCopperRoute { intent, .. } => {
                intent.validate().map_err(|e| e.to_string())?;
                for leg in &intent.legs {
                    track(leg.width_nm)?;
                }
                for v in &intent.vias {
                    via(v.pad_diameter_nm, v.drill_diameter_nm)?;
                }
            }
            OperationKind::AddStraightTrack { intent, .. } => track(intent.width_nm)?,
            OperationKind::AddThroughVia { intent, .. } => via(intent.size_nm, intent.drill_nm)?,
            _ => (),
        }
    }
    Ok(())
}

pub(super) fn prepare(
    endpoint: &str,
    source: &LiveBoardObservation,
    kinds: &[OperationKind],
) -> Result<RouteEvidence, String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?;
    for kind in kinds {
        if let OperationKind::AddCopperRoute { intent, .. } = kind {
            kicad_engine::compile_copper_route(&source.snapshot, board, intent)
                .map_err(|e| e.to_string())?;
        }
    }
    let rules = routing_rules::inspect(endpoint, source, &net_ids(kinds)?)?.evidence;
    minimums(kinds, &rules.saved.constraints)?;
    let config =
        ClientConfig::new(endpoint, "auto-kicad-route-evidence").map_err(|e| e.to_string())?;
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    client.get_version().map_err(|e| e.to_string())?;
    let native = existing_item_edit::capture(&mut client, source)?;
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
        || kicad_adapter::routing_rules::read_saved_routing_rules(&source.board_path)?
            != rules.saved
    {
        return Err("source changed during route evidence capture".into());
    }
    Ok(RouteEvidence {
        rules,
        native_digest: native.digest,
    })
}

pub(super) fn bind(kinds: &mut [OperationKind], evidence: &RouteEvidence) -> Result<(), String> {
    let rules = evidence.rules.digest()?;
    for kind in kinds {
        if let OperationKind::AddCopperRoute {
            rule_evidence,
            native_evidence,
            ..
        } = kind
        {
            *rule_evidence = rules.clone();
            *native_evidence = evidence.native_digest.clone();
        }
    }
    Ok(())
}

pub(super) fn matches(operations: &[Operation], evidence: &RouteEvidence) -> Result<(), String> {
    let rules = evidence.rules.digest()?;
    for operation in operations {
        if let OperationKind::AddCopperRoute {
            rule_evidence,
            native_evidence,
            ..
        } = &operation.kind
            && (*rule_evidence != rules || *native_evidence != evidence.native_digest)
        {
            return Err("routing rules, native objects or connectivity changed since planning; save/reload and create a new plan".into());
        }
    }
    Ok(())
}

pub(super) fn rebind_anchor(
    anchor: &RoutingAnchor,
    source: &ProjectSnapshot,
    board: &StableId,
) -> Result<RoutingAnchor, String> {
    if let RoutingAnchor::Pad {
        pad_id,
        footprint_placement_id,
    } = anchor
    {
        let (placement, pad) = kicad_engine::resolve_routing_endpoint(
            source,
            &kicad_model::RoutingEndpointRef {
                pad_id: pad_id.clone(),
                footprint_placement_id: footprint_placement_id.clone(),
            },
        )
        .map_err(|e| e.to_string())?;
        let placement_id = kicad_model::footprint_placement_id(board, &placement.source_uuid);
        Ok(RoutingAnchor::Pad {
            pad_id: kicad_model::footprint_pad_id(&placement_id, &pad.source_uuid),
            footprint_placement_id: placement_id,
        })
    } else {
        Ok(anchor.clone())
    }
}

fn verify_components(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    operations: &[Operation],
    receipts: &kicad_adapter::NativePcbBatchReceipt,
) -> Result<(), String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?;
    let mut tracks = 0;
    let mut vias = 0;
    for op in operations {
        match &op.kind {
            OperationKind::AddStraightTrack { .. } => tracks += 1,
            OperationKind::AddThroughVia { .. } => vias += 1,
            OperationKind::AddCopperRoute { intent, .. } => {
                let route = kicad_engine::compile_copper_route(&source.snapshot, board, intent)
                    .map_err(|e| e.to_string())?;
                let component = client
                    .connected_copper_ids(
                        &source.board_filename,
                        route.anchor_native_ids[0].as_str(),
                        &intent.net.name,
                    )
                    .map_err(|e| e.to_string())?;
                let route_tracks = receipts
                    .tracks
                    .get(tracks..tracks + route.tracks.len())
                    .ok_or("route track receipts are incomplete")?;
                let route_vias = receipts
                    .vias
                    .get(vias..vias + route.vias.len())
                    .ok_or("route via receipts are incomplete")?;
                if route
                    .anchor_native_ids
                    .iter()
                    .any(|id| !component.contains(id.as_str()))
                    || route_tracks
                        .iter()
                        .any(|t| !component.contains(&t.source_uuid))
                    || route_vias
                        .iter()
                        .any(|v| !component.contains(&v.source_uuid))
                {
                    return Err(
                        "route objects and anchors do not share the intended native component"
                            .into(),
                    );
                }
                tracks += route.tracks.len();
                vias += route.vias.len();
            }
            _ => (),
        }
    }
    if tracks != receipts.tracks.len() || vias != receipts.vias.len() {
        return Err("unmapped route receipts".into());
    }
    Ok(())
}

pub(super) fn preserves_clusters(
    source: &ProjectSnapshot,
    before: &[Vec<String>],
    after: &[Vec<String>],
    routed_nets: &BTreeSet<String>,
) -> bool {
    let pads = source
        .native_footprints
        .iter()
        .flat_map(|f| &f.placements)
        .flat_map(|f| &f.pads)
        .map(|p| {
            (
                p.source_uuid.as_str(),
                p.net.as_ref().map(|n| n.net_id.as_str()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let original = before
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let current = after
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let original_partition = before
        .iter()
        .enumerate()
        .flat_map(|(i, cluster)| cluster.iter().map(move |pad| (pad.as_str(), i)))
        .collect::<BTreeMap<_, _>>();
    original == current
        && kicad_engine::preserves_pad_connectivity(before, after)
        && after.iter().all(|cluster| {
            let groups = cluster
                .iter()
                .filter_map(|id| original_partition.get(id.as_str()))
                .collect::<BTreeSet<_>>();
            if groups.len() <= 1 {
                return true;
            }
            let nets = cluster
                .iter()
                .map(|id| pads.get(id.as_str()).copied().flatten())
                .collect::<BTreeSet<_>>();
            nets.len() == 1
                && nets
                    .first()
                    .copied()
                    .flatten()
                    .is_some_and(|net| routed_nets.contains(net))
        })
}

#[derive(Clone, Copy)]
pub(super) enum ExecutionContext {
    // The disposable validation pipeline owns its DRC reports and rejection state.
    Preview,
    Source,
}

pub(super) fn execute(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    operations: &[Operation],
    endpoint: &str,
    context: ExecutionContext,
) -> Result<ProjectSnapshot, BoardGraphicsCommitOutcome> {
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let kinds = operations
        .iter()
        .map(|op| op.kind.clone())
        .collect::<Vec<_>>();
    let evidence = prepare(endpoint, source, &kinds).map_err(reject)?;
    matches(operations, &evidence).map_err(reject)?;
    let revision = source
        .snapshot
        .revision()
        .map_err(|e| reject(e.to_string()))?;
    let batch = kicad_adapter::prepare_bound_native_batch(operations, &source.snapshot, &revision)?;
    if batch.graphics.len() > 100 {
        return Err(reject("route batch graphics exceed 100".into()));
    }
    let before = existing_item_edit::capture(client, source).map_err(reject)?;
    if before.digest != evidence.native_digest {
        return Err(reject("native route evidence changed".into()));
    }
    let protected = existing_footprint::protected_files(&source.board_path).map_err(reject)?;
    let files = shadow_source_fingerprint(&source.board_path);
    let policy = DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout: Duration::from_secs(30),
    };
    let baseline = match context {
        ExecutionContext::Preview => None,
        ExecutionContext::Source => {
            let CheckResult::Completed(report) = run_pcb_drc(&source.board_path, &policy) else {
                return Err(reject("route baseline DRC unavailable".into()));
            };
            Some(report)
        }
    };
    let current = prepare(endpoint, source, &kinds).map_err(reject)?;
    matches(operations, &current).map_err(reject)?;
    if shadow_source_fingerprint(&source.board_path) != files
        || existing_footprint::protected_files(&source.board_path).map_err(reject)? != protected
    {
        return Err(reject("route source files changed before execution".into()));
    }
    let receipts = BoardGraphicsExecutor {
        max_segments: kicad_model::copper_route::MAX_BATCH_COPPER + 100,
    }
    .execute_native_batch(
        client,
        &source.board_filename,
        &batch.graphics,
        &batch.tracks,
        &batch.vias,
        "auto-kicad approved copper route",
    )?;
    let new_ids = receipts
        .tracks
        .iter()
        .map(|t| t.source_uuid.clone())
        .chain(receipts.vias.iter().map(|v| v.source_uuid.clone()))
        .chain(receipts.graphics.iter().map(|g| g.graphic_id.clone()))
        .collect::<Vec<_>>();
    let saved =
        kicad_adapter::read_verified_native_routing(client, &source.board_filename, &receipts)
            .map_err(unknown)?;
    let mut expected = None;
    for reload in [false, true] {
        if reload {
            client
                .revert_board(&source.board_filename)
                .map_err(|e| unknown(e.to_string()))?;
        }
        let routing =
            kicad_adapter::read_verified_native_routing(client, &source.board_filename, &receipts)
                .map_err(unknown)?;
        if !kicad_adapter::created_routing_matches_reloaded(&saved, &routing) {
            return Err(unknown("route content changed on reload".into()));
        }
        let actual = client
            .capture_existing_board_items(&source.board_filename)
            .map_err(|e| unknown(e.to_string()))?;
        if !before
            .items
            .matches_after_additions(&actual, &new_ids)
            .map_err(|e| unknown(e.to_string()))?
        {
            return Err(unknown(
                "route changed original native board objects".into(),
            ));
        }
        let footprints = client
            .capture_footprint_instance_templates(&source.board_filename)
            .map_err(|e| unknown(e.to_string()))?;
        if footprints.len() != before.footprints.len() {
            return Err(unknown("route changed footprint set".into()));
        }
        for original in &before.footprints {
            let current = footprints
                .iter()
                .find(|f| f.metadata().source_uuid == original.metadata().source_uuid)
                .ok_or_else(|| unknown("route removed an original footprint".into()))?;
            if !original
                .matches_saved_instance(current)
                .map_err(|e| unknown(e.to_string()))?
            {
                return Err(unknown(
                    "route changed complete native footprint content".into(),
                ));
            }
        }
        let pads = before
            .clusters
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let clusters = client
            .connected_pad_clusters(&source.board_filename, &pads)
            .map_err(|e| unknown(e.to_string()))?;
        if !preserves_clusters(
            &source.snapshot,
            &before.clusters,
            &clusters,
            &net_ids(&kinds).map_err(unknown)?.into_iter().collect(),
        ) {
            return Err(unknown(
                "route split pad connectivity or merged different net identities".into(),
            ));
        }
        verify_components(client, source, operations, &receipts).map_err(unknown)?;
        let predicted = expected_native_shadow_snapshot(source, operations, &receipts, routing)
            .map_err(unknown)?;
        let observed = IpcDiscoveryService
            .live_board_observation(endpoint, None)
            .map_err(unknown)?;
        if observed.snapshot.canonical() != predicted
            || observed.instance_token != source.instance_token
            || observed.board_path != source.board_path
            || observed.project_dir != source.project_dir
        {
            return Err(unknown(
                "saved route differs from approved native expectation".into(),
            ));
        }
        let rules = routing_rules::inspect(endpoint, &observed, &net_ids(&kinds).map_err(unknown)?)
            .map_err(unknown)?
            .evidence;
        if rules != evidence.rules {
            return Err(unknown(format!(
                "routing rules changed during execution: saved={} stackup={} netclasses={}",
                rules.saved == evidence.rules.saved,
                rules.stackup == evidence.rules.stackup,
                rules.netclasses == evidence.rules.netclasses
            )));
        }
        expected = Some(predicted);
    }
    if existing_footprint::protected_files(&source.board_path).map_err(unknown)? != protected {
        return Err(unknown(
            "route changed protected project, rules or schematic".into(),
        ));
    }
    if let Some(baseline) = baseline {
        let CheckResult::Completed(after) = run_pcb_drc(&source.board_path, &policy) else {
            return Err(unknown("route saved DRC unavailable".into()));
        };
        let delta = compare_drc(&baseline, &after);
        if !delta.introduced.is_empty() || after.unconnected_items > baseline.unconnected_items {
            return Err(unknown(format!(
                "route regressed DRC or connectivity: introduced {:?}; unconnected {} -> {}",
                delta.introduced, baseline.unconnected_items, after.unconnected_items
            )));
        }
    }
    expected.ok_or_else(|| unknown("route saved verification missing".into()))
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardRouteRequest {
    pub route_id: String,
    pub observed_revision: String,
    pub start: RouteAnchorRequest,
    pub end: RouteAnchorRequest,
    pub net: BoardRoutingNetRequest,
    #[schemars(length(min = 1, max = 16))]
    pub legs: Vec<RouteLegRequest>,
    #[schemars(length(max = 15))]
    pub vias: Vec<RouteViaRequest>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteAnchorRequest {
    Pad {
        pad_id: String,
        footprint_placement_id: String,
    },
    TrackEnd {
        source_uuid: String,
        end: RouteEndRequest,
    },
    Via {
        source_uuid: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteEndRequest {
    Start,
    End,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutePointRequest {
    pub x_nm: i64,
    pub y_nm: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteLegRequest {
    /// Canonical enabled copper layer: F.Cu, B.Cu, or In1.Cu through In30.Cu.
    pub layer: String,
    pub width_nm: i64,
    #[schemars(length(min = 2, max = 129))]
    pub points: Vec<RoutePointRequest>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteViaRequest {
    pub via_id: String,
    pub position: RoutePointRequest,
    pub pad_diameter_nm: i64,
    pub drill_diameter_nm: i64,
}
impl BoardRouteRequest {
    pub(super) fn into_model(self) -> Result<CopperRouteIntent, String> {
        let mut value = serde_json::to_value(&self).map_err(|e| e.to_string())?;
        value["observed_revision"] = Value::String(
            self.observed_revision
                .strip_prefix("sha256:")
                .ok_or("route revision must use the observed sha256-prefixed value")?
                .into(),
        );
        let intent: CopperRouteIntent = serde_json::from_value(value).map_err(|e| e.to_string())?;
        intent.validate().map_err(|e| e.to_string())?;
        Ok(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"route_id":"route","observed_revision":format!("sha256:{}", "0".repeat(64)),"start":{"kind":"via","source_uuid":"a"},"end":{"kind":"via","source_uuid":"b"},"net":{"net_id":"net","name":"GND"},"legs":[{"layer":"F.Cu","width_nm":250000,"points":[{"x_nm":0,"y_nm":0},{"x_nm":1000000,"y_nm":0}]}],"vias":[]})
    }
    fn kind() -> OperationKind {
        OperationKind::AddCopperRoute {
            board: StableId::new("board").unwrap(),
            intent: serde_json::from_value::<BoardRouteRequest>(request())
                .unwrap()
                .into_model()
                .unwrap(),
            rule_evidence: digest_bytes(b"rules"),
            native_evidence: digest_bytes(b"native"),
        }
    }
    #[test]
    fn public_route_grammar_is_explicit_bounded_and_uses_observed_revisions() {
        let route = serde_json::from_value::<BoardRouteRequest>(request())
            .unwrap()
            .into_model()
            .unwrap();
        assert_eq!(route.primitives().unwrap().0.len(), 1);
        let changes: [fn(&mut Value); 6] = [
            |v| v["observed_revision"] = json!("0".repeat(64)),
            |v| v["legs"][0]["layer"] = json!("F.Mask"),
            |v| v["legs"][0]["points"] = json!([]),
            |v| v["start"]["kind"] = json!("arc"),
            |v| v["legs"][0]["width_nm"] = json!(0),
            |v| v["rule_evidence"] = json!("caller-forged-evidence"),
        ];
        for change in changes {
            let mut value = request();
            change(&mut value);
            assert!(
                serde_json::from_value::<BoardRouteRequest>(value)
                    .map_err(|e| e.to_string())
                    .and_then(BoardRouteRequest::into_model)
                    .is_err()
            );
        }
    }
    #[test]
    fn saved_minima_are_exact_and_do_not_infer_missing_constraints() {
        let kinds = [kind()];
        assert!(minimums(&kinds, &SavedRoutingConstraints::default()).is_ok());
        let mut limits = SavedRoutingConstraints {
            min_track_width_nm: Some(250000),
            ..Default::default()
        };
        assert!(minimums(&kinds, &limits).is_ok());
        limits.min_track_width_nm = Some(250001);
        assert!(minimums(&kinds, &limits).is_err());
        let via = OperationKind::AddThroughVia {
            board: StableId::new("board").unwrap(),
            intent: ThroughViaIntent::new(
                StableId::new("v").unwrap(),
                kicad_model::RoutePoint { x_nm: 0, y_nm: 0 }.coordinate(),
                600000,
                300000,
            )
            .unwrap(),
        };
        let mut limits = SavedRoutingConstraints {
            min_via_annular_width_nm: Some(150000),
            ..Default::default()
        };
        assert!(minimums(std::slice::from_ref(&via), &limits).is_ok());
        limits.min_via_annular_width_nm = Some(150001);
        assert!(minimums(&[via], &limits).is_err());
    }
    #[test]
    fn route_pad_partitions_preserve_every_pad_and_disallow_foreign_net_merges() {
        let mut source = crate::tests::fresh_shadow_observation().snapshot;
        for pad in source
            .native_footprints
            .as_mut()
            .unwrap()
            .placements
            .iter_mut()
            .flat_map(|f| &mut f.pads)
        {
            pad.net = Some(kicad_model::ObservedNetIdentity {
                net_id: StableId::new("route-net").unwrap(),
                name: "ROUTE".into(),
            });
        }
        let routed = BTreeSet::from(["route-net".to_owned()]);
        let mut pads = source
            .native_footprints
            .as_ref()
            .unwrap()
            .placements
            .iter()
            .flat_map(|f| &f.pads)
            .map(|p| p.source_uuid.to_string())
            .collect::<Vec<_>>();
        pads.sort();
        assert!(pads.len() >= 2);
        let before = pads.iter().map(|p| vec![p.clone()]).collect::<Vec<_>>();
        let merged = vec![pads.clone()];
        assert!(preserves_clusters(&source, &before, &merged, &routed));
        assert!(!preserves_clusters(
            &source,
            &before,
            &merged,
            &BTreeSet::new()
        ));
        assert!(!preserves_clusters(&source, &merged, &before, &routed));
        assert!(!preserves_clusters(
            &source,
            &before,
            &[pads[1..].to_vec()],
            &routed
        ));
        source.native_footprints.as_mut().unwrap().placements[0].pads[0].net =
            Some(kicad_model::ObservedNetIdentity {
                net_id: StableId::new("other-net").unwrap(),
                name: "OTHER".into(),
            });
        assert!(!preserves_clusters(&source, &before, &merged, &routed));
    }
    #[test]
    fn detected_rule_drift_revokes_approval_even_when_original_evidence_returns() {
        use kicad_adapter::routing_rules::{RuleFileEvidence, SavedRoutingRules};
        #[derive(Clone)]
        struct Discovery(Arc<Mutex<RouteEvidence>>);
        impl DiscoveryService for Discovery {
            fn discover(
                &self,
                _: &str,
                _: Option<&std::path::Path>,
            ) -> Result<DiscoverySnapshot, String> {
                Err("unused".into())
            }
            fn copper_route_evidence(
                &self,
                _: &str,
                _: &LiveBoardObservation,
                _: &[OperationKind],
            ) -> Result<RouteEvidence, String> {
                Ok(self.0.lock().unwrap().clone())
            }
        }
        let source = crate::tests::fresh_shadow_observation();
        let file = RuleFileEvidence {
            path: "/bound/project".into(),
            present: true,
            size: 1,
            digest: Some(digest_bytes(b"file")),
        };
        let evidence = RouteEvidence {
            native_digest: digest_bytes(b"native"),
            rules: RoutingRuleEvidence {
                saved: SavedRoutingRules {
                    project: file.clone(),
                    custom_rules: file,
                    constraints: SavedRoutingConstraints::default(),
                },
                stackup: source.snapshot.native_stackup.clone().unwrap(),
                netclasses: BTreeMap::new(),
            },
        };
        let state = Arc::new(Mutex::new(evidence.clone()));
        let server = Server::with_discovery(Arc::new(Discovery(state.clone())));
        let mut kinds = [kind()];
        bind(&mut kinds, &evidence).unwrap();
        let op = Operation {
            operation_id: StableId::new("op").unwrap(),
            kind: kinds[0].clone(),
            targets: vec![],
            preconditions: vec![],
            expected_effects: vec![],
            risk_class: RiskClass::Medium,
            idempotency_key: "op".into(),
            source_run_id: "test".into(),
        };
        let plan = ChangePlan {
            plan_id: StableId::new("plan").unwrap(),
            source_run_id: "test".into(),
            base_revision: source.snapshot.revision().unwrap(),
            instance_token: source.instance_token.as_str().to_owned(),
            operations: vec![op],
        };
        let hash = plan.hash().unwrap();
        let stored = StoredChangeSession {
            session: ChangeSession {
                plan,
                plan_hash: hash,
                state: kicad_engine::SessionState::AwaitingApproval,
                approval_hash: Some("approved".into()),
                snapshot: source.snapshot.clone(),
                audit: vec![],
                committed_idempotency: BTreeMap::new(),
            },
            endpoint: "test".into(),
            board_filename: source.board_filename.clone(),
            board_path: source.board_path.clone(),
            project_dir: source.project_dir.clone(),
            source_files: shadow_source_fingerprint(&source.board_path),
            routing_evidence: Some(evidence.clone()),
            geometry_evidence: None,
            execution_state: StoredExecutionState::Ready,
            shadow_validation: None,
            validation_evidence_hash: Some("evidence".into()),
            approval_hash: Some("approved".into()),
        };
        assert!(server.verify_native_source(&stored, &source).is_ok());
        server
            .sessions
            .lock()
            .unwrap()
            .insert("plan".into(), stored.clone());
        state
            .lock()
            .unwrap()
            .rules
            .saved
            .constraints
            .min_track_width_nm = Some(999);
        assert!(server.verify_native_source(&stored, &source).is_err());
        server.invalidate_native_approval("plan");
        *state.lock().unwrap() = evidence;
        let persisted = server.sessions.lock().unwrap();
        let invalidated = &persisted["plan"];
        assert!(
            invalidated.approval_hash.is_none()
                && invalidated.validation_evidence_hash.is_none()
                && invalidated.session.approval_hash.is_none()
        );
        assert_eq!(
            invalidated.session.state,
            kicad_engine::SessionState::Rejected
        );
        let mut different = stored.session.plan.clone();
        if let OperationKind::AddCopperRoute { rule_evidence, .. } =
            &mut different.operations[0].kind
        {
            *rule_evidence = digest_bytes(b"changed");
        }
        assert_ne!(different.hash().unwrap(), stored.session.plan_hash);
    }
}
