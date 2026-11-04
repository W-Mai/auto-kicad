//! Restart-safe original-input and native baseline evidence; no paths grant write authority.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationRecoveryBaseline {
    schema_version: u32,
    project: PathBuf,
    board: PathBuf,
    board_sha256: String,
    footprint_ids: Vec<(String, String)>,
    dependencies_sha256: String,
    dependencies: Value,
    native: native::NativeReport,
    connectivity: Value,
    drc_json: String,
}

fn digest_value(value: &impl Serialize) -> Result<String, String> {
    Ok(hash(&serde_json::to_vec(value).map_err(|e| e.to_string())?))
}

fn dependencies(audit: &SchematicAudit, ids: &[(String, String)]) -> Result<Value, String> {
    audit.verify()?;
    let context = audit
        .inner
        .runner
        .execution_context()
        .ok_or("missing native context")?;
    let mut environment = Sha256::new();
    // The private configuration directory is freshly allocated after restart; its original
    // authority is covered by the policy and source manifest instead of its temporary name.
    for name in context
        .environment_names()
        .filter(|name| *name != "KICAD_CONFIG_HOME")
    {
        let value = context
            .environment_value(name)
            .ok_or("missing environment value")?;
        for token in [name, value] {
            let bytes = token.as_encoded_bytes();
            environment.update((bytes.len() as u64).to_le_bytes());
            environment.update(bytes);
        }
    }
    let footprints = ids
        .iter()
        .map(|(nickname, entry_name)| FootprintLibraryIdentifier {
            nickname: nickname.clone(),
            entry_name: entry_name.clone(),
        })
        .collect::<Vec<_>>();
    let policy = &audit.inner.policy;
    Ok(serde_json::json!({
        "policy":[policy.project,policy.schematic,policy.configuration,policy.symbols,policy.footprints],
        "extra_roots":policy.extra_roots,
        "source":audit.inner.source,
        "configuration_names":audit.inner.source_config_names,
        "symbols":audit.inner.symbol_context.files(),
        "symbol_tables":audit.inner.symbols.table_evidence(),
        "symbol_rows":audit.inner.symbols.effective_rows()?,
        "footprints":audit.inner.footprints.schematic_files(&footprints)?.files(),
        "footprint_tables":audit.inner.footprints.table_evidence(),
        "footprint_rows":audit.inner.footprints.effective_rows()?,
        "native_executable":context.executable(),
        "native_executable_sha256":context.executable_digest(),
        "environment_sha256":hex::encode(environment.finalize()),
        "board_python":Inputs::read(Path::new(native::PYTHON))?,
        "worker_sha256":hash(native::WORKER.as_bytes()),
    }))
}

impl SynchronizationBoardObservation {
    pub fn recovery_baseline(&self) -> Result<SynchronizationRecoveryBaseline, String> {
        self.verify()?;
        let mut ids = self
            .source
            .audit
            .inner
            .footprint_ids
            .iter()
            .map(|id| (id.nickname.clone(), id.entry_name.clone()))
            .collect::<BTreeSet<_>>();
        for footprint in self.board.footprints().values() {
            if let Some((nickname, entry)) = footprint.library.split_once(':') {
                ids.insert((nickname.into(), entry.into()));
            }
        }
        let ids = ids.into_iter().collect::<Vec<_>>();
        let dependencies = dependencies(&self.source.audit, &ids)?;
        let result = SynchronizationRecoveryBaseline {
            schema_version: 1,
            project: self.source.audit.inner.policy.project.clone(),
            board: self.path.clone(),
            board_sha256: hash(&self.bytes),
            dependencies_sha256: digest_value(&dependencies)?,
            dependencies,
            footprint_ids: ids,
            native: self.native.report.clone(),
            connectivity: serde_json::to_value(&self.connectivity).map_err(|e| e.to_string())?,
            drc_json: self.drc.report.raw().get().into(),
        };
        self.verify()?;
        Ok(result)
    }
}

impl PreparedSynchronizationPublication {
    pub fn recovery_baseline(&self) -> Result<SynchronizationRecoveryBaseline, String> {
        self.matched.preview.observation.recovery_baseline()
    }
}

