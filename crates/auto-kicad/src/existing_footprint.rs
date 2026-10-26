use super::*;

fn protected_files(board: &std::path::Path) -> Result<Vec<Option<RevisionDigest>>, String> {
    ["kicad_pro", "kicad_dru", "kicad_sch"]
        .into_iter()
        .map(|extension| {
            let path = board.with_extension(extension);
            match std::fs::read(&path) {
                Ok(bytes) => Ok(Some(digest_bytes(&bytes))),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("protected source read failed: {error}")),
            }
        })
        .collect()
}

pub(super) fn execute(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    operation: &Operation,
    endpoint: &str,
) -> BoardGraphicsCommitOutcome {
    let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    let expected = match kicad_engine::expected_footprint_move(&source.snapshot, &operation.kind) {
        Ok(expected) => expected,
        Err(error) => return reject(error.to_string()),
    };
    let OperationKind::MoveExistingFootprint {
        source_uuid,
        position,
        orientation_mdeg,
        native_evidence,
        ..
    } = &operation.kind
    else {
        return reject("expected footprint transform".into());
    };
    let native_before = match client.capture_footprint_instance_templates(&source.board_filename) {
        Ok(templates) => templates,
        Err(e) => return reject(e.to_string()),
    };
    if footprint_evidence(&native_before).as_ref() != Ok(native_evidence) {
        return reject("native footprint content changed since planning".into());
    }
    let protected = match protected_files(&source.board_path) {
        Ok(files) => files,
        Err(reason) => return reject(reason),
    };
    let policy = DrcPolicy {
        allowed_root: source.project_dir.clone(),
        timeout: Duration::from_secs(30),
    };
    let CheckResult::Completed(baseline) = run_pcb_drc(&source.board_path, &policy) else {
        return reject("source baseline DRC is unavailable".into());
    };
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
        || protected_files(&source.board_path).as_ref() != Ok(&protected)
    {
        return reject("source changed before footprint update".into());
    }
    let current_native = match client.capture_footprint_instance_templates(&source.board_filename) {
        Ok(templates) => templates,
        Err(e) => return reject(e.to_string()),
    };
    if footprint_evidence(&current_native).as_ref() != Ok(native_evidence) {
        return reject("native footprint content changed during baseline validation".into());
    }
    let revision = match source.snapshot.revision() {
        Ok(revision) => revision,
        Err(e) => return reject(e.to_string()),
    };
    let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
        client,
        std::slice::from_ref(operation),
        &source.snapshot,
        &revision,
        &source.board_filename,
        "auto-kicad approved footprint transform",
    );
    if outcome != BoardGraphicsCommitOutcome::CommittedAndSaved {
        return outcome;
    }
    let result = (|| -> Result<(), String> {
        for reload in [false, true] {
            if reload {
                client
                    .revert_board(&source.board_filename)
                    .map_err(|e| e.to_string())?;
            }
            let native_after = client
                .capture_footprint_instance_templates(&source.board_filename)
                .map_err(|e| e.to_string())?;
            if native_before.len() != native_after.len() {
                return Err("native footprint identity set changed".into());
            }
            for before in &native_before {
                let after = native_after
                    .iter()
                    .find(|item| item.metadata().source_uuid == before.metadata().source_uuid)
                    .ok_or("native footprint identity disappeared")?;
                let matches = if before.metadata().source_uuid == source_uuid.to_string() {
                    before
                        .matches_transform(after, position.x, position.y, *orientation_mdeg)
                        .map_err(|e| e.to_string())?
                } else {
                    before
                        .matches_saved_instance(after)
                        .map_err(|e| e.to_string())?
                };
                if !matches {
                    return Err(format!(
                        "native footprint fields or geometry changed unexpectedly: {}",
                        before.metadata().source_uuid
                    ));
                }
            }
            let documents = client
                .get_open_documents(DocumentQuery::Pcb)
                .map_err(|e| e.to_string())?;
            if documents.len() != 1 {
                return Err("PCB document changed after footprint update".into());
            }
            let version = client.get_version().map_err(|e| e.to_string())?;
            let observed = observe_live_board(
                client,
                version,
                documents[0].clone(),
                &source.board_filename,
            )
            .map_err(|e| e.to_string())?;
            if observed.snapshot.canonical() != expected
                || observed.instance_token != source.instance_token
                || observed.board_path != source.board_path
                || observed.project_dir != source.project_dir
            {
                return Err(
                    "saved footprint transform differs from the approved postcondition".into(),
                );
            }
        }
        if protected_files(&source.board_path)? != protected {
            return Err("footprint update changed project rules or schematic source".into());
        }
        let CheckResult::Completed(after) = run_pcb_drc(&source.board_path, &policy) else {
            return Err("saved footprint DRC is unavailable".into());
        };
        let delta = compare_drc(&baseline, &after);
        if !delta.introduced.is_empty() || after.unconnected_items > baseline.unconnected_items {
            return Err("saved footprint update regressed DRC or connectivity".into());
        }
        Ok(())
    })();
    result
        .map(|()| BoardGraphicsCommitOutcome::CommittedAndSaved)
        .unwrap_or_else(unknown)
}

pub(super) fn footprint_evidence(
    templates: &[kicad_ipc::FootprintInstanceTemplate],
) -> Result<RevisionDigest, String> {
    let mut records = templates
        .iter()
        .map(|template| {
            let bytes = template.persistent_evidence().map_err(|e| e.to_string())?;
            Ok((
                template.metadata().source_uuid.clone(),
                digest_bytes(&bytes),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    records.sort_by(|a, b| a.0.cmp(&b.0));
    if records.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("duplicate native footprint identity".into());
    }
    Ok(digest_bytes(
        &serde_json::to_vec(&records).map_err(|e| e.to_string())?,
    ))
}
