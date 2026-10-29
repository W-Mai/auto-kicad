//! Actual-source native publication. Public session and human approval policy belongs to MCP orchestration.
use super::*;
use kicad_adapter::schematic_audit::MatchedSchematicPreview;
use std::collections::BTreeSet;

#[derive(Debug, Serialize)]
pub struct NativeSchematicPublication {
    pub attempt_id: String,
    pub matched_preview_digest: String,
    pub native_verification_digest: String,
    pub candidate_artifacts: kicad_model::SchematicArtifactRevision,
}

#[derive(Debug)]
pub struct SchematicPublicationFailure {
    pub unknown: bool,
    pub attempt_id: Option<String>,
    pub reason: String,
}
impl std::fmt::Display for SchematicPublicationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.reason.fmt(formatter)
    }
}
impl std::error::Error for SchematicPublicationFailure {}

fn rejected(reason: impl ToString) -> SchematicPublicationFailure {
    SchematicPublicationFailure {
        unknown: false,
        attempt_id: None,
        reason: reason.to_string(),
    }
}

impl ProjectWriter {
    /// Publish exact matched existing-source edits, then validate the actual native result before completion.
    pub fn publish_matched_schematic(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: &MatchedSchematicPreview,
    ) -> Result<NativeSchematicPublication, SchematicPublicationFailure> {
        self.publish_matched_internal(plan_hash, approval_hash, matched, || Ok(()))
    }

