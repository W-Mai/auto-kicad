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

    fn prepare(&self, policy: &SchematicAuditPolicy) -> Result<SchematicAudit, String> {
        if self.schema_version != 1
            || policy.project != self.project
            || policy.schematic.with_extension("kicad_pcb") != self.board
            || self.footprint_ids.len() > 8192
            || self.drc_json.len() > 32 * 1024 * 1024
        {
            return Err("synchronization recovery baseline/policy mismatch".into());
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
        if actual.native.report != self.native
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
        actual.verify()?;
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
