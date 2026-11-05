//! Matched project-only publication checks. The durable writer owns all file writes.
use super::*;
use crate::schematic_audit::source_transition::verify_candidate_inputs_at_prefix;

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationProjectExpectation {
    pub differences:
        Vec<crate::schematic_audit::synchronization::project::ProjectNetclassDifference>,
    pub netclasses: ProjectNetclassDelta,
    pub drc: kicad_checks::CompleteDrcExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erc: Option<crate::schematic_audit::preview::ErcExpectation>,
}

pub struct MatchedSynchronizationProjectPreview {
    preview: Arc<SynchronizationProjectPreview>,
    expectation: SynchronizationProjectExpectation,
    digest: String,
}

pub struct PreparedSynchronizationProjectPublication {
    matched: MatchedSynchronizationProjectPreview,
    board_inputs: Inputs,
    candidates: BTreeMap<String, Vec<u8>>,
    order: Vec<String>,
    valid: AtomicBool,
}

pub struct VerifiedSynchronizationProjectPublication {
    check: PreparedSynchronizationProjectPublication,
    actual: Arc<SynchronizationBoardObservation>,
    evidence: Value,
    digest: String,
}

impl SynchronizationProjectPreview {
    /// Match complete project, native-class and DRC differences. This grants no human approval.
    pub fn match_expectation(
        self: &Arc<Self>,
        mut expectation: SynchronizationProjectExpectation,
    ) -> Result<MatchedSynchronizationProjectPreview, String> {
        self.verify()?;
        expectation.differences = canonical_project_differences(&expectation.differences)?;
        if expectation.differences != self.candidate.differences() {
            return Err("project_difference_expectation_mismatch".into());
        }
        self.match_netclass_expectation(&expectation.netclasses)?;
        self.match_drc_expectation(&expectation.drc)?;
        self.match_erc_expectation(expectation.erc.as_ref())?;
        let digest = hash(
            &serde_json::to_vec(&serde_json::json!({
                "preview": self.digest, "expectation": expectation,
            }))
            .map_err(|e| e.to_string())?,
        );
        Ok(MatchedSynchronizationProjectPreview {
            preview: self.clone(),
            expectation,
            digest,
        })
    }
}

impl MatchedSynchronizationProjectPreview {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn preview(&self) -> &Arc<SynchronizationProjectPreview> {
        &self.preview
    }
    pub fn expectation(&self) -> &SynchronizationProjectExpectation {
        &self.expectation
    }
    pub fn prepare_publication(self) -> Result<PreparedSynchronizationProjectPublication, String> {
        self.preview.verify()?;
        if self.preview.region_profile || self.preview.observation.regions.is_some() {
            return Err(
                "project region transitions require the complete PCB/refill publication workflow"
                    .into(),
            );
        }
        if !self.preview.candidate.has_changes() {
            return Err("synchronization_has_no_changes".into());
        }
        let observation = &self.preview.observation;
        let path = &observation.project.requested;
        if observation.project.canonical.as_ref() != Some(path)
            || fs::symlink_metadata(path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("project publication requires a canonical regular destination".into());
        }
        let relative = path
            .strip_prefix(&observation.source.audit.inner.policy.project)
            .map_err(|e| e.to_string())?
            .to_str()
            .ok_or("non-UTF8 project path")?
            .to_owned();
        let candidates =
            BTreeMap::from([(relative.clone(), self.preview.candidate.bytes().to_vec())]);
        let mut board_inputs = Inputs::default();
        if board_inputs.track(&observation.path)?.bytes.as_deref() != Some(&observation.bytes) {
            return Err("project publication PCB differs from the captured source".into());
        }
        let check = PreparedSynchronizationProjectPublication {
            matched: self,
            board_inputs,
            candidates,
            order: vec![relative],
            valid: AtomicBool::new(true),
        };
        check.verify_before_write()?;
        Ok(check)
    }
}

