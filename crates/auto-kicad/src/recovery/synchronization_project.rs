//! Versioned project-first synchronization publication and reverse-order recovery evidence.
use super::*;
use file_publication::PublicationFile;
use kicad_adapter::schematic_audit::{
    SchematicAuditPolicy,
    synchronization::{
        MatchedSynchronizationProjectBoardPreview, SynchronizationProjectRecoveryBaseline,
        VerifiedSynchronizationProjectRecovery, VerifiedSynchronizationRestoration,
    },
};
use synchronization::{
    DOMAIN, PENDING, SynchronizationArtifact, archive_region_preview, artifact, read_artifact,
    region_payload_bytes, validate_region_preview,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationProjectPublicationIntent {
    files: Vec<PublicationFile>,
    candidates: Vec<SynchronizationArtifact>,
    original_board: SynchronizationArtifact,
    baseline: SynchronizationArtifact,
    preview: SynchronizationArtifact,
    expectation: SynchronizationArtifact,
    matched_digest: String,
}

pub(super) struct FileStart<'a> {
    path: &'a Path,
    original: &'a [u8],
    candidate: &'a [u8],
}
pub(super) struct Start<'a> {
    files: Vec<FileStart<'a>>,
    original_board: &'a [u8],
    baseline: Vec<u8>,
    preview: Vec<u8>,
    native_regions: Vec<(&'static str, &'a [u8])>,
    expectation: Vec<u8>,
    matched_digest: &'a str,
    pub(super) checkpoint: &'a mut dyn FnMut(&str) -> io::Result<()>,
}

#[derive(Debug, Serialize)]
pub struct NativeSynchronizationProjectPublication {
    pub attempt_id: String,
    pub matched_preview_digest: String,
    pub native_verification_digest: String,
    pub board: PathBuf,
    pub writes: Vec<Value>,
}

fn candidate_name(index: usize) -> String {
    format!(
        "candidate-{index}.{}",
        if index == 0 { "kicad_pro" } else { "kicad_pcb" }
    )
}
fn backup_digest(backup: &Backup) -> io::Result<kicad_model::RevisionDigest> {
    let raw = backup
        .sha256
        .strip_prefix("sha256:")
        .ok_or_else(|| invalid("invalid legacy backup digest encoding"))?;
    kicad_model::RevisionDigest::new(raw).map_err(io::Error::other)
}

fn execution_directory(record: &ExecutionRecord) -> io::Result<&Path> {
    record
        .backups
        .first()
        .and_then(|backup| backup.saved.parent())
        .ok_or_else(|| invalid("missing project synchronization backup directory"))
}

pub(super) fn make_intent(
    project: &Path,
    directory: &Path,
    attempt: &str,
    start: &Start<'_>,
) -> io::Result<SynchronizationProjectPublicationIntent> {
    if !(1..=2).contains(&start.files.len())
        || start.original_board.len() as u64 > MAX_FILE_BYTES
        || start.baseline.len() > 64 * 1024 * 1024
        || start.preview.len() > 64 * 1024 * 1024
        || start.expectation.len() > 64 * 1024 * 1024
    {
        return Err(invalid(
            "project synchronization publication exceeds bounds",
        ));
    }
    let mut total = start.original_board.len()
        + start.baseline.len()
        + start.preview.len()
        + start.expectation.len()
        + region_payload_bytes(&start.native_regions)?;
    let mut files = Vec::new();
    let mut candidates = Vec::new();
    for (index, source) in start.files.iter().enumerate() {
        total = total
            .checked_add(source.candidate.len())
            .ok_or_else(|| invalid("publication size overflow"))?;
        let before = SourceExistence::Present {
            sha256: crate::digest_bytes(source.original),
        };
        let staging = source
            .path
            .with_file_name(format!(".{attempt}-{index}.auto-kicad-sync-project.tmp"));
        let expected = if index == 0 {
            start.files[0].path.to_owned()
        } else {
            start.files[0].path.with_extension("kicad_pcb")
        };
        if source.path != expected
            || start.files[0].path.extension().and_then(|s| s.to_str()) != Some("kicad_pro")
            || source.candidate.is_empty()
            || source.candidate.len() as u64 > MAX_FILE_BYTES
            || source.original == source.candidate
            || total > 256 * 1024 * 1024
            || confined_existence_with_limit(project, source.path, MAX_FILE_BYTES)? != before
            || confined_existence_with_limit(project, &staging, MAX_FILE_BYTES)?
                != SourceExistence::Absent
        {
            return Err(invalid(
                "invalid or stale ordered project synchronization file",
            ));
        }
        files.push(PublicationFile {
            original: source.path.into(),
            before,
            candidate_sha256: crate::digest_bytes(source.candidate),
            staging,
        });
        candidates.push(artifact(
            directory,
            &candidate_name(index),
            source.candidate,
        )?);
    }
    let intent = SynchronizationProjectPublicationIntent {
        files,
        candidates,
        original_board: artifact(directory, "original.kicad_pcb", start.original_board)?,
        baseline: artifact(directory, "project-baseline.json", &start.baseline)?,
        preview: artifact(
            directory,
            "project-preview.json",
            &archive_region_preview(
                directory,
                &start.preview,
                &start.native_regions,
                &crate::digest_bytes(
                    start
                        .files
                        .last()
                        .filter(|_| start.files.len() == 2)
                        .map(|file| file.candidate)
                        .unwrap_or(start.original_board),
                ),
            )?,
        )?,
        expectation: artifact(directory, "project-expectation.json", &start.expectation)?,
        matched_digest: start.matched_digest.into(),
    };
    sync_directory(directory)?;
    Ok(intent)
}

