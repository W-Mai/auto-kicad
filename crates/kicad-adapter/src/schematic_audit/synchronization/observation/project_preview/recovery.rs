//! Restart reconstruction of exact project transitions; no write or approval authority.
use super::*;

mod source;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationProjectRecoveryBaseline {
    schema_version: u32,
    original: SynchronizationRecoveryBaseline,
    project: PathBuf,
    before_project_sha256: String,
    candidate_project_sha256: String,
    candidate_board_sha256: String,
    project_worker_sha256: String,
    resource_root: PathBuf,
    actions: Vec<ProjectNetclassChange>,
    allowed_nets: BTreeSet<String>,
    source_classes: BTreeSet<String>,
    netclasses: ProjectNetclassSnapshot,
    source: Value,
    erc_json: String,
}

pub struct VerifiedSynchronizationProjectRecovery {
    current: SchematicAudit,
    selected: Inputs,
    original: Arc<SynchronizationBoardObservation>,
    evidence: Value,
    digest: String,
    valid: AtomicBool,
}

fn scope(
    observation: &SynchronizationBoardObservation,
    classes: &ProjectNetclassSnapshot,
    declaration_aware: bool,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let netlist = observation.source.audit.netlist();
    let allowed = classes
        .effective()
        .keys()
        .cloned()
        .chain(netlist.nets.iter().map(|net| net.name.clone()))
        .filter(|name| !name.is_empty())
        .collect();
    let references = if declaration_aware {
        super::super::super::source_classes::references(
            &observation.source.audit,
            observation
                .project
                .bytes
                .as_deref()
                .ok_or("missing original project settings")?,
        )?
    } else {
        super::source::source_class_references(netlist, classes.configured())?
    };
    Ok((allowed, references))
}

fn source_projection(observation: &SynchronizationBoardObservation) -> Result<Value, String> {
    let source = &observation.source;
    let audit = &source.audit;
    let component_pins = audit.identities().components().iter().map(|identity| {
        let mut units = identity.units.iter().map(SourceUnitKey::from).collect::<Vec<_>>();
        units.sort();
        let pins = audit.pins().component_pin_numbers(&units)
            .ok_or("missing complete original component pin inventory")?;
        Ok(serde_json::json!({"units":units.iter().map(source::source_unit).collect::<Vec<_>>(),"pins":pins}))
    }).collect::<Result<Vec<_>, String>>()?;
    Ok(serde_json::json!({
        "netlist": source::netlist(audit.netlist()),
        "identities": source::identities(audit.identities()),
        "sheets": audit.sheet_inventory()?,
        "pins": audit.pins().pins().iter().map(source::pin).collect::<Vec<_>>(),
        "unexported": audit.pins().unexported().iter().map(source::unexported).collect::<Vec<_>>(),
        "component_pin_inventories": component_pins,
        "connectivity": crate::schematic_audit::preview::connectivity_json(source.connectivity.connectivity()),
        "components": source.components(),
        "ineligible_units": source.ineligible_units(),
    }))
}

impl SynchronizationProjectRecoveryBaseline {
    pub fn has_regions(&self) -> bool {
        self.original.has_regions()
    }

    pub(super) fn capture(
        preview: &SynchronizationProjectPreview,
        candidate_board: &[u8],
    ) -> Result<Self, String> {
        preview.verify()?;
        let observation = &preview.observation;
        let (allowed_nets, source_classes) = scope(observation, &preview.before, true)?;
        let result = Self {
            schema_version: 2,
            original: observation.recovery_baseline()?,
            project: observation.project.requested.clone(),
            before_project_sha256: preview.candidate.before_sha256().into(),
            candidate_project_sha256: preview.candidate.after_sha256().into(),
            candidate_board_sha256: hash(candidate_board),
            project_worker_sha256: hash(native::PROJECT_WORKER.as_bytes()),
            resource_root: observation.source.audit.inner.resource_root.clone(),
            actions: preview.actions.clone(),
            allowed_nets,
            source_classes,
            netclasses: preview.before.clone(),
            source: source_projection(observation)?,
            erc_json: observation.source.audit.erc().raw().get().into(),
        };
        preview.verify()?;
        Ok(result)
    }

