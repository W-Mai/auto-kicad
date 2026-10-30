//! One-way source publication checks with complete actual-source native validation.
use super::*;
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

fn verify_candidate_inputs(
    captures: &[&Inputs],
    project: &Path,
    candidates: &BTreeMap<String, Vec<u8>>,
    created: &BTreeSet<String>,
) -> Result<(), String> {
    if created.iter().any(|file| !candidates.contains_key(file)) {
        return Err("creation set differs from candidate files".into());
    }
    let mut inputs = BTreeMap::new();
    for capture in captures {
        for (path, input) in &capture.files {
            if let Some(old) = inputs.insert(path, input)
                && (old.canonical != input.canonical
                    || old.digest != input.digest
                    || old.size != input.size)
            {
                return Err("conflicting captured source authority".into());
            }
        }
    }
    for relative in candidates.keys() {
        let input = inputs
            .get(&project.join(relative))
            .ok_or("candidate source was not captured")?;
        if input.bytes.is_none() != created.contains(relative) {
            return Err("candidate existence differs from captured authority".into());
        }
    }
    for (path, input) in inputs {
        let actual = Inputs::read(path)?;
        let relative = path.strip_prefix(project).ok().and_then(Path::to_str);
        let candidate = relative.and_then(|relative| candidates.get(relative));
        let is_created = relative.is_some_and(|relative| created.contains(relative));
        if if is_created {
            actual.canonical.as_ref() != Some(path)
                || fs::symlink_metadata(path)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_symlink()
        } else {
            actual.canonical != input.canonical
        } {
            return Err("published source/context path changed".into());
        }
        if let Some(bytes) = candidate {
            if actual.bytes.as_ref() != Some(bytes) {
                return Err("published source differs from compiled candidate".into());
            }
        } else if actual.digest != input.digest || actual.size != input.size {
            return Err(format!(
                "protected publication input changed: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_transition_preserves_every_other_bound_input_and_path_identity() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        let source = root.join("root.kicad_sch");
        let child = root.join("child.kicad_sch");
        let config = root.join("settings.json");
        let absent = root.join("optional.json");
        fs::write(&source, b"before").unwrap();
        fs::write(&child, b"child").unwrap();
        fs::write(&config, b"configuration").unwrap();
        let mut inputs = Inputs::default();
        for path in [&source, &child, &config, &absent] {
            inputs.track(path).unwrap();
        }
        let mut candidate = BTreeMap::from([("root.kicad_sch".into(), b"candidate".to_vec())]);
        assert!(verify_candidate_inputs(&[&inputs], &root, &candidate, &BTreeSet::new()).is_err());
        fs::write(&source, b"candidate").unwrap();
        verify_candidate_inputs(&[&inputs], &root, &candidate, &BTreeSet::new()).unwrap();
        for path in [&child, &config, &absent] {
            let original = fs::read(path).ok();
            fs::write(path, b"unexpected").unwrap();
            assert!(
                verify_candidate_inputs(&[&inputs], &root, &candidate, &BTreeSet::new()).is_err()
            );
            if let Some(bytes) = original {
                fs::write(path, bytes).unwrap();
            } else {
                fs::remove_file(path).unwrap();
            }
        }
        candidate.insert("new.kicad_sch".into(), b"new".to_vec());
        assert!(verify_candidate_inputs(&[&inputs], &root, &candidate, &BTreeSet::new()).is_err());
        candidate.remove("new.kicad_sch");
        #[cfg(unix)]
        {
            fs::rename(&source, root.join("redirected")).unwrap();
            std::os::unix::fs::symlink(root.join("redirected"), &source).unwrap();
            assert!(
                verify_candidate_inputs(&[&inputs], &root, &candidate, &BTreeSet::new()).is_err()
            );
        }
    }

    #[test]
    fn new_source_transition_requires_captured_absence_and_exact_creation_intent() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        let target = root.join("child.kicad_sch");
        let mut inputs = Inputs::default();
        inputs.track(&target).unwrap();
        let candidates = BTreeMap::from([("child.kicad_sch".into(), b"candidate".to_vec())]);
        let created = BTreeSet::from(["child.kicad_sch".into()]);
        assert!(verify_candidate_inputs(&[&inputs], &root, &candidates, &created).is_err());
        fs::write(&target, b"candidate").unwrap();
        verify_candidate_inputs(&[&inputs], &root, &candidates, &created).unwrap();
        assert!(verify_candidate_inputs(&[&inputs], &root, &candidates, &BTreeSet::new()).is_err());
        fs::write(&target, b"later-user-bytes").unwrap();
        assert!(verify_candidate_inputs(&[&inputs], &root, &candidates, &created).is_err());
        #[cfg(unix)]
        {
            let redirected = root.join("redirected.kicad_sch");
            fs::write(&redirected, b"candidate").unwrap();
            fs::remove_file(&target).unwrap();
            std::os::unix::fs::symlink(&redirected, &target).unwrap();
            assert!(verify_candidate_inputs(&[&inputs], &root, &candidates, &created).is_err());
        }
    }
}