pub(super) fn validate_intent(project: &Path, record: &ExecutionRecord) -> io::Result<()> {
    let Some(intent) = &record.sync_project_publication else {
        return Ok(());
    };
    if record.schema_version != 5
        || record.domain != DOMAIN
        || record.project_dir != project
        || record.publication.is_some()
        || record.sync_publication.is_some()
        || !record.created_sources.is_empty()
        || !valid_id(&record.attempt_id)
        || !(1..=2).contains(&intent.files.len())
        || intent.files.len() != intent.candidates.len()
        || intent.files.len() != record.backups.len()
    {
        return Err(invalid("invalid project synchronization journal schema"));
    }
    let directory = execution_directory(record)?;
    let mut artifacts = intent.candidates.iter().chain([
        &intent.original_board,
        &intent.baseline,
        &intent.preview,
        &intent.expectation,
    ]);
    let total = artifacts.try_fold(0_u64, |size, artifact| size.checked_add(artifact.bytes));
    if total.is_none_or(|size| size > 256 * 1024 * 1024)
        || [&intent.baseline, &intent.preview, &intent.expectation]
            .iter()
            .any(|artifact| artifact.bytes > 64 * 1024 * 1024)
        || intent.matched_digest.len() != 64
        || !intent.matched_digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid(
            "project synchronization artifact bounds or matched digest are invalid",
        ));
    }
    if directory.file_name().and_then(|s| s.to_str()) != Some(&record.attempt_id) {
        return Err(invalid(
            "invalid project synchronization execution directory",
        ));
    }
    for (index, file) in intent.files.iter().enumerate() {
        let SourceExistence::Present { sha256 } = &file.before else {
            return Err(invalid("project synchronization requires existing files"));
        };
        let backup_sha256 = backup_digest(&record.backups[index])?;
        if file.original
            != intent.files[0].original.with_extension(if index == 0 {
                "kicad_pro"
            } else {
                "kicad_pcb"
            })
            || file.staging
                != file.original.with_file_name(format!(
                    ".{}-{index}.auto-kicad-sync-project.tmp",
                    record.attempt_id
                ))
            || *sha256 == file.candidate_sha256
            || intent.candidates[index].sha256 != file.candidate_sha256
            || record.backups[index].original != file.original
            || backup_sha256 != *sha256
            || record.backups[index].saved != directory.join(format!("source-{index}.backup"))
        {
            return Err(invalid(
                "invalid ordered project synchronization file/backup",
            ));
        }
        confined_existence_with_limit(project, &file.original, MAX_FILE_BYTES)?;
        confined_existence_with_limit(project, &file.staging, MAX_FILE_BYTES)?;
        let saved = read_bounded(&record.backups[index].saved, MAX_FILE_BYTES)?;
        if crate::digest_bytes(&saved) != backup_sha256 {
            return Err(invalid("project synchronization original backup changed"));
        }
        read_artifact(directory, &intent.candidates[index], &candidate_name(index))?;
    }
    read_artifact(directory, &intent.preview, "project-preview.json")?;
    read_artifact(directory, &intent.expectation, "project-expectation.json")?;
    let original_board = read_artifact(directory, &intent.original_board, "original.kicad_pcb")?;
    let baseline: SynchronizationProjectRecoveryBaseline = serde_json::from_slice(&read_artifact(
        directory,
        &intent.baseline,
        "project-baseline.json",
    )?)?;
    validate_region_preview(
        directory,
        &read_artifact(directory, &intent.preview, "project-preview.json")?,
        baseline.has_regions(),
        if intent.files.len() == 2 {
            &intent.files[1].candidate_sha256
        } else {
            &intent.original_board.sha256
        },
    )?;
    let evidence = serde_json::to_value(&baseline)?;
    if baseline.project_dir() != project
        || baseline.project_path() != intent.files[0].original
        || baseline.board_path() != intent.files[0].original.with_extension("kicad_pcb")
        || baseline.write_count() != intent.files.len()
        || evidence["before_project_sha256"] != backup_digest(&record.backups[0])?.as_str()
        || evidence["candidate_project_sha256"] != intent.files[0].candidate_sha256.as_str()
        || evidence["original"]["board_sha256"] != crate::digest_bytes(&original_board).as_str()
        || evidence["candidate_board_sha256"]
            != if intent.files.len() == 2 {
                intent.files[1].candidate_sha256.as_str()
            } else {
                intent.original_board.sha256.as_str()
            }
        || intent.files.len() == 2
            && backup_digest(&record.backups[1])? != intent.original_board.sha256
    {
        return Err(invalid(
            "project synchronization baseline does not bind exact files",
        ));
    }
    if let Some(value) = record.evidence.get("verification") {
        let verification: SynchronizationArtifact = serde_json::from_value(value.clone())?;
        read_artifact(directory, &verification, "project-verification.json")?;
    }
    Ok(())
}