    pub fn project_path(&self) -> &Path {
        &self.project
    }
    pub fn board_path(&self) -> &Path {
        self.original.board_path()
    }
    pub fn project_dir(&self) -> &Path {
        self.original.project_dir()
    }
    pub fn write_count(&self) -> usize {
        if self.candidate_board_sha256 == self.original.board_sha256() {
            1
        } else {
            2
        }
    }
    pub fn digest(&self) -> Result<String, String> {
        Ok(hash(&serde_json::to_vec(self).map_err(|e| e.to_string())?))
    }

    fn replay(
        &self,
        original_project: &[u8],
        candidate_project: &[u8],
        allowed: &BTreeSet<String>,
        references: &BTreeSet<String>,
    ) -> Result<(), String> {
        if original_project.len() as u64 > MAX_FILE
            || candidate_project.len() as u64 > MAX_FILE
            || hash(original_project) != self.before_project_sha256
            || hash(candidate_project) != self.candidate_project_sha256
            || self.before_project_sha256 == self.candidate_project_sha256
        {
            return Err("project recovery backup/candidate mismatch".into());
        }
        let compiled =
            compile_project_netclasses(original_project, &self.actions, allowed, references)?;
        if compiled.bytes() != candidate_project {
            return Err("project recovery typed action replay mismatch".into());
        }
        Ok(())
    }

    fn current(
        &self,
        policy: &SchematicAuditPolicy,
        original_project: &[u8],
        original_board: &[u8],
        candidate_project: &[u8],
        prefix: usize,
    ) -> Result<(SchematicAudit, Inputs), String> {
        if !matches!(self.schema_version, 1 | 2)
            || self.project_worker_sha256 != hash(native::PROJECT_WORKER.as_bytes())
            || self.project != policy.schematic.with_extension("kicad_pro")
            || self.board_path() != policy.schematic.with_extension("kicad_pcb")
            || original_board.len() > 128 * 1024 * 1024
            || hash(original_board) != self.original.board_sha256()
            || prefix > self.write_count()
        {
            return Err("project recovery policy/version/prefix mismatch".into());
        }
        // Prove the project can affect only typed netclass selections before native loading.
        self.replay(
            original_project,
            candidate_project,
            &self.allowed_nets,
            &self.source_classes,
        )?;
        let mut selected = Inputs::synchronization();
        for (path, expected) in [
            (
                self.project.as_path(),
                if prefix == 0 {
                    self.before_project_sha256.as_str()
                } else {
                    self.candidate_project_sha256.as_str()
                },
            ),
            (
                self.board_path(),
                if prefix == 2 {
                    self.candidate_board_sha256.as_str()
                } else {
                    self.original.board_sha256()
                },
            ),
        ] {
            let file = selected.track(path)?;
            if file.canonical.as_deref() != Some(path) || file.digest.as_deref() != Some(expected) {
                return Err(
                    "project recovery actual file differs from exact recorded prefix".into(),
                );
            }
        }
        let audit = self
            .original
            .prepare_project_transition(policy, original_project)?;
        if audit.inner.resource_root != self.resource_root {
            return Err(
                "project recovery resource anchor differs from current server policy".into(),
            );
        }
        selected.verify()?;
        Ok((audit, selected))
    }

    fn match_original(
        &self,
        actual: &SynchronizationBoardObservation,
        original_project: &[u8],
        candidate_project: &[u8],
    ) -> Result<ProjectNetclassSnapshot, String> {
        self.original.verify_original_observation(actual)?;
        let classes = actual.observe_project_netclasses()?;
        if classes != self.netclasses
            || source_projection(actual)? != self.source
            || !kicad_checks::erc::CompleteErcReport::parse(&self.erc_json)
                .map_err(|e| e.to_string())?
                .equivalent_to(actual.source.audit.erc())
                .map_err(|e| e.to_string())?
        {
            return Err("project recovery complete original source/classes mismatch".into());
        }
        let (allowed, references) = scope(actual, &classes, self.schema_version == 2)?;
        if allowed != self.allowed_nets || references != self.source_classes {
            return Err(
                "project recovery original native scope differs from recorded scope".into(),
            );
        }
        self.replay(original_project, candidate_project, &allowed, &references)?;
        Ok(classes)
    }

