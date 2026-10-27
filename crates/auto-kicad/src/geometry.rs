//! Complete native geometry evidence, approved execution and saved verification.
use crate::{DiscoveryService, IpcDiscoveryService, digest_bytes};
use kicad_adapter::{
    LiveBoardObservation, geometry::observe_generated_context,
    routing_rules::read_saved_routing_rules,
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
    let observed = observe_generated_context(&mut client, &source.board_filename, board)?;
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
    pub library_context: Option<kicad_adapter::library_catalog::NativeDrcLibraries>,
    pub saved: SavedRoutingRules,
    pub rules: Vec<RoutingRuleEvidence>,
    pub source_evidence: RevisionDigest,
    pub additions: Option<crate::geometry_additive::AdditiveEvidence>,
    pub affected_net_ids: Vec<String>,
}

pub(super) fn contains(operations: &[Operation]) -> bool {
    operations
        .iter()
        .any(|o| matches!(o.kind, OperationKind::EditGeometry { .. }))
}

pub(super) fn connected_client(
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
    prepare_with_library_context(endpoint, source, request, None, &[], None)
}

pub(super) fn prepare_with_library_context(
    endpoint: &str,
    source: &LiveBoardObservation,
    request: &GeometryRequest,
    reference: Option<&kicad_adapter::library_catalog::NativeDrcLibraries>,
    kinds: &[OperationKind],
    creation_identities: Option<&kicad_adapter::additive_identities::AdditiveCreationIdentities>,
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
    let observed = observe_generated_context(&mut client, &source.board_filename, board)?;
    let prepared = observed.prepare(request)?;
    let baseline = GeometryBaselineEvidence::capture(&observed)?;
    let library_context = if !kinds.is_empty()
        || observed
            .generated()
            .is_some_and(|g| g.generated_count() > 0)
    {
        Some(crate::geometry_drc::libraries(
            &mut client,
            source,
            reference,
        )?)
    } else {
        None
    };
    let additions = if kinds.is_empty() {
        None
    } else {
        let mut additions = crate::geometry_additive::compile(source, kinds)?;
        if let Some(ids) = creation_identities {
            ids.validate_batch(&additions.batch)?;
            additions.identities = ids.clone();
        }
        prepared
            .clone()
            .with_additions(&mut client, additions.batch.clone())?
            .with_addition_identities(additions.identities.clone())?;
        crate::copper_route::minimums(kinds, &saved.constraints)?;
        Some(additions)
    };
    let mut named_nets = prepared.compiled().affected_nets().to_vec();
    if let Some(additions) = &additions {
        for name in additions.net_names() {
            let net = observed
                .context()
                .nets
                .iter()
                .find(|n| n.name == name)
                .ok_or("additive net is absent from complete geometry context")?;
            if !named_nets.contains(net) {
                named_nets.push(net.clone());
            }
        }
    }
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
    let mut additions = additions;
    if let Some(additions) = &mut additions {
        let native = digest_bytes(
            &serde_json::to_vec(&(observed.context(), &baseline, &source.snapshot))
                .map_err(|e| e.to_string())?,
        );
        let rule_digest = digest_bytes(&serde_json::to_vec(&rules).map_err(|e| e.to_string())?);
        additions.bind(&native, &rule_digest);
    }
    let source_evidence = digest_bytes(
        &serde_json::to_vec(&(
            observed.context(),
            &baseline,
            &library_context,
            &saved,
            &rules,
            &source.snapshot,
            &additions,
        ))
        .map_err(|e| e.to_string())?,
    );
    Ok(GeometryPlanningEvidence {
        context: observed.context().clone(),
        baseline,
        library_context,
        saved,
        rules,
        source_evidence,
        additions,
        affected_net_ids: ids,
    })
}

pub(super) fn request(operations: &[Operation]) -> Result<&GeometryRequest, String> {
    kicad_engine::geometry_plan::geometry_plan(operations)
        .map(|plan| plan.request)
        .map_err(|e| e.to_string())
}

