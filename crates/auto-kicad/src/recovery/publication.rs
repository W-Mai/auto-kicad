//! Durable file IO only. Native validation and human publication approval are separate layers.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchematicPublicationIntent {
    pub files: Vec<SchematicPublicationFile>,
}

pub use super::file_publication::PublicationFile as SchematicPublicationFile;

/// Captured before-state and exact candidate bytes supplied by a validated orchestration layer.
pub struct SchematicWrite {
    pub original: PathBuf,
    pub before: SourceExistence,
    pub bytes: Vec<u8>,
}

/// Borrows the OS writer lock. No method grants a committed native result.
pub struct SchematicPublication<'writer> {
    writer: &'writer ProjectWriter,
    attempt: ExecutionAttempt,
    next: Option<usize>,
}

fn staging_path(original: &Path, attempt: &str, index: usize) -> PathBuf {
    original.with_file_name(format!(".{attempt}-{index}.auto-kicad-schematic.tmp"))
}

fn present(digest: &kicad_model::RevisionDigest) -> SourceExistence {
    SourceExistence::Present {
        sha256: digest.clone(),
    }
}

impl ProjectWriter {
    pub fn prepare_schematic_publication(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        writes: &[SchematicWrite],
    ) -> io::Result<SchematicPublication<'_>> {
        self.prepare_publication_internal(plan_hash, approval_hash, evidence, writes, |_| Ok(()))
    }

    fn prepare_publication_internal(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        writes: &[SchematicWrite],
        mut checkpoint: impl FnMut(usize) -> io::Result<()>,
    ) -> io::Result<SchematicPublication<'_>> {
        self.ensure_ready()?;
        require_closed_project(&self.project)?;
        if writes.is_empty() || writes.len() > 32 {
            return Err(invalid(
                "schematic publication requires 1..32 changed files",
            ));
        }
        let mut total = 0;
        let mut paths = BTreeSet::new();
        let mut existing = Vec::new();
        let mut created = Vec::new();
        for write in writes {
            total += write.bytes.len();
            if write.bytes.len() > 32 * 1024 * 1024
                || total > 128 * 1024 * 1024
                || !paths.insert(&write.original)
                || schematic_existence(&self.project, &write.original)? != write.before
                || write.before == present(&crate::digest_bytes(&write.bytes))
            {
                return Err(invalid(
                    "schematic write is oversized, repeated, stale or unchanged",
                ));
            }
            match write.before {
                SourceExistence::Absent => created.push(write.original.clone()),
                SourceExistence::Present { .. } => existing.push(write.original.clone()),
            }
        }
        let mut attempt =
            self.begin_schematic(plan_hash, approval_hash, evidence, &existing, &created)?;
        let files = writes
            .iter()
            .enumerate()
            .map(|(index, write)| SchematicPublicationFile {
                original: write.original.clone(),
                before: write.before.clone(),
                candidate_sha256: crate::digest_bytes(&write.bytes),
                staging: staging_path(&write.original, attempt.attempt_id(), index),
            })
            .collect();
        attempt.record.schema_version = 3;
        attempt.record.publication = Some(SchematicPublicationIntent { files });
        validate_intent(&self.project, &attempt.record)?;
        for file in &attempt.record.publication.as_ref().expect("intent").files {
            if confined_existence(&self.project, &file.staging)? != SourceExistence::Absent {
                return Err(invalid("publication staging path already exists"));
            }
        }
        attempt.persist()?;
        for (index, write) in writes.iter().enumerate() {
            require_closed_project(&self.project)?;
            if schematic_existence(&self.project, &write.original)? != write.before {
                return Err(invalid("source changed before candidate staging"));
            }
            let file = &attempt.record.publication.as_ref().expect("intent").files[index];
            file_publication::stage(&self.project, file, &write.bytes, 32 * 1024 * 1024)?;
            checkpoint(index)?;
        }
        let publication = SchematicPublication {
            writer: self,
            attempt,
            next: Some(0),
        };
        publication.check_progress(0)?;
        Ok(publication)
    }
}

