//! PCB-only synchronization journal. Optional project transitions require separate qualification.
mod regions;
use super::*;
use file_publication::PublicationFile;
use kicad_adapter::schematic_audit::synchronization::{
    MatchedSynchronizationPreview, SynchronizationRecoveryBaseline,
};
pub(super) use regions::{archive_region_preview, region_payload_bytes, validate_region_preview};

pub(super) const DOMAIN: &str = "pcb_sync";
pub(super) const PENDING: &str = ".auto-kicad-sync-pending";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationArtifact {
    pub(super) path: PathBuf,
    pub(super) sha256: kicad_model::RevisionDigest,
    pub(super) bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationPublicationIntent {
    file: PublicationFile,
    candidate: SynchronizationArtifact,
    baseline: SynchronizationArtifact,
    preview: SynchronizationArtifact,
    expectation: SynchronizationArtifact,
    matched_digest: String,
}

pub(super) struct Start<'a> {
    board: &'a Path,
    before: SourceExistence,
    candidate: &'a [u8],
    baseline: Vec<u8>,
    preview: Vec<u8>,
    native_regions: Vec<(&'static str, &'a [u8])>,
    expectation: Vec<u8>,
    matched_digest: &'a str,
    pub(super) checkpoint: &'a mut dyn FnMut(&str) -> io::Result<()>,
}

#[derive(Debug, Serialize)]
pub struct NativeSynchronizationPublication {
    pub attempt_id: String,
    pub matched_preview_digest: String,
    pub native_verification_digest: String,
    pub board: PathBuf,
    pub candidate_sha256: String,
}

#[derive(Debug)]
pub struct SynchronizationPublicationFailure {
    pub unknown: bool,
    pub attempt_id: Option<String>,
    pub reason: String,
}
impl std::fmt::Display for SynchronizationPublicationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.reason.fmt(f)
    }
}
impl std::error::Error for SynchronizationPublicationFailure {}

pub(super) fn artifact(
    directory: &Path,
    name: &str,
    bytes: &[u8],
) -> io::Result<SynchronizationArtifact> {
    let path = directory.join(name);
    write_new(&path, bytes)?;
    Ok(SynchronizationArtifact {
        path,
        sha256: crate::digest_bytes(bytes),
        bytes: bytes.len() as u64,
    })
}

pub(super) fn read_artifact(
    directory: &Path,
    artifact: &SynchronizationArtifact,
    name: &str,
) -> io::Result<Vec<u8>> {
    if artifact.path != directory.join(name) || artifact.bytes > MAX_FILE_BYTES {
        return Err(invalid(
            "invalid synchronization artifact location or bound",
        ));
    }
    let bytes = read_bounded(&artifact.path, MAX_FILE_BYTES)?;
    if bytes.len() as u64 != artifact.bytes || crate::digest_bytes(&bytes) != artifact.sha256 {
        return Err(invalid("synchronization artifact changed"));
    }
    Ok(bytes)
}

pub(super) fn make_intent(
    project: &Path,
    directory: &Path,
    attempt: &str,
    start: &Start<'_>,
) -> io::Result<SynchronizationPublicationIntent> {
    let region_bytes = region_payload_bytes(&start.native_regions)?;
    if start.board.extension().and_then(|s| s.to_str()) != Some("kicad_pcb")
        || start.candidate.is_empty()
        || start.candidate.len() as u64 > MAX_FILE_BYTES
        || start.baseline.len() > 64 * 1024 * 1024
        || start.preview.len() > 64 * 1024 * 1024
        || start.expectation.len() > 64 * 1024 * 1024
        || start.candidate.len()
            + start.baseline.len()
            + start.preview.len()
            + start.expectation.len()
            + region_bytes
            > 256 * 1024 * 1024
        || !matches!(start.before, SourceExistence::Present { .. })
        || confined_existence_with_limit(project, start.board, MAX_FILE_BYTES)? != start.before
    {
        return Err(invalid(
            "invalid or stale synchronization publication start",
        ));
    }
    let file = PublicationFile {
        original: start.board.into(),
        before: start.before.clone(),
        candidate_sha256: crate::digest_bytes(start.candidate),
        staging: start
            .board
            .with_file_name(format!(".{attempt}-0.auto-kicad-sync.tmp")),
    };
    if file.before
        == (SourceExistence::Present {
            sha256: file.candidate_sha256.clone(),
        })
        || confined_existence_with_limit(project, &file.staging, MAX_FILE_BYTES)?
            != SourceExistence::Absent
    {
        return Err(invalid(
            "synchronization publication is unchanged or stage exists",
        ));
    }
    let intent = SynchronizationPublicationIntent {
        file,
        candidate: artifact(directory, "candidate.kicad_pcb", start.candidate)?,
        baseline: artifact(directory, "baseline.json", &start.baseline)?,
        preview: artifact(
            directory,
            "preview.json",
            &archive_region_preview(
                directory,
                &start.preview,
                &start.native_regions,
                &crate::digest_bytes(start.candidate),
            )?,
        )?,
        expectation: artifact(directory, "expectation.json", &start.expectation)?,
        matched_digest: start.matched_digest.into(),
    };
    sync_directory(directory)?;
    Ok(intent)
}