impl SynchronizationRecoveryBaseline {
    pub fn project_dir(&self) -> &Path {
        &self.project
    }
    pub fn board_path(&self) -> &Path {
        &self.board
    }
    pub fn board_sha256(&self) -> &str {
        &self.board_sha256
    }

    /// Rebuild authority from the server's current policy, never from journal-supplied roots.
    pub fn verify_dependencies(&self, policy: &SchematicAuditPolicy) -> Result<(), String> {
        self.prepare(policy).map(|_| ())
    }

    fn validate_policy(&self, policy: &SchematicAuditPolicy) -> Result<(), String> {
        if self.schema_version != 1
            || policy.project != self.project
            || policy.schematic.with_extension("kicad_pcb") != self.board
            || self.footprint_ids.len() > 8192
            || self.drc_json.len() > 32 * 1024 * 1024
        {
            return Err("synchronization recovery baseline/policy mismatch".into());
        }
        Ok(())
    }

    fn prepare(&self, policy: &SchematicAuditPolicy) -> Result<SchematicAudit, String> {
        self.validate_policy(policy)?;
        let policy = SchematicAuditPolicy::new(
            &policy.project,
            &policy.schematic,
            &policy.configuration,
            &policy.symbols,
            &policy.footprints,
            &policy.extra_roots,
        )?;
        let audit = SchematicAudit::prepare(&policy)?;
        let current = dependencies(&audit, &self.footprint_ids)?;
        if digest_value(&self.dependencies)? != self.dependencies_sha256 {
            return Err("synchronization recovery dependency manifest digest mismatch".into());
        }
        if current != self.dependencies {
            let changed = self
                .dependencies
                .as_object()
                .ok_or("invalid dependency manifest")?
                .iter()
                .filter(|(key, value)| current.get(*key) != Some(*value))
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>();
            return Err(format!(
                "synchronization recovery protected dependencies changed: {}",
                changed.join(", ")
            ));
        }
        Ok(audit)
    }

    /// Allow only an exact typed project transition while retaining legacy manifest semantics.
    pub(in crate::schematic_audit::synchronization) fn prepare_project_transition(
        &self,
        policy: &SchematicAuditPolicy,
        original_project: &[u8],
    ) -> Result<SchematicAudit, String> {
        self.validate_policy(policy)?;
        if original_project.len() as u64 > MAX_FILE
            || digest_value(&self.dependencies)? != self.dependencies_sha256
        {
            return Err("invalid project recovery original/dependency manifest".into());
        }
        let policy = SchematicAuditPolicy::new(
            &policy.project,
            &policy.schematic,
            &policy.configuration,
            &policy.symbols,
            &policy.footprints,
            &policy.extra_roots,
        )?;
        let audit = SchematicAudit::prepare(&policy)?;
        let current = dependencies(&audit, &self.footprint_ids)?;
        let path = policy.schematic.with_extension("kicad_pro");
        if normalize_project_dependency(current, &path, original_project)? != self.dependencies {
            return Err("project recovery protected dependencies changed".into());
        }
        audit.verify()?;
        Ok(audit)
    }

    pub(in crate::schematic_audit::synchronization) fn verify_original_observation(
        &self,
        actual: &SynchronizationBoardObservation,
    ) -> Result<(), String> {
        if hash(&actual.bytes) != self.board_sha256
            || actual.native.report != self.native
            || serde_json::to_value(&actual.connectivity).map_err(|e| e.to_string())?
                != self.connectivity
        {
            return Err("synchronization recovery native baseline mismatch".into());
        }
        let drc =
            kicad_checks::CompleteDrcReport::parse(&self.drc_json).map_err(|e| e.to_string())?;
        drc.compare_complete(&actual.drc.report)
            .map_err(|e| e.to_string())?
            .match_expectation(&kicad_checks::CompleteDrcExpectation::default())
            .map_err(|e| e.to_string())?;
        actual.verify()
    }