pub(super) fn prepare_operations(
    endpoint: &str,
    source: &LiveBoardObservation,
    operations: &[Operation],
) -> Result<GeometryPlanningEvidence, String> {
    let plan = kicad_engine::geometry_plan::geometry_plan(operations).map_err(|e| e.to_string())?;
    let kinds = plan
        .additions
        .iter()
        .map(|op| op.kind.clone())
        .collect::<Vec<_>>();
    prepare_with_library_context(endpoint, source, plan.request, None, &kinds, None)
}

pub(super) fn matches(
    operations: &[Operation],
    evidence: &GeometryPlanningEvidence,
) -> Result<(), String> {
    let plan = kicad_engine::geometry_plan::geometry_plan(operations).map_err(|e| e.to_string())?;
    let kinds = plan
        .additions
        .iter()
        .map(|op| op.kind.clone())
        .collect::<Vec<_>>();
    if plan.context != &evidence.context
        || plan.source_evidence != &evidence.source_evidence
        || evidence
            .additions
            .as_ref()
            .map(|a| a.kinds.as_slice())
            .unwrap_or(&[])
            != kinds
    {
        return Err(
            "complete geometry source, additions or rule evidence changed since planning".into(),
        );
    }
    Ok(())
}

fn equivalent_rules(a: &GeometryPlanningEvidence, b: &GeometryPlanningEvidence) -> bool {
    let file = |a: &kicad_adapter::routing_rules::RuleFileEvidence,
                b: &kicad_adapter::routing_rules::RuleFileEvidence| {
        a.present == b.present && a.digest == b.digest && a.size == b.size
    };
    a.baseline == b.baseline
        && match (&a.additions, &b.additions) {
            (None, None) => true,
            (Some(a), Some(b)) => a.batch == b.batch && a.identities == b.identities,
            _ => false,
        }
        && match (&a.library_context, &b.library_context) {
            (None, None) => true,
            (Some(a), Some(b)) => a.equivalent_to(b),
            _ => false,
        }
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
    pub baseline: kicad_checks::CompleteDrcReport,
    pub after: kicad_checks::CompleteDrcReport,
    pub delta: kicad_checks::BaselineDelta,
    pub creation_map: std::collections::BTreeMap<String, String>,
    pub baseline_map: std::collections::BTreeMap<String, String>,
    pub diagnostic_map: std::collections::BTreeMap<String, String>,
    pub baseline_drc_input: Option<crate::geometry_drc::PrivateDrcEvidence>,
    pub after_drc_input: Option<crate::geometry_drc::PrivateDrcEvidence>,
    pub baseline_connectivity: Option<crate::geometry_connectivity::ReportConnectivity>,
    pub after_connectivity: Option<crate::geometry_connectivity::ReportConnectivity>,
}

fn drc(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    observed: &kicad_adapter::teardrop_refill::TeardropRefillObservation,
    libraries: Option<&kicad_adapter::library_catalog::NativeDrcLibraries>,
    timeout: std::time::Duration,
    connectivity_required: bool,
) -> Result<
    (
        kicad_checks::CompleteDrcReport,
        Option<crate::geometry_drc::PrivateDrcEvidence>,
        Option<crate::geometry_connectivity::ReportConnectivity>,
    ),
    String,
