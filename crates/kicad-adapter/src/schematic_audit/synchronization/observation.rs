//! Complete closed-input synchronization candidates with independent native readback.
use super::*;
use board::{SavedSynchronizationBoard, SynchronizationCandidate, SynchronizationIntent};
use kicad_checks::{
    DrcPolicy,
    connectivity::{ClosedConnectivityEvidence, check_saved_connectivity},
};
use std::time::Duration;
mod project_preview;
mod publication;
pub use project_preview::{
    MatchedSynchronizationProjectBoardPreview, MatchedSynchronizationProjectPreview,
    NetclassRecordChange, PreparedSynchronizationProjectBoardPublication,
    PreparedSynchronizationProjectPublication, ProjectNetclassDelta, SourceNetclass,
    SynchronizationIntentPreview, SynchronizationProjectBoardExpectation,
    SynchronizationProjectBoardPreview, SynchronizationProjectExpectation,
    SynchronizationProjectPreview, SynchronizationProjectRecoveryBaseline,
    SynchronizationProjectWrite, VerifiedSynchronizationProjectBoardPublication,
    VerifiedSynchronizationProjectPublication, VerifiedSynchronizationProjectRecovery,
};
pub use publication::{
    MatchedSynchronizationPreview, PreparedSynchronizationPublication, SynchronizationExpectation,
    SynchronizationRecoveryBaseline, VerifiedSynchronizationPublication,
    VerifiedSynchronizationRestoration,
};

pub struct SynchronizationBoardObservation {
    source: SynchronizationSource,
    path: PathBuf,
    bytes: Arc<[u8]>,
    project: InputFile,
    rules: InputFile,
    board: SavedSynchronizationBoard,
    native: native::NativeBoardProjection,
    drc: drc::SynchronizationDrcCapture,
    connectivity: ClosedConnectivityEvidence,
    _owner: runtime::owned_temp::TempDir,
    valid: AtomicBool,
    digest: String,
}

pub struct SynchronizationBoardPreview {
    project_transition_pending: bool,
    observation: Arc<SynchronizationBoardObservation>,
    candidate: SynchronizationCandidate,
    native: native::NativeBoardProjection,
    drc: drc::SynchronizationDrcCapture,
    connectivity: ClosedConnectivityEvidence,
    candidate_path: PathBuf,
    _owner: runtime::owned_temp::TempDir,
    valid: AtomicBool,
    digest: String,
    evidence: Value,
    drc_delta: kicad_checks::CompleteDrcDelta,
    templates: Vec<templates::SavedFootprintTemplate>,
}

