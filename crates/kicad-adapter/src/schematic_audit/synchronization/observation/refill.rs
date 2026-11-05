//! Independently matched private refill executions for exact synchronization candidates.
use super::*;
use crate::live_geometry::ClosedRefillCandidate;

pub(super) struct SynchronizationRegionRefill {
    candidate: ClosedRefillCandidate,
    inputs: Inputs,
    runners: Vec<NetlistRunner>,
    trees: Vec<(PathBuf, BTreeSet<PathBuf>)>,
    evidence: Value,
    owners: Vec<runtime::owned_temp::TempDir>,
}
impl SynchronizationRegionRefill {
    pub(super) fn candidate(&self) -> &ClosedRefillCandidate {
        &self.candidate
    }
    pub(super) fn native_artifacts(&self) -> Result<Vec<(&'static str, &[u8])>, String> {
        self.verify_private()?;
        let first = self
            .owners
            .first()
            .ok_or("missing native refill input owner")?
            .path()
            .join("project");
        let input = self
            .inputs
            .files
            .keys()
            .find(|path| {
                path.parent() == Some(first.as_path())
                    && path.extension().and_then(|value| value.to_str()) == Some("kicad_pcb")
            })
            .ok_or("missing native refill input bytes")?;
        let paths = [
            input.to_owned(),
            self.owners[0].path().join("native-output.kicad_pcb"),
            self.owners[1].path().join("native-output.kicad_pcb"),
        ];
        let names = [
            "region-input.kicad_pcb",
            "region-native-0.kicad_pcb",
            "region-native-1.kicad_pcb",
        ];
        names
            .into_iter()
            .zip(paths)
            .map(|(name, path)| {
                let bytes = self
                    .inputs
                    .files
                    .get(&path)
                    .and_then(|file| file.bytes.as_deref())
                    .ok_or("missing captured native region artifact")?;
                Ok((name, bytes))
            })
            .collect()
    }
    pub(super) fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub(super) fn verify(&self, audit: &SchematicAudit) -> Result<(), String> {
        audit.verify()?;
        self.verify_private()
    }
    pub(super) fn verify_prefix(
        &self,
        audit: &SchematicAudit,
        prefix: &crate::schematic_audit::source_transition::VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        audit.verify_publication_prefix(prefix)?;
        self.verify_private()
    }
    fn verify_private(&self) -> Result<(), String> {
        self.inputs.verify()?;
        for runner in &self.runners {
            runner
                .execution_context()
                .ok_or("missing refill execution context")?
                .verify()
                .map_err(|e| e.to_string())?;
        }
        for (root, tree) in &self.trees {
            if tree_names(root)? != *tree {
                return Err("private synchronization refill namespace changed".into());
            }
        }
        if self.owners.len() != 2 || self.runners.len() != 2 || self.trees.len() != 2 {
            return Err("synchronization refill lacks independent execution ownership".into());
        }
        Ok(())
    }
}

pub(super) fn refill_candidate(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    candidate: &str,
    project: &[u8],
    rules: Option<&[u8]>,
) -> Result<SynchronizationRegionRefill, String> {
    audit.verify()?;
    if Path::new(filename).file_name() != Some(filename) {
        return Err("refill requires one source board basename".into());
    }
    let _bounded = crate::live_geometry::TeardropTextPartition::parse(candidate)?;
    let mut inputs = Inputs::default();
    let mut owners = Vec::new();
    let mut runners = Vec::new();
    let mut outputs = Vec::new();
    let mut trees = Vec::new();
    let mut reports = Vec::new();
    let mut seeds = Vec::new();
    for _ in 0..2 {
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-sync-refill-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let config = root.join("config/10.0");
        fs::create_dir_all(&config).map_err(|e| e.to_string())?;
        let mut seed = BTreeMap::new();
        for path in &audit.inner.private_config_names {
            let captured = audit
                .inner
                .private_inputs
                .files
                .get(path)
                .ok_or("missing captured refill configuration")?;
            let basename = path
                .file_name()
                .ok_or("missing refill configuration name")?;
            let bytes = captured
                .bytes
                .as_deref()
                .ok_or("missing refill configuration bytes")?;
            write_new(&config.join(basename), bytes)?;
            seed.insert(basename.to_string_lossy().into_owned(), hash(bytes));
        }
        seeds.push(hash(&serde_json::to_vec(&seed).map_err(|e| e.to_string())?));
        let work = root.join("project");
        fs::create_dir(&work).map_err(|e| e.to_string())?;
        let board = work.join(filename);
        write_new(&board, candidate.as_bytes())?;
        let project = crate::native_project::project_with_root(
            project,
            audit
                .inner
                .resource_root
                .to_str()
                .ok_or("non-UTF8 refill resource anchor")?,
        )?;
        write_new(&board.with_extension("kicad_pro"), &project)?;
        if let Some(rules) = rules {
            write_new(&board.with_extension("kicad_dru"), rules)?;
        }
        let runner = audit
            .inner
            .runner
            .clone()
            .with_timeout(Duration::from_secs(60))
            .freeze_context(Some(config.parent().ok_or("missing refill config parent")?))
            .map_err(|e| e.to_string())?;
        let native = runner
            .run_private_pcb_refill(&work, &board)
            .map_err(|e| format!("native synchronization refill: {e:?}"))?;
        if native.input_sha256() != hash(candidate.as_bytes()) {
            return Err("refill input differs from the exact synchronization candidate".into());
        }
        write_new(
            &root.join("native-output.kicad_pcb"),
            native.output_board().as_bytes(),
        )?;
        let report = serde_json::json!({"native":native.report(),"context":runner.execution_context().ok_or("missing refill context")?.digest(),
            "input_sha256":native.input_sha256(),"output_sha256":hash(native.output_board().as_bytes())});
        write_new(
            &root.join("native-evidence.json"),
            &serde_json::to_vec(&report).map_err(|e| e.to_string())?,
        )?;
        let tree = tree_names(&root)?;
        for path in &tree {
            if path.is_file() {
                inputs.track(path)?;
            }
        }
        outputs.push(native.output_board().to_owned());
        let mut report = report;
        report["artifacts"] = serde_json::json!({"input":board,"native_output":root.join("native-output.kicad_pcb"),"evidence":root.join("native-evidence.json")});
        reports.push(report);
        runners.push(runner);
        trees.push((root, tree));
        owners.push(owner);
        audit.verify()?;
    }
    if seeds[0] != seeds[1] {
        return Err("independent refill configuration seeds differ".into());
    }
    let candidate = ClosedRefillCandidate::compose_for_project_root(
        candidate,
        &outputs[0],
        &outputs[1],
        audit
            .inner
            .resource_root
            .to_str()
            .ok_or("non-UTF8 refill resource root")?,
    )?;
    let evidence = serde_json::json!({"schema_version":1,"recipe":"frozen-private-pcb-refill-v1","configuration_seed":seeds[0],
        "native_executions":reports,"content":candidate.evidence(),"source_files_read_only":true,"publication_authorized":false});
    let result = SynchronizationRegionRefill {
        candidate,
        inputs,
        runners,
        trees,
        evidence,
        owners,
    };
    result.verify(audit)?;
    Ok(result)
}
