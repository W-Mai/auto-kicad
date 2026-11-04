//! Complete PCB candidates composed with exact project-only authority.
use super::*;
mod publication;
pub use publication::{
    MatchedSynchronizationProjectBoardPreview, PreparedSynchronizationProjectBoardPublication,
    SynchronizationProjectBoardExpectation, SynchronizationProjectWrite,
    VerifiedSynchronizationProjectBoardPublication,
};

pub struct SynchronizationProjectBoardPreview {
    project: Arc<SynchronizationProjectPreview>,
    board: Arc<SynchronizationBoardPreview>,
    observed: Arc<SynchronizationBoardObservation>,
    classes: ProjectNetclassSnapshot,
    delta: ProjectNetclassDelta,
    drc_delta: kicad_checks::CompleteDrcDelta,
    evidence: Value,
    digest: String,
    valid: AtomicBool,
}

impl SynchronizationBoardObservation {
    /// Compose a complete PCB candidate with selected project changes. No write or approval.
    pub fn preview_board_and_project(
        self: &Arc<Self>,
        intent: &SynchronizationIntent,
        actions: &[ProjectNetclassChange],
    ) -> Result<Arc<SynchronizationProjectBoardPreview>, String> {
        if !intent.project_netclasses.is_empty()
            && serde_json::to_value(&intent.project_netclasses).map_err(|e| e.to_string())?
                != serde_json::to_value(actions).map_err(|e| e.to_string())?
        {
            return Err("composed project actions differ from the typed intent".into());
        }
        let mut board = intent.clone();
        board.project_netclasses.clear();
        board.validate_board(true)?;
        let project = self.preview_project_netclasses(actions)?;
        self.compose_project(&board, project)
    }

    pub(super) fn compose_project(
        self: &Arc<Self>,
        intent: &SynchronizationIntent,
        project: Arc<SynchronizationProjectPreview>,
    ) -> Result<Arc<SynchronizationProjectBoardPreview>, String> {
        if !Arc::ptr_eq(self, &project.observation) {
            return Err("project preview belongs to another source observation".into());
        }
        project.verify()?;
        if !project.candidate.has_changes() {
            return Err(
                "combined synchronization requires an actual selected project change".into(),
            );
        }
        // This core candidate cannot match the legacy board-only publication expectation.
        let board = self.preview_candidate(intent, true, true)?;
        let audit = &self.source.audit;
        let relative = |path: &Path| -> Result<String, String> {
            path.strip_prefix(&audit.inner.policy.project)
                .map_err(|e| e.to_string())?
                .to_str()
                .map(String::from)
                .ok_or("non-UTF8 composed project path".into())
        };
        let candidates = BTreeMap::from([
            (
                relative(&self.project.requested)?,
                project.candidate.bytes().to_vec(),
            ),
            (relative(&self.path)?, board.candidate.bytes().to_vec()),
        ]);
        let observed = audit.observe_candidate_files(&candidates)?;
        let source = observed.synchronization_source()?;
        if !source::verify_source_classes(&project.observed.source, &source)?.is_empty() {
            return Err("composed candidate changes the matched project source classes".into());
        }
        let path = observed.inner.policy.project.join(relative(&self.path)?);
        let observed = source.observe_board(&path)?;
        let classes = observed.observe_project_netclasses()?;
        if classes.configured() != project.after.configured()
            || board.native.board.text() != observed.native.board.text()
            || board.connectivity.endpoints() != observed.connectivity.endpoints()
            || board.connectivity.components() != observed.connectivity.components()
            || observed.bytes.as_ref() != board.candidate.bytes()
            || observed.project.bytes.as_deref() != Some(project.candidate.bytes())
            || observed.rules.bytes != self.rules.bytes
        {
            return Err(
                "composed project changes protected PCB/native data/configured classes".into(),
            );
        }
        let assignments = crate::schematic_audit::synchronization::project::assignment_changes(
            self.project
                .bytes
                .as_deref()
                .ok_or("missing original project settings")?,
            project.candidate.bytes(),
        )?;
        native::verify_project_endpoint_netclasses(
            &self.native.report,
            &board.native.report,
            &observed.native.report,
            board.candidate.replacement_pads(),
            &assignments,
        )?;
        let mut delta = ProjectNetclassDelta::between(&project.before, &classes);
        delta.source_nets = project.delta.source_nets.clone();
        let drc_delta = self
            .drc
            .report
            .compare_complete(&observed.drc.report)
            .map_err(|e| e.to_string())?;
        project.verify()?;
        board.verify()?;
        observed.verify()?;
        let evidence = serde_json::json!({
            "schema_version": 1, "observation_digest": self.digest,
            "project_preview_digest": project.digest, "board_candidate_digest": board.candidate.digest(),
            "board_differences": board.candidate.differences(), "project_differences": project.candidate.differences(),
            "project_worker_sha256": hash(native::PROJECT_WORKER.as_bytes()),
            "native_before": self.native.report,
            "native_candidate_original_project": board.native.report,
            "native_candidate_project": observed.native.report,
            "netclasses_before": project.before, "netclasses_after": classes, "netclass_delta": delta,
            "selected_assignment_changes": assignments,
            "source_netclasses_before": source::source_netclasses(audit.netlist())?,
            "source_netclasses_after": source::source_netclasses(observed.source.audit.netlist())?,
            "drc_before": self.drc.evidence, "drc_after": observed.drc.evidence, "drc_delta": drc_delta,
            "connectivity_before": self.connectivity, "connectivity_after": observed.connectivity,
            "publication_authorized": false,
        });
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let preview = Arc::new(SynchronizationProjectBoardPreview {
            project,
            board,
            observed,
            classes,
            delta,
            drc_delta,
            evidence,
            digest,
            valid: AtomicBool::new(true),
        });
        preview.verify()?;
        Ok(preview)
    }
}

impl SynchronizationProjectBoardPreview {
    pub fn board_candidate(&self) -> &SynchronizationCandidate {
        self.board.candidate()
    }
    pub fn project_candidate(&self) -> &ProjectNetclassCandidate {
        self.project.candidate()
    }
    pub fn netclasses_after(&self) -> &ProjectNetclassSnapshot {
        &self.classes
    }
    pub fn netclass_delta(&self) -> &ProjectNetclassDelta {
        &self.delta
    }
    pub fn drc_delta(&self) -> &kicad_checks::CompleteDrcDelta {
        &self.drc_delta
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("composed project preview was invalidated".into());
        }
        let result = (|| {
            self.project.verify()?;
            self.board.verify()?;
            self.observed.verify()?;
            if self.observed.bytes.as_ref() != self.board.candidate.bytes()
                || self.observed.project.bytes.as_deref() != Some(self.project.candidate.bytes())
            {
                return Err("composed project preview differs from exact candidates".into());
            }
            Ok(())
        })();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
}
