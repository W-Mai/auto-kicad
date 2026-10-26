use super::*;
use kicad_engine::{ExistingItemChange, expected_existing_item_edit, preserves_pad_connectivity};
use kicad_ipc::{ExistingBoardItemSet, ExistingItemUpdate, FootprintInstanceTemplate};

pub(super) struct Evidence {
    pub(super) items: ExistingBoardItemSet,
    pub(super) footprints: Vec<FootprintInstanceTemplate>,
    pub(super) clusters: Vec<Vec<String>>,
    pub(super) digest: RevisionDigest,
}

pub(super) fn capture(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
) -> Result<Evidence, String> {
    let unsupported = client
        .get_unsupported_move_copper(&source.board_filename)
        .map_err(|e| e.to_string())?;
    if !unsupported.is_empty() {
        return Err("zones or unsupported copper prevent existing item editing".into());
    }
    let footprints = client
        .capture_footprint_instance_templates(&source.board_filename)
        .map_err(|e| e.to_string())?;
    let items = client
        .capture_existing_board_items(&source.board_filename)
        .map_err(|e| e.to_string())?;
    let pads = source
        .snapshot
        .native_footprints
        .as_ref()
        .ok_or("missing complete footprint observation")?
        .placements
        .iter()
        .flat_map(|f| f.pads.iter().map(|p| p.source_uuid.to_string()))
        .collect::<Vec<_>>();
    let clusters = client
        .connected_pad_clusters(&source.board_filename, &pads)
        .map_err(|e| e.to_string())?;
    if client.instance_token() != Some(&source.instance_token) {
        return Err("editor changed during existing item observation".into());
    }
    let digest = digest_bytes(
        &serde_json::to_vec(&json!({
            "items":digest_bytes(&items.persistent_evidence().map_err(|e| e.to_string())?),
            "footprints":existing_footprint::footprint_evidence(&footprints)?,
            "clusters":clusters,
        }))
        .map_err(|e| e.to_string())?,
    );
    Ok(Evidence {
        items,
        footprints,
        clusters,
        digest,
    })
}

fn updates(changes: &[ExistingItemChange]) -> Vec<ExistingItemUpdate> {
    let point = |p: kicad_engine::ItemPointNm| (p.x, p.y);
    changes
        .iter()
        .map(|c| match c {
            ExistingItemChange::Track {
                source_uuid,
                start_nm,
                end_nm,
                width_nm,
                ..
            } => ExistingItemUpdate::Track {
                source_uuid: source_uuid.to_string(),
                start_nm: start_nm.map(point),
                end_nm: end_nm.map(point),
                width_nm: *width_nm,
            },
            ExistingItemChange::Via {
                source_uuid,
                position_nm,
                diameter_nm,
                drill_nm,
                ..
            } => ExistingItemUpdate::Via {
                source_uuid: source_uuid.to_string(),
                position_nm: position_nm.map(point),
                diameter_nm: *diameter_nm,
                drill_nm: *drill_nm,
            },
            ExistingItemChange::GraphicSegment {
                source_uuid,
                start_nm,
                end_nm,
                width_nm,
                ..
            } => ExistingItemUpdate::GraphicSegment {
                source_uuid: source_uuid.to_string(),
                start_nm: start_nm.map(point),
                end_nm: end_nm.map(point),
                width_nm: *width_nm,
            },
        })
        .collect()
}

pub(super) fn prepare_evidence(
    endpoint: &str,
    source: &LiveBoardObservation,
    changes: &[ExistingItemChange],
) -> Result<RevisionDigest, String> {
    let config =
        ClientConfig::new(endpoint, "auto-kicad-existing-edit-check").map_err(|e| e.to_string())?;
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let evidence = capture(&mut client, source)?;
    evidence
        .items
        .updated(&updates(changes))
        .map_err(|e| e.to_string())?;
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
    {
        return Err("source changed while preparing existing item evidence".into());
    }
    Ok(evidence.digest)
}

pub(super) fn expected_snapshot(
    source: &ProjectSnapshot,
    kind: &OperationKind,
) -> Result<ProjectSnapshot, String> {
    match kind {
        OperationKind::DeleteExistingItems { .. } => {
            kicad_engine::expected_item_deletion(source, kind)
        }
        OperationKind::FlipExistingFootprint { .. } => {
            kicad_engine::expected_footprint_flip(source, kind)
        }
        _ => expected_existing_item_edit(source, kind),
    }
    .map_err(|e| e.to_string())
}

fn native_axis(axis: kicad_engine::FootprintFlipAxis) -> kicad_ipc::FootprintFlipAxis {
    match axis {
        kicad_engine::FootprintFlipAxis::TopBottom => kicad_ipc::FootprintFlipAxis::TopBottom,
        kicad_engine::FootprintFlipAxis::LeftRight => kicad_ipc::FootprintFlipAxis::LeftRight,
    }
}