    fn publish_matched_internal(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        matched: &MatchedSchematicPreview,
        after_write: impl FnOnce() -> io::Result<()>,
    ) -> Result<NativeSchematicPublication, SchematicPublicationFailure> {
        self.ensure_ready().map_err(rejected)?;
        let check = matched.prepare_publication().map_err(rejected)?;
        if check.project_dir() != self.project {
            return Err(rejected("publication writer belongs to another project"));
        }
        let writes = check
            .writes()
            .map_err(rejected)?
            .into_iter()
            .map(|write| SchematicWrite {
                original: write.original,
                before: SourceExistence::Present {
                    sha256: write.before_sha256,
                },
                bytes: write.bytes.to_vec(),
            })
            .collect::<Vec<_>>();
        let preview_bytes = serde_json::to_vec(check.preview_evidence()).map_err(rejected)?;
        let matching_bytes = serde_json::to_vec(check.matching_evidence()).map_err(rejected)?;
        let budget = preview_bytes.len()
            + matching_bytes.len()
            + writes.iter().map(|write| write.bytes.len()).sum::<usize>();
        if budget > 256 * 1024 * 1024 {
            return Err(rejected(
                "publication artifact budget exceeded before preparation",
            ));
        }
        check.verify_before_write().map_err(rejected)?;
        let prior = read_records(&self.records, &self.project, usize::MAX)
            .map_err(rejected)?
            .into_iter()
            .map(|record| record.attempt_id)
            .collect::<BTreeSet<_>>();
        let evidence = serde_json::json!({"matched_preview_digest":check.matched_digest(),
            "native_preview_digest":check.preview_digest(),"candidate_artifacts":check.candidate_revision(),"artifacts":[]});
        let mut publication =
            match self.prepare_schematic_publication(plan_hash, approval_hash, evidence, &writes) {
                Ok(publication) => publication,
                Err(error) => {
                    let current = read_records(&self.records, &self.project, usize::MAX);
                    let id = current
                        .as_ref()
                        .ok()
                        .and_then(|records| {
                            records
                                .iter()
                                .find(|record| !prior.contains(&record.attempt_id))
                        })
                        .map(|record| record.attempt_id.clone());
                    return Err(SchematicPublicationFailure {
                        unknown: id.is_some() || current.is_err() || self.ensure_ready().is_err(),
                        attempt_id: id,
                        reason: error.to_string(),
                    });
                }
            };
        let id = publication.attempt_id().to_owned();
        let result = (|| -> Result<_, Box<dyn std::error::Error>> {
            publication.retain_artifact("preview.json", &preview_bytes)?;
            publication.retain_artifact("matching.json", &matching_bytes)?;
            for (index, write) in writes.iter().enumerate() {
                publication
                    .retain_artifact(&format!("candidate-{index}.kicad_sch"), &write.bytes)?;
            }
            check.verify_before_write().map_err(io::Error::other)?;
            while publication.publish_next()? {}
            publication.verify_complete()?;
            after_write()?;
            let proof = check.verify_published().map_err(io::Error::other)?;
            proof.verify().map_err(io::Error::other)?;
            publication
                .retain_artifact("verification.json", &serde_json::to_vec(proof.evidence())?)?;
            publication.verify_complete()?;
            Ok(proof)
        })();
        let proof = match result {
            Ok(proof) => proof,
            Err(error) => {
                let reason = error.to_string();
                let _ = publication
                    .record_unknown(serde_json::json!({"kind":"unknown","reason":reason}));
                return Err(SchematicPublicationFailure {
                    unknown: true,
                    attempt_id: Some(id),
                    reason,
                });
            }
        };
        let receipt = NativeSchematicPublication {
            attempt_id: id.clone(),
            matched_preview_digest: proof.matched_digest().into(),
            native_verification_digest: proof.digest().into(),
            candidate_artifacts: proof.revision().clone(),
        };
        publication
            .finish_verified(&proof)
            .map_err(|error| SchematicPublicationFailure {
                unknown: true,
                attempt_id: Some(id),
                reason: error.to_string(),
            })?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_adapter::schematic_audit::{
        SchematicAudit, SchematicAuditPolicy, SchematicEditExpectation,
    };
    use kicad_schematic::editing::{EditAction, EditScope, SchematicEdit, SymbolProperty};

    #[test]
    #[ignore = "requires an explicitly owned ECC83/configuration copy and installed KiCad CLI"]
    fn native_final_failures_remain_unknown_and_preserve_later_user_content() {
        let source = fs::canonicalize(
            std::env::var_os("AUTO_KICAD_NATIVE_PUBLICATION_SOURCE")
                .expect("owned schematic source"),
        )
        .unwrap();
        let config = fs::canonicalize(
            std::env::var_os("AUTO_KICAD_NATIVE_PUBLICATION_CONFIG").expect("owned configuration"),
        )
        .unwrap();
        let output = PathBuf::from(
            std::env::var_os("AUTO_KICAD_NATIVE_PUBLICATION_OUTPUT").expect("new output directory"),
        );
        fs::create_dir(&output).unwrap();
        let project = source.parent().unwrap();
        for root in [project, &config] {
            assert_eq!(
                fs::read_to_string(root.join(".auto-kicad-audit-owned")).unwrap(),
                "schematic-audit-acceptance\n"
            );
        }
        let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
        let policy = SchematicAuditPolicy::new(
            project,
            &source,
            &config,
            &shared.join("symbols"),
            &shared.join("footprints"),
            &[],
        )
        .unwrap();
        let configuration = config.join("10.0/kicad_common.json");
        let original_source = fs::read(&source).unwrap();
        let original_config = fs::read(&configuration).unwrap();
        let mut cases = Vec::new();
        for mode in [
            "lost-final-check",
            "configuration-drift",
            "user-source-edit",
        ] {
            let store = ExecutionStore::open(&output.join(mode)).unwrap();
            let writer = store.acquire(project).unwrap();
            let audit = SchematicAudit::prepare(&policy).unwrap();
            let target = audit
                .identities()
                .units()
                .iter()
                .find(|unit| unit.reference == "R2")
                .unwrap();
            let preview = audit
                .preview_edits(&[SchematicEdit {
                    file: target.file.clone(),
                    scope: EditScope::FileAllInstances,
                    action: EditAction::SetProperty {
                        uuid: target.source_uuid.clone(),
                        property: SymbolProperty::Value,
                        value: "AK_FINAL_FAILURE".into(),
                    },
                }])
                .unwrap();
            let candidate =
                preview.candidate().files()[source.file_name().unwrap().to_str().unwrap()].clone();
            let matched = preview
                .match_expectations(&SchematicEditExpectation::default())
                .unwrap();
            let error = writer
                .publish_matched_internal("plan", "approval", &matched, || match mode {
                    "lost-final-check" => Err(invalid("final verification acknowledgement lost")),
                    "configuration-drift" => {
                        let mut bytes = original_config.clone();
                        bytes.extend_from_slice(b"\n");
                        fs::write(&configuration, bytes)
                    }
                    _ => fs::write(&source, b"later-user-content"),
                })
                .unwrap_err();
            assert!(error.unknown);
            let id = error.attempt_id.unwrap();
            assert!(writer.ensure_ready().is_err());
            assert!(matched.verify().is_err());
            assert!(matched.prepare_publication().is_err());
            drop(writer);
            if mode == "user-source-edit" {
                assert!(store.plan_recovery(project, &id).is_err());
                assert_eq!(fs::read(&source).unwrap(), b"later-user-content");
                // Owned control restores the exact candidate before requesting guarded recovery.
                fs::write(&source, &candidate).unwrap();
            } else {
                assert!(
                    fs::read(&source).unwrap() == candidate,
                    "source differs from candidate after uncertain publication"
                );
            }
            fs::write(&configuration, &original_config).unwrap();
            let plan = store.plan_recovery(project, &id).unwrap();
            let approval = store
                .preview_recovery(project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            store
                .commit_recovery(project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap();
            assert!(
                fs::read(&source).unwrap() == original_source,
                "recovery did not restore original bytes"
            );
            let restored = SchematicAudit::prepare(&policy).unwrap();
            assert_eq!(restored.netlist(), audit.netlist());
            assert!(restored.erc().equivalent_to(audit.erc()).unwrap());
            assert_eq!(
                restored.observe_pin_connectivity().unwrap().connectivity(),
                audit.observe_pin_connectivity().unwrap().connectivity()
            );
            let private = [
                audit.private_root().to_owned(),
                preview.observed().private_root().to_owned(),
                restored.private_root().to_owned(),
            ];
            drop(restored);
            drop(matched);
            drop(preview);
            drop(audit);
            assert!(private.iter().all(|path| !path.exists()));
            cases.push(serde_json::json!({"mode":mode,"unknown":true,"native_baseline_restored":true,"private_snapshots_removed":true}));
        }
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"passed":true,"cases":cases,"gui_launched":false}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
