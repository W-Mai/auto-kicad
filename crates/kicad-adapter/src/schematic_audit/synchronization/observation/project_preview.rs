//! Source-bound project-only candidates with complete isolated native readback.
use super::*;
use crate::schematic_audit::synchronization::project::{
    ProjectNetclassCandidate, ProjectNetclassChange, compile_project_netclasses,
    native::{ConfiguredClass, EffectiveClass, ProjectNetclassSnapshot},
};
use serde::Deserialize;
mod combined;
mod intent;
pub use intent::SynchronizationIntentPreview;
mod publication;
mod recovery;
pub use combined::{
    MatchedSynchronizationProjectBoardPreview, PreparedSynchronizationProjectBoardPublication,
    SynchronizationProjectBoardExpectation, SynchronizationProjectBoardPreview,
    SynchronizationProjectWrite, VerifiedSynchronizationProjectBoardPublication,
};
pub use recovery::{
    SynchronizationProjectRecoveryBaseline, VerifiedSynchronizationProjectRecovery,
};
mod source;
pub use publication::{
    MatchedSynchronizationProjectPreview, PreparedSynchronizationProjectPublication,
    SynchronizationProjectExpectation, VerifiedSynchronizationProjectPublication,
};
pub use source::SourceNetclass;

/// Complete changed native records; no parameter or provenance is omitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetclassRecordChange<T> {
    pub before: Option<T>,
    pub after: Option<T>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassDelta {
    pub source_nets: BTreeMap<String, NetclassRecordChange<SourceNetclass>>,
    pub configured: BTreeMap<String, NetclassRecordChange<ConfiguredClass>>,
    pub effective: BTreeMap<String, NetclassRecordChange<EffectiveClass>>,
}

fn changed_records<T: Clone + Eq>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
) -> BTreeMap<String, NetclassRecordChange<T>> {
    before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| {
            (
                key.clone(),
                NetclassRecordChange {
                    before: before.get(key).cloned(),
                    after: after.get(key).cloned(),
                },
            )
        })
        .collect()
}

impl ProjectNetclassDelta {
    fn between(before: &ProjectNetclassSnapshot, after: &ProjectNetclassSnapshot) -> Self {
        Self {
            source_nets: BTreeMap::new(),
            configured: changed_records(before.configured(), after.configured()),
            effective: changed_records(before.effective(), after.effective()),
        }
    }
    pub fn match_expectation(&self, expected: &Self) -> Result<(), String> {
        if self != expected {
            return Err(
                "complete configured/effective netclass changes differ from expectation".into(),
            );
        }
        Ok(())
    }
}

/// A read-only project candidate. Matching evidence cannot publish source files.
pub struct SynchronizationProjectPreview {
    observation: Arc<SynchronizationBoardObservation>,
    candidate: ProjectNetclassCandidate,
    actions: Vec<ProjectNetclassChange>,
    before: ProjectNetclassSnapshot,
    after: ProjectNetclassSnapshot,
    observed: Arc<SynchronizationBoardObservation>,
    delta: ProjectNetclassDelta,
    drc_delta: kicad_checks::CompleteDrcDelta,
    erc_delta: kicad_checks::erc::ErcDelta,
    evidence: Value,
    digest: String,
    valid: AtomicBool,
}

