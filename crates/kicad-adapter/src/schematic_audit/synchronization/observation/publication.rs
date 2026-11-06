//! Sealed exact matching and separate original/published PCB verification phases.
use super::*;
use serde::{Deserialize, Serialize};
mod recovery;
pub use recovery::{SynchronizationRecoveryBaseline, VerifiedSynchronizationRestoration};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationExpectation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regions: Option<crate::live_geometry::ClosedRefillDelta>,
    pub differences: Vec<board::SynchronizationDifference>,
    pub drc: kicad_checks::CompleteDrcExpectation,
}

pub struct MatchedSynchronizationPreview {
    preview: Arc<SynchronizationBoardPreview>,
    expectation: SynchronizationExpectation,
    digest: String,
}
pub struct PreparedSynchronizationPublication {
    matched: MatchedSynchronizationPreview,
}
pub struct VerifiedSynchronizationPublication {
    actual: Arc<SynchronizationBoardObservation>,
    candidate_sha256: String,
    matched_digest: String,
    digest: String,
    evidence: Value,
}

pub(super) fn canonical_differences(
    rows: &[board::SynchronizationDifference],
) -> Result<Vec<board::SynchronizationDifference>, String> {
    if rows.len() > 100_000 {
        return Err("synchronization expectation exceeds difference count bound".into());
    }
    let mut bytes = 0usize;
    for row in rows {
        for value in [
            Some(row.uuid.as_str()),
            Some(row.field.as_str()),
            row.before.as_deref(),
            row.after.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            bytes = bytes
                .checked_add(value.len())
                .ok_or("synchronization expectation byte overflow")?;
            if bytes > 16 * 1024 * 1024 {
                return Err("synchronization expectation exceeds byte bound".into());
            }
        }
    }
    let mut rows = rows.to_vec();
    rows.sort_by(|a, b| (&a.uuid, &a.field).cmp(&(&b.uuid, &b.field)));
    if rows
        .windows(2)
        .any(|pair| pair[0].uuid == pair[1].uuid && pair[0].field == pair[1].field)
    {
        return Err("duplicate synchronization difference identity/field".into());
    }
    Ok(rows)
}

impl SynchronizationBoardPreview {
    /// Match every exact source/DRC difference without granting human approval or a file write.
    pub fn match_expectation(
        self: &Arc<Self>,
        mut expectation: SynchronizationExpectation,
    ) -> Result<MatchedSynchronizationPreview, String> {
        if self.project_transition_pending {
            return Err("composed project candidate cannot use legacy board-only approval".into());
        }
        expectation.differences = canonical_differences(&expectation.differences)?;
        if expectation.regions.as_ref() != self.candidate.region_delta() {
            return Err("synchronization_region_expectation_mismatch".into());
        }
        if expectation.differences != canonical_differences(self.candidate.differences())? {
            return Err("synchronization_diff_expectation_mismatch".into());
        }
        self.match_drc_expectation(&expectation.drc)?;
        let digest = hash(
            &serde_json::to_vec(
                &serde_json::json!({"preview":self.digest,"expectation":expectation}),
            )
            .map_err(|e| e.to_string())?,
        );
        Ok(MatchedSynchronizationPreview {
            preview: self.clone(),
            expectation,
            digest,
        })
    }
}

impl MatchedSynchronizationPreview {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn preview(&self) -> &Arc<SynchronizationBoardPreview> {
        &self.preview
    }
    pub fn expectation(&self) -> &SynchronizationExpectation {
        &self.expectation
    }
    /// Freeze the validated original phase. The durable writer still owns approval and publication.
    pub fn prepare_publication(self) -> Result<PreparedSynchronizationPublication, String> {
        self.preview.verify()?;
        if !self.preview.candidate.has_changes() {
            return Err("synchronization_has_no_changes".into());
        }
        Ok(PreparedSynchronizationPublication { matched: self })
    }
}

