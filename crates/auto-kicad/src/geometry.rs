//! Complete native geometry evidence, approved execution and saved verification.
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

use kicad_adapter::{
    geometry_candidate::{GeometryBaselineEvidence, GeometryCandidateEvidence},
    routing_rules::{RoutingRuleEvidence, SavedRoutingRules},
};
use kicad_engine::{Operation, OperationKind, geometry::GeometryContext};
use kicad_model::{RevisionDigest, geometry::GeometryRequest};
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryPlanningEvidence {
    pub context: GeometryContext,
    pub baseline: GeometryBaselineEvidence,
    pub saved: SavedRoutingRules,
    pub rules: Vec<RoutingRuleEvidence>,
    pub source_evidence: RevisionDigest,
}

pub(super) fn contains(operations: &[Operation]) -> bool {
    operations
        .iter()
        .any(|o| matches!(o.kind, OperationKind::EditGeometry { .. }))
}

fn connected_client(
    endpoint: &str,
    source: &LiveBoardObservation,
) -> Result<KiCadClient<NngTransport>, String> {
    let mut config =
        ClientConfig::new(endpoint, "auto-kicad-geometry").map_err(|e| e.to_string())?;
    config.timeout = std::time::Duration::from_secs(60);
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let version = client.get_version().map_err(|e| e.to_string())?;
    if version.full_version != "10.0.7" || client.instance_token() != Some(&source.instance_token) {
        return Err("geometry requires the bound KiCad 10.0.7 editor".into());
    }
    Ok(client)
}

pub(super) fn prepare(
    endpoint: &str,
    source: &LiveBoardObservation,
    request: &GeometryRequest,
) -> Result<GeometryPlanningEvidence, String> {
    if serde_json::to_vec(request)
        .map_err(|e| e.to_string())?
        .len()
        > 2 * 1024 * 1024
    {
        return Err("geometry request exceeds 2 MiB".into());
    }
    let mut client = connected_client(endpoint, source)?;
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?;
    let saved = read_saved_routing_rules(&source.board_path)?;
    let observed = observe_context(&mut client, &source.board_filename, board)?;
    let prepared = observed.prepare(request)?;
    let baseline = GeometryBaselineEvidence::capture(&observed)?;
    let mut named_nets = prepared.compiled().affected_nets().to_vec();
    named_nets.sort_by(|a, b| a.name.cmp(&b.name));
    let ids = named_nets
        .iter()
        .map(|n| n.net_id.to_string())
        .collect::<Vec<_>>();
    let mut rules = Vec::new();
    for ids in ids.chunks(64) {
        rules.push(crate::routing_rules::inspect(endpoint, source, ids)?.evidence);
    }
    observed.verify_current(&mut client, &source.board_filename)?;
    if IpcDiscoveryService.live_board_observation(endpoint, None)? != *source
        || read_saved_routing_rules(&source.board_path)? != saved
    {
        return Err("source or rules changed while preparing geometry".into());
    }
    let source_evidence = digest_bytes(
        &serde_json::to_vec(&(
            observed.context(),
            &baseline,
            &saved,
            &rules,
            &source.snapshot,
        ))
        .map_err(|e| e.to_string())?,
    );
    Ok(GeometryPlanningEvidence {
        context: observed.context().clone(),
        baseline,
        saved,
        rules,
        source_evidence,
    })
}

pub(super) fn request(operations: &[Operation]) -> Result<&GeometryRequest, String> {
    match operations {
        [
            Operation {
                kind: OperationKind::EditGeometry { request, .. },
                ..
            },
        ] => Ok(request),
        _ => Err("geometry requires one exclusive operation".into()),
    }
}

pub(super) fn matches(
    operations: &[Operation],
    evidence: &GeometryPlanningEvidence,
) -> Result<(), String> {
    match operations {
        [
            Operation {
                kind:
                    OperationKind::EditGeometry {
                        context,
                        source_evidence,
                        ..
                    },
                ..
            },
        ] if context == &evidence.context && source_evidence == &evidence.source_evidence => Ok(()),
        _ => Err("complete geometry source or rule evidence changed since planning".into()),
    }
}