    /// Actual restored PCB must independently reproduce the original native baseline.
    pub fn verify_restored(
        &self,
        policy: &SchematicAuditPolicy,
    ) -> Result<VerifiedSynchronizationRestoration, String> {
        let audit = self.prepare(policy)?;
        if native::bounded_read(&self.board, 128 * 1024 * 1024).map(|b| hash(&b))?
            != self.board_sha256
        {
            return Err("synchronization recovery did not restore original PCB bytes".into());
        }
        let source = audit.synchronization_source()?;
        let actual = source.observe_board(&self.board)?;
        self.verify_original_observation(&actual)?;
        let evidence = serde_json::json!({"baseline_sha256":digest_value(self)?,"project":self.project,"board":self.board,
            "actual_native":actual.native.report,"actual_connectivity":actual.connectivity,
            "complete_drc_json":actual.drc.report.raw().get(),"actual_source_native_restored":true});
        let digest = digest_value(&evidence)?;
        Ok(VerifiedSynchronizationRestoration {
            actual,
            digest,
            evidence,
        })
    }
}

/// Constructed only after exact restored bytes and a complete fresh native observation agree.
pub struct VerifiedSynchronizationRestoration {
    actual: Arc<SynchronizationBoardObservation>,
    digest: String,
    evidence: Value,
}
impl VerifiedSynchronizationRestoration {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn verify(&self) -> Result<(), String> {
        self.actual.verify()
    }
}

// Only the comparison copy changes. Its original metadata comes from verified backup bytes.
fn normalize_project_dependency(
    mut current: Value,
    path: &Path,
    original: &[u8],
) -> Result<Value, String> {
    if original.len() as u64 > MAX_FILE {
        return Err("project recovery backup exceeds bound".into());
    }
    let source = current
        .get_mut("source")
        .and_then(Value::as_object_mut)
        .ok_or("missing recovery source manifest")?;
    let total = source
        .get("total")
        .and_then(Value::as_u64)
        .ok_or("missing recovery source byte total")?;
    let input = source
        .get_mut("files")
        .and_then(Value::as_object_mut)
        .and_then(|files| files.get_mut(path.to_str()?))
        .ok_or("missing exact project recovery dependency")?;
    let metadata: Value = serde_json::to_value(InputFile {
        requested: path.into(),
        canonical: Some(path.into()),
        digest: Some(hash(original)),
        size: original.len() as u64,
        bytes: None,
    })
    .map_err(|e| e.to_string())?;
    if input.get("requested") != metadata.get("requested")
        || input.get("canonical") != metadata.get("canonical")
    {
        return Err("project recovery dependency path is not canonical".into());
    }
    let size = input
        .get("size")
        .and_then(Value::as_u64)
        .ok_or("missing current project size")?;
    let total = total
        .checked_sub(size)
        .and_then(|n| n.checked_add(original.len() as u64))
        .filter(|n| *n <= MAX_TOTAL)
        .ok_or("invalid project recovery source byte total")?;
    *input = metadata;
    source.insert("total".into(), total.into());
    Ok(current)
}

#[cfg(test)]
mod transition_tests {
    use super::*;

    #[test]
    fn project_dependency_transition_changes_only_exact_metadata_and_total() {
        let path = Path::new("/owned/project/main.kicad_pro");
        let input = InputFile {
            requested: path.into(),
            canonical: Some(path.into()),
            digest: Some(hash(b"candidate")),
            size: 9,
            bytes: None,
        };
        let current = serde_json::json!({"source":{"files":{path.to_str().unwrap():input,
            "/owned/project/main.kicad_sch":{"digest":"immutable","size":10}},"total":19},
            "worker_sha256":"legacy", "other":{"unknown":true}});
        let normalized = normalize_project_dependency(current.clone(), path, b"before").unwrap();
        assert_eq!(normalized["source"]["total"], 16);
        assert_eq!(
            normalized["source"]["files"][path.to_str().unwrap()]["digest"],
            hash(b"before")
        );
        assert_eq!(
            normalized["source"]["files"]["/owned/project/main.kicad_sch"],
            current["source"]["files"]["/owned/project/main.kicad_sch"]
        );
        assert_eq!(normalized["worker_sha256"], current["worker_sha256"]);
        assert_eq!(normalized["other"], current["other"]);
        let mut redirected = current.clone();
        redirected["source"]["files"][path.to_str().unwrap()]["canonical"] =
            "/outside/project".into();
        assert!(normalize_project_dependency(redirected, path, b"before").is_err());
        let mut underflow = current.clone();
        underflow["source"]["total"] = 1.into();
        assert!(normalize_project_dependency(underflow, path, b"before").is_err());
        assert!(
            normalize_project_dependency(current, Path::new("/outside/file"), b"before").is_err()
        );
    }
}