impl PreparedSynchronizationPublication {
    pub fn native_region_artifacts(&self) -> Result<Vec<(&'static str, &[u8])>, String> {
        match self.matched.preview.region_refill.as_ref() {
            Some(frame) => frame.native_artifacts(),
            None => Ok(Vec::new()),
        }
    }

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
    pub fn board_path(&self) -> &Path {
        &self.matched.preview.observation.path
    }
    pub fn original_bytes(&self) -> &[u8] {
        &self.matched.preview.observation.bytes
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
    pub fn expectation(&self) -> &SynchronizationExpectation {
        self.matched.expectation()
    }
    pub fn verify_before_write(&self) -> Result<(), String> {
        self.matched.preview.verify()
    }

    /// Reobserve the actual saved candidate; never use the old PCB-before observation after replacement.
    pub fn verify_published(&self) -> Result<VerifiedSynchronizationPublication, String> {
        let preview = &self.matched.preview;
        preview.verify_private()?;
        if fs::canonicalize(self.board_path()).map_err(|e| e.to_string())? != self.board_path()
            || native::bounded_read(self.board_path(), 128 * 1024 * 1024)? != self.candidate_bytes()
        {
            return Err(
                "actual synchronization PCB differs from the exact approved candidate".into(),
            );
        }
        let source = &preview.observation.source;
        let actual = if preview.region_after.is_some() {
            source.observe_region_board(self.board_path())?
        } else {
            source.observe_board(self.board_path())?
        };
        if match (&preview.region_after, &actual.regions) {
            (Some(before), Some(after)) => !before.same_native(after),
            (None, None) => false,
            _ => true,
        } {
            return Err("published native region ownership differs from approved preview".into());
        }
        if !actual
            .native
            .same_report(&preview.native, preview.observation.regions.is_some())?
            || actual.connectivity != preview.connectivity
        {
            return Err("actual synchronization native board/serialization/connectivity differs from the matched preview".into());
        }
        actual.drc.require_capture(&preview.drc)?;
        preview.verify_private()?;
        actual.verify()?;
        let candidate_sha256 = hash(self.candidate_bytes());
        let evidence = serde_json::json!({"schema_version":1,"project":self.project_dir(),"board":self.board_path(),
            "candidate_sha256":candidate_sha256,"matched_preview_digest":self.matched.digest(),
            "source_digest":actual.source.digest(),"actual_observation_digest":actual.digest,
            "native":actual.native.report,"connectivity":actual.connectivity,"drc":actual.drc.evidence,
            "actual_source_native_verified":true,"human_approval_granted":false});
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        Ok(VerifiedSynchronizationPublication {
            actual,
            candidate_sha256,
            matched_digest: self.matched.digest.clone(),
            digest,
            evidence,
        })
    }
}

impl VerifiedSynchronizationPublication {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn matched_digest(&self) -> &str {
        &self.matched_digest
    }
    pub fn board_path(&self) -> &Path {
        &self.actual.path
    }
    pub fn project_dir(&self) -> &Path {
        &self.actual.source.audit.inner.policy.project
    }
    pub fn candidate_sha256(&self) -> &str {
        &self.candidate_sha256
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn verify(&self) -> Result<(), String> {
        self.actual.verify()?;
        if self.actual.board.digest() != self.candidate_sha256 {
            return Err("published synchronization candidate changed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_difference_matching_preserves_values_and_rejects_duplicates() {
        let a = board::SynchronizationDifference {
            uuid: "a".into(),
            field: "net".into(),
            before: Some("before".into()),
            after: Some("after".into()),
        };
        let b = board::SynchronizationDifference {
            uuid: "b".into(),
            ..a.clone()
        };
        assert_eq!(
            canonical_differences(&[b.clone(), a.clone()]).unwrap(),
            vec![a.clone(), b]
        );
        assert!(canonical_differences(&[a.clone(), a.clone()]).is_err());
        let changed = board::SynchronizationDifference {
            after: Some("different".into()),
            ..a.clone()
        };
        assert_ne!(
            canonical_differences(&[a]).unwrap(),
            canonical_differences(&[changed]).unwrap()
        );
    }
}