fn require_closed(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("missing synchronization project parent")?;
    for extension in ["kicad_pcb", "kicad_pro"] {
        let source = path.with_extension(extension);
        let name = source
            .file_name()
            .ok_or("missing synchronization filename")?
            .to_str()
            .ok_or("non-UTF8 synchronization filename")?;
        let lock = parent.join(format!("~{name}.lck"));
        match fs::symlink_metadata(&lock) {
            Ok(_) => {
                return Err("synchronization requires closed PCB and schematic documents".into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot inspect synchronization document lock: {e}")),
        }
    }
    Ok(())
}
fn verify_input(input: &InputFile) -> Result<(), String> {
    let current = Inputs::read(&input.requested)?;
    if current.canonical != input.canonical
        || current.digest != input.digest
        || current.size != input.size
    {
        return Err("synchronization project/rules source changed".into());
    }
    Ok(())
}
fn snapshot(
    root: &Path,
    filename: &std::ffi::OsStr,
    bytes: &[u8],
    project: &InputFile,
    rules: &InputFile,
) -> Result<PathBuf, String> {
    let path = root.join(filename);
    write_new(&path, bytes)?;
    write_new(
        &path.with_extension("kicad_pro"),
        project
            .bytes
            .as_deref()
            .ok_or("synchronization project is absent")?,
    )?;
    if let Some(bytes) = &rules.bytes {
        write_new(&path.with_extension("kicad_dru"), bytes)?;
    }
    Ok(path)
}

impl SynchronizationSource {
    /// Observe one closed saved PCB using the same project settings as the captured schematic.
    pub fn observe_board(
        &self,
        path: &Path,
    ) -> Result<Arc<SynchronizationBoardObservation>, String> {
        self.verify()?;
        let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
        if canonical != path
            || !path.starts_with(&self.audit.inner.policy.project)
            || path.extension().and_then(|v| v.to_str()) != Some("kicad_pcb")
        {
            return Err("synchronization PCB must be canonical and confined".into());
        }
        if path.with_extension("kicad_pro")
            != self
                .audit
                .inner
                .policy
                .schematic
                .with_extension("kicad_pro")
        {
            return Err("synchronization board and schematic must share project settings".into());
        }
        require_closed(path)?;
        let bytes: Arc<[u8]> = native::bounded_read(path, 128 * 1024 * 1024)?.into();
        let project = Inputs::read(&path.with_extension("kicad_pro"))?;
        let rules = Inputs::read(&path.with_extension("kicad_dru"))?;
        let captured = self
            .audit
            .inner
            .source
            .files
            .get(&project.requested)
            .ok_or("uncaptured schematic project settings")?;
        if captured.digest != project.digest || captured.canonical != project.canonical {
            return Err("schematic and PCB project settings differ".into());
        }
        let board = SavedSynchronizationBoard::parse(
            std::str::from_utf8(&bytes).map_err(|e| e.to_string())?,
        )?;
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-sync-observation-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let filename = path.file_name().ok_or("missing board basename")?;
        let saved = snapshot(&root, filename, &bytes, &project, &rules)?;
        let policy = DrcPolicy {
            allowed_root: root,
            timeout: Duration::from_secs(60),
        };
        let connectivity =
            check_saved_connectivity(&saved, &policy).map_err(|e| format!("{e:?}"))?;
        let native = native::capture(
            &self.audit,
            filename,
            &bytes,
            project.bytes.as_deref().ok_or("missing project bytes")?,
        )?;
        board.verify_native_units(&native.board)?;
        let drc = drc::capture(&self.audit, filename, &board, &project, &rules)?;
        let digest=hash(&serde_json::to_vec(&serde_json::json!({"source":self.digest,"board":board.digest(),"project":project,"rules":rules,"native":native.digest,"connectivity":connectivity,"drc":drc.evidence})).map_err(|e|e.to_string())?);
        let observed = Arc::new(SynchronizationBoardObservation {
            source: self.clone(),
            path: path.into(),
            bytes,
            project,
            rules,
            board,
            native,
            drc,
            connectivity,
            _owner: owner,
            valid: AtomicBool::new(true),
            digest,
        });
        observed.verify()?;
        Ok(observed)
    }
}
impl SynchronizationBoardObservation {
    /// Bound raw selectors retain legacy/empty assignments and exact observed pattern indices.
    pub fn project_netclass_selections(
        &self,
    ) -> Result<super::project::ProjectNetclassSelections, String> {
        self.verify()?;
        let observed = super::project::inspect_project_netclass_selections(
            self.project
                .bytes
                .as_deref()
                .ok_or("missing captured project settings")?,
        )?;
        self.verify()?;
        Ok(observed)
    }

    /// Observe complete configured and effective classes with exact native project readback.
    /// This remains closed-input observation and grants no project write approval.
    pub fn observe_project_netclasses(
        &self,
    ) -> Result<project::native::ProjectNetclassSnapshot, String> {
        self.verify()?;
        let projected = native::capture_project(
            &self.source.audit,
            self.path.file_name().ok_or("missing board basename")?,
            &self.bytes,
            self.project
                .bytes
                .as_deref()
                .ok_or("missing project bytes")?,
        )?;
        if projected.report != self.native.report {
            return Err(
                "project-class observation differs from complete original native board".into(),
            );
        }
        projected.verify(&self.source.audit)?;
        let classes = projected
            .project_classes
            .ok_or("missing complete project class inventory")?;
        self.verify()?;
        Ok(classes)
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn source(&self) -> &SynchronizationSource {
        &self.source
    }
    pub fn board(&self) -> &SavedSynchronizationBoard {
        &self.board
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("synchronization observation was invalidated".into());
        }
        let result = (|| {
            self.source.verify()?;
            require_closed(&self.path)?;
            if fs::canonicalize(&self.path).map_err(|e| e.to_string())? != self.path
                || native::bounded_read(&self.path, 128 * 1024 * 1024)?.as_slice()
                    != self.bytes.as_ref()
            {
                return Err("synchronization board source changed".into());
            }
            verify_input(&self.project)?;
            verify_input(&self.rules)?;
            self.native.verify(&self.source.audit)?;
            self.drc.verify(&self.source.audit)?;
            Ok(())
        })();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
    fn prepare_templates(
        &self,
        intent: &SynchronizationIntent,
    ) -> Result<
        (
            Vec<templates::SavedFootprintTemplate>,
            SynchronizationIntent,
        ),
        String,
    > {
        use board::SynchronizationAction;
        let mut result = Vec::new();
        let mut compiled = intent.clone();
        let mut selected = BTreeSet::new();
        let mut bytes = 0u64;
        for (index, action) in intent.actions.iter().enumerate() {
            let (component_id, placement, fields, replacement) = match action {
                SynchronizationAction::Add {
                    component_id,
                    placement,
                    fields,
                } => (component_id, placement.clone(), fields, None),
                SynchronizationAction::Replace {
                    component_id,
                    footprint_uuid,
                    fields,
                } => {
                    let native = self
                        .native
                        .report
                        .footprints
                        .iter()
                        .find(|fp| &fp.uuid == footprint_uuid)
                        .ok_or("unknown replacement footprint")?;
                    let degrees = native.orientation_degrees.rem_euclid(360.0) * 1000.0;
                    if !degrees.is_finite() || (degrees - degrees.round()).abs() > 0.000001 {
                        return Err(
                            "replacement orientation cannot be represented exactly in millidegrees"
                                .into(),
                        );
                    }
                    let side = match native.layer {
                        0 => board::SynchronizationSide::Front,
                        2 => board::SynchronizationSide::Back,
                        _ => return Err("unsupported replacement footprint side".into()),
                    };
                    (
                        component_id,
                        board::SynchronizationPlacement {
                            position_nm: native.position_nm,
                            orientation_mdeg: degrees.round() as u32 % 360000,
                            side,
                        },
                        fields,
                        Some(footprint_uuid),
                    )
                }
                _ => continue,
            };
            let component = self
                .source
                .components()
                .iter()
                .find(|c| c.id() == component_id)
                .ok_or("unknown source component addition")?;
            if !selected.insert(component_id)
                || !component.eligible_for_board()
                || component.placed_units != (1..=component.units.len() as u32).collect()
            {
                return Err("repeated, excluded or incomplete source component addition".into());
            }
            let linked = self
                .board
                .footprints()
                .values()
                .filter(|fp| {
                    fp.path.as_deref().is_some_and(|path| {
                        component
                            .aliases
                            .iter()
                            .any(|alias| alias == path.trim_end_matches('/'))
                    })
                })
                .collect::<Vec<_>>();
            match replacement {
                Some(id) if linked.len() == 1 && &linked[0].uuid == id => {
                    if Some(linked[0].library.as_str()) == component.footprint.as_deref() {
                        return Err(
                            "replacement requires a changed authoritative library assignment"
                                .into(),
                        );
                    }
                }
                Some(_) => {
                    return Err(
                        "replacement target must have the exact complete source component link"
                            .into(),
                    );
                }
                None if !linked.is_empty() => {
                    return Err("source addition already has a board footprint".into());
                }
                None => {}
            }
            let seed = hash(
                &serde_json::to_vec(&serde_json::json!([
                    "sync-template-v1",
                    self.source.digest(),
                    self.board.digest(),
                    intent,
                    index
                ]))
                .map_err(|e| e.to_string())?,
            );
            let root_id = replacement
                .cloned()
                .unwrap_or_else(|| templates::derived(&seed, "root", 0));
            let mut template =
                templates::capture(&self.source, component, &placement, &seed, &root_id)?;
            if replacement.is_some() {
                let connected = self
                    .connectivity
                    .components()
                    .iter()
                    .filter(|group| group.len() > 1)
                    .flatten()
                    .cloned()
                    .collect();
                self.board.inherit_replacement(
                    &mut template,
                    &connected,
                    &seed,
                    &self.native.board,
                )?;
            }
            bytes = bytes
                .checked_add(template.footprint.len() as u64)
                .ok_or("template byte count overflow")?;
            if bytes > 128 * 1024 * 1024 {
                return Err("combined source templates exceed board byte bound".into());
            }
            compiled.actions[index] = SynchronizationAction::Relink {
                component_id: component_id.clone(),
                footprint_uuid: root_id,
                fields: fields.clone(),
                update_nets: true,
            };
            result.push(template);
        }
        Ok((result, compiled))
    }
    /// Compile exact candidates and validate every authorized native change before exposing a preview.
    pub fn preview(
        self: &Arc<Self>,
        intent: &SynchronizationIntent,
    ) -> Result<Arc<SynchronizationBoardPreview>, String> {
        self.preview_candidate(intent, false, false)
    }
    fn preview_candidate(
        self: &Arc<Self>,
        intent: &SynchronizationIntent,
        project_transition_pending: bool,
        allow_empty: bool,
    ) -> Result<Arc<SynchronizationBoardPreview>, String> {
        self.verify()?;
        intent.validate_board(allow_empty)?;
        let (templates, compiled_intent) = self.prepare_templates(intent)?;
        let candidate = if templates.is_empty() {
            self.source.compile_board_for_preview(
                &self.board,
                intent,
                &self.connectivity,
                allow_empty,
            )?
        } else {
            let augmented = self.board.with_templates(&templates)?;
            let temporary = runtime::owned_temp::Builder::new()
                .prefix("auto-kicad-sync-added-")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let temporary_root = fs::canonicalize(temporary.path()).map_err(|e| e.to_string())?;
            let saved = snapshot(
                &temporary_root,
                self.path.file_name().ok_or("missing board basename")?,
                augmented.text().as_bytes(),
                &self.project,
                &self.rules,
            )?;
            let connectivity = check_saved_connectivity(
                &saved,
                &DrcPolicy {
                    allowed_root: temporary_root,
                    timeout: Duration::from_secs(60),
                },
            )
            .map_err(|e| format!("template candidate connectivity: {e:?}"))?;
            self.source
                .compile_board_for_preview(
                    &augmented,
                    &compiled_intent,
                    &connectivity,
                    allow_empty,
                )?
                .bind_templates(&self.board, intent, &templates)?
        };
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-sync-preview-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let filename = self.path.file_name().ok_or("missing board basename")?;
        let path = snapshot(
            &root,
            filename,
            candidate.bytes(),
            &self.project,
            &self.rules,
        )?;
        let policy = DrcPolicy {
            allowed_root: root,
            timeout: Duration::from_secs(60),
        };
        let connectivity =
            check_saved_connectivity(&path, &policy).map_err(|e| format!("{e:?}"))?;
        let native = native::capture(
            &self.source.audit,
            filename,
            candidate.bytes(),
            self.project
                .bytes
                .as_deref()
                .ok_or("missing project bytes")?,
        )?;
        let raw = SavedSynchronizationBoard::parse(
            std::str::from_utf8(candidate.bytes()).map_err(|e| e.to_string())?,
        )?;
        raw.verify_native_units(&native.board)?;
        let source_class_requirements = if project_transition_pending {
            None
        } else {
            Some(native::verify_source_netclasses(
                &self.source,
                self.project
                    .bytes
                    .as_deref()
                    .ok_or("missing project settings")?,
                &native.report,
            )?)
        };
        if !project_transition_pending {
            native::verify_retained_netclasses(&self.native.report, &native.report)?;
            native::verify_replacement_netclasses(
                &self.native.report,
                &native.report,
                candidate.replacement_pads(),
            )?;
        }
        for template in &templates {
            template
                .verify_composed(&raw, candidate.differences())
                .map_err(|e| format!("raw replacement composition: {e}"))?;
            template
                .verify_native_composed(&native.board, candidate.differences())
                .map_err(|e| format!("native replacement serialization: {e}"))?;
        }
        if self
            .native
            .board
            .protected_native_digest(candidate.differences())?
            != native
                .board
                .protected_native_digest(candidate.differences())?
        {
            return Err("native synchronization changed unapproved board data".into());
        }
        if connectivity.endpoints().iter().any(|endpoint| {
            self.connectivity.endpoint(endpoint.uuid()).is_none()
                && !candidate.added_pads().contains(endpoint.uuid())
        }) || candidate
            .added_pads()
            .iter()
            .any(|id| connectivity.endpoint(id).is_none())
        {
            return Err(
                "native synchronization endpoint births lack complete template receipts".into(),
            );
        }
        verify_physical_partitions(
            &self.connectivity,
            &connectivity,
            candidate.replacement_pads(),
        )?;
        let drc = drc::capture(
            &self.source.audit,
            filename,
            &raw,
            &self.project,
            &self.rules,
        )?;
        let drc_delta = self
            .drc
            .report
            .compare_complete(&drc.report)
            .map_err(|e| e.to_string())?;
        self.verify()?;
        let mut evidence = serde_json::json!({"schema_version":1,"observation_digest":self.digest,"source_digest":self.source.digest(),"candidate_digest":candidate.digest(),
            "before_board_sha256":self.board.digest(),"candidate_board_sha256":hash(candidate.bytes()),"project":self.project,"rules":self.rules,
            "has_changes":candidate.has_changes(),"differences":candidate.differences(),"source_netclass_requirements":source_class_requirements,"templates":candidate.template_evidence(),"native_before":self.native.report,"native_after":native.report,
            "drc_before":self.drc.evidence,"drc_after":drc.evidence,"drc_delta":drc_delta,"connectivity_before":self.connectivity,"connectivity_after":connectivity,"native_after_digest":native.digest,"publication_authorized":false});
        if project_transition_pending {
            evidence["project_class_authority"] =
                serde_json::json!("pending_composed_project_validation");
        }
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        let preview = Arc::new(SynchronizationBoardPreview {
            project_transition_pending,
            observation: self.clone(),
            candidate,
            native,
            drc,
            connectivity,
            candidate_path: path,
            _owner: owner,
            valid: AtomicBool::new(true),
            digest,
            evidence,
            drc_delta,
            templates,
        });
        preview.verify()?;
        Ok(preview)
    }
}
fn verify_physical_partitions(
    before: &ClosedConnectivityEvidence,
    after: &ClosedConnectivityEvidence,
    replacements: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mapped = |id: &str| {
        replacements
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_owned())
    };
    let retained = before
        .endpoints()
        .iter()
        .map(|endpoint| mapped(endpoint.uuid()))
        .filter(|id| after.endpoint(id).is_some())
        .collect::<BTreeSet<_>>();
    let old = before
        .components()
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|id| mapped(id))
                .filter(|id| retained.contains(id))
                .collect::<BTreeSet<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect::<BTreeSet<_>>();
    let new = after
        .components()
        .iter()
        .map(|group| {
            group
                .iter()
                .filter(|id| retained.contains(*id))
                .cloned()
                .collect::<BTreeSet<_>>()
        })
        .filter(|group| !group.is_empty())
        .collect::<BTreeSet<_>>();
    if old != new {
        return Err("native synchronization changed retained physical copper partitions".into());
    }
    Ok(())
}
impl SynchronizationBoardPreview {
    pub fn native_template_artifacts(&self) -> impl Iterator<Item = (&str, &str, &[u8])> {
        self.templates.iter().flat_map(|template| {
            template
                .artifacts()
                .map(move |(name, bytes)| (template.root_id.as_str(), name, bytes))
        })
    }
    pub fn candidate(&self) -> &SynchronizationCandidate {
        &self.candidate
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn drc_delta(&self) -> &kicad_checks::CompleteDrcDelta {
        &self.drc_delta
    }
    /// Exact DRC matching grants neither human approval nor publication authority.
    pub fn match_drc_expectation(
        &self,
        expected: &kicad_checks::CompleteDrcExpectation,
    ) -> Result<(), String> {
        self.verify()?;
        self.drc_delta
            .match_expectation(expected)
            .map_err(|e| e.to_string())
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    fn verify_private(&self) -> Result<(), String> {
        self.observation.source.verify()?;
        require_closed(&self.observation.path)?;
        verify_input(&self.observation.project)?;
        verify_input(&self.observation.rules)?;
        self.native.verify(&self.observation.source.audit)?;
        self.drc.verify(&self.observation.source.audit)?;
        for template in &self.templates {
            template.verify(&self.observation.source)?;
        }
        if native::bounded_read(&self.candidate_path, 128 * 1024 * 1024)? != self.candidate.bytes()
        {
            return Err("private synchronization candidate changed".into());
        }
        self.connectivity
            .verify_saved_inputs(
                &self.candidate_path,
                &DrcPolicy {
                    allowed_root: self._owner.path().into(),
                    timeout: Duration::from_secs(60),
                },
            )
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }
    fn verify_private_prefix(
        &self,
        prefix: &crate::schematic_audit::source_transition::VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        let audit = &self.observation.source.audit;
        audit.verify_publication_prefix(prefix)?;
        require_closed(&self.observation.path)?;
        verify_input(&self.observation.rules)?;
        self.native.verify_execution(audit)?;
        self.drc.verify_prefix(audit, prefix)?;
        for template in &self.templates {
            template.verify_prefix(&self.observation.source, prefix)?;
        }
        if native::bounded_read(&self.candidate_path, 128 * 1024 * 1024)? != self.candidate.bytes()
        {
            return Err("private synchronization candidate changed".into());
        }
        self.connectivity
            .verify_saved_inputs(
                &self.candidate_path,
                &DrcPolicy {
                    allowed_root: self._owner.path().into(),
                    timeout: Duration::from_secs(60),
                },
            )
            .map_err(|e| format!("{e:?}"))?;
        Ok(())
    }
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("synchronization preview was invalidated".into());
        }
        let result = (|| {
            self.observation.verify()?;
            self.verify_private()?;
            Ok(())
        })();
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
}
