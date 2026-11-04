//! Exact captured-input comparison at each server-owned publication prefix.
use super::*;

pub(super) fn verify_candidate_inputs(
    captures: &[&Inputs],
    project: &Path,
    candidates: &BTreeMap<String, Vec<u8>>,
    created: &BTreeSet<String>,
) -> Result<(), String> {
    let order = candidates.keys().cloned().collect::<Vec<_>>();
    verify_candidate_inputs_at_prefix(captures, project, candidates, created, &order, order.len())
}

/// Candidate membership and order originate inside the server; this does not grant a write.
pub(super) fn verify_candidate_inputs_at_prefix(
    captures: &[&Inputs],
    project: &Path,
    candidates: &BTreeMap<String, Vec<u8>>,
    created: &BTreeSet<String>,
    order: &[String],
    published: usize,
) -> Result<(), String> {
    let selected = order.iter().collect::<BTreeSet<_>>();
    if published > order.len()
        || selected.len() != order.len()
        || selected != candidates.keys().collect()
        || created.iter().any(|file| !candidates.contains_key(file))
        || candidates.keys().any(|file| {
            file.is_empty()
                || !Path::new(file)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
        })
    {
        return Err("publication prefix differs from exact candidate membership/order".into());
    }
    let applied = order[..published].iter().collect::<BTreeSet<_>>();
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
        let candidate = relative
            .filter(|relative| {
                applied
                    .iter()
                    .any(|selected| selected.as_str() == *relative)
            })
            .and_then(|relative| candidates.get(relative));
        let is_created =
            candidate.is_some() && relative.is_some_and(|relative| created.contains(relative));
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
mod prefix_tests {
    use super::*;
    #[test]
    fn every_prefix_requires_exact_candidates_and_preserves_all_unselected_inputs() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        let order = vec!["root.kicad_pro".to_owned(), "root.kicad_pcb".to_owned()];
        let candidates = BTreeMap::from([
            (order[0].clone(), b"project-after".to_vec()),
            (order[1].clone(), b"board-after".to_vec()),
        ]);
        let protected = root.join("root.kicad_sch");
        fs::write(&protected, b"schematic").unwrap();
        let absent = root.join("root.kicad_dru");
        let mut inputs = Inputs::default();
        for file in &order {
            fs::write(root.join(file), b"before").unwrap();
            inputs.track(&root.join(file)).unwrap();
        }
        inputs.track(&protected).unwrap();
        inputs.track(&absent).unwrap();
        let verify = |prefix| {
            verify_candidate_inputs_at_prefix(
                &[&inputs],
                &root,
                &candidates,
                &BTreeSet::new(),
                &order,
                prefix,
            )
        };
        for actual_prefix in 0..=2 {
            for (index, file) in order.iter().enumerate() {
                fs::write(
                    root.join(file),
                    if index < actual_prefix {
                        &candidates[file][..]
                    } else {
                        b"before"
                    },
                )
                .unwrap();
            }
            for claimed_prefix in 0..=2 {
                assert_eq!(
                    verify(claimed_prefix).is_ok(),
                    actual_prefix == claimed_prefix
                );
            }
            for file in &order {
                let path = root.join(file);
                let original = fs::read(&path).unwrap();
                fs::write(&path, b"later-user-bytes").unwrap();
                assert!(verify(actual_prefix).is_err());
                fs::write(path, original).unwrap();
            }
            fs::write(&absent, b"unexpected-custom-rule").unwrap();
            assert!(verify(actual_prefix).is_err());
            fs::remove_file(&absent).unwrap();
            fs::write(&protected, b"unexpected-schematic").unwrap();
            assert!(verify(actual_prefix).is_err());
            fs::write(&protected, b"schematic").unwrap();
            verify(actual_prefix).unwrap();
        }
        assert!(verify(3).is_err());
        let invalid_order = vec![order[0].clone(), order[0].clone()];
        assert!(
            verify_candidate_inputs_at_prefix(
                &[&inputs],
                &root,
                &candidates,
                &BTreeSet::new(),
                &invalid_order,
                0
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod complete_candidate_tests {
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

/// Exact server-owned candidate sequence. This grants no file write or human approval.
pub(super) struct CapturedPublicationTransition {
    before: Inputs,
    project: PathBuf,
    candidates: BTreeMap<String, Vec<u8>>,
    order: Vec<String>,
}

/// Constructible only after the complete captured source state matches one exact prefix.
pub(super) struct VerifiedInputPrefix<'a> {
    transition: &'a CapturedPublicationTransition,
    published: usize,
}

impl CapturedPublicationTransition {
    pub(super) fn new(
        captures: &[&Inputs],
        project: &Path,
        candidates: BTreeMap<String, Vec<u8>>,
        order: Vec<String>,
    ) -> Result<Self, String> {
        canonical_dir(project)?;
        if !(1..=2).contains(&order.len())
            || Path::new(&order[0]).extension().and_then(|v| v.to_str()) != Some("kicad_pro")
            || (order.len() == 2
                && Path::new(&order[0]).with_extension("kicad_pcb") != Path::new(&order[1]))
        {
            return Err(
                "project publication requires an ordered same-basename project/PCB pair".into(),
            );
        }
        let mut before = Inputs::default();
        for capture in captures {
            for (path, input) in &capture.files {
                if let Some(previous) = before.files.get(path) {
                    if previous.canonical != input.canonical
                        || previous.digest != input.digest
                        || previous.size != input.size
                    {
                        return Err("conflicting project publication source captures".into());
                    }
                } else {
                    before.total = before
                        .total
                        .checked_add(input.size)
                        .ok_or("publication capture size overflow")?;
                    before.files.insert(path.clone(), input.clone());
                }
            }
        }
        let result = Self {
            before,
            project: project.into(),
            candidates,
            order,
        };
        result.verify_prefix(0)?;
        for relative in &result.order {
            let path = project.join(relative);
            let captured = result
                .before
                .files
                .get(&path)
                .ok_or("uncaptured publication destination")?;
            if captured.canonical.as_ref() != Some(&path)
                || captured.bytes.as_deref() == result.candidates.get(relative).map(Vec::as_slice)
                || !fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .file_type()
                    .is_file()
            {
                return Err(
                    "project publication requires changed bytes at canonical regular destinations"
                        .into(),
                );
            }
        }
        Ok(result)
    }
    pub(super) fn verify_prefix(
        &self,
        published: usize,
    ) -> Result<VerifiedInputPrefix<'_>, String> {
        let prefix = VerifiedInputPrefix {
            transition: self,
            published,
        };
        prefix.verify_inputs(&[])?;
        Ok(prefix)
    }
    pub(super) fn order(&self) -> &[String] {
        &self.order
    }
    pub(super) fn project(&self) -> &Path {
        &self.project
    }
    pub(super) fn original(&self, relative: &str) -> &[u8] {
        self.before.files[&self.project.join(relative)]
            .bytes
            .as_deref()
            .expect("captured existing publication destination")
    }
    pub(super) fn candidate(&self, relative: &str) -> &[u8] {
        &self.candidates[relative]
    }
}

impl VerifiedInputPrefix<'_> {
    pub(super) fn verify_inputs(&self, additional: &[&Inputs]) -> Result<(), String> {
        let mut captures = vec![&self.transition.before];
        captures.extend_from_slice(additional);
        verify_candidate_inputs_at_prefix(
            &captures,
            &self.transition.project,
            &self.transition.candidates,
            &BTreeSet::new(),
            &self.transition.order,
            self.published,
        )
    }
}

impl SchematicAudit {
    /// Validate exact mutable-file authority before checking immutable private/resources/context.
    pub(super) fn verify_publication_prefix(
        &self,
        prefix: &VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        prefix.verify_inputs(&[&self.inner.source])?;
        self.verify_context()?;
        self.verify_hierarchy(&self.inner.parsed)
    }
}

#[cfg(test)]
mod sealed_prefix_tests {
    use super::*;
    #[test]
    fn sealed_prefixes_recheck_extra_proof_inputs_and_reject_non_project_write_sequences() {
        let owner = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owner.path()).unwrap();
        let pro = root.join("root.kicad_pro");
        let pcb = root.join("root.kicad_pcb");
        let library = root.join("library.kicad_mod");
        for path in [&pro, &pcb, &library] {
            fs::write(path, b"before").unwrap();
        }
        let mut source = Inputs::default();
        source.track(&pro).unwrap();
        source.track(&pcb).unwrap();
        let mut proof = Inputs::default();
        proof.track(&pro).unwrap();
        proof.track(&library).unwrap();
        let candidates = BTreeMap::from([
            ("root.kicad_pro".into(), b"project".to_vec()),
            ("root.kicad_pcb".into(), b"board".to_vec()),
        ]);
        let order = vec!["root.kicad_pro".into(), "root.kicad_pcb".into()];
        assert!(
            CapturedPublicationTransition::new(
                &[&source],
                &root,
                candidates.clone(),
                vec!["root.kicad_pcb".into(), "root.kicad_pro".into()]
            )
            .is_err()
        );
        let transition =
            CapturedPublicationTransition::new(&[&source], &root, candidates, order).unwrap();
        transition
            .verify_prefix(0)
            .unwrap()
            .verify_inputs(&[&proof])
            .unwrap();
        fs::write(&pro, b"project").unwrap();
        let prefix = transition.verify_prefix(1).unwrap();
        prefix.verify_inputs(&[&proof]).unwrap();
        fs::write(&library, b"later-library").unwrap();
        assert!(prefix.verify_inputs(&[&proof]).is_err());
        fs::write(&library, b"before").unwrap();
        fs::write(&pcb, b"board").unwrap();
        assert!(prefix.verify_inputs(&[&proof]).is_err());
        transition
            .verify_prefix(2)
            .unwrap()
            .verify_inputs(&[&proof])
            .unwrap();
    }
}