fn flipped_template(
    before: &Evidence,
    kind: &OperationKind,
) -> Result<Option<FootprintInstanceTemplate>, String> {
    if let OperationKind::FlipExistingFootprint {
        source_uuid, axis, ..
    } = kind
    {
        let target = before
            .footprints
            .iter()
            .find(|f| f.metadata().source_uuid == source_uuid.as_str())
            .ok_or("missing native flip target")?;
        Ok(Some(
            target
                .expected_flipped(native_axis(*axis))
                .map_err(|e| e.to_string())?,
        ))
    } else {
        Ok(None)
    }
}

fn deletion_ids(kind: &OperationKind, footprints: bool) -> Vec<String> {
    if let OperationKind::DeleteExistingItems { targets, .. } = kind {
        targets
            .iter()
            .filter(|t| (t.kind == kicad_engine::ExistingItemKind::Footprint) == footprints)
            .map(|t| t.source_uuid.to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn expected_native_items(
    before: &Evidence,
    kind: &OperationKind,
) -> Result<ExistingBoardItemSet, String> {
    match kind {
        OperationKind::DeleteExistingItems { .. } => {
            let ids = deletion_ids(kind, false);
            if ids.is_empty() {
                Ok(before.items.clone())
            } else {
                before.items.without(&ids).map_err(|e| e.to_string())
            }
        }
        OperationKind::EditExistingItems { changes, .. } => before
            .items
            .updated(&updates(changes))
            .map_err(|e| e.to_string()),
        OperationKind::FlipExistingFootprint { .. } => Ok(before.items.clone()),
        _ => Err("unsupported existing item operation".into()),
    }
}

pub(super) fn prepare_deletion_evidence(
    endpoint: &str,
    source: &LiveBoardObservation,
    kind: &OperationKind,
) -> Result<RevisionDigest, String> {
    expected_snapshot(&source.snapshot, kind)?;
    let config =
        ClientConfig::new(endpoint, "auto-kicad-delete-check").map_err(|e| e.to_string())?;
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let evidence = capture(&mut client, source)?;
    expected_native_items(&evidence, kind)?;
    for id in deletion_ids(kind, true) {
        if !evidence
            .footprints
            .iter()
            .any(|f| f.metadata().source_uuid == id)
        {
            return Err("native footprint deletion target is missing".into());
        }
    }
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
    {
        return Err("source changed while preparing deletion evidence".into());
    }
    Ok(evidence.digest)
}

pub(super) fn prepare_flip_evidence(
    endpoint: &str,
    source: &LiveBoardObservation,
    kind: &OperationKind,
) -> Result<RevisionDigest, String> {
    expected_snapshot(&source.snapshot, kind)?;
    let config = ClientConfig::new(endpoint, "auto-kicad-flip-check").map_err(|e| e.to_string())?;
    let mut client = KiCadClient::new(
        NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
        config,
    );
    let evidence = capture(&mut client, source)?;
    flipped_template(&evidence, kind)?.ok_or("expected flip operation")?;
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
    {
        return Err("source changed while preparing flip evidence".into());
    }
    Ok(evidence.digest)
}

pub(super) fn execute(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    operation: &Operation,
    endpoint: &str,
) -> BoardGraphicsCommitOutcome {
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let (changes, native_evidence) = match &operation.kind {
        OperationKind::EditExistingItems {
            changes,
            native_evidence,
            ..
        } => (updates(changes), native_evidence),
        OperationKind::FlipExistingFootprint {
            native_evidence, ..
        }
        | OperationKind::DeleteExistingItems {
            native_evidence, ..
        } => (Vec::new(), native_evidence),
        _ => return reject("expected existing item operation".into()),
    };
    let preflight = (|| -> Result<_, String> {
        let expected = expected_snapshot(&source.snapshot, &operation.kind)?;
        let before = capture(client, source)?;
        if &before.digest != native_evidence {
            return Err("native items or connectivity changed since planning".into());
        }
        let expected_flip = flipped_template(&before, &operation.kind)?;
        let expected_native = expected_native_items(&before, &operation.kind)?;
        let protected = existing_footprint::protected_files(&source.board_path)?;
        let policy = DrcPolicy {
            allowed_root: source.project_dir.clone(),
            timeout: Duration::from_secs(30),
        };
        let CheckResult::Completed(baseline) = run_pcb_drc(&source.board_path, &policy) else {
            return Err("baseline DRC unavailable".into());
        };
        if IpcDiscoveryService
            .live_board_observation(endpoint, None)
            .as_ref()
            != Ok(source)
            || existing_footprint::protected_files(&source.board_path).as_ref() != Ok(&protected)
            || &capture(client, source)?.digest != native_evidence
        {
            return Err("source changed before existing item update".into());
        }
        Ok((
            expected,
            before,
            changes,
            expected_native,
            expected_flip,
            protected,
            policy,
            baseline,
        ))
    })();
    let (expected, before, changes, expected_native, expected_flip, protected, policy, baseline) =
        match preflight {
            Ok(v) => v,
            Err(e) => return reject(e),
        };
    let outcome = kicad_adapter::execute_native_edit_transaction(
        client,
        &source.board_filename,
        "auto-kicad approved existing item edit",
        |client| {
            if let OperationKind::DeleteExistingItems { targets, .. } = &operation.kind {
                let ids = targets
                    .iter()
                    .map(|t| t.source_uuid.to_string())
                    .collect::<Vec<_>>();
                // Both receipt variants are provisional until the complete saved set is verified below.
                return client
                    .delete_board_items(&source.board_filename, &ids)
                    .map(|_| ())
                    .map_err(|e| e.to_string());
            }
            if let OperationKind::FlipExistingFootprint {
                source_uuid, axis, ..
            } = &operation.kind
            {
                let receipt = client
                    .flip_existing_footprint(
                        &source.board_filename,
                        source_uuid.as_str(),
                        native_axis(*axis),
                    )
                    .map_err(|e| e.to_string())?;
                if !expected_flip
                    .as_ref()
                    .ok_or("missing expected flip")?
                    .matches_flip_result(&receipt)
                    .map_err(|e| e.to_string())?
                {
                    return Err("native flip receipt differs from independent expectation".into());
                }
                Ok(())
            } else {
                client
                    .update_existing_board_items(&source.board_filename, &before.items, &changes)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }
        },
    );
    if outcome != BoardGraphicsCommitOutcome::CommittedAndSaved {
        return outcome;
    }
    let verify = (|| -> Result<(), String> {
        for reload in [false, true] {
            if reload {
                client
                    .revert_board(&source.board_filename)
                    .map_err(|e| e.to_string())?;
            }
            let actual = client
                .capture_existing_board_items(&source.board_filename)
                .map_err(|e| e.to_string())?;
            if !expected_native
                .matches_saved(&actual)
                .map_err(|e| e.to_string())?
            {
                return Err("saved native item content differs from approved fields".into());
            }
            let footprints = client
                .capture_footprint_instance_templates(&source.board_filename)
                .map_err(|e| e.to_string())?;
            let removed = deletion_ids(&operation.kind, true);
            if footprints.len() + removed.len() != before.footprints.len() {
                return Err("footprint set changed".into());
            }
            for original in before
                .footprints
                .iter()
                .filter(|f| !removed.contains(&f.metadata().source_uuid))
            {
                let actual = footprints
                    .iter()
                    .find(|f| f.metadata().source_uuid == original.metadata().source_uuid)
                    .ok_or("footprint missing after edit")?;
                let matches = if let Some(expected) = expected_flip
                    .as_ref()
                    .filter(|f| f.metadata().source_uuid == original.metadata().source_uuid)
                {
                    expected.matches_flip_result(actual)
                } else {
                    original.matches_saved_instance(actual)
                }
                .map_err(|e| e.to_string())?;
                if !matches {
                    return Err("saved footprint content differs from approved intent".into());
                }
            }
            let surviving_pads = expected
                .native_footprints
                .as_ref()
                .ok_or("missing expected footprints")?
                .placements
                .iter()
                .flat_map(|f| f.pads.iter().map(|p| p.source_uuid.to_string()))
                .collect::<Vec<_>>();
            let clusters = client
                .connected_pad_clusters(&source.board_filename, &surviving_pads)
                .map_err(|e| e.to_string())?;
            if !preserves_pad_connectivity(&before.clusters, &clusters) {
                return Err("existing item edit disconnected previously connected pads".into());
            }
            let observed = IpcDiscoveryService.live_board_observation(endpoint, None)?;
            if observed.snapshot.canonical() != expected
                || observed.instance_token != source.instance_token
                || observed.board_path != source.board_path
                || observed.project_dir != source.project_dir
            {
                return Err("saved existing item state differs from approved intent".into());
            }
        }
        if existing_footprint::protected_files(&source.board_path)? != protected {
            return Err("existing item edit changed protected source files".into());
        }
        let CheckResult::Completed(after) = run_pcb_drc(&source.board_path, &policy) else {
            return Err("saved DRC unavailable".into());
        };
        let delta = compare_drc(&baseline, &after);
        if !delta.introduced.is_empty() || after.unconnected_items > baseline.unconnected_items {
            return Err(format!(
                "existing item edit regressed DRC or connectivity: introduced {:?}; unconnected {} -> {}",
                delta
                    .introduced
                    .iter()
                    .map(|v| (&v.rule_type, &v.description))
                    .collect::<Vec<_>>(),
                baseline.unconnected_items,
                after.unconnected_items
            ));
        }
        Ok(())
    })();
    match verify {
        Ok(()) => BoardGraphicsCommitOutcome::CommittedAndSaved,
        Err(e) => unknown(e),
    }
}