pub(super) fn validate_intent(project: &Path, record: &ExecutionRecord) -> io::Result<()> {
    let Some(intent) = &record.sync_publication else {
        return Ok(());
    };
    let file = &intent.file;
    let SourceExistence::Present { sha256 } = &file.before else {
        return Err(invalid("synchronization requires an existing PCB"));
    };
    if record.schema_version != 4
        || record.domain != DOMAIN
        || record.project_dir != project
        || record.publication.is_some()
        || !record.created_sources.is_empty()
        || record.backups.len() != 1
        || !valid_id(&record.attempt_id)
        || file.original.extension().and_then(|s| s.to_str()) != Some("kicad_pcb")
        || file.staging
            != file
                .original
                .with_file_name(format!(".{}-0.auto-kicad-sync.tmp", record.attempt_id))
        || file.before
            == (SourceExistence::Present {
                sha256: file.candidate_sha256.clone(),
            })
        || record.backups[0].original != file.original
        || record.backups[0].sha256 != sha256.to_string()
        || intent.candidate.sha256 != file.candidate_sha256
    {
        return Err(invalid("invalid synchronization journal intent"));
    }
    confined_existence_with_limit(project, &file.original, MAX_FILE_BYTES)?;
    confined_existence_with_limit(project, &file.staging, MAX_FILE_BYTES)?;
    let directory = record.backups[0]
        .saved
        .parent()
        .ok_or_else(|| invalid("backup has no parent"))?;
    if directory.file_name().and_then(|s| s.to_str()) != Some(&record.attempt_id) {
        return Err(invalid("invalid synchronization execution directory"));
    }
    read_artifact(directory, &intent.candidate, "candidate.kicad_pcb")?;
    read_artifact(directory, &intent.preview, "preview.json")?;
    read_artifact(directory, &intent.expectation, "expectation.json")?;
    let baseline: SynchronizationRecoveryBaseline = serde_json::from_slice(&read_artifact(
        directory,
        &intent.baseline,
        "baseline.json",
    )?)?;
    validate_region_preview(
        directory,
        &read_artifact(directory, &intent.preview, "preview.json")?,
        baseline.has_regions(),
        &intent.candidate.sha256,
    )?;
    if baseline.project_dir() != project
        || baseline.board_path() != file.original
        || baseline.board_sha256() != sha256.as_str()
    {
        return Err(invalid(
            "synchronization native baseline does not bind original PCB",
        ));
    }
    Ok(())
}

pub(super) fn baseline(record: &ExecutionRecord) -> io::Result<SynchronizationRecoveryBaseline> {
    validate_intent(&record.project_dir, record)?;
    let intent = record
        .sync_publication
        .as_ref()
        .ok_or_else(|| invalid("missing synchronization intent"))?;
    let directory = record.backups[0]
        .saved
        .parent()
        .expect("validated backup parent");
    Ok(serde_json::from_slice(&read_artifact(
        directory,
        &intent.baseline,
        "baseline.json",
    )?)?)
}