fn equivalent_rules(a: &GeometryPlanningEvidence, b: &GeometryPlanningEvidence) -> bool {
    let file = |a: &kicad_adapter::routing_rules::RuleFileEvidence,
                b: &kicad_adapter::routing_rules::RuleFileEvidence| {
        a.present == b.present && a.digest == b.digest && a.size == b.size
    };
    a.baseline == b.baseline
        && a.saved.constraints == b.saved.constraints
        && file(&a.saved.project, &b.saved.project)
        && file(&a.saved.custom_rules, &b.saved.custom_rules)
        && a.rules.len() == b.rules.len()
        && a.rules
            .iter()
            .zip(&b.rules)
            .all(|(a, b)| a.equivalent_preview_rules(b))
}

/// Complete verified output; only native execution can construct this approval evidence.
#[derive(Clone, Debug, Serialize)]
pub struct GeometryExecution {
    pub preview_editor_pid: Option<u32>,
    #[serde(serialize_with = "serialize_observation")]
    pub observation: LiveBoardObservation,
    pub candidate: GeometryCandidateEvidence,
    pub summary: Value,
    pub baseline: kicad_checks::DrcReport,
    pub after: kicad_checks::DrcReport,
    pub delta: kicad_checks::BaselineDelta,
    pub creation_map: std::collections::BTreeMap<String, String>,
}

fn drc(
    source: &LiveBoardObservation,
    timeout: std::time::Duration,
) -> Result<kicad_checks::DrcReport, String> {
    let policy = kicad_checks::DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout,
    };
    match kicad_checks::run_pcb_drc(&source.board_path, &policy) {
        kicad_checks::CheckResult::Completed(report) => Ok(report),
        result => Err(format!("geometry DRC unavailable: {result:?}")),
    }
}