impl SchematicPublication<'_> {
    pub fn attempt_id(&self) -> &str {
        self.attempt.attempt_id()
    }

    fn files(&self) -> &[SchematicPublicationFile] {
        &self
            .attempt
            .record
            .publication
            .as_ref()
            .expect("publication intent")
            .files
    }

    fn check_progress(&self, next: usize) -> io::Result<()> {
        require_closed_project(&self.writer.project)?;
        if !matching_pending_marker(
            &self.writer.project,
            SCHEMATIC_PENDING,
            &self.attempt.path,
            self.attempt_id(),
        )? {
            return Err(invalid("publication intent lost its pending marker"));
        }
        if read_bounded(&self.attempt.path, MAX_RECORD_BYTES)?
            != serde_json::to_vec(&self.attempt.record)?
        {
            return Err(invalid("publication journal changed"));
        }
        for (index, file) in self.files().iter().enumerate() {
            let (expected_source, expected_stage) = if index < next {
                (present(&file.candidate_sha256), SourceExistence::Absent)
            } else {
                (file.before.clone(), present(&file.candidate_sha256))
            };
            if schematic_existence(&self.writer.project, &file.original)? != expected_source
                || confined_existence(&self.writer.project, &file.staging)? != expected_stage
            {
                return Err(invalid("publication source or staging bytes changed"));
            }
        }
        Ok(())
    }

    /// Publish one exact file. Any failure consumes continuation and leaves durable uncertainty.
    pub fn publish_next(&mut self) -> io::Result<bool> {
        self.publish_next_internal(|_| Ok(()))
    }

    fn publish_next_internal(
        &mut self,
        mut checkpoint: impl FnMut(&str) -> io::Result<()>,
    ) -> io::Result<bool> {
        let index = self
            .next
            .take()
            .ok_or_else(|| invalid("publication cannot resume after uncertainty"))?;
        self.check_progress(index)?;
        if index == self.files().len() {
            self.next = Some(index);
            return Ok(false);
        }
        let file = &self.files()[index];
        file_publication::publish(
            &self.writer.project,
            file,
            32 * 1024 * 1024,
            &mut checkpoint,
        )?;
        self.check_progress(index + 1)?;
        self.next = Some(index + 1);
        Ok(true)
    }

    pub fn verify_complete(&self) -> io::Result<()> {
        if self.next != Some(self.files().len()) {
            return Err(invalid("publication is incomplete or uncertain"));
        }
        self.check_progress(self.files().len())
    }

    pub fn record_unknown(self, outcome: Value) -> io::Result<()> {
        self.attempt.finish("unknown", outcome)
    }

    pub(super) fn retain_artifact(&mut self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.check_progress(
            self.next
                .ok_or_else(|| invalid("publication is uncertain"))?,
        )?;
        if name.len() > 80
            || Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
            || bytes.len() > 128 * 1024 * 1024
        {
            return Err(invalid("invalid or oversized publication artifact"));
        }
        let previous = self
            .attempt
            .record
            .evidence
            .get("artifacts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let total = previous
            .iter()
            .filter_map(|entry| entry.get("bytes").and_then(Value::as_u64))
            .sum::<u64>();
        if total + bytes.len() as u64 > 256 * 1024 * 1024 {
            return Err(invalid("publication artifact budget exceeded"));
        }
        let path = self
            .attempt
            .path
            .parent()
            .expect("record parent")
            .join(name);
        write_new(&path, bytes)?;
        sync_directory(path.parent().expect("artifact parent"))?;
        let mut artifacts = previous;
        artifacts.push(serde_json::json!({"name":name,"path":path,"bytes":bytes.len(),"sha256":crate::digest_bytes(bytes)}));
        self.attempt.record.evidence["artifacts"] = serde_json::json!(artifacts);
        self.attempt.persist()
    }

    pub(super) fn finish_verified(
        self,
        proof: &kicad_adapter::schematic_audit::VerifiedSchematicPublication,
    ) -> io::Result<()> {
        self.verify_complete()?;
        proof.verify().map_err(io::Error::other)?;
        if proof.project_dir() != self.writer.project
            || self
                .attempt
                .record
                .evidence
                .get("matched_preview_digest")
                .and_then(Value::as_str)
                != Some(proof.matched_digest())
            || self.attempt.record.evidence.get("candidate_artifacts")
                != Some(&serde_json::to_value(proof.revision())?)
        {
            return Err(invalid(
                "native proof does not match the publication intent",
            ));
        }
        for file in self.files() {
            let relative = file
                .original
                .strip_prefix(&self.writer.project)
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| invalid("invalid source path"))?;
            if !proof.revision().files.iter().any(|artifact| {
                artifact.relative_path == relative
                    && artifact.content_digest == file.candidate_sha256
            }) {
                return Err(invalid(
                    "native proof omits a publication file or candidate digest",
                ));
            }
        }
        self.attempt.finish("committed", serde_json::json!({
            "kind":"committed", "native_verification_digest":proof.digest(),
            "matched_preview_digest":proof.matched_digest(), "candidate_artifacts":proof.revision(),
        }))
    }
}