fn baseline(record: &ExecutionRecord) -> io::Result<SynchronizationProjectRecoveryBaseline> {
    validate_intent(&record.project_dir, record)?;
    let intent = record
        .sync_project_publication
        .as_ref()
        .ok_or_else(|| invalid("missing project synchronization intent"))?;
    Ok(serde_json::from_slice(&read_artifact(
        execution_directory(record)?,
        &intent.baseline,
        "project-baseline.json",
    )?)?)
}
fn original_inputs(record: &ExecutionRecord) -> io::Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let intent = record
        .sync_project_publication
        .as_ref()
        .ok_or_else(|| invalid("missing project synchronization intent"))?;
    let directory = execution_directory(record)?;
    Ok((
        read_bounded(&record.backups[0].saved, MAX_FILE_BYTES)?,
        read_artifact(directory, &intent.original_board, "original.kicad_pcb")?,
        read_artifact(directory, &intent.candidates[0], &candidate_name(0))?,
    ))
}
fn current_prefix(record: &ExecutionRecord) -> io::Result<usize> {
    let intent = record
        .sync_project_publication
        .as_ref()
        .ok_or_else(|| invalid("missing project synchronization intent"))?;
    let current = intent
        .files
        .iter()
        .map(|file| {
            confined_existence_with_limit(&record.project_dir, &file.original, MAX_FILE_BYTES)
        })
        .collect::<io::Result<Vec<_>>>()?;
    (0..=intent.files.len())
        .find(|prefix| {
            intent.files.iter().enumerate().all(|(index, file)| {
                current[index]
                    == if index < *prefix {
                        SourceExistence::Present {
                            sha256: file.candidate_sha256.clone(),
                        }
                    } else {
                        file.before.clone()
                    }
            })
        })
        .ok_or_else(|| invalid("project synchronization files are not one exact ordered prefix"))
}
pub(super) fn check_dependencies(
    record: &ExecutionRecord,
    policy: &SchematicAuditPolicy,
) -> io::Result<()> {
    let baseline = baseline(record)?;
    let (project, board, candidate) = original_inputs(record)?;
    baseline
        .verify_recovery_prefix(
            policy,
            &project,
            &board,
            &candidate,
            current_prefix(record)?,
        )
        .map_err(io::Error::other)?
        .verify()
        .map_err(io::Error::other)
}
pub(super) fn intended_state(
    record: &ExecutionRecord,
    path: &Path,
    current: &SourceExistence,
) -> Option<bool> {
    record.sync_project_publication.as_ref().map(|intent| {
        intent.files.iter().any(|file| {
            path == file.original
                && (current == &file.before
                    || current
                        == &SourceExistence::Present {
                            sha256: file.candidate_sha256.clone(),
                        })
        })
    })
}
pub(super) fn recovery_staging(
    project: &Path,
    record: &ExecutionRecord,
) -> io::Result<Vec<CreatedSchematicRecovery>> {
    validate_intent(project, record)?;
    let Some(intent) = &record.sync_project_publication else {
        return Ok(Vec::new());
    };
    intent
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let current = confined_existence_with_limit(project, &file.staging, MAX_FILE_BYTES)?;
            if matches!(current, SourceExistence::Present { .. }) {
                let bytes = read_bounded(&file.staging, MAX_FILE_BYTES)?;
                let candidate = read_artifact(
                    execution_directory(record)?,
                    &intent.candidates[index],
                    &candidate_name(index),
                )?;
                if !candidate.starts_with(&bytes) {
                    return Err(invalid(
                        "project synchronization stage is not an exact candidate prefix",
                    ));
                }
            }
            Ok(CreatedSchematicRecovery {
                original: file.staging.clone(),
                current,
            })
        })
        .collect()
}

