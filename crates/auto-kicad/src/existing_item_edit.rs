use super::*;
use kicad_engine::{ExistingItemChange, expected_existing_item_edit, preserves_pad_connectivity};
use kicad_ipc::{ExistingBoardItemSet, ExistingItemUpdate, FootprintInstanceTemplate};

pub(super) struct Evidence {
    items: ExistingBoardItemSet,
    footprints: Vec<FootprintInstanceTemplate>,
    pads: Vec<String>,
    clusters: Vec<Vec<String>>,
    digest: RevisionDigest,
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
        pads,
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

pub(super) fn execute(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    operation: &Operation,
    endpoint: &str,
) -> BoardGraphicsCommitOutcome {
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let OperationKind::EditExistingItems {
        changes,
        native_evidence,
        ..
    } = &operation.kind
    else {
        return reject("expected existing item edits".into());
    };
    let preflight = (|| -> Result<_, String> {
        let expected = expected_existing_item_edit(&source.snapshot, &operation.kind)
            .map_err(|e| e.to_string())?;
        let before = capture(client, source)?;
        if &before.digest != native_evidence {
            return Err("native items or connectivity changed since planning".into());
        }
        let changes = updates(changes);
        let expected_native = before.items.updated(&changes).map_err(|e| e.to_string())?;
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
            protected,
            policy,
            baseline,
        ))
    })();
    let (expected, before, changes, expected_native, protected, policy, baseline) = match preflight
    {
        Ok(v) => v,
        Err(e) => return reject(e),
    };
    let outcome = kicad_adapter::execute_native_edit_transaction(
        client,
        &source.board_filename,
        "auto-kicad approved existing item edit",
        |client| {
            client
                .update_existing_board_items(&source.board_filename, &before.items, &changes)
                .map(|_| ())
                .map_err(|e| e.to_string())
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
            if existing_footprint::footprint_evidence(&footprints)?
                != existing_footprint::footprint_evidence(&before.footprints)?
            {
                return Err("existing item edit changed footprint content".into());
            }
            let clusters = client
                .connected_pad_clusters(&source.board_filename, &before.pads)
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
        if !compare_drc(&baseline, &after).introduced.is_empty()
            || after.unconnected_items > baseline.unconnected_items
        {
            return Err("existing item edit regressed DRC or connectivity".into());
        }
        Ok(())
    })();
    match verify {
        Ok(()) => BoardGraphicsCommitOutcome::CommittedAndSaved,
        Err(e) => unknown(e),
    }
}