pub(super) fn intended_state(
    record: &ExecutionRecord,
    path: &Path,
    current: &SourceExistence,
) -> Option<bool> {
    record.sync_publication.as_ref().map(|intent| {
        path == intent.file.original
            && (current == &intent.file.before
                || current
                    == &(SourceExistence::Present {
                        sha256: intent.file.candidate_sha256.clone(),
                    }))
    })
}

pub(super) fn recovery_staging(
    project: &Path,
    record: &ExecutionRecord,
) -> io::Result<Vec<CreatedSchematicRecovery>> {
    validate_intent(project, record)?;
    let Some(intent) = &record.sync_publication else {
        return Ok(Vec::new());
    };
    let current = confined_existence_with_limit(project, &intent.file.staging, MAX_FILE_BYTES)?;
    if matches!(current, SourceExistence::Present { .. }) {
        let bytes = read_bounded(&intent.file.staging, MAX_FILE_BYTES)?;
        let candidate = read_artifact(
            record.backups[0].saved.parent().expect("backup parent"),
            &intent.candidate,
            "candidate.kicad_pcb",
        )?;
        // Exclusive absent-before stages can be interrupted mid-write. Bind the exact current
        // prefix digest to a fresh recovery plan; arbitrary or later-modified bytes reject.
        if !candidate.starts_with(&bytes) {
            return Err(invalid(
                "synchronization stage is not an exact candidate prefix",
            ));
        }
    }
    Ok(vec![CreatedSchematicRecovery {
        original: intent.file.staging.clone(),
        current,
    }])
}

pub(super) fn require_no_interrupted(records: &Path, project: &Path) -> io::Result<()> {
    if read_records(records, project, usize::MAX)?.iter().any(|r| {
        r.domain == DOMAIN
            && matches!(
                r.state.as_str(),
                "in_flight" | "in_flight_or_interrupted" | "unknown"
            )
    }) {
        return Err(invalid(
            "synchronization journal requires guarded recovery before project writes",
        ));
    }
    Ok(())
}

impl ProjectWriter {
    /// Human approval and session lifetime are enforced by the public orchestration layer.
    pub fn publish_matched_synchronization(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: MatchedSynchronizationPreview,
    ) -> Result<NativeSynchronizationPublication, SynchronizationPublicationFailure> {
        self.publish_sync_internal(plan_hash, approval_hash, matched, |_| Ok(()))
    }

