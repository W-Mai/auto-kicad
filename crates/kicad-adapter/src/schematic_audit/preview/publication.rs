//! One-way source publication checks with complete actual-source native validation.
use super::*;
use crate::schematic_audit::source_transition::verify_candidate_inputs;
use kicad_model::{RevisionDigest, SchematicArtifactRevision};

/// Exact existing/new-source changes from a compiled and matched candidate.
pub struct SchematicPublicationWrite<'a> {
    pub original: PathBuf,
    pub before_sha256: Option<RevisionDigest>,
    pub bytes: &'a [u8],
}

/// Non-cloneable transition from matched preview to actual source verification.
pub struct SchematicPublicationCheck {
    matched: MatchedSchematicPreview,
}

/// Constructible only by complete native validation of the actual published sources.
pub struct VerifiedSchematicPublication {
    check: SchematicPublicationCheck,
    observed: SchematicAudit,
    evidence: Value,
    digest: String,
}

impl MatchedSchematicPreview {
    pub fn prepare_publication(&self) -> Result<SchematicPublicationCheck, String> {
        self.verify()?;
        self.preview()
            .state
            .compare_exchange(READY, PUBLISHING, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "schematic preview was consumed or invalidated")?;
        let check = SchematicPublicationCheck {
            matched: self.clone(),
        };
        check.verify_before_write()?;
        Ok(check)
    }
}

impl SchematicPublicationCheck {
    fn preview(&self) -> &SchematicEditPreview {
        self.matched.preview()
    }
    pub fn project_dir(&self) -> &Path {
        &self.preview().source.inner.policy.project
    }
    pub fn matched_digest(&self) -> &str {
        self.matched.digest()
    }
    pub fn preview_digest(&self) -> &str {
        self.preview().digest()
    }
    pub fn candidate_revision(&self) -> &SchematicArtifactRevision {
        &self.preview().candidate.parsed().revision
    }
    pub fn preview_evidence(&self) -> &Value {
        self.preview().evidence()
    }
    pub fn matching_evidence(&self) -> &Value {
        self.matched.evidence()
    }

    fn checked(&self, verify: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        if self.preview().state.load(Ordering::Acquire) != PUBLISHING {
            return Err("schematic publication check was invalidated".into());
        }
        let result = verify();
        if result.is_err() {
            self.preview().state.store(INVALID, Ordering::Release);
        }
        result
    }

    fn verify_catalogs(&self) -> Result<(), String> {
        let edits = self
            .preview()
            .candidate
            .changes()
            .iter()
            .map(|change| SchematicEdit {
                file: change.file.clone(),
                scope: kicad_schematic::editing::EditScope::FileAllInstances,
                action: change.action.clone(),
            })
            .collect::<Vec<_>>();
        verify_edit_sources(&self.preview().source, &edits)
    }

    pub fn verify_before_write(&self) -> Result<(), String> {
        self.checked(|| {
            self.preview().source.verify()?;
            self.preview().additional.verify()?;
            self.preview().observed.verify()?;
            self.verify_catalogs()
        })
    }

    pub fn writes(&self) -> Result<Vec<SchematicPublicationWrite<'_>>, String> {
        // Publish created children before references to them; per-file atomicity still applies.
        self.preview()
            .candidate
            .created_files()
            .iter()
            .chain(self.preview().candidate.replacements().keys())
            .map(|relative| {
                let original = self.project_dir().join(relative);
                let input = self
                    .preview()
                    .source
                    .inner
                    .source
                    .files
                    .get(&original)
                    .or_else(|| self.preview().additional.inputs.files.get(&original))
                    .ok_or("publication source was not captured")?;
                let before_sha256 = input
                    .bytes
                    .as_deref()
                    .map(|bytes| {
                        RevisionDigest::new(hash(bytes)).map_err(|error| error.to_string())
                    })
                    .transpose()?;
                if before_sha256.is_none()
                    != self.preview().candidate.created_files().contains(relative)
                {
                    return Err("source existence differs from compiled creation intent".into());
                }
                Ok(SchematicPublicationWrite {
                    original,
                    before_sha256,
                    bytes: &self.preview().candidate.files()[relative],
                })
            })
            .collect()
    }

    fn verify_published_inputs(&self) -> Result<(), String> {
        self.checked(|| {
            let preview = self.preview();
            preview.additional.verify_directories()?;
            verify_candidate_inputs(
                &[&preview.source.inner.source, &preview.additional.inputs],
                self.project_dir(),
                preview.candidate.files(),
                preview.candidate.created_files(),
            )?;
            preview.source.verify_context()?;
            preview
                .source
                .verify_hierarchy(preview.candidate.parsed())?;
            preview.observed.verify()?;
            self.verify_catalogs()
        })
    }

    /// Observe actual published files with the original fixed resource policy and exact candidate expectations.
    pub fn verify_published(self) -> Result<VerifiedSchematicPublication, String> {
        let result = (|| {
            self.verify_published_inputs()?;
            let observed = SchematicAudit::prepare(&self.preview().source.inner.policy)?;
            if observed.inner.parsed != *self.preview().candidate.parsed()
                || observed.netlist() != self.preview().observed.netlist()
                || !observed
                    .erc()
                    .equivalent_to(self.preview().observed.erc())
                    .map_err(|error| error.to_string())?
                || observed.observe_pin_connectivity()?.connectivity()
                    != self.preview().connectivity_after()
            {
                return Err("published native hierarchy/netlist/ERC/connectivity differs from matched candidate".into());
            }
            observed.verify()?;
            self.verify_published_inputs()?;
            Ok(observed)
        })();
        let observed = match result {
            Ok(observed) => observed,
            Err(error) => {
                self.preview().state.store(INVALID, Ordering::Release);
                return Err(error);
            }
        };
        let evidence = serde_json::json!({
            "schema_version": 1, "matched_preview_digest": self.matched_digest(),
            "native_preview_digest": self.preview_digest(), "candidate_artifacts": self.candidate_revision(),
            "published_context": observed.evidence()?,
            "published_native": format!("{:?}", observed.netlist()),
            "published_connectivity": connectivity_json(self.preview().connectivity_after()),
            "complete_native_candidate_matched": true, "protected_inputs_unchanged": true,
            "human_approval_granted": false,
        });
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|error| error.to_string())?);
        let proof = VerifiedSchematicPublication {
            check: self,
            observed,
            evidence,
            digest,
        };
        proof.verify()?;
        Ok(proof)
    }
}

impl VerifiedSchematicPublication {
    pub fn verify(&self) -> Result<(), String> {
        self.check.checked(|| {
            self.observed.verify()?;
            self.check.verify_published_inputs()
        })
    }
    pub fn project_dir(&self) -> &Path {
        self.check.project_dir()
    }
    pub fn matched_digest(&self) -> &str {
        self.check.matched_digest()
    }
    pub fn revision(&self) -> &SchematicArtifactRevision {
        self.check.candidate_revision()
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn private_root(&self) -> &Path {
        self.observed.private_root()
    }
}