impl PreparedSynchronizationProjectPublication {
    pub fn project_dir(&self) -> &Path {
        &self
            .matched
            .preview
            .observation
            .source
            .audit
            .inner
            .policy
            .project
    }
    pub fn project_path(&self) -> &Path {
        &self.matched.preview.observation.project.requested
    }
    pub fn board_path(&self) -> &Path {
        &self.matched.preview.observation.path
    }
    pub fn original_bytes(&self) -> &[u8] {
        self.matched
            .preview
            .observation
            .project
            .bytes
            .as_deref()
            .expect("captured project")
    }
    pub fn candidate_bytes(&self) -> &[u8] {
        self.matched.preview.candidate.bytes()
    }
    pub fn matched_digest(&self) -> &str {
        self.matched.digest()
    }
    pub fn preview_evidence(&self) -> &Value {
        self.matched.preview.evidence()
    }
    pub fn expectation(&self) -> &SynchronizationProjectExpectation {
        self.matched.expectation()
    }

    fn checked<T>(&self, verify: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("project publication check was invalidated".into());
        }
        let result = verify();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }

    fn verify_phase(&self, published: usize) -> Result<(), String> {
        self.checked(|| {
            let preview = &self.matched.preview;
            let observation = &preview.observation;
            let audit = &observation.source.audit;
            require_closed(&observation.path)?;
            verify_candidate_inputs_at_prefix(
                &[&audit.inner.source, &self.board_inputs],
                self.project_dir(),
                &self.candidates,
                &BTreeSet::new(),
                &self.order,
                published,
            )?;
            audit.verify_context()?;
            audit.verify_hierarchy(&audit.inner.parsed)?;
            observation.native.verify_execution(audit)?;
            preview.observed.verify()?;
            Ok(())
        })
    }

    pub fn verify_before_write(&self) -> Result<(), String> {
        self.checked(|| {
            self.matched.preview.verify()?;
            self.verify_phase(0)
        })
    }

    /// Verify actual candidate bytes and rebuild native authority using the original server policy.
    /// Never call the original immutable audit's ordinary verify after the approved project write.
    pub fn verify_published(self) -> Result<VerifiedSynchronizationProjectPublication, String> {
        let actual = self.checked(|| {
            self.verify_phase(1)?;
            let preview = &self.matched.preview;
            let actual = observe_published_candidate(
                &preview.observed,
                &preview.observation.source.audit.inner.policy,
                self.board_path(),
                &preview.after,
            )?;
            self.verify_phase(1)?;
            Ok(actual)
        })?;
        let evidence = serde_json::json!({
            "schema_version": 1, "project": self.project_path(), "board": self.board_path(),
            "candidate_sha256": hash(self.candidate_bytes()), "matched_preview_digest": self.matched_digest(),
            "actual_source_digest": actual.source.digest(), "actual_observation_digest": actual.digest,
            "project_worker_sha256": hash(native::PROJECT_WORKER.as_bytes()),
            "native": actual.native.report, "netclasses": self.matched.preview.after,
            "connectivity": actual.connectivity, "drc": actual.drc.evidence,
            "actual_source_native_verified": true, "human_approval_granted": false,
        });
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let proof = VerifiedSynchronizationProjectPublication {
            check: self,
            actual,
            evidence,
            digest,
        };
        proof.verify()?;
        Ok(proof)
    }
}

impl VerifiedSynchronizationProjectPublication {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn project_dir(&self) -> &Path {
        self.check.project_dir()
    }
    pub fn project_path(&self) -> &Path {
        self.check.project_path()
    }
    pub fn board_path(&self) -> &Path {
        self.check.board_path()
    }
    pub fn matched_digest(&self) -> &str {
        self.check.matched_digest()
    }
    pub fn verify(&self) -> Result<(), String> {
        self.check.checked(|| {
            self.check.verify_phase(1)?;
            self.actual.verify()
        })
    }
}