/// After beginning a native transaction, any unverified outcome requires manual recovery.
/// No error path retries, reloads, cancels or overwrites a possibly executing request.
pub(super) fn execute(
    endpoint: &str,
    source: &LiveBoardObservation,
    request: &GeometryRequest,
    expected: &GeometryPlanningEvidence,
    approved: Option<&GeometryExecution>,
    timeout: std::time::Duration,
) -> Result<GeometryExecution, kicad_adapter::BoardGraphicsCommitOutcome> {
    use kicad_adapter::{
        BoardGraphicsCommitOutcome,
        live_geometry::{RefillDefinitionEvidence, observe_live_geometry},
    };
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let current = prepare(endpoint, source, request).map_err(reject)?;
    if &current != expected {
        return Err(reject(
            "complete geometry evidence changed before execution".into(),
        ));
    }
    if let Some(approved) = approved
        && approved.candidate.baseline() != current.baseline
    {
        return Err(reject(
            "approved geometry baseline differs from source".into(),
        ));
    }
    let mut client = connected_client(endpoint, source).map_err(reject)?;
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or_else(|| reject("missing board identity".into()))?;
    let filename = &source.board_filename;
    let observed = observe_context(&mut client, filename, board).map_err(reject)?;
    let prepared = observed.prepare(request).map_err(reject)?;
    let items = client
        .capture_existing_board_items(filename)
        .map_err(|e| reject(e.to_string()))?;
    let footprints = client
        .capture_footprint_instance_templates(filename)
        .map_err(|e| reject(e.to_string()))?;
    let pads = source
        .snapshot
        .native_footprints
        .as_ref()
        .ok_or_else(|| reject("missing complete footprint observations".into()))?
        .placements
        .iter()
        .flat_map(|f| f.pads.iter().map(|p| p.source_uuid.to_string()))
        .collect::<Vec<_>>();
    let clusters = client
        .connected_pad_clusters(filename, &pads)
        .map_err(|e| reject(e.to_string()))?;
    let files = crate::shadow_source_fingerprint(&source.board_path);
    let protected =
        crate::existing_footprint::protected_files(&source.board_path).map_err(reject)?;
    let baseline = drc(source, timeout).map_err(reject)?;
    if let Some(approved) = approved
        && !same_drc(&approved.baseline, &baseline)
    {
        return Err(reject(
            "source DRC differs from approved preview baseline".into(),
        ));
    }
    if prepare(endpoint, source, request).map_err(reject)? != *expected
        || crate::shadow_source_fingerprint(&source.board_path) != files
        || crate::existing_footprint::protected_files(&source.board_path).map_err(reject)?
            != protected
    {
        return Err(reject("geometry source changed during preflight".into()));
    }
    observed
        .verify_current(&mut client, filename)
        .map_err(reject)?;
    // A lost begin/commit response is also uncertain; do not infer cancellation.
    let transaction = client.begin_commit().map_err(|e| unknown(e.to_string()))?;
    let receipts = prepared
        .apply_in_transaction(&mut client, filename)
        .map_err(unknown)?;
    client
        .commit_changes(&transaction, "Apply approved geometry definitions")
        .map_err(|e| unknown(e.to_string()))?;
    let mutated = observe_live_geometry(&mut client, filename).map_err(unknown)?;
    prepared
        .verify_definitions(&mutated, &receipts)
        .map_err(unknown)?;
    client
        .save_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    client
        .revert_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let before_fill = observe_live_geometry(&mut client, filename).map_err(unknown)?;
    prepared
        .verify_definitions(&before_fill, &receipts)
        .map_err(unknown)?;
    if request.refill == kicad_model::geometry::RefillMode::All {
        client = match client.request_zone_refill(filename) {
            kicad_ipc::ZoneFillAttempt::Returned { client, .. } => client,
            kicad_ipc::ZoneFillAttempt::NotDispatched { reason, .. } => {
                return Err(unknown(format!(
                    "definitions applied but refill not dispatched: {reason}"
                )));
            }
            kicad_ipc::ZoneFillAttempt::Uncertain(reader) => {
                return Err(unknown(format!(
                    "refill uncertain; no further writes: {}",
                    reader.reason()
                )));
            }
        };
    }
    let filled = observe_live_geometry(&mut client, filename).map_err(unknown)?;
    if RefillDefinitionEvidence::from_observation(&before_fill).map_err(unknown)?
        != RefillDefinitionEvidence::from_observation(&filled).map_err(unknown)?
    {
        return Err(unknown("refill changed geometry definitions".into()));
    }
    client
        .save_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    client
        .revert_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let saved_files = crate::shadow_source_fingerprint(&source.board_path);
    let reloaded = observe_live_geometry(&mut client, filename).map_err(unknown)?;
    let policy = kicad_checks::DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout,
    };
    let independent = kicad_checks::refill::refill_closed_copy(&source.board_path, &policy)
        .map_err(|e| unknown(format!("independent copper verification failed: {e:?}")))?;
    let candidate = GeometryCandidateEvidence::capture(
        &prepared,
        &receipts,
        &before_fill,
        &reloaded,
        &independent,
    )
    .map_err(unknown)?;
    if candidate.baseline() != expected.baseline {
        return Err(unknown(
            "native transaction baseline differs from plan".into(),
        ));
    }
    let creation_map = approved
        .map(|a| a.candidate.equivalent_to(&candidate))
        .transpose()
        .map_err(unknown)?
        .unwrap_or_default();
    let removed = prepared
        .compiled()
        .outline()
        .map(|o| o.removed_ids())
        .unwrap_or(&[])
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let added = receipts
        .outlines()
        .iter()
        .map(|r| r.source_uuid.clone())
        .collect::<Vec<_>>();
    let after_items = client
        .capture_existing_board_items(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let preserved = if request.outline.is_some() {
        items.matches_after_outline_replacement(&after_items, &removed, &added)
    } else {
        items.matches_saved(&after_items)
    }
    .map_err(|e| unknown(e.to_string()))?;
    if !preserved {
        return Err(unknown(
            "geometry changed unrelated native routing or graphics".into(),
        ));
    }
    if request.outline.is_some() {
        kicad_checks::outline::check_saved_outline(&source.board_path, &policy)
            .map_err(|e| unknown(format!("saved outline topology failed: {e:?}")))?;
    }
    let after_footprints = client
        .capture_footprint_instance_templates(filename)
        .map_err(|e| unknown(e.to_string()))?;
    if footprints.len() != after_footprints.len() {
        return Err(unknown("geometry changed footprint identities".into()));
    }
    for before in &footprints {
        let after = after_footprints
            .iter()
            .find(|f| f.metadata().source_uuid == before.metadata().source_uuid)
            .ok_or_else(|| unknown("geometry removed a footprint".into()))?;
        if !before
            .matches_saved_instance(after)
            .map_err(|e| unknown(e.to_string()))?
        {
            return Err(unknown(
                "geometry changed complete native footprint content".into(),
            ));
        }
    }
    let after_clusters = client
        .connected_pad_clusters(filename, &pads)
        .map_err(|e| unknown(e.to_string()))?;
    let nets = prepared
        .compiled()
        .affected_nets()
        .iter()
        .map(|n| n.net_id.to_string())
        .collect();
    if !crate::copper_route::preserves_clusters(&source.snapshot, &clusters, &after_clusters, &nets)
    {
        return Err(unknown(
            "geometry split connectivity or merged pads outside affected named nets".into(),
        ));
    }
    let after = drc(source, timeout).map_err(unknown)?;
    let delta = kicad_checks::compare_drc(&baseline, &after);
    if !delta.introduced.is_empty() || after.unconnected_items > baseline.unconnected_items {
        return Err(unknown(format!(
            "geometry regressed DRC or connectivity: {delta:?}"
        )));
    }
    if let Some(approved) = approved {
        let projected = mapped_drc(&approved.after, &creation_map);
        if !same_drc(&projected, &after) {
            return Err(unknown("source DRC differs from approved candidate".into()));
        }
    }
    let final_geometry = observe_context(&mut client, filename, board).map_err(unknown)?;
    let observation = IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .map_err(unknown)?;
    if observation.instance_token != source.instance_token
        || observation.board_path != source.board_path
        || observation.project_dir != source.project_dir
        || observation.snapshot.project_id != source.snapshot.project_id
        || observation.snapshot.board.document_id != source.snapshot.board.document_id
    {
        return Err(unknown(
            "geometry editor or document identity changed".into(),
        ));
    }
    let mut final_rules = Vec::new();
    let mut named_nets = prepared.compiled().affected_nets().to_vec();
    named_nets.sort_by(|a, b| a.name.cmp(&b.name));
    let ids = named_nets
        .iter()
        .map(|n| n.net_id.to_string())
        .collect::<Vec<_>>();
    for ids in ids.chunks(64) {
        final_rules.push(
            crate::routing_rules::inspect(endpoint, &observation, ids)
                .map_err(unknown)?
                .evidence,
        );
    }
    if final_rules != expected.rules
        || read_saved_routing_rules(&source.board_path).map_err(unknown)? != expected.saved
        || crate::existing_footprint::protected_files(&source.board_path).map_err(unknown)?
            != protected
    {
        return Err(unknown(
            "geometry changed protected rules or project files".into(),
        ));
    }
    let final_candidate = GeometryCandidateEvidence::capture(
        &prepared,
        &receipts,
        &before_fill,
        final_geometry.live(),
        &independent,
    )
    .map_err(unknown)?;
    if final_candidate != candidate
        || crate::shadow_source_fingerprint(&source.board_path) != saved_files
    {
        return Err(unknown(
            "geometry result changed during postcondition verification".into(),
        ));
    }
    final_geometry
        .verify_current(&mut client, filename)
        .map_err(unknown)?;
    Ok(GeometryExecution {
        preview_editor_pid: None,
        observation,
        candidate,
        summary: final_geometry.summary().map_err(unknown)?,
        baseline,
        after,
        delta,
        creation_map,
    })
}

