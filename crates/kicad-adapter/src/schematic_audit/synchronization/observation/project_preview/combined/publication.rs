//! Exact combined expectation and phase checks; all file IO belongs to the durable writer.
use super::*;
use crate::schematic_audit::source_transition::{
    CapturedPublicationTransition, VerifiedInputPrefix,
};
use crate::schematic_audit::synchronization::observation::publication::canonical_differences;
use crate::schematic_audit::synchronization::project::ProjectNetclassDifference;

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationProjectBoardExpectation {
    pub differences: Vec<board::SynchronizationDifference>,
    pub project_differences: Vec<ProjectNetclassDifference>,
    pub netclasses: ProjectNetclassDelta,
    pub drc: kicad_checks::CompleteDrcExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erc: Option<crate::schematic_audit::preview::ErcExpectation>,
}

pub struct MatchedSynchronizationProjectBoardPreview {
    preview: Arc<SynchronizationProjectBoardPreview>,
    expectation: SynchronizationProjectBoardExpectation,
    digest: String,
}

pub struct SynchronizationProjectWrite<'a> {
    pub path: PathBuf,
    pub original: &'a [u8],
    pub candidate: &'a [u8],
}

pub struct PreparedSynchronizationProjectBoardPublication {
    matched: MatchedSynchronizationProjectBoardPreview,
    transition: CapturedPublicationTransition,
    valid: AtomicBool,
}

pub struct VerifiedSynchronizationProjectBoardPublication {
    check: PreparedSynchronizationProjectBoardPublication,
    actual: Arc<SynchronizationBoardObservation>,
    evidence: Value,
    digest: String,
}

impl SynchronizationProjectBoardPreview {
    /// Bind every exact PCB/project/source/rule/DRC change without granting human approval.
    pub fn match_expectation(
        self: &Arc<Self>,
        mut expected: SynchronizationProjectBoardExpectation,
    ) -> Result<MatchedSynchronizationProjectBoardPreview, String> {
        self.verify()?;
        expected.differences = canonical_differences(&expected.differences)?;
        expected.project_differences = super::super::publication::canonical_project_differences(
            &expected.project_differences,
        )?;
        if expected.differences != canonical_differences(self.board.candidate.differences())?
            || expected.project_differences != self.project.candidate.differences()
        {
            return Err("combined_project_board_difference_expectation_mismatch".into());
        }
        self.delta.match_expectation(&expected.netclasses)?;
        self.project.match_erc_expectation(expected.erc.as_ref())?;
        self.drc_delta
            .match_expectation(&expected.drc)
            .map_err(|e| e.to_string())?;
        let digest = hash(
            &serde_json::to_vec(
                &serde_json::json!({"preview": self.digest, "expectation": expected}),
            )
            .map_err(|e| e.to_string())?,
        );
        Ok(MatchedSynchronizationProjectBoardPreview {
            preview: self.clone(),
            expectation: expected,
            digest,
        })
    }
}