pub(super) enum NativeRestoration {
    Board(VerifiedSynchronizationRestoration),
    Project(VerifiedSynchronizationProjectRecovery),
}
impl NativeRestoration {
    pub(super) fn verify(&self) -> Result<(), String> {
        match self {
            Self::Board(p) => p.verify(),
            Self::Project(p) => p.verify(),
        }
    }
    pub(super) fn digest(&self) -> &str {
        match self {
            Self::Board(p) => p.digest(),
            Self::Project(p) => p.digest(),
        }
    }
    pub(super) fn evidence(&self) -> &Value {
        match self {
            Self::Board(p) => p.evidence(),
            Self::Project(p) => p.evidence(),
        }
    }
}
pub(super) fn verify_restored(
    record: &ExecutionRecord,
    policy: &SchematicAuditPolicy,
) -> io::Result<NativeRestoration> {
    if record.sync_project_publication.is_none() {
        return synchronization::baseline(record)?
            .verify_restored(policy)
            .map(NativeRestoration::Board)
            .map_err(io::Error::other);
    }
    let baseline = baseline(record)?;
    let (project, board, candidate) = original_inputs(record)?;
    baseline
        .verify_restored(policy, &project, &board, &candidate)
        .map(NativeRestoration::Project)
        .map_err(io::Error::other)
}
pub(super) fn schematic(record: &ExecutionRecord) -> io::Result<PathBuf> {
    Ok(baseline(record)?.board_path().with_extension("kicad_sch"))
}

impl ProjectWriter {
    /// The public orchestration layer enforces human approval and session lifetime.
    pub fn publish_matched_project_synchronization(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: MatchedSynchronizationProjectBoardPreview,
    ) -> Result<NativeSynchronizationProjectPublication, SynchronizationPublicationFailure> {
        self.publish_sync_project_internal(plan_hash, approval_hash, matched, |_| Ok(()))
    }