fn serialize_observation<S: serde::Serializer>(
    value: &LiveBoardObservation,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let revision = value
        .snapshot
        .revision()
        .map_err(serde::ser::Error::custom)?;
    json!({"snapshot_revision":revision,"project_id":value.snapshot.project_id,"board_id":value.snapshot.board.document_id,"kicad_version":value.snapshot.kicad_version,"instance_token":value.instance_token.as_str(),"board_filename":value.board_filename,"board_path":value.board_path,"project_dir":value.project_dir}).serialize(serializer)
}

fn mapped_drc(
    report: &kicad_checks::DrcReport,
    map: &std::collections::BTreeMap<String, String>,
) -> kicad_checks::DrcReport {
    let mut result = report.clone();
    for violation in &mut result.violations {
        if let kicad_checks::ViolationIdentity::Uuids { uuids, .. } = &mut violation.identity {
            for id in uuids.iter_mut() {
                if let Some(mapped) = map.get(id) {
                    *id = mapped.clone();
                }
            }
            uuids.sort();
        }
        for item in &mut violation.items {
            if let Some(id) = &mut item.uuid
                && let Some(mapped) = map.get(id)
            {
                *id = mapped.clone();
            }
        }
    }
    result
}

pub(super) fn preview(
    server_root: &std::path::Path,
    settings: &crate::library_settings::SharedLibrarySettings,
    endpoint: &str,
    source: &LiveBoardObservation,
    operations: &[Operation],
    timeout: std::time::Duration,
) -> crate::ShadowValidationOutcome {
    use crate::{ShadowValidationOutcome as Outcome, ShadowValidationState};
    use kicad_adapter::{
        BoardGraphicsCommitOutcome,
        lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    };
    let Ok(_guard) = crate::PREVIEW_EDITOR.try_lock() else {
        return Outcome::rejected(
            "preview editor is busy; retry after the active preview finishes",
        );
    };
    let request = match request(operations) {
        Ok(request) => request,
        Err(e) => return Outcome::rejected(e),
    };
    let expected = match prepare(endpoint, source, request)
        .and_then(|e| matches(operations, &e).map(|()| e))
    {
        Ok(e) => e,
        Err(e) => return Outcome::rejected(e),
    };
    let fingerprint = crate::shadow_source_fingerprint(&source.board_path);
    let (export, path) = match crate::export_live_shadow(server_root, endpoint, source) {
        Ok(value) => value,
        Err(e) => return Outcome::rejected(e),
    };
    let mut launcher = match OwnedPcbnewLauncher::new(server_root) {
        Ok(value) => value,
        Err(e) => return Outcome::rejected(e.to_string()),
    };
    let result = (|| -> Result<GeometryExecution, BoardGraphicsCommitOutcome> {
        let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
        let shadow = launcher
            .create_shadow(&path)
            .map_err(|e| reject(e.to_string()))?;
        let root = std::fs::canonicalize(server_root).map_err(|e| reject(e.to_string()))?;
        if !shadow.is_within(&root) {
            return Err(reject("geometry shadow escaped server root".into()));
        }
        crate::footprint_authoring::prepare_shadow_library_table(
            settings,
            &source.project_dir,
            shadow.root(),
            operations,
            Some(&source.snapshot),
        )
        .map_err(reject)?;
        let identity = launcher
            .launch(&shadow)
            .map_err(|e| reject(e.to_string()))?;
        let copied = crate::observe_owned_shadow(&mut launcher).map_err(reject)?;
        let source_board = source
            .snapshot
            .board
            .document_id
            .as_ref()
            .ok_or_else(|| reject("missing source board identity".into()))?;
        let scoped = copied
            .snapshot
            .in_board_scope(source.snapshot.project_id.clone(), source_board.clone())
            .map_err(|e| reject(e.to_string()))?;
        if scoped != source.snapshot.canonical() {
            return Err(reject(
                "saved geometry PCB differs from source editor; save and create a new plan".into(),
            ));
        }
        let mut client = connected_client(&identity.endpoint, &copied).map_err(reject)?;
        let board = copied
            .snapshot
            .board
            .document_id
            .as_ref()
            .ok_or_else(|| reject("missing shadow identity".into()))?;
        let context =
            observe_context(&mut client, &copied.board_filename, board).map_err(reject)?;
        let mut rebound = request.clone();
        rebound.observed_revision = context.context().revision.clone();
        for edit in &mut rebound.zones {
            let net = match edit {
                kicad_model::geometry::ZoneEdit::Create { definition, .. } => {
                    Some(&mut definition.net)
                }
                kicad_model::geometry::ZoneEdit::Update { patch, .. } => patch.net.as_mut(),
                kicad_model::geometry::ZoneEdit::Delete { .. } => None,
            };
            if let Some(net) = net {
                net.net_id = kicad_model::board_net_id(board, &net.name);
            }
        }
        let copied_evidence = prepare(&identity.endpoint, &copied, &rebound).map_err(reject)?;
        if !equivalent_rules(&expected, &copied_evidence) {
            return Err(reject(
                "saved geometry definitions, copper or effective rules differ from source".into(),
            ));
        }
        let mut result = execute(
            &identity.endpoint,
            &copied,
            &rebound,
            &copied_evidence,
            None,
            timeout,
        )?;
        result.preview_editor_pid = Some(identity.pid);
        Ok(result)
    })();
    let mut outcome = match result {
        Ok(result) => Outcome {
            state: ShadowValidationState::Committed,
            fresh_revision: source.snapshot.revision().ok(),
            reopened_revision: result.observation.snapshot.revision().ok(),
            reopened: true,
            normalized_snapshot_matches: true,
            baseline: Some(result.baseline.clone()),
            after: Some(result.after.clone()),
            delta: Some(result.delta.clone()),
            source_unchanged: true,
            shadow_cleaned: false,
            prepared_footprints: Vec::new(),
            reason: None,
            geometry: Some(result),
        },
        Err(BoardGraphicsCommitOutcome::Rejected { reason }) => Outcome::rejected(reason),
        Err(other) => Outcome::unknown(format!("geometry preview incomplete: {other:?}")),
    };
    if let Err(reason) = prepare(endpoint, source, request).and_then(|e| {
        matches(operations, &e)?;
        if e != expected {
            return Err("geometry source evidence changed during preview".into());
        }
        Ok(())
    }) {
        outcome.state = ShadowValidationState::Rejected;
        outcome.reason = Some(reason);
    }
    outcome = crate::cleanup_shadow(&mut launcher, &source.board_path, &fingerprint, outcome);
    if let Err(e) = std::fs::remove_dir_all(&export.0) {
        outcome.state = ShadowValidationState::Unknown;
        outcome.shadow_cleaned = false;
        outcome.reason = Some(format!("geometry export cleanup failed: {e}"));
    }
    outcome
}

