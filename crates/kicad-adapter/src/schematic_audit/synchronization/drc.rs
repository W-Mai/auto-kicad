//! Complete private PCB/schematic-parity DRC with immutable source and resource context.
use super::*;
use board::SavedSynchronizationBoard;
use kicad_checks::{
    CompleteDrcReport, DrcPolicy,
    connectivity::{ClosedConnectivityEvidence, check_saved_connectivity},
};
use std::time::Duration;

pub(super) struct SynchronizationDrcCapture {
    pub report: CompleteDrcReport,
    pub evidence: Value,
    source: Inputs,
    private: Inputs,
    project_root: PathBuf,
    project_tree: BTreeSet<PathBuf>,
    resources: PathBuf,
    resource_tree: BTreeSet<PathBuf>,
    configuration: PathBuf,
    config_files: Vec<PathBuf>,
    footprint_context: SchematicFootprintContext,
    footprint_ids: Vec<FootprintLibraryIdentifier>,
    runner: NetlistRunner,
    _owner: runtime::owned_temp::TempDir,
}

impl SynchronizationDrcCapture {
    pub fn verify(&self, audit: &SchematicAudit) -> Result<(), String> {
        audit.verify()?;
        self.source.verify()?;
        self.verify_private(audit)
    }
    pub(super) fn verify_prefix(
        &self,
        audit: &SchematicAudit,
        prefix: &crate::schematic_audit::source_transition::VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        audit.verify_publication_prefix(prefix)?;
        prefix.verify_inputs(&[&self.source])?;
        self.verify_private(audit)
    }
    fn verify_private(&self, audit: &SchematicAudit) -> Result<(), String> {
        self.private.verify()?;
        if tree_names(&self.project_root)? != self.project_tree
            || tree_names(&self.resources)? != self.resource_tree
            || config_names(&self.configuration)? != self.config_files
            || audit
                .inner
                .footprints
                .schematic_files(&self.footprint_ids)?
                != self.footprint_context
        {
            return Err(
                "synchronization DRC source/resource/configuration namespace changed".into(),
            );
        }
        self.runner
            .execution_context()
            .ok_or("missing DRC execution context")?
            .verify()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

fn qualified_ids(
    audit: &SchematicAudit,
    board: &SavedSynchronizationBoard,
) -> Result<Vec<FootprintLibraryIdentifier>, String> {
    let mut ids = audit
        .inner
        .footprint_ids
        .iter()
        .map(|id| (format!("{}:{}", id.nickname, id.entry_name), id.clone()))
        .collect::<BTreeMap<_, _>>();
    for fp in board.footprints().values() {
        if fp.library.is_empty() || !fp.library.contains(':') {
            continue;
        }
        let (nickname, entry_name) = fp
            .library
            .split_once(':')
            .ok_or("invalid board footprint library identity")?;
        let id = FootprintLibraryIdentifier {
            nickname: nickname.into(),
            entry_name: entry_name.into(),
        };
        ids.insert(fp.library.clone(), id);
    }
    Ok(ids.into_values().collect())
}

pub(super) fn capture(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    board: &SavedSynchronizationBoard,
    project: &InputFile,
    rules: &InputFile,
) -> Result<SynchronizationDrcCapture, String> {
    capture_profile(audit, filename, board, project, rules, None)
}

pub(super) fn capture_for_regions(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    board: &SavedSynchronizationBoard,
    project: &InputFile,
    rules: &InputFile,
    native: &native::NativeBoardProjection,
) -> Result<SynchronizationDrcCapture, String> {
    native.verify(audit)?;
    if native.report.board_sha256 != board.digest()
        || Some(&native.report.project_sha256) != project.digest.as_ref()
    {
        return Err(
            "generated DRC native readback belongs to different board/project inputs".into(),
        );
    }
    capture_profile(audit, filename, board, project, rules, Some(native))
}

fn capture_profile(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    board: &SavedSynchronizationBoard,
    project: &InputFile,
    rules: &InputFile,
    native: Option<&native::NativeBoardProjection>,
) -> Result<SynchronizationDrcCapture, String> {
    audit.verify()?;
    let generated = if let Some(native) = native {
        let partition = crate::live_geometry::TeardropTextPartition::parse(board.text())?;
        if partition.generated_count() > 0 {
            if rules.bytes.is_some() {
                return Err("generated custom-rule DRC requires independently qualified identity-invariant evaluation".into());
            }
            Some(
                crate::live_geometry::ClosedGeneratedDrcSnapshot::bind_native_readback(
                    board.text(),
                    native.board.text(),
                )?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let owner = runtime::owned_temp::Builder::new()
        .prefix("auto-kicad-sync-drc-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let owned = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
    let project_root = owned.join("project");
    let resources = owned.join("resources");
    fs::create_dir_all(&resources).map_err(|e| e.to_string())?;
    let configuration = owned.join("config/10.0");
    fs::create_dir_all(&configuration).map_err(|e| e.to_string())?;
    let mut source = Inputs::default();
    for path in &audit.inner.private_config_names {
        source.copy(
            path,
            &configuration.join(path.file_name().ok_or("missing configuration basename")?),
        )?;
    }
    let original_project_root = &audit.inner.resource_root;
    let schematic = project_root.join(&audit.inner.parsed.root_relative_path);
    for file in &audit.inner.parsed.files {
        let original = audit
            .inner
            .policy
            .project
            .join(&file.artifact.relative_path);
        let captured = audit
            .inner
            .source
            .files
            .get(&original)
            .ok_or("missing captured schematic source")?;
        source.copy_expected(
            &original,
            &project_root.join(&file.artifact.relative_path),
            captured.size,
            captured.digest.as_deref(),
        )?;
    }
    let local_root = schematic
        .parent()
        .ok_or("missing private schematic parent")?;
    let pcb = local_root.join(filename);
    if pcb.with_extension("kicad_sch") != schematic {
        return Err("synchronization DRC requires paired schematic and PCB basenames".into());
    }
    write_new(
        &pcb,
        generated
            .as_ref()
            .map(|snapshot| snapshot.text())
            .unwrap_or(board.text())
            .as_bytes(),
    )?;
    let original_pro = project
        .bytes
        .as_deref()
        .ok_or("missing synchronization project settings")?;
    let compiled_pro = crate::native_project::project_with_root(
        original_pro,
        original_project_root
            .to_str()
            .ok_or("non-UTF8 original project root")?,
    )?;
    source.track(&project.requested)?;
    write_new(&pcb.with_extension("kicad_pro"), &compiled_pro)?;
    source.copy(&rules.requested, &pcb.with_extension("kicad_dru"))?;
    source.copy(
        &audit.inner.policy.schematic.with_extension("kicad_prl"),
        &schematic.with_extension("kicad_prl"),
    )?;
    let audit_private_schematic = audit
        .inner
        .owner
        .path()
        .join("project")
        .join(&audit.inner.parsed.root_relative_path);
    let audit_private_root = audit_private_schematic
        .parent()
        .ok_or("missing audit private parent")?;
    source.copy(
        &audit_private_root.join("sym-lib-table"),
        &local_root.join("sym-lib-table"),
    )?;

    // The complete source+board union is required: manual footprints may share a library
    // nickname with source assignments while using different entries.
    let footprint_ids = qualified_ids(audit, board)?;
    let footprint_context = audit.inner.footprints.schematic_files(&footprint_ids)?;
    let mut fp_copies = BTreeMap::new();
    for file in footprint_context.files() {
        if let Some(path) = &file.path {
            let index = fp_copies.len();
            let target = fp_copies
                .entry(file.identifier.nickname.clone())
                .or_insert_with(|| resources.join(format!("footprints/{index}.pretty")))
                .clone();
            fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            source.copy_expected(
                path,
                &target.join(format!("{}.kicad_mod", file.identifier.entry_name)),
                file.size,
                file.digest.as_deref(),
            )?;
        }
    }
    write_new(
        &local_root.join("fp-lib-table"),
        relocate(
            audit.inner.footprints.effective_rows()?,
            &fp_copies,
            original_project_root,
            TableKind::Footprint,
        )?
        .as_bytes(),
    )?;

    let runner = NetlistRunner::production()
        .map_err(|e| e.to_string())?
        .with_timeout(Duration::from_secs(120))
        .freeze_context(Some(&owned.join("config")))
        .map_err(|e| e.to_string())?;
    let policy = DrcPolicy {
        allowed_root: project_root.clone(),
        timeout: Duration::from_secs(120),
    };
    let connectivity: ClosedConnectivityEvidence = check_saved_connectivity(&pcb, &policy)
        .map_err(|e| format!("DRC connectivity capture: {e:?}"))?;
    let mut private = Inputs::default();
    let mut trees = BTreeMap::new();
    for root in [&project_root, &resources] {
        let names = tree_names(root)?;
        for path in &names {
            if path.is_file() {
                private.track(path)?;
            }
        }
        trees.insert(root.clone(), names);
    }
    // Only the owned configuration may settle during the first check. Project/resource
    // bytes are protected before initialization, including absent local preferences.
    let first = runner
        .run_complete_pcb_drc(&project_root, &pcb)
        .map_err(|e| e.to_string())?;
    private.verify()?;
    source.verify()?;
    bind_initialized_preferences(
        &mut private,
        &mut trees,
        &project_root,
        &schematic.with_extension("kicad_prl"),
    )?;
    let config_files = config_names(&configuration)?;
    for path in &config_files {
        private.track(path)?;
    }
    let report = runner
        .run_complete_pcb_drc(&project_root, &pcb)
        .map_err(|e| e.to_string())?;
    if !first
        .equivalent_to(&report, &BTreeMap::new())
        .map_err(|e| e.to_string())?
        && !first
            .equivalent_with_closed_connectivity(
                &report,
                &BTreeMap::new(),
                &connectivity,
                &connectivity,
            )
            .map_err(|e| e.to_string())?
    {
        return Err(
            "complete synchronization DRC changed across identical fixed native runs".into(),
        );
    }
    connectivity
        .verify_saved_inputs(&pcb, &policy)
        .map_err(|e| format!("DRC connectivity input drift: {e:?}"))?;
    let mut evidence = serde_json::json!({"schema_version":1,"board_sha256":board.digest(),"project_source_sha256":hash(original_pro),"project_snapshot_sha256":hash(&compiled_pro),"rules":rules,
        "source_inputs":source,"private_inputs":private,"footprints":footprint_context,"unqualified_board_footprints":board.footprints().values().filter(|fp| !fp.library.contains(':')).map(|fp| &fp.uuid).collect::<Vec<_>>(),
        "execution":runner.execution_context().ok_or("missing DRC execution context")?.digest(),"complete_report":report,"connectivity":connectivity,
        "flags":["--schematic-parity","--all-track-errors","--severity-all","--units=mm"],"repeated_native_equal":true});
    if let Some(generated) = &generated {
        evidence["closed_generated_drc"] =
            serde_json::to_value(generated).map_err(|e| e.to_string())?;
    }
    let capture = SynchronizationDrcCapture {
        report,
        evidence,
        source,
        private,
        project_tree: trees
            .remove(&project_root)
            .ok_or("missing DRC project namespace")?,
        resource_tree: trees
            .remove(&resources)
            .ok_or("missing DRC resource namespace")?,
        project_root,
        resources,
        configuration,
        config_files,
        footprint_context,
        footprint_ids,
        runner,
        _owner: owner,
    };
    capture.verify(audit)?;
    Ok(capture)
}