pub(super) fn canonical_project_differences(
    rows: &[crate::schematic_audit::synchronization::project::ProjectNetclassDifference],
) -> Result<Vec<crate::schematic_audit::synchronization::project::ProjectNetclassDifference>, String>
{
    if rows.len() > 100_000 {
        return Err("project difference count exceeds bound".into());
    }
    let mut bytes = 0usize;
    for row in rows {
        if row.path.is_empty() || row.path.len() > 16 || row.path.iter().any(String::is_empty) {
            return Err("empty project difference identity".into());
        }
        for value in row
            .path
            .iter()
            .map(String::as_str)
            .chain(row.before.as_deref())
            .chain(row.after.as_deref())
        {
            bytes = bytes
                .checked_add(value.len())
                .ok_or("project difference size overflow")?;
            if bytes > 128 * 1024 * 1024 {
                return Err("project difference bytes exceed bound".into());
            }
        }
    }
    let mut rows = rows.to_vec();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    if rows.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err("duplicate project difference identity".into());
    }
    Ok(rows)
}

pub(super) fn observe_published_candidate(
    observed: &SynchronizationBoardObservation,
    policy: &SchematicAuditPolicy,
    board_path: &Path,
    classes: &ProjectNetclassSnapshot,
) -> Result<Arc<SynchronizationBoardObservation>, String> {
    let audit = SchematicAudit::prepare(policy)?;
    let expected = &observed.source.audit;
    let source = audit.synchronization_source()?;
    if audit.inner.parsed != expected.inner.parsed
        || audit.netlist() != expected.netlist()
        || audit.identities() != expected.identities()
        || audit.pins() != expected.pins()
        || audit.sheet_inventory()? != expected.sheet_inventory()?
        || !audit
            .erc()
            .equivalent_to(expected.erc())
            .map_err(|e| e.to_string())?
        || source.components() != observed.source.components()
        || source.ineligible_units() != observed.source.ineligible_units()
        || source.connectivity.connectivity() != observed.source.connectivity.connectivity()
    {
        return Err(
            "published project source hierarchy/netlist/pins/ERC differs from matched preview"
                .into(),
        );
    }
    let actual = if observed.regions.is_some() {
        source.observe_region_board(board_path)?
    } else {
        source.observe_board(board_path)?
    };
    if match (&observed.regions, &actual.regions) {
        (Some(before), Some(after)) => !before.same_native(after),
        (None, None) => false,
        _ => true,
    } {
        return Err(
            "published project native region ownership differs from the matched candidate".into(),
        );
    }
    if !actual
        .native
        .same_report(&observed.native, observed.regions.is_some())?
        || !actual
            .native
            .same_saved_content(&observed.native, observed.regions.is_some())?
        || actual.connectivity != observed.connectivity
        || actual.observe_project_netclasses()? != *classes
    {
        return Err(
            "published project native board/classes/connectivity differs from matched preview"
                .into(),
        );
    }
    actual
        .drc
        .report
        .compare_complete(&observed.drc.report)
        .map_err(|e| e.to_string())?
        .match_expectation(&kicad_checks::CompleteDrcExpectation::default())
        .map_err(|e| e.to_string())?;
    actual.verify()?;
    Ok(actual)
}

#[cfg(test)]
mod difference_tests {
    use super::*;
    #[test]
    fn exact_project_difference_identity_rejects_duplicates_and_preserves_raw_tokens() {
        use crate::schematic_audit::synchronization::project::ProjectNetclassDifference;
        let row = ProjectNetclassDifference {
            path: vec!["net_settings".into(), "classes".into(), "Power".into()],
            before: Some("1e-000003".into()),
            after: Some("-0.00".into()),
        };
        assert_eq!(
            canonical_project_differences(std::slice::from_ref(&row)).unwrap(),
            vec![row.clone()]
        );
        assert!(canonical_project_differences(&[row.clone(), row.clone()]).is_err());
        let mut invalid = row.clone();
        invalid.path = vec!["x".into(); 17];
        assert!(canonical_project_differences(&[invalid]).is_err());
        let mut changed = row.clone();
        changed.before = Some("0.000001".into());
        assert_ne!(
            canonical_project_differences(&[row]).unwrap(),
            canonical_project_differences(&[changed]).unwrap()
        );
    }
}