impl SynchronizationBoardObservation {
    /// Compile bounded project changes, re-observing both the unchanged source and PCB.
    /// Physical edits, write approval and publication require their separate workflow.
    pub fn preview_project_netclasses(
        self: &Arc<Self>,
        actions: &[ProjectNetclassChange],
    ) -> Result<Arc<SynchronizationProjectPreview>, String> {
        self.verify()?;
        let before = self.observe_project_netclasses()?;
        let audit = &self.source.audit;
        let allowed_nets = before
            .effective()
            .keys()
            .cloned()
            .chain(audit.netlist().nets.iter().map(|net| net.name.clone()))
            .filter(|name| !name.is_empty())
            .collect();
        let source_classes = super::super::source_classes::references(
            audit,
            self.project
                .bytes
                .as_deref()
                .ok_or("missing captured project settings")?,
        )?;
        let candidate = compile_project_netclasses(
            self.project
                .bytes
                .as_deref()
                .ok_or("missing captured project settings")?,
            actions,
            &allowed_nets,
            &source_classes,
        )?;
        if candidate.has_changes() && self.board.has_zones() {
            return Err(
                "project netclass changes on zoned boards require complete refill qualification"
                    .into(),
            );
        }
        if candidate.has_changes() && self.rules.bytes.is_some() {
            return Err(
                "project netclass changes with custom rules require separate qualification".into(),
            );
        }
        let relative = |path: &Path| -> Result<String, String> {
            path.strip_prefix(&audit.inner.policy.project)
                .map_err(|e| e.to_string())?
                .to_str()
                .map(String::from)
                .ok_or("non-UTF8 project candidate path".into())
        };
        let candidates = BTreeMap::from([
            (
                relative(&self.project.requested)?,
                candidate.bytes().to_vec(),
            ),
            (relative(&self.path)?, self.bytes.to_vec()),
        ]);
        let observed = audit.observe_candidate_files(&candidates)?;
        let source = observed.synchronization_source()?;
        let (source_delta, erc_delta) = source::verify_defined_source_classes(
            &self.source,
            &source,
            self.project
                .bytes
                .as_deref()
                .ok_or("missing original project settings")?,
            actions,
        )?;
        let board_path = observed.inner.policy.project.join(relative(&self.path)?);
        let observed = source.observe_board(&board_path)?;
        let after = observed.observe_project_netclasses()?;
        // Project rules cannot authorize any change to the board or its native serialization.
        if self.bytes != observed.bytes
            || self.native.board.text() != observed.native.board.text()
            || self.native.report.native_version != observed.native.report.native_version
            || self.native.report.board_sha256 != observed.native.report.board_sha256
            || self.native.report.native_saved_sha256 != observed.native.report.native_saved_sha256
            || self.native.report.footprints != observed.native.report.footprints
            || self.native.report.pads != observed.native.report.pads
            || self.native.report.copper != observed.native.report.copper
            || self.connectivity.endpoints() != observed.connectivity.endpoints()
            || self.connectivity.components() != observed.connectivity.components()
            || observed.project.bytes.as_deref() != Some(candidate.bytes())
            || self.rules.bytes != observed.rules.bytes
        {
            return Err(
                "project candidate changes protected PCB/native geometry/connectivity/rules".into(),
            );
        }
        let mut delta = ProjectNetclassDelta::between(&before, &after);
        delta.source_nets = source_delta;
        let drc_delta = self
            .drc
            .report
            .compare_complete(&observed.drc.report)
            .map_err(|e| e.to_string())?;
        self.verify()?;
        observed.verify()?;
        let evidence = serde_json::json!({
            "schema_version": 1,
            "project_worker_sha256": hash(native::PROJECT_WORKER.as_bytes()),
            "observation_digest": self.digest,
            "source_digest": self.source.digest(),
            "candidate_source_digest": observed.source.digest(),
            "before_project_sha256": candidate.before_sha256(),
            "candidate_project_sha256": candidate.after_sha256(),
            "has_changes": candidate.has_changes(),
            "project_differences": candidate.differences(),
            "project_actions": actions,
            "netclasses_before": before,
            "netclasses_after": after,
            "netclass_delta": delta, "erc_delta": erc_delta,
            "erc_before": audit.erc(), "erc_after": observed.source.audit.erc(),
            "native_before": self.native.report,
            "native_after": observed.native.report,
            "drc_before": self.drc.evidence,
            "drc_after": observed.drc.evidence,
            "drc_delta": drc_delta,
            "connectivity_before": self.connectivity,
            "connectivity_after": observed.connectivity,
            "source_netclasses_before": source::source_netclasses(audit.netlist())?,
            "source_netclasses_after": source::source_netclasses(observed.source.audit.netlist())?,
            "source_context_before": audit.evidence()?,
            "source_context_after": observed.source.audit.evidence()?,
            "publication_authorized": false,
        });
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let preview = Arc::new(SynchronizationProjectPreview {
            observation: self.clone(),
            candidate,
            actions: actions.to_vec(),
            before,
            after,
            observed,
            delta,
            drc_delta,
            erc_delta,
            evidence,
            digest,
            valid: AtomicBool::new(true),
        });
        preview.verify()?;
        Ok(preview)
    }
}

impl SynchronizationProjectPreview {
    pub fn candidate(&self) -> &ProjectNetclassCandidate {
        &self.candidate
    }
    pub fn erc_delta(&self) -> &kicad_checks::erc::ErcDelta {
        &self.erc_delta
    }
    pub(super) fn match_erc_expectation(
        &self,
        expected: Option<&crate::schematic_audit::preview::ErcExpectation>,
    ) -> Result<(), String> {
        crate::schematic_audit::preview::match_erc(
            &self.erc_delta,
            expected.unwrap_or(&Default::default()),
        )?;
        Ok(())
    }
    pub fn netclasses_before(&self) -> &ProjectNetclassSnapshot {
        &self.before
    }
    pub fn netclasses_after(&self) -> &ProjectNetclassSnapshot {
        &self.after
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
    pub fn match_netclass_expectation(
        &self,
        expected: &ProjectNetclassDelta,
    ) -> Result<(), String> {
        self.verify()?;
        self.delta.match_expectation(expected)
    }
    pub fn match_drc_expectation(
        &self,
        expected: &kicad_checks::CompleteDrcExpectation,
    ) -> Result<(), String> {
        self.verify()?;
        self.drc_delta
            .match_expectation(expected)
            .map_err(|e| e.to_string())
    }
    /// Observed drift permanently invalidates this preview, even after byte restoration.
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("project preview was invalidated".into());
        }
        let result = (|| {
            self.observation.verify()?;
            self.observed.verify()?;
            if self.observed.project.bytes.as_deref() != Some(self.candidate.bytes()) {
                return Err("project preview differs from compiled candidate".into());
            }
            Ok(())
        })();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_record_deltas_preserve_membership_values_and_exact_expectations() {
        let before = BTreeMap::from([
            ("retained".into(), 1),
            ("removed".into(), 2),
            ("changed".into(), 3),
        ]);
        let after = BTreeMap::from([
            ("retained".into(), 1),
            ("added".into(), 4),
            ("changed".into(), 5),
        ]);
        let changes = changed_records(&before, &after);
        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes["removed"],
            NetclassRecordChange {
                before: Some(2),
                after: None
            }
        );
        assert_eq!(
            changes["added"],
            NetclassRecordChange {
                before: None,
                after: Some(4)
            }
        );
        assert_eq!(
            changes["changed"],
            NetclassRecordChange {
                before: Some(3),
                after: Some(5)
            }
        );
        let delta = ProjectNetclassDelta::default();
        delta.match_expectation(&delta).unwrap();
        let mut foreign = delta.clone();
        foreign.configured.insert(
            "foreign".into(),
            NetclassRecordChange {
                before: None,
                after: None,
            },
        );
        assert!(delta.match_expectation(&foreign).is_err());
    }
}