    fn publish_sync_internal(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: MatchedSynchronizationPreview,
        mut checkpoint: impl FnMut(&str) -> io::Result<()>,
    ) -> Result<NativeSynchronizationPublication, SynchronizationPublicationFailure> {
        let mut attempt_id = None;
        let result = (|| -> io::Result<_> {
            self.ensure_ready()?;
            require_closed_project(&self.project)?;
            let check = matched.prepare_publication().map_err(io::Error::other)?;
            if check.project_dir() != self.project {
                return Err(invalid("synchronization writer belongs to another project"));
            }
            let baseline_bytes =
                serde_json::to_vec(&check.recovery_baseline().map_err(io::Error::other)?)?;
            check.verify_before_write().map_err(io::Error::other)?;
            let mut start = Start {
                board: check.board_path(),
                before: SourceExistence::Present {
                    sha256: crate::digest_bytes(check.original_bytes()),
                },
                candidate: check.candidate_bytes(),
                baseline: baseline_bytes,
                preview: serde_json::to_vec(check.preview_evidence())?,
                native_regions: check.native_region_artifacts().map_err(io::Error::other)?,
                expectation: serde_json::to_vec(check.expectation())?,
                matched_digest: check.matched_digest(),
                checkpoint: &mut checkpoint,
            };
            let mut attempt = self.begin_internal(
                DOMAIN,
                plan_hash,
                approval_hash,
                serde_json::json!({"matched_preview_digest":check.matched_digest()}),
                &[check.board_path().to_owned()],
                ExecutionStart::Synchronization(&mut start),
            )?;
            attempt_id = Some(attempt.attempt_id().to_owned());
            let intent = attempt
                .record
                .sync_publication
                .as_ref()
                .expect("sync intent")
                .clone();
            let result = (|| -> io::Result<_> {
                check.verify_before_write().map_err(io::Error::other)?;
                file_publication::stage_checkpoints(
                    &self.project,
                    &intent.file,
                    check.candidate_bytes(),
                    MAX_FILE_BYTES,
                    &mut checkpoint,
                )?;
                checkpoint("staged")?;
                check.verify_before_write().map_err(io::Error::other)?;
                require_closed_project(&self.project)?;
                attempt.verify_sync_journal()?;
                file_publication::publish(
                    &self.project,
                    &intent.file,
                    MAX_FILE_BYTES,
                    &mut checkpoint,
                )?;
                let proof = check.verify_published().map_err(io::Error::other)?;
                checkpoint("native_verified")?;
                proof.verify().map_err(io::Error::other)?;
                let evidence = serde_json::to_vec(proof.evidence())?;
                if evidence.len() > 64 * 1024 * 1024 {
                    return Err(invalid("synchronization native proof exceeds bound"));
                }
                let retained = artifact(
                    attempt.path.parent().expect("record parent"),
                    "verification.json",
                    &evidence,
                )?;
                sync_directory(attempt.path.parent().expect("record parent"))?;
                checkpoint("verification_retained")?;
                attempt.record.evidence["verification"] = serde_json::to_value(retained)?;
                attempt.persist()?;
                checkpoint("verification_recorded")?;
                proof.verify().map_err(io::Error::other)?;
                attempt.verify_sync_journal()?;
                Ok(proof)
            })();
            let proof = match result {
                Ok(proof) => proof,
                Err(e) => {
                    let reason = e.to_string();
                    let _ = attempt.finish(
                        "unknown",
                        serde_json::json!({"kind":"unknown","reason":reason}),
                    );
                    return Err(e);
                }
            };
            let receipt = NativeSynchronizationPublication {
                attempt_id: attempt.attempt_id().into(),
                matched_preview_digest: proof.matched_digest().into(),
                native_verification_digest: proof.digest().into(),
                board: proof.board_path().into(),
                candidate_sha256: proof.candidate_sha256().into(),
            };
            if proof.project_dir() != self.project
                || proof.board_path() != intent.file.original
                || proof.matched_digest() != intent.matched_digest
                || proof.candidate_sha256() != intent.file.candidate_sha256.as_str()
            {
                return Err(invalid(
                    "synchronization native proof does not bind publication intent",
                ));
            }
            proof.verify().map_err(io::Error::other)?;
            attempt.finish_checkpoint("committed", serde_json::to_value(&receipt)?, || {
                checkpoint("terminal_record")
            })?;
            checkpoint("terminal")?;
            Ok(receipt)
        })();
        result.map_err(|e| SynchronizationPublicationFailure {
            unknown: attempt_id.is_some() || self.ensure_ready().is_err(),
            attempt_id,
            reason: e.to_string(),
        })
    }
}

impl ExecutionAttempt {
    fn verify_sync_journal(&self) -> io::Result<()> {
        if !matching_pending_marker(
            &self.record.project_dir,
            PENDING,
            &self.path,
            self.attempt_id(),
        )? || read_bounded(&self.path, MAX_RECORD_BYTES)? != serde_json::to_vec(&self.record)?
        {
            return Err(invalid("synchronization pending marker or journal changed"));
        }
        validate_intent(&self.record.project_dir, &self.record)
    }
}

impl ExecutionStore {
    pub(crate) fn synchronization_schematic_for_attempt(
        &self,
        project: &Path,
        attempt_id: &str,
    ) -> io::Result<Option<PathBuf>> {
        let project = fs::canonicalize(project)?;
        let record = self.execution_record(&project, attempt_id)?;
        if record.domain != DOMAIN {
            return Ok(None);
        }
        Ok(Some(if record.sync_project_publication.is_some() {
            synchronization_project::schematic(&record)?
        } else {
            baseline(&record)?.board_path().with_extension("kicad_sch")
        }))
    }
    pub(crate) fn synchronization_schematic_for_recovery(
        &self,
        project: &Path,
        recovery_id: &str,
    ) -> io::Result<Option<PathBuf>> {
        let project = fs::canonicalize(project)?;
        let recovery = self.read_recovery(&project, recovery_id)?;
        self.synchronization_schematic_for_attempt(&project, &recovery.record.attempt_id)
    }
}

#[cfg(test)]
mod tests;