impl MatchedSynchronizationProjectBoardPreview {
    pub fn preview(&self) -> &Arc<SynchronizationProjectBoardPreview> {
        &self.preview
    }
    pub fn expectation(&self) -> &SynchronizationProjectBoardExpectation {
        &self.expectation
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn prepare_publication(
        self,
    ) -> Result<PreparedSynchronizationProjectBoardPublication, String> {
        self.preview.verify()?;
        let observation = &self.preview.project.observation;
        let root = &observation.source.audit.inner.policy.project;
        let relative = |path: &Path| -> Result<String, String> {
            path.strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .map(String::from)
                .ok_or("non-UTF8 publication path".into())
        };
        let project = relative(&observation.project.requested)?;
        let mut order = vec![project.clone()];
        let mut candidates =
            BTreeMap::from([(project, self.preview.project.candidate.bytes().to_vec())]);
        if self.preview.board.candidate.has_changes() {
            let board = relative(&observation.path)?;
            order.push(board.clone());
            candidates.insert(board, self.preview.board.candidate.bytes().to_vec());
        }
        let mut pcb_inputs = Inputs::default();
        if pcb_inputs.track(&observation.path)?.bytes.as_deref() != Some(&observation.bytes) {
            return Err("publication PCB capture differs from matched original".into());
        }
        let transition = CapturedPublicationTransition::new(
            &[&observation.source.audit.inner.source, &pcb_inputs],
            root,
            candidates,
            order,
        )?;
        let check = PreparedSynchronizationProjectBoardPublication {
            matched: self,
            transition,
            valid: AtomicBool::new(true),
        };
        check.verify_before_write()?;
        Ok(check)
    }
}

impl PreparedSynchronizationProjectBoardPublication {
    pub fn recovery_baseline(&self) -> Result<SynchronizationProjectRecoveryBaseline, String> {
        self.verify_before_write()?;
        SynchronizationProjectRecoveryBaseline::capture(
            &self.matched.preview.project,
            self.matched.preview.board_candidate().bytes(),
        )
    }
    pub fn project_dir(&self) -> &Path {
        self.transition.project()
    }
    pub fn board_path(&self) -> &Path {
        &self.matched.preview.project.observation.path
    }
    pub fn project_path(&self) -> &Path {
        &self.matched.preview.project.observation.project.requested
    }
    pub fn matched_digest(&self) -> &str {
        self.matched.digest()
    }
    pub fn preview_evidence(&self) -> &Value {
        self.matched.preview.evidence()
    }
    pub fn expectation(&self) -> &SynchronizationProjectBoardExpectation {
        self.matched.expectation()
    }
    /// Project first; omit an unchanged PCB rather than manufacturing a second write.
    pub fn writes(&self) -> Vec<SynchronizationProjectWrite<'_>> {
        self.transition
            .order()
            .iter()
            .map(|relative| SynchronizationProjectWrite {
                path: self.project_dir().join(relative),
                original: self.transition.original(relative),
                candidate: self.transition.candidate(relative),
            })
            .collect()
    }
    fn checked<T>(&self, verify: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("combined project publication check was invalidated".into());
        }
        let result = verify();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
    pub fn verify_before_write(&self) -> Result<(), String> {
        self.checked(|| {
            self.matched.preview.verify()?;
            self.verify_publication_prefix(0)
        })
    }
    /// Revalidate the exact write prefix and every immutable private/source resource.
    pub fn verify_publication_prefix(&self, published: usize) -> Result<(), String> {
        self.checked(|| {
            let prefix = self.transition.verify_prefix(published)?;
            self.verify_private_prefix(&prefix)
        })
    }
    fn verify_private_prefix(&self, prefix: &VerifiedInputPrefix<'_>) -> Result<(), String> {
        let preview = &self.matched.preview;
        let observation = &preview.project.observation;
        let audit = &observation.source.audit;
        audit.verify_publication_prefix(prefix)?;
        require_closed(&observation.path)?;
        observation.native.verify_execution(audit)?;
        observation.drc.verify_prefix(audit, prefix)?;
        preview.board.verify_private_prefix(prefix)?;
        preview.project.observed.verify()?;
        preview.observed.verify()?;
        Ok(())
    }
    /// Rebuild actual source/native authority after every ordered file has its exact candidate.
    pub fn verify_published(
        self,
    ) -> Result<VerifiedSynchronizationProjectBoardPublication, String> {
        let actual = self.checked(|| {
            self.verify_publication_prefix(self.transition.order().len())?;
            let preview = &self.matched.preview;
            let actual = super::super::publication::observe_published_candidate(
                &preview.observed,
                &preview.project.observation.source.audit.inner.policy,
                self.board_path(),
                &preview.classes,
            )?;
            self.verify_publication_prefix(self.transition.order().len())?;
            Ok(actual)
        })?;
        let writes = self.writes().iter().map(|write| serde_json::json!({"path":write.path,"original_sha256":hash(write.original),"candidate_sha256":hash(write.candidate)})).collect::<Vec<_>>();
        let evidence = serde_json::json!({"schema_version":1,"matched_preview_digest":self.matched_digest(),"writes":writes,
            "source_digest":actual.source.digest(),"actual_observation_digest":actual.digest,
            "native":actual.native.report,"netclasses":self.matched.preview.classes,
            "connectivity":actual.connectivity,"drc":actual.drc.evidence,
            "project_worker_sha256":hash(native::PROJECT_WORKER.as_bytes()),
            "actual_source_native_verified":true,"human_approval_granted":false});
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let proof = VerifiedSynchronizationProjectBoardPublication {
            check: self,
            actual,
            evidence,
            digest,
        };
        proof.verify()?;
        Ok(proof)
    }
}

impl VerifiedSynchronizationProjectBoardPublication {
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
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn verify(&self) -> Result<(), String> {
        self.check.checked(|| {
            self.check
                .verify_publication_prefix(self.check.transition.order().len())?;
            self.actual.verify()
        })
    }
}