impl ExecutionAttempt {
    pub(super) fn persist(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec(&self.record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("publication intent exceeds size limit"));
        }
        let temporary =
            journal_temporary(&self.path, self.record.domain == synchronization::DOMAIN);
        write_new(&temporary, &bytes)?;
        let result = fs::rename(&temporary, &self.path)
            .and_then(|()| sync_directory(self.path.parent().expect("record parent")));
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

/// Validate the complete original/staging namespace before using journal content as authority.
pub(super) fn validate_intent(project: &Path, record: &ExecutionRecord) -> io::Result<()> {
    let Some(intent) = &record.publication else {
        return Ok(());
    };
    if record.domain != "schematic"
        || record.schema_version != 3
        || intent.files.is_empty()
        || intent.files.len() > 32
        || intent.files.len() != record.backups.len() + record.created_sources.len()
    {
        return Err(invalid("invalid publication intent shape"));
    }
    let mut destinations = BTreeSet::new();
    for (index, file) in intent.files.iter().enumerate() {
        if !destinations.insert(file.original.clone())
            || file.staging != staging_path(&file.original, &record.attempt_id, index)
            || file.before == present(&file.candidate_sha256)
        {
            return Err(invalid("invalid publication path or unchanged candidate"));
        }
        schematic_existence(project, &file.original)?;
        confined_existence(project, &file.staging)?;
        match &file.before {
            SourceExistence::Absent => {
                if record
                    .created_sources
                    .iter()
                    .filter(|source| source.original == file.original)
                    .count()
                    != 1
                {
                    return Err(invalid("publication new-source evidence mismatch"));
                }
            }
            SourceExistence::Present { sha256 } => {
                if record
                    .backups
                    .iter()
                    .filter(|backup| {
                        backup.original == file.original && backup.sha256 == sha256.to_string()
                    })
                    .count()
                    != 1
                {
                    return Err(invalid("publication backup evidence mismatch"));
                }
            }
        }
    }
    if intent
        .files
        .iter()
        .any(|file| destinations.contains(&file.staging))
    {
        return Err(invalid("publication staging overlaps source destinations"));
    }
    Ok(())
}

pub(super) fn intended_state(
    record: &ExecutionRecord,
    path: &Path,
    current: &SourceExistence,
) -> Option<bool> {
    record.publication.as_ref().map(|intent| {
        intent.files.iter().any(|file| {
            file.original == path
                && (current == &file.before || current == &present(&file.candidate_sha256))
        })
    })
}

pub(super) fn recovery_staging(
    project: &Path,
    record: &ExecutionRecord,
) -> io::Result<Vec<CreatedSchematicRecovery>> {
    validate_intent(project, record)?;
    record
        .publication
        .iter()
        .flat_map(|intent| &intent.files)
        .map(|file| {
            let current = confined_existence(project, &file.staging)?;
            if current != SourceExistence::Absent && current != present(&file.candidate_sha256) {
                return Err(invalid(
                    "publication staging changed or is incomplete; inspect before recovery",
                ));
            }
            Ok(CreatedSchematicRecovery {
                original: file.staging.clone(),
                current,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Owned {
        root: PathBuf,
        project: PathBuf,
        store: ExecutionStore,
    }
    impl Owned {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "auto-kicad-publication-contract-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let project = root.join("project");
            fs::create_dir(&project).unwrap();
            let store = ExecutionStore::open(&root.join("state")).unwrap();
            Self {
                root,
                project,
                store,
            }
        }
        fn writes(&self) -> Vec<SchematicWrite> {
            ["a", "b", "new"]
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let original = self.project.join(format!("{name}.kicad_sch"));
                    let before = if index < 2 {
                        let bytes = format!("original-{name}");
                        fs::write(&original, &bytes).unwrap();
                        present(&crate::digest_bytes(bytes.as_bytes()))
                    } else {
                        SourceExistence::Absent
                    };
                    SchematicWrite {
                        original,
                        before,
                        bytes: format!("candidate-{name}").into_bytes(),
                    }
                })
                .collect()
        }
        fn restore(&self, id: &str) -> RecoveryRecord {
            let plan = self.store.plan_recovery(&self.project, id).unwrap();
            let approval = self
                .store
                .preview_recovery(&self.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            self.store
                .commit_recovery(&self.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap()
        }
        fn assert_restored(&self, writes: &[SchematicWrite], stages: &[PathBuf]) {
            for write in writes {
                assert_eq!(
                    schematic_existence(&self.project, &write.original).unwrap(),
                    write.before
                );
            }
            assert!(stages.iter().all(|path| !path.exists()));
            self.store
                .acquire(&self.project)
                .unwrap()
                .ensure_ready()
                .unwrap();
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn recovery_uses_durable_intent_at_every_file_boundary_without_post_evidence() {
        for boundary in 0..=3 {
            let owned = Owned::new();
            let writes = owned.writes();
            let writer = owned.store.acquire(&owned.project).unwrap();
            let mut publication = writer
                .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
                .unwrap();
            let id = publication.attempt_id().to_owned();
            let stages = publication
                .files()
                .iter()
                .map(|file| file.staging.clone())
                .collect::<Vec<_>>();
            for _ in 0..boundary {
                assert!(publication.publish_next().unwrap());
            }
            assert_eq!(publication.verify_complete().is_ok(), boundary == 3);
            if boundary == 3 {
                assert!(!publication.publish_next().unwrap());
            }
            drop(publication);
            drop(writer);
            let record = owned.store.execution_record(&owned.project, &id).unwrap();
            assert_eq!(record.schema_version, 3);
            assert!(record.post_attempt.is_none());
            assert!(record.created_sources[0].post_attempt.is_none());
            assert!(
                owned
                    .store
                    .acquire(&owned.project)
                    .unwrap()
                    .ensure_ready()
                    .is_err()
            );
            let alternate = ExecutionStore::open(&owned.root.join("other-state")).unwrap();
            assert!(
                alternate
                    .acquire(&owned.project)
                    .unwrap()
                    .ensure_ready()
                    .is_err()
            );
            let plan = owned.restore(&id);
            assert_eq!(plan.schema_version, 4);
            assert_eq!(plan.files.len(), 2);
            assert_eq!(plan.created_sources.len(), 1);
            assert_eq!(plan.staging_files.len(), 3);
            owned.assert_restored(&writes, &stages);
        }
    }

    #[test]
    fn ambiguous_link_rename_and_directory_sync_boundaries_consume_continuation() {
        for new in [false, true] {
            for boundary in ["linked", "published", "synchronized"] {
                if !new && boundary == "linked" {
                    continue;
                }
                let owned = Owned::new();
                let mut writes = owned.writes();
                let write = writes.remove(if new { 2 } else { 0 });
                let writer = owned.store.acquire(&owned.project).unwrap();
                let mut publication = writer
                    .prepare_schematic_publication(
                        "plan",
                        "approval",
                        Value::Null,
                        std::slice::from_ref(&write),
                    )
                    .unwrap();
                let id = publication.attempt_id().to_owned();
                let stages = publication
                    .files()
                    .iter()
                    .map(|file| file.staging.clone())
                    .collect::<Vec<_>>();
                assert!(
                    publication
                        .publish_next_internal(|step| if step == boundary {
                            Err(invalid("lost boundary acknowledgement"))
                        } else {
                            Ok(())
                        })
                        .is_err()
                );
                assert!(publication.publish_next().is_err());
                assert!(publication.verify_complete().is_err());
                drop(publication);
                drop(writer);
                owned.restore(&id);
                owned.assert_restored(&[write], &stages);
            }
        }
    }

    #[test]
    fn unrelated_source_and_stage_edits_never_gain_authority_from_observed_post_state() {
        for stage in [false, true] {
            let owned = Owned::new();
            let writes = owned.writes();
            let writer = owned.store.acquire(&owned.project).unwrap();
            let publication = writer
                .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
                .unwrap();
            let id = publication.attempt_id().to_owned();
            let changed = if stage {
                publication.files()[0].staging.clone()
            } else {
                writes[0].original.clone()
            };
            fs::write(&changed, b"later-user-content").unwrap();
            publication.record_unknown(Value::Null).unwrap();
            drop(writer);
            assert!(owned.store.plan_recovery(&owned.project, &id).is_err());
            assert_eq!(fs::read(changed).unwrap(), b"later-user-content");
            assert!(
                owned
                    .store
                    .acquire(&owned.project)
                    .unwrap()
                    .ensure_ready()
                    .is_err()
            );
        }
    }

    #[test]
    fn fresh_recovery_can_restore_a_partially_restored_publication() {
        let owned = Owned::new();
        let writes = owned.writes();
        let writer = owned.store.acquire(&owned.project).unwrap();
        let mut publication = writer
            .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
            .unwrap();
        let id = publication.attempt_id().to_owned();
        let stages = publication
            .files()
            .iter()
            .map(|file| file.staging.clone())
            .collect::<Vec<_>>();
        while publication.publish_next().unwrap() {}
        drop(publication);
        drop(writer);
        let plan = owned.store.plan_recovery(&owned.project, &id).unwrap();
        let approval = owned
            .store
            .preview_recovery(&owned.project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        let error = owned
            .store
            .commit_recovery_internal(
                &owned.project,
                &plan.recovery_id,
                &plan.plan_hash,
                &approval,
                |index| {
                    if index == 0 {
                        Err(invalid("interrupted recovery"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(error.unknown);
        assert!(
            owned
                .store
                .commit_recovery(
                    &owned.project,
                    &plan.recovery_id,
                    &plan.plan_hash,
                    &approval
                )
                .is_err()
        );
        owned.restore(&id);
        owned.assert_restored(&writes, &stages);
    }

    #[test]
    fn staging_failure_keeps_complete_intent_and_absent_future_stages_recoverable() {
        let owned = Owned::new();
        let writes = owned.writes();
        let writer = owned.store.acquire(&owned.project).unwrap();
        assert!(
            writer
                .prepare_publication_internal("plan", "approval", Value::Null, &writes, |index| {
                    if index == 0 {
                        Err(invalid("staging sync acknowledgement lost"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        drop(writer);
        let record = owned.store.history(&owned.project).unwrap().remove(0);
        let stages = record
            .publication
            .as_ref()
            .unwrap()
            .files
            .iter()
            .map(|file| file.staging.clone())
            .collect::<Vec<_>>();
        assert!(stages[0].exists());
        assert!(!stages[1].exists());
        owned.restore(&record.attempt_id);
        owned.assert_restored(&writes, &stages);
    }

    #[test]
    fn native_evidence_artifacts_remain_confined_and_digest_bound() {
        let owned = Owned::new();
        let writes = owned.writes();
        let writer = owned.store.acquire(&owned.project).unwrap();
        let mut publication = writer
            .prepare_schematic_publication(
                "plan",
                "approval",
                serde_json::json!({"artifacts":[]}),
                &writes,
            )
            .unwrap();
        let id = publication.attempt_id().to_owned();
        let stages = publication
            .files()
            .iter()
            .map(|file| file.staging.clone())
            .collect::<Vec<_>>();
        for name in ["../escape", ".", "..", "nested/file", "/absolute", ""] {
            assert!(publication.retain_artifact(name, b"evidence").is_err());
        }
        for name in [
            "preview.json",
            "matching.json",
            "candidate-0.kicad_sch",
            "verification.json",
        ] {
            publication.retain_artifact(name, b"evidence").unwrap();
        }
        publication.check_progress(0).unwrap();
        for artifact in publication.attempt.record.evidence["artifacts"]
            .as_array()
            .unwrap()
        {
            let path = Path::new(artifact["path"].as_str().unwrap());
            assert_eq!(path.parent(), publication.attempt.path.parent());
            assert_eq!(
                artifact["sha256"],
                serde_json::json!(crate::digest_bytes(&fs::read(path).unwrap()))
            );
        }
        publication.record_unknown(Value::Null).unwrap();
        drop(writer);
        owned.restore(&id);
        owned.assert_restored(&writes, &stages);
    }

    #[test]
    fn preflight_and_progress_reject_stale_inputs_collisions_locks_and_journal_drift() {
        let owned = Owned::new();
        let writes = owned.writes();
        let writer = owned.store.acquire(&owned.project).unwrap();
        assert!(
            writer
                .prepare_schematic_publication("plan", "approval", Value::Null, &[])
                .is_err()
        );
        fs::write(&writes[2].original, b"external").unwrap();
        assert!(
            writer
                .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
                .is_err()
        );
        fs::remove_file(&writes[2].original).unwrap();
        let lock = owned.project.join("~child.kicad_sch.lck");
        fs::write(&lock, b"editor").unwrap();
        assert!(
            writer
                .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
                .is_err()
        );
        fs::remove_file(lock).unwrap();
        let mut publication = writer
            .prepare_schematic_publication("plan", "approval", Value::Null, &writes)
            .unwrap();
        let id = publication.attempt_id().to_owned();
        let path = publication.attempt.path.clone();
        let mut forged = publication.attempt.record.clone();
        forged.publication.as_mut().unwrap().files[0].staging = owned.project.join("external.txt");
        fs::write(path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(publication.publish_next().is_err());
        drop(publication);
        drop(writer);
        assert!(owned.store.plan_recovery(&owned.project, &id).is_err());
        assert_eq!(fs::read(&writes[0].original).unwrap(), b"original-a");
    }
}