    /// Reconstruct the complete original in isolation at one exact forward/reverse prefix.
    /// No live file is modified and no approval or writer lock is granted.
    pub fn verify_recovery_prefix(
        &self,
        policy: &SchematicAuditPolicy,
        original_project: &[u8],
        original_board: &[u8],
        candidate_project: &[u8],
        prefix: usize,
    ) -> Result<VerifiedSynchronizationProjectRecovery, String> {
        let (current, selected) = self.current(
            policy,
            original_project,
            original_board,
            candidate_project,
            prefix,
        )?;
        let relative = |path: &Path| -> Result<String, String> {
            path.strip_prefix(&policy.project)
                .map_err(|e| e.to_string())?
                .to_str()
                .map(String::from)
                .ok_or("non-UTF8 recovery source path".into())
        };
        let candidates = BTreeMap::from([
            (relative(&self.project)?, original_project.to_vec()),
            (relative(self.board_path())?, original_board.to_vec()),
        ]);
        let original_audit = current.observe_candidate_files(&candidates)?;
        let board = original_audit
            .inner
            .policy
            .project
            .join(relative(self.board_path())?);
        let source = original_audit.synchronization_source()?;
        let original = self.original.observe_recovery_source(&source, &board)?;
        let classes = self.match_original(&original, original_project, candidate_project)?;
        self.proof(current, selected, original, classes, prefix, false)
    }

    /// Clear pending state only after this fresh actual-source proof, under the writer lock.
    pub fn verify_restored(
        &self,
        policy: &SchematicAuditPolicy,
        original_project: &[u8],
        original_board: &[u8],
        candidate_project: &[u8],
    ) -> Result<VerifiedSynchronizationProjectRecovery, String> {
        let (current, selected) = self.current(
            policy,
            original_project,
            original_board,
            candidate_project,
            0,
        )?;
        let source = current.synchronization_source()?;
        let original = self
            .original
            .observe_recovery_source(&source, self.board_path())?;
        let classes = self.match_original(&original, original_project, candidate_project)?;
        self.proof(current, selected, original, classes, 0, true)
    }

    fn proof(
        &self,
        current: SchematicAudit,
        selected: Inputs,
        original: Arc<SynchronizationBoardObservation>,
        classes: ProjectNetclassSnapshot,
        prefix: usize,
        actual_restored: bool,
    ) -> Result<VerifiedSynchronizationProjectRecovery, String> {
        let evidence = serde_json::json!({"schema_version":1,"baseline_sha256":self.digest()?,
            "project":self.project,"board":self.board_path(),"prefix":prefix,"write_count":self.write_count(),
            "resource_root":current.inner.resource_root,"project_worker_sha256":self.project_worker_sha256,
            "current_selected_inputs":selected,"complete_original_source":source_projection(&original)?,
            "original_native":original.native.report,"original_netclasses":classes,
            "original_drc":original.drc.evidence,"original_connectivity":original.connectivity,"original_complete_drc_json":original.drc.report.raw().get(),
            "original_complete_erc_json":original.source.audit.erc().raw().get(),
            "typed_replay_with_fresh_original_scope":true,"actual_source_native_restored":actual_restored,
            "publication_authorized":false});
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let proof = VerifiedSynchronizationProjectRecovery {
            current,
            selected,
            original,
            evidence,
            digest,
            valid: AtomicBool::new(true),
        };
        proof.verify()?;
        Ok(proof)
    }
}

impl VerifiedSynchronizationProjectRecovery {
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("project recovery proof was invalidated".into());
        }
        let result = self
            .current
            .verify()
            .and_then(|()| self.selected.verify())
            .and_then(|()| self.original.verify());
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
}