    fn publish_sync_project_internal(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: MatchedSynchronizationProjectBoardPreview,
        mut checkpoint: impl FnMut(&str) -> io::Result<()>,
    ) -> Result<NativeSynchronizationProjectPublication, SynchronizationPublicationFailure> {
        let mut attempt_id = None;
        let result = (|| -> io::Result<_> {
            self.ensure_ready()?;
            require_closed_project(&self.project)?;
            let check = matched.prepare_publication().map_err(io::Error::other)?;
            if check.project_dir() != self.project {
                return Err(invalid(
                    "project synchronization writer belongs to another project",
                ));
            }
            let baseline =
                serde_json::to_vec(&check.recovery_baseline().map_err(io::Error::other)?)?;
            let original_board = read_bounded(check.board_path(), MAX_FILE_BYTES)?;
            check.verify_before_write().map_err(io::Error::other)?;
            let writes = check.writes();
            let candidates = writes
                .iter()
                .map(|write| write.candidate.to_vec())
                .collect::<Vec<_>>();
            let mut start = Start {
                files: writes
                    .iter()
                    .map(|write| FileStart {
                        path: &write.path,
                        original: write.original,
                        candidate: write.candidate,
                    })
                    .collect(),
                original_board: &original_board,
                baseline,
                preview: serde_json::to_vec(check.preview_evidence())?,
                native_regions: check.native_region_artifacts().map_err(io::Error::other)?,
                expectation: serde_json::to_vec(check.expectation())?,
                matched_digest: check.matched_digest(),
                checkpoint: &mut checkpoint,
            };
            let sources = writes
                .iter()
                .map(|write| write.path.clone())
                .collect::<Vec<_>>();
            let mut attempt = self.begin_internal(
                DOMAIN,
                plan_hash,
                approval_hash,
                serde_json::json!({"matched_preview_digest":check.matched_digest()}),
                &sources,
                ExecutionStart::SynchronizationProject(&mut start),
            )?;
            attempt_id = Some(attempt.attempt_id().to_owned());
            drop(start);
            drop(writes);
            let intent = attempt
                .record
                .sync_project_publication
                .as_ref()
                .expect("project sync intent")
                .clone();
            let result = (|| -> io::Result<_> {
                check.verify_before_write().map_err(io::Error::other)?;
                for (index, file) in intent.files.iter().enumerate() {
                    check
                        .verify_publication_prefix(index)
                        .map_err(io::Error::other)?;
                    file_publication::stage_checkpoints(
                        &self.project,
                        file,
                        &candidates[index],
                        MAX_FILE_BYTES,
                        |phase| checkpoint(&format!("file_{index}_{phase}")),
                    )?;
                    checkpoint(&format!("file_{index}_staged"))?;
                    check
                        .verify_publication_prefix(index)
                        .map_err(io::Error::other)?;
                    require_closed_project(&self.project)?;
                    attempt.verify_sync_project_journal()?;
                    file_publication::publish(&self.project, file, MAX_FILE_BYTES, |phase| {
                        checkpoint(&format!("file_{index}_{phase}"))
                    })?;
                    check
                        .verify_publication_prefix(index + 1)
                        .map_err(io::Error::other)?;
                }
                let proof = check.verify_published().map_err(io::Error::other)?;
                checkpoint("native_verified")?;
                proof.verify().map_err(io::Error::other)?;
                let evidence = serde_json::to_vec(proof.evidence())?;
                if evidence.len() > 64 * 1024 * 1024 {
                    return Err(invalid(
                        "project synchronization native proof exceeds bound",
                    ));
                }
                let retained = artifact(
                    attempt.path.parent().expect("record parent"),
                    "project-verification.json",
                    &evidence,
                )?;
                sync_directory(attempt.path.parent().expect("record parent"))?;
                checkpoint("verification_retained")?;
                attempt.record.evidence["verification"] = serde_json::to_value(retained)?;
                attempt.persist()?;
                checkpoint("verification_recorded")?;
                proof.verify().map_err(io::Error::other)?;
                attempt.verify_sync_project_journal()?;
                Ok(proof)
            })();
            let proof = match result {
                Ok(proof) => proof,
                Err(error) => {
                    let _ = attempt.finish(
                        "unknown",
                        serde_json::json!({"kind":"unknown","reason":error.to_string()}),
                    );
                    return Err(error);
                }
            };
            if proof.project_dir() != self.project
                || proof.board_path() != intent.files[0].original.with_extension("kicad_pcb")
                || proof.matched_digest() != intent.matched_digest
            {
                return Err(invalid(
                    "project synchronization native proof does not bind publication intent",
                ));
            }
            let receipt = NativeSynchronizationProjectPublication { attempt_id:attempt.attempt_id().into(),
                matched_preview_digest:proof.matched_digest().into(), native_verification_digest:proof.digest().into(),
                board:proof.board_path().into(), writes:intent.files.iter().map(|file|
                    serde_json::json!({"path":file.original,"candidate_sha256":file.candidate_sha256})).collect() };
            proof.verify().map_err(io::Error::other)?;
            attempt.finish_checkpoint("committed", serde_json::to_value(&receipt)?, || {
                checkpoint("terminal_record")
            })?;
            checkpoint("terminal")?;
            Ok(receipt)
        })();
        result.map_err(|error| SynchronizationPublicationFailure {
            unknown: attempt_id.is_some() || self.ensure_ready().is_err(),
            attempt_id,
            reason: error.to_string(),
        })
    }
}

impl ExecutionAttempt {
    fn verify_sync_project_journal(&self) -> io::Result<()> {
        if !matching_pending_marker(
            &self.record.project_dir,
            PENDING,
            &self.path,
            self.attempt_id(),
        )? || read_bounded(&self.path, MAX_RECORD_BYTES)? != serde_json::to_vec(&self.record)?
        {
            return Err(invalid(
                "project synchronization pending marker or journal changed",
            ));
        }
        validate_intent(&self.record.project_dir, &self.record)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_tests;