fn same_drc(left: &kicad_checks::DrcReport, right: &kicad_checks::DrcReport) -> bool {
    fn canonical(report: &kicad_checks::DrcReport) -> Result<Value, serde_json::Error> {
        let mut value = serde_json::to_value(report)?;
        value
            .as_object_mut()
            .expect("serialized report object")
            .remove("source");
        if let Some(violations) = value["violations"].as_array_mut() {
            for violation in violations.iter_mut() {
                if let Some(uuids) = violation["identity"]["Uuids"]["uuids"].as_array_mut() {
                    uuids.sort_by_key(Value::to_string);
                }
                if let Some(items) = violation["items"].as_array_mut() {
                    items.sort_by_key(Value::to_string);
                }
            }
            violations.sort_by_key(Value::to_string);
        }
        Ok(value)
    }
    matches!((canonical(left),canonical(right)),(Ok(a),Ok(b)) if a==b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_observation_binds_complete_snapshot_without_duplicating_it() {
        use kicad_model::{Board, ProjectSnapshot, Schematic, StableId};
        #[derive(Serialize)]
        struct Evidence<'a>(
            #[serde(serialize_with = "serialize_observation")] &'a LiveBoardObservation,
        );
        let mut source = LiveBoardObservation {
            snapshot: ProjectSnapshot {
                project_id: StableId::new("project").unwrap(),
                kicad_version: "10.0.7".into(),
                capabilities: vec![],
                schematic: Schematic::default(),
                board: Board {
                    document_id: Some(StableId::new("board").unwrap()),
                    ..Board::default()
                },
                native_footprints: None,
                native_routing: None,
                native_stackup: None,
                opaque_graphics: vec![],
            },
            instance_token: kicad_ipc::InstanceToken::new("bound-instance").unwrap(),
            board_filename: "board.kicad_pcb".into(),
            board_path: "/project/board.kicad_pcb".into(),
            project_dir: "/project".into(),
        };
        let evidence = serde_json::to_value(Evidence(&source)).unwrap();
        assert_eq!(
            evidence["snapshot_revision"],
            serde_json::to_value(source.snapshot.revision().unwrap()).unwrap()
        );
        assert_eq!(evidence["instance_token"], "bound-instance");
        assert!(evidence.get("snapshot").is_none());
        source
            .snapshot
            .capabilities
            .push("changed-observation".into());
        assert_ne!(
            evidence["snapshot_revision"],
            serde_json::to_value(Evidence(&source)).unwrap()["snapshot_revision"]
        );
    }

    #[test]
    fn candidate_drc_correspondence_maps_only_creation_identities_and_keeps_complete_diagnostics() {
        use kicad_checks::{DrcItem, DrcReport, DrcViolation, ViolationIdentity};
        let report = DrcReport {
            parser_version: "1".into(),
            schema: "1".into(),
            kicad_version: "10.0.7".into(),
            source: "preview".into(),
            ignored_checks: 0,
            unconnected_items: 0,
            violations: vec![DrcViolation {
                identity: ViolationIdentity::Uuids {
                    rule_type: "test".into(),
                    uuids: vec!["original".into(), "created".into()],
                },
                rule_type: "test".into(),
                severity: "warning".into(),
                description: "unchanged diagnostic".into(),
                items: vec![
                    DrcItem {
                        uuid: Some("created".into()),
                        description: "created".into(),
                        position: None,
                    },
                    DrcItem {
                        uuid: Some("original".into()),
                        description: "original".into(),
                        position: None,
                    },
                ],
            }],
        };
        assert!(same_drc(
            &report,
            &mapped_drc(&report, &std::collections::BTreeMap::new())
        ));
        let map = std::collections::BTreeMap::from([("created".into(), "source-created".into())]);
        let mapped = mapped_drc(&report, &map);
        assert_eq!(
            mapped.violations[0].items[0].uuid.as_deref(),
            Some("source-created")
        );
        assert_eq!(mapped.violations[0].items[0].description, "created");
        assert_eq!(
            mapped.violations[0].items[1].uuid.as_deref(),
            Some("original")
        );
        assert!(!same_drc(&report, &mapped));
        let mut actual = mapped.clone();
        actual.source = "source".into();
        actual.violations[0].items.reverse();
        assert!(same_drc(&mapped, &actual));
        actual.violations[0].severity = "error".into();
        assert!(!same_drc(&mapped, &actual));
        actual = mapped.clone();
        actual.ignored_checks += 1;
        assert!(!same_drc(&mapped, &actual));
    }
}