> {
    let policy = kicad_checks::DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout,
    };
    let connectivity_context = if connectivity_required {
        let board = source
            .snapshot
            .board
            .document_id
            .as_ref()
            .ok_or("missing connectivity board identity")?;
        let context = observe_generated_context(client, &source.board_filename, board)?;
        let complete = context
            .generated()
            .ok_or("missing complete connectivity context")?;
        if complete.native() != observed.native()
            || complete.text().raw_text() != observed.text().raw_text()
        {
            return Err("DRC and connectivity observations differ".into());
        }
        let proof = crate::geometry_connectivity::capture(client, source, &context, &policy)?;
        Some((context, proof))
    } else {
        None
    };
    let (report, input) = if observed.generated_count() > 0 {
        let (report, input) = crate::geometry_drc::run(
            client,
            source,
            observed,
            libraries.ok_or("missing generated DRC library context")?,
            timeout,
            connectivity_required,
        )?;
        (report, Some(input))
    } else {
        (
            kicad_checks::run_pcb_drc_complete(&source.board_path, &policy)
                .map_err(|error| format!("geometry DRC unavailable: {error:?}"))?,
            None,
        )
    };
    if report.normalized().kicad_version != source.snapshot.kicad_version
        || source.board_path.file_name().and_then(|name| name.to_str())
            != Some(report.normalized().source.as_str())
    {
        return Err("geometry DRC version or source differs from bound editor".into());
    }
    let connectivity = connectivity_context
        .map(|(context, proof)| {
            proof.verify_current(
                client,
                &source.board_filename,
                &context,
                &source.board_path,
                &policy,
            )?;
            crate::geometry_connectivity::ReportConnectivity::bind(proof, &report, input.as_ref())
        })
        .transpose()?;
    Ok((report, input, connectivity))
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
        BoardGraphicsCommitOutcome, live_geometry::RefillDefinitionEvidence,
        teardrop_refill::TeardropRefillObservation,
    };
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let current = prepare_with_library_context(
        endpoint,
        source,
        request,
        expected.library_context.as_ref(),
        expected.additive_kinds(),
        expected.additions.as_ref().map(|a| &a.identities),
    )
    .map_err(reject)?;
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
    let observed = observe_generated_context(&mut client, filename, board).map_err(reject)?;
    let prepared = observed.prepare(request).map_err(reject)?;
    let prepared = match &expected.additions {
        Some(additions) => prepared
            .with_additions(&mut client, additions.batch.clone())
            .and_then(|p| p.with_addition_identities(additions.identities.clone()))
            .map_err(reject)?,
        None => prepared,
    };
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
    let initial = observed
        .generated()
        .ok_or_else(|| reject("complete geometry observation missing".into()))?;
    let (baseline, baseline_drc_input, baseline_connectivity) = drc(
        &mut client,
        source,
        initial,
        expected.library_context.as_ref(),
        timeout,
        expected.additions.is_some(),
    )
    .map_err(reject)?;
    let baseline_map = if let Some(approved) = approved {
        match approved.candidate.generated_baseline() {
            Some(before) => before
                .equivalent_to(&initial.generated_correspondence())
                .map_err(reject)?,
            None if initial.generated_count() == 0 => std::collections::BTreeMap::new(),
            None => return Err(reject("approved generated baseline missing".into())),
        }
    } else {
        std::collections::BTreeMap::new()
    };
    if let Some(approved) = approved
        && !crate::geometry_connectivity::equivalent(
            &approved.baseline,
            &baseline,
            &baseline_map,
            approved.baseline_connectivity.as_ref(),
            baseline_connectivity.as_ref(),
        )
        .map_err(reject)?
    {
        return Err(reject(
            "source DRC differs from approved preview baseline".into(),
        ));
    }
    if prepare_with_library_context(
        endpoint,
        source,
        request,
        expected.library_context.as_ref(),
        expected.additive_kinds(),
        expected.additions.as_ref().map(|a| &a.identities),
    )
    .map_err(reject)?
        != *expected
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
    let mutated = TeardropRefillObservation::capture(&mut client, filename).map_err(unknown)?;
    if expected.additions.is_some() {
        prepared
            .verify_additive_definitions(&mut client, filename, &mutated, &receipts, false)
            .map_err(unknown)?;
    } else {
        prepared
            .verify_generated_definitions(&mutated, &receipts)
            .map_err(unknown)?;
    }
    client
        .save_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    client
        .revert_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let before_fill = TeardropRefillObservation::capture(&mut client, filename).map_err(unknown)?;
    let additive_definitions = if expected.additions.is_some() {
        Some(
            prepared
                .verify_additive_definitions(&mut client, filename, &before_fill, &receipts, true)
                .map_err(unknown)?,
        )
    } else {
        prepared
            .verify_generated_definitions_after_reload(&before_fill, &receipts)
            .map_err(unknown)?;
        None
    };
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
    let filled = TeardropRefillObservation::capture(&mut client, filename).map_err(unknown)?;
    if RefillDefinitionEvidence::from_observation(
        &before_fill.authored_geometry().map_err(unknown)?,
    )
    .map_err(unknown)?
        != RefillDefinitionEvidence::from_observation(&filled.authored_geometry().map_err(unknown)?)
            .map_err(unknown)?
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
    let reloaded = TeardropRefillObservation::capture(&mut client, filename).map_err(unknown)?;
    let policy = kicad_checks::DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout,
    };
    let independent = kicad_checks::refill::refill_closed_copy(&source.board_path, &policy)
        .map_err(|e| unknown(format!("independent copper verification failed: {e:?}")))?;
    let candidate = match &additive_definitions {
        Some(definitions) => GeometryCandidateEvidence::capture_generated_additions(
            &prepared,
            &receipts,
            definitions,
            &filled,
            &reloaded,
            &independent,
        ),
        None => GeometryCandidateEvidence::capture_generated(
            &prepared,
            &receipts,
            &before_fill,
            &filled,
            &reloaded,
            &independent,
        ),
    }
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
    let generated_map = approved
        .map(|a| a.candidate.generated_mapping(&candidate))
        .transpose()
        .map_err(unknown)?
        .unwrap_or_default();
    let diagnostic_map = merge_diagnostic_maps(&creation_map, &generated_map).map_err(unknown)?;
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
    let preserved = if let Some(additions) = receipts.additions() {
        let ids = additions
            .graphics
            .iter()
            .map(|r| r.graphic_id.clone())
            .chain(additions.tracks.iter().map(|r| r.source_uuid.clone()))
            .chain(additions.vias.iter().map(|r| r.source_uuid.clone()))
            .collect::<Vec<_>>();
        items.matches_after_additions(&after_items, &ids)
    } else if request.outline.is_some() {
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
    if request.outline.is_some()
        || expected
            .additions
            .as_ref()
            .is_some_and(|a| a.changes_outline())
    {
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
    if let Some(receipts) = receipts.additions() {
        let operations = crate::geometry_additive::operations(source, expected.additive_kinds())
            .map_err(unknown)?;
        crate::copper_route::verify_components(&mut client, source, &operations, receipts)
            .map_err(unknown)?;
    }
    let after_clusters = client
        .connected_pad_clusters(filename, &pads)
        .map_err(|e| unknown(e.to_string()))?;
    let nets = expected.affected_net_ids.iter().cloned().collect();
    if !crate::copper_route::preserves_clusters(&source.snapshot, &clusters, &after_clusters, &nets)
    {
        return Err(unknown(
            "geometry split connectivity or merged pads outside affected named nets".into(),
        ));
    }
    let (after, after_drc_input, after_connectivity) = drc(
        &mut client,
        source,
        &reloaded,
        expected.library_context.as_ref(),
        timeout,
        expected.additions.is_some(),
    )
    .map_err(unknown)?;
    let delta = kicad_checks::compare_drc(baseline.normalized(), after.normalized());
    if !delta.introduced.is_empty()
        || after.normalized().unconnected_items > baseline.normalized().unconnected_items
    {
        return Err(unknown(format!(
            "geometry regressed DRC or connectivity: {delta:?}"
        )));
    }
    if let Some(approved) = approved
        && !crate::geometry_connectivity::equivalent(
            &approved.after,
            &after,
            &diagnostic_map,
            approved.after_connectivity.as_ref(),
            after_connectivity.as_ref(),
        )
        .map_err(unknown)?
    {
        return Err(unknown("source DRC differs from approved candidate".into()));
    }
    let final_geometry =
        observe_generated_context(&mut client, filename, board).map_err(unknown)?;
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
    let ids = &expected.affected_net_ids;
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
    let final_generated = final_geometry
        .generated()
        .ok_or_else(|| unknown("complete final generated observation missing".into()))?;
    let final_candidate = match &additive_definitions {
        Some(definitions) => GeometryCandidateEvidence::capture_generated_additions(
            &prepared,
            &receipts,
            definitions,
            &filled,
            final_generated,
            &independent,
        ),
        None => GeometryCandidateEvidence::capture_generated(
            &prepared,
            &receipts,
            &before_fill,
            &filled,
            final_generated,
            &independent,
        ),
    }
    .map_err(unknown)?;
    if final_candidate != candidate
        || crate::shadow_source_fingerprint(&source.board_path) != saved_files
    {
        return Err(unknown(
            "geometry result changed during postcondition verification".into(),
        ));
    }
    if let Some(expected) = &expected.library_context
        && &crate::geometry_drc::libraries(&mut client, &observation, Some(expected))
            .map_err(unknown)?
            != expected
    {
        return Err(unknown(
            "geometry library sources changed during execution".into(),
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
        baseline_map,
        diagnostic_map,
        baseline_drc_input,
        after_drc_input,
        baseline_connectivity,
        after_connectivity,
    })
}

fn merge_diagnostic_maps(
    explicit: &std::collections::BTreeMap<String, String>,
    generated: &std::collections::BTreeMap<String, String>,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut result = explicit.clone();
    let mut targets = explicit.values().collect::<std::collections::BTreeSet<_>>();
    if targets.len() != explicit.len() {
        return Err("explicit diagnostic mapping is not bijective".into());
    }
    for (from, to) in generated {
        if result.insert(from.clone(), to.clone()).is_some() || !targets.insert(to) {
            return Err("generated and explicit diagnostic identities collide".into());
        }
    }
    Ok(result)
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
    let expected = match prepare_operations(endpoint, source, operations)
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
        if let Some(libraries) = &expected.library_context {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(shadow.root().join("fp-lib-table"))
                .map_err(|e| reject(e.to_string()))?;
            file.write_all(libraries.table().as_bytes())
                .map_err(|e| reject(e.to_string()))?;
        } else {
            crate::footprint_authoring::prepare_shadow_library_table(
                settings,
                &source.project_dir,
                shadow.root(),
                operations,
                Some(&source.snapshot),
            )
            .map_err(reject)?;
        }
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
        let context = observe_generated_context(&mut client, &copied.board_filename, board)
            .map_err(reject)?;
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
        let source_plan = kicad_engine::geometry_plan::geometry_plan(operations)
            .map_err(|e| reject(e.to_string()))?;
        let copied_kinds = if source_plan.additions.is_empty() {
            Vec::new()
        } else {
            crate::bind_shadow_operations_with_source(
                source_plan.additions,
                &copied.snapshot,
                Some(&source.snapshot),
            )
            .map_err(reject)?
            .into_iter()
            .map(|op| op.kind)
            .collect()
        };
        let copied_evidence = prepare_with_library_context(
            &identity.endpoint,
            &copied,
            &rebound,
            expected.library_context.as_ref(),
            &copied_kinds,
            expected.additions.as_ref().map(|a| &a.identities),
        )
        .map_err(reject)?;
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
            baseline: Some(result.baseline.normalized().clone()),
            after: Some(result.after.normalized().clone()),
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
    if let Err(reason) = prepare_operations(endpoint, source, operations).and_then(|e| {
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

impl GeometryPlanningEvidence {
    pub(super) fn additive_kinds(&self) -> &[OperationKind] {
        self.additions
            .as_ref()
            .map(|a| a.kinds.as_slice())
            .unwrap_or(&[])
    }
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
}
