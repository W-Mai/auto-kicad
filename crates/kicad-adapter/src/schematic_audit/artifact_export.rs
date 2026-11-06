//! Read-only native artifact candidates bound to complete source and resource authority.
use super::*;
use kicad_schematic::NativeArtifactRecipe;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const INVENTORY_WORKER: &str = include_str!("../../helpers/manufacturing_inventory.py");
const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactFormat {
    Gerber,
    Drill,
    Bom,
    SchematicPdf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactFile {
    pub path: String,
    pub format: ArtifactFormat,
    pub bytes: u64,
    pub sha256: String,
}

/// Owns generated private bytes and their immutable native/source provenance, never user approval.
pub struct NativeArtifactExport {
    original: SchematicAudit,
    observed: SchematicAudit,
    inputs: Inputs,
    native_inputs: Inputs,
    native_configuration: PathBuf,
    native_project: FrozenExportProject,
    project_source_inputs: Inputs,
    files: Vec<ArtifactFile>,
    root: PathBuf,
    tree: BTreeSet<PathBuf>,
    evidence: Value,
    digest: String,
    evidence_path: PathBuf,
    _owner: runtime::owned_temp::TempDir,
}

fn input_bytes(input: &InputFile) -> Result<&[u8], String> {
    input
        .bytes
        .as_deref()
        .ok_or_else(|| "required artifact input is missing".into())
}

/// Native export preferences belong to a worker copy, never the verified source audit.
struct ExportProject {
    root: PathBuf,
    source: PathBuf,
    preferences: PathBuf,
    inputs: Inputs,
    tree: BTreeSet<PathBuf>,
    preference_seed: InputFile,
}

#[derive(Serialize)]
struct FrozenExportProject {
    root: PathBuf,
    inputs: Inputs,
    tree: BTreeSet<PathBuf>,
}

impl ExportProject {
    fn copy(
        original: &Path,
        source: &Path,
        destination: &Path,
        originals: &mut Inputs,
    ) -> Result<Self, String> {
        let relative = source.strip_prefix(original).map_err(|e| e.to_string())?;
        if relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err("export schematic is outside its captured project".into());
        }
        fs::create_dir(destination).map_err(|e| e.to_string())?;
        let source = destination.join(relative);
        let preferences = source.with_extension("kicad_prl");
        let mut inputs = Inputs::default();
        for path in tree_names(original)? {
            let target = destination.join(path.strip_prefix(original).map_err(|e| e.to_string())?);
            if path.is_dir() {
                fs::create_dir_all(&target).map_err(|e| e.to_string())?;
            } else {
                write_new(&target, input_bytes(originals.track(&path)?)?)?;
                if target != preferences {
                    inputs.track(&target)?;
                }
            }
        }
        let preference_seed = Inputs::read(&preferences)?;
        let tree = tree_names(destination)?
            .into_iter()
            .filter(|path| path != &preferences)
            .collect();
        let result = Self {
            root: destination.into(),
            source,
            preferences,
            inputs,
            tree,
            preference_seed,
        };
        originals.verify()?;
        result.verify()?;
        Ok(result)
    }

    fn verify(&self) -> Result<(), String> {
        self.inputs.verify()?;
        if tree_names(&self.root)?
            .into_iter()
            .filter(|path| path != &self.preferences)
            .collect::<BTreeSet<_>>()
            != self.tree
        {
            return Err("native export changed the worker project namespace".into());
        }
        if let Some(bytes) = Inputs::read(&self.preferences)?.bytes {
            kicad_checks::validate_json_structure(&bytes).map_err(|e| e.to_string())?;
            serde_json::from_slice::<BTreeMap<String, Box<RawValue>>>(&bytes)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn freeze(self) -> Result<FrozenExportProject, String> {
        self.verify()?;
        let tree = tree_names(&self.root)?;
        let mut inputs = Inputs::default();
        for path in &tree {
            if path.is_file() {
                inputs.track(path)?;
            }
        }
        let result = FrozenExportProject {
            root: self.root,
            inputs,
            tree,
        };
        result.verify()?;
        Ok(result)
    }
}

impl FrozenExportProject {
    fn verify(&self) -> Result<(), String> {
        self.inputs.verify()?;
        if tree_names(&self.root)? != self.tree {
            return Err("sealed export project namespace changed".into());
        }
        Ok(())
    }
}

impl SchematicAudit {
    /// Generate all selected formats privately using exact captured CAD inputs.
    pub fn prepare_artifact_export(
        &self,
        formats: &[ArtifactFormat],
    ) -> Result<NativeArtifactExport, String> {
        self.verify()?;
        let selected = formats.iter().copied().collect::<BTreeSet<_>>();
        if selected.is_empty() || selected.len() != formats.len() || formats.len() > 4 {
            return Err("artifact formats must be an explicit distinct nonempty subset".into());
        }
        let mut inputs = Inputs::default();
        let board_path = self.inner.policy.schematic.with_extension("kicad_pcb");
        let pcb_selected =
            selected.contains(&ArtifactFormat::Gerber) || selected.contains(&ArtifactFormat::Drill);
        let mut candidates = BTreeMap::new();
        let relative = |path: &Path| -> Result<String, String> {
            path.strip_prefix(&self.inner.policy.project)
                .map_err(|e| e.to_string())?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "non-UTF8 artifact input".into())
        };
        if pcb_selected {
            let captured = Inputs::read(&board_path)?;
            let bytes = input_bytes(&captured)?;
            if bytes.len() > 32 * 1024 * 1024 {
                return Err("artifact PCB exceeds bound".into());
            }
            candidates.insert(relative(&board_path)?, bytes.to_vec());
            inputs.track(&board_path)?;
        }
        let original_project = self.inner.policy.schematic.with_extension("kicad_pro");
        let project = Inputs::read(&original_project)?;
        let project_bytes = input_bytes(&project)?;
        capture_drawing_sheet(self, project_bytes, &mut inputs)?;
        if selected.contains(&ArtifactFormat::SchematicPdf) {
            reject_external_fonts(self)?;
        }
        candidates.insert(
            relative(&original_project)?,
            crate::native_project::project_with_root(
                project_bytes,
                self.inner
                    .resource_root
                    .to_str()
                    .ok_or("non-UTF8 artifact resource root")?,
            )?,
        );
        let observed = self.observe_candidate_files(&candidates)?;
        if observed.netlist() != self.netlist() {
            return Err("artifact context changes complete native source membership".into());
        }
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-artifact-export-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let owned = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let root = owned.join("artifacts");
        fs::create_dir(&root).map_err(|e| e.to_string())?;
        let mut project_source_inputs = Inputs::default();
        let project = ExportProject::copy(
            &observed.inner.policy.project,
            &observed.inner.policy.schematic,
            &owned.join("project"),
            &mut project_source_inputs,
        )?;
        let project_seed = serde_json::json!({"inputs":project.inputs,"tree":project.tree,"local_preferences":project.preference_seed});
        let native_root = &project.root;
        let native_source = &project.source;
        let native_board = native_source.with_extension("kicad_pcb");
        // Export commands may persist their own preferences. Seed an isolated configuration from
        // the verified audit without granting mutations to the immutable audit configuration.
        let configuration = owned.join("configuration");
        let native_configuration = configuration.join("10.0");
        fs::create_dir_all(&native_configuration).map_err(|e| e.to_string())?;
        for path in &observed.inner.private_config_names {
            let captured = observed
                .inner
                .private_inputs
                .files
                .get(path)
                .ok_or("missing captured export configuration")?;
            write_new(
                &native_configuration
                    .join(path.file_name().ok_or("missing configuration filename")?),
                input_bytes(captured)?,
            )?;
        }
        let initial_configuration = hash(
            &serde_json::to_vec(
                &observed
                    .inner
                    .private_config_names
                    .iter()
                    .map(|path| {
                        let captured = observed
                            .inner
                            .private_inputs
                            .files
                            .get(path)
                            .expect("captured configuration");
                        (
                            path.file_name()
                                .expect("configuration filename")
                                .to_string_lossy()
                                .into_owned(),
                            captured.digest.clone(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            )
            .map_err(|e| e.to_string())?,
        );
        let runner = observed
            .inner
            .runner
            .clone()
            .freeze_context(Some(&configuration))
            .map_err(|e| e.to_string())?;
        let physical = if pcb_selected {
            inputs.track(Path::new(PYTHON))?;
            Some(manufacturing_inventory(
                native_root,
                &runner,
                &native_board,
                &owned,
            )?)
        } else {
            None
        };
        let copper = physical
            .as_ref()
            .map(|value| -> Result<Vec<String>, String> {
                let layers = value["copper_layers"]
                    .as_array()
                    .ok_or("missing native copper stack")?;
                layers
                    .iter()
                    .map(|layer| {
                        layer
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "invalid native copper layer".into())
                    })
                    .collect()
            })
            .transpose()?
            .unwrap_or_default();
        let mut files = Vec::new();
        for format in &selected {
            let (destination, recipe, input) = match format {
                ArtifactFormat::Gerber => {
                    let directory = root.join("gerber");
                    fs::create_dir(&directory).map_err(|e| e.to_string())?;
                    let mut layers = copper.clone();
                    layers.extend(
                        ["F.Mask", "B.Mask", "F.SilkS", "B.SilkS", "Edge.Cuts"].map(str::to_owned),
                    );
                    (
                        directory,
                        NativeArtifactRecipe::Gerber { layers },
                        native_board.as_path(),
                    )
                }
                ArtifactFormat::Drill => {
                    let directory = root.join("drill");
                    fs::create_dir(&directory).map_err(|e| e.to_string())?;
                    (
                        directory,
                        NativeArtifactRecipe::Drill,
                        native_board.as_path(),
                    )
                }
                ArtifactFormat::Bom => (
                    root.join("bom.csv"),
                    NativeArtifactRecipe::Bom,
                    native_source.as_path(),
                ),
                ArtifactFormat::SchematicPdf => (
                    root.join("schematic.pdf"),
                    NativeArtifactRecipe::SchematicPdf,
                    native_source.as_path(),
                ),
            };
            self.verify()?;
            inputs.verify()?;
            observed.verify()?;
            project.verify()?;
            project_source_inputs.verify()?;
            runner
                .run_artifact_export(native_root, input, &destination, &recipe)
                .map_err(|error| {
                    format!(
                        "{format:?} native export failed: {}",
                        format!("{error:?}").chars().take(8192).collect::<String>()
                    )
                })?;
            self.verify()?;
            inputs.verify()?;
            observed.verify()?;
            project.verify()?;
            project_source_inputs.verify()?;
            let paths = if destination.is_dir() {
                plain_files(&destination)?
            } else {
                vec![destination]
            };
            if paths.is_empty() {
                return Err("native artifact recipe produced no files".into());
            }
            for path in paths {
                let bytes = bounded_artifact(&path)?;
                validate_artifact(*format, &path, &bytes, self)?;
                files.push(ArtifactFile {
                    path: path
                        .strip_prefix(&root)
                        .map_err(|e| e.to_string())?
                        .to_str()
                        .ok_or("non-UTF8 artifact filename")?
                        .into(),
                    format: *format,
                    bytes: bytes.len() as u64,
                    sha256: hash(&bytes),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if files.len() > 512 || files.iter().map(|file| file.bytes).sum::<u64>() > 256 * 1024 * 1024
        {
            return Err("complete artifact inventory exceeds bound".into());
        }
        let mut native_inputs = Inputs::default();
        for path in config_names(&native_configuration)? {
            native_inputs.track(&path)?;
        }
        let native_project = project.freeze()?;
        let tree = artifact_tree(&root)?;
        let evidence = serde_json::json!({"schema_version":1,"native_version":"10.0.7","formats":selected,"files":files,
            "source_revision":self.revision(),"source":self.evidence()?,"native_source":observed.evidence()?,
            "additional_inputs":inputs,"export_configuration_seed":initial_configuration,"export_configuration_final":native_inputs,"export_project_seed":project_seed,"export_project_final":native_project,"export_project_sources":project_source_inputs,"physical":physical,"manufacturing_worker_sha256":hash(INVENTORY_WORKER.as_bytes()),
            "native_execution":runner.execution_context().ok_or("missing artifact native execution context")?.digest(),
            "sheet_count":self.netlist().sheets.len(),"source_files_immutable":true,"publication_authorized":false});
        let evidence_bytes = serde_json::to_vec(&evidence).map_err(|e| e.to_string())?;
        let digest = hash(&evidence_bytes);
        let evidence_path = owned.join("native-evidence.json");
        write_new(&evidence_path, &evidence_bytes)?;
        let prepared = NativeArtifactExport {
            original: self.clone(),
            observed,
            inputs,
            native_inputs,
            native_configuration,
            native_project,
            project_source_inputs,
            files,
            root,
            tree,
            evidence,
            digest,
            evidence_path,
            _owner: owner,
        };
        prepared.verify()?;
        Ok(prepared)
    }
}

impl NativeArtifactExport {
    pub fn files(&self) -> &[ArtifactFile] {
        &self.files
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence_reference(&self) -> Result<Value, String> {
        self.verify()?;
        Ok(
            serde_json::json!({"path":self.evidence_path,"sha256":self.digest,"bytes":fs::metadata(&self.evidence_path).map_err(|e|e.to_string())?.len()}),
        )
    }

    pub fn source_revision(&self) -> &str {
        self.original.revision()
    }
    pub fn source_dependencies(&self) -> Result<Value, String> {
        self.verify()?;
        let context = self
            .original
            .inner
            .runner
            .execution_context()
            .ok_or("missing source execution context")?;
        let native = InputFile {
            requested: context.executable().into(),
            canonical: Some(context.executable().into()),
            digest: Some(context.executable_digest().into()),
            size: fs::metadata(context.executable())
                .map_err(|e| e.to_string())?
                .len(),
            bytes: None,
            limit: MAX_FILE,
        };
        Ok(
            serde_json::json!({"source":self.original.inner.source,"additional":self.inputs,"native_executable":native,"inherited_environment_sha256":context.inherited_environment_digest(),
            "configuration_root":self.original.inner.source_config,"configuration_files":self.original.inner.source_config_names}),
        )
    }

    pub fn verify(&self) -> Result<(), String> {
        self.original.verify()?;
        self.observed.verify()?;
        self.inputs.verify()?;
        self.native_inputs.verify()?;
        self.native_project.verify()?;
        self.project_source_inputs.verify()?;
        if config_names(&self.native_configuration)?
            != self.native_inputs.files.keys().cloned().collect::<Vec<_>>()
        {
            return Err("export configuration namespace changed".into());
        }
        if hash(&bounded_artifact(&self.evidence_path)?) != self.digest {
            return Err("native artifact evidence changed".into());
        }

        for file in &self.files {
            let bytes = bounded_artifact(&self.root.join(&file.path))?;
            if bytes.len() as u64 != file.bytes || hash(&bytes) != file.sha256 {
                return Err("artifact content changed".into());
            }
        }
        if artifact_tree(&self.root)? != self.tree {
            return Err("artifact namespace changed".into());
        }
        Ok(())
    }
    pub fn read_artifact(&self, file: &ArtifactFile) -> Result<Vec<u8>, String> {
        if !self.files.contains(file) {
            return Err("artifact is not in the exact approved inventory".into());
        }
        let bytes = bounded_artifact(&self.root.join(&file.path))?;
        if bytes.len() as u64 != file.bytes || hash(&bytes) != file.sha256 {
            return Err("artifact bytes changed".into());
        }
        Ok(bytes)
    }
}

fn plain_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > 4 {
            return Err("artifact directory depth exceeds bound".into());
        }
        for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push((path, depth + 1));
            } else if kind.is_file() {
                files.push(path);
            } else {
                return Err("artifact contains a symlink or special file".into());
            }
            if files.len() + pending.len() > 512 {
                return Err("artifact file count exceeds bound".into());
            }
        }
    }
    files.sort();
    Ok(files)
}
fn artifact_tree(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    plain_files(root)?
        .into_iter()
        .map(|path| {
            path.strip_prefix(root)
                .map(Path::to_path_buf)
                .map_err(|e| e.to_string())
        })
        .collect()
}
fn bounded_artifact(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 * 1024 {
        return Err("artifact is missing, empty or exceeds bound".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("artifact changed while reading".into());
    }
    Ok(bytes)
}

fn validate_artifact(
    format: ArtifactFormat,
    path: &Path,
    bytes: &[u8],
    audit: &SchematicAudit,
) -> Result<(), String> {
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match format {
        ArtifactFormat::Gerber if suffix == "gbrjob" => {
            serde_json::from_slice::<Value>(bytes).map_err(|e| e.to_string())?;
        }
        ArtifactFormat::Gerber => {
            let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
            if suffix != "gbr"
                || !text.contains("%MOMM*%")
                || !text.contains("M02*")
                || !text.contains("TF.FileFunction,")
            {
                return Err("Gerber lacks native mm/X2/completion evidence".into());
            }
        }
        ArtifactFormat::Drill => {
            let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
            if suffix != "drl"
                || !text.starts_with("M48")
                || !text.contains("METRIC")
                || !text.contains("M30")
            {
                return Err("drill output lacks complete native Excellon/mm evidence".into());
            }
        }
        ArtifactFormat::Bom => {
            let rows = csv_rows(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)?;
            if rows.first()
                != Some(
                    &["Reference", "Value", "Footprint", "Quantity", "DNP"]
                        .map(str::to_owned)
                        .to_vec(),
                )
            {
                return Err("BOM field order differs from the fixed recipe".into());
            }
            let mut source_flags = BTreeMap::new();
            for file in &audit.inner.parsed.files {
                for symbol in &file.symbols {
                    if source_flags
                        .insert(
                            (
                                file.artifact.relative_path.as_str(),
                                symbol.source_uuid.as_str(),
                            ),
                            (symbol.in_bom.unwrap_or(true), symbol.dnp.unwrap_or(false)),
                        )
                        .is_some()
                    {
                        return Err("duplicate BOM source-unit identity".into());
                    }
                }
            }
            let mut expected = BTreeSet::new();
            for component in audit.identities().components() {
                if component_bom_flags(component, &source_flags)? == (true, false) {
                    expected.insert(component.reference.clone());
                }
            }
            let mut references = BTreeSet::new();
            for row in rows.iter().skip(1) {
                if row.len() != 5 || row[3] != "1" || !references.insert(row[0].clone()) {
                    return Err("BOM grouping/reference inventory differs from recipe".into());
                }
                let component = audit
                    .netlist()
                    .components
                    .iter()
                    .find(|component| component.reference == row[0])
                    .ok_or("BOM contains an unknown component")?;
                if row[1] != component.value
                    || row[2] != component.footprint.as_deref().unwrap_or("")
                    || !matches!(row[4].as_str(), "" | "0" | "No" | "no" | "false")
                {
                    return Err(
                        "BOM value/footprint/DNP differs from captured native source".into(),
                    );
                }
            }
            if references != expected {
                return Err(
                    "BOM omits or includes source components contrary to captured BOM/DNP flags"
                        .into(),
                );
            }
        }
        ArtifactFormat::SchematicPdf => {
            if suffix != "pdf"
                || !bytes.starts_with(b"%PDF-")
                || !bytes.windows(5).any(|part| part == b"%%EOF")
                || bytes.len() < 256
            {
                return Err("PDF is not a complete native document".into());
            }
        }
    }
    Ok(())
}

fn component_bom_flags(
    component: &kicad_schematic::identities::ComponentIdentity,
    source_flags: &BTreeMap<(&str, &str), (bool, bool)>,
) -> Result<(bool, bool), String> {
    let flags = component
        .units
        .iter()
        .map(|unit| {
            source_flags
                .get(&(unit.file.as_str(), unit.source_uuid.as_str()))
                .copied()
                .ok_or_else(|| "BOM component has no complete captured source flags".to_owned())
        })
        .collect::<Result<BTreeSet<_>, String>>()?;
    if flags.len() != 1 {
        return Err("BOM units have inconsistent or missing exclusion flags".into());
    }
    flags
        .first()
        .copied()
        .ok_or_else(|| "BOM component has no source units".into())
}

fn csv_rows(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut end_quote = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                    end_quote = true;
                }
            } else {
                field.push(character);
            }
        } else {
            match character {
                '"' if field.is_empty() && !end_quote => quoted = true,
                ',' => {
                    row.push(std::mem::take(&mut field));
                    end_quote = false;
                }
                '\n' | '\r' => {
                    if character == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    end_quote = false;
                }
                _ if end_quote || character == '"' => {
                    return Err("invalid native BOM CSV quoting".into());
                }
                _ => field.push(character),
            }
        }
        if rows.len() > 100000 || field.len() > 65536 {
            return Err("BOM row/field exceeds bounds".into());
        }
    }
    if quoted {
        return Err("unfinished native BOM CSV field".into());
    }
    if !row.is_empty() || !field.is_empty() || end_quote {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn capture_drawing_sheet(
    audit: &SchematicAudit,
    project: &[u8],
    inputs: &mut Inputs,
) -> Result<(), String> {
    let root = serde_json::from_slice::<BTreeMap<String, Box<RawValue>>>(project)
        .map_err(|e| e.to_string())?;
    let Some(schematic) = root.get("schematic") else {
        return Ok(());
    };
    let fields = serde_json::from_str::<BTreeMap<String, Box<RawValue>>>(schematic.get())
        .map_err(|e| e.to_string())?;
    let Some(raw) = fields.get("drawing_sheet_file") else {
        return Ok(());
    };
    let value = serde_json::from_str::<String>(raw.get()).map_err(|e| e.to_string())?;
    if value.is_empty() {
        return Ok(());
    }
    let resource = value.replace(
        "${KIPRJMOD}",
        audit
            .inner
            .resource_root
            .to_str()
            .ok_or("non-UTF8 drawing-sheet root")?,
    );
    if resource.contains("${") {
        return Err("drawing-sheet variable cannot be bound to captured resources".into());
    }
    let path = PathBuf::from(resource);
    let path = if path.is_absolute() {
        path
    } else {
        audit.inner.resource_root.join(path)
    };
    let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !canonical.starts_with(&audit.inner.policy.project)
        && !audit
            .inner
            .policy
            .extra_roots
            .iter()
            .any(|root| canonical.starts_with(root))
    {
        return Err("custom drawing sheet is outside authorized resource roots".into());
    }
    inputs.track(&canonical)?;
    Ok(())
}

fn reject_external_fonts(audit: &SchematicAudit) -> Result<(), String> {
    fn inspect(node: &kiutils_sexpr::Node) -> Result<(), String> {
        if let kiutils_sexpr::Node::List { items, .. } = node {
            fn atom(node: &kiutils_sexpr::Node) -> Option<&str> {
                match node {
                    kiutils_sexpr::Node::Atom {
                        atom:
                            kiutils_sexpr::Atom::Symbol(value) | kiutils_sexpr::Atom::Quoted(value),
                        ..
                    } => Some(value.as_str()),
                    _ => None,
                }
            }
            if items.first().and_then(atom) == Some("face")
                && items
                    .get(1)
                    .and_then(atom)
                    .is_some_and(|name| !name.is_empty() && name != "KiCad Font")
            {
                return Err("external PDF font requires captured font-file authority".into());
            }
            for child in items {
                inspect(child)?;
            }
        }
        Ok(())
    }
    for file in &audit.inner.parsed.files {
        let input = audit
            .inner
            .source
            .files
            .get(
                &audit
                    .inner
                    .policy
                    .project
                    .join(&file.artifact.relative_path),
            )
            .ok_or("missing PDF source")?;
        let bytes = input_bytes(input)?;
        let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
        for node in &kiutils_sexpr::parse_one(text)
            .map_err(|e| e.to_string())?
            .nodes
        {
            inspect(node)?;
        }
    }
    Ok(())
}

fn manufacturing_inventory(
    project_root: &Path,
    runner: &NetlistRunner,
    board: &Path,
    root: &Path,
) -> Result<Value, String> {
    let helper = root.join("manufacturing-inventory.py");
    write_new(&helper, INVENTORY_WORKER.as_bytes())?;
    let output = root.join("manufacturing.json");
    let stderr = root.join("manufacturing.log");
    let context = runner
        .execution_context()
        .ok_or("missing manufacturing context")?;
    context.verify().map_err(|e| e.to_string())?;
    let mut command = Command::new(PYTHON);
    context.apply(&mut command, project_root);
    command
        .args(["-I", "-B"])
        .arg(&helper)
        .arg(board)
        .stdout(Stdio::from(
            fs::File::create(&output).map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(
            fs::File::create(&stderr).map_err(|e| e.to_string())?,
        ));
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let result = (|| -> Result<(), String> {
        loop {
            for (path, limit) in [(&output, 64 * 1024 * 1024), (&stderr, 1024 * 1024)] {
                if fs::metadata(path).map_err(|e| e.to_string())?.len() > limit {
                    return Err("manufacturing reader output exceeds bound".into());
                }
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if status.success() {
                    return Ok(());
                }
                return Err("native manufacturing reader failed".into());
            }
            if started.elapsed() > Duration::from_secs(120) {
                return Err("native manufacturing reader timed out".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    result?;
    context.verify().map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_slice(&fs::read(&output).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if value["native_version"] != "10.0.7"
        || value["board_sha256"] != hash(&fs::read(board).map_err(|e| e.to_string())?)
    {
        return Err("native manufacturing inventory differs from captured board".into());
    }
    Ok(value)
}

#[cfg(test)]
mod project_isolation_tests {
    use super::*;

    #[test]
    fn bom_flags_require_every_resolved_unit_in_its_exact_source_file() {
        use kicad_schematic::identities::{ComponentIdentity, SymbolUnitIdentity};
        let component = ComponentIdentity {
            reference: "U1".into(),
            sheet_path: "/root".into(),
            units: (1..=3)
                .map(|unit| SymbolUnitIdentity {
                    file: "child.kicad_sch".into(),
                    sheet_path: "/root".into(),
                    source_uuid: format!("00000000-0000-4000-8000-{unit:012}"),
                    reference: "U1".into(),
                    unit,
                })
                .collect(),
        };
        let mut flags = component
            .units
            .iter()
            .map(|unit| {
                (
                    (unit.file.as_str(), unit.source_uuid.as_str()),
                    (true, false),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            component_bom_flags(&component, &flags).unwrap(),
            (true, false)
        );
        let key = (
            component.units[2].file.as_str(),
            component.units[2].source_uuid.as_str(),
        );
        flags.insert(key, (true, true));
        assert!(component_bom_flags(&component, &flags).is_err());
        flags.values_mut().for_each(|value| *value = (false, false));
        assert_eq!(
            component_bom_flags(&component, &flags).unwrap(),
            (false, false)
        );
        flags.values_mut().for_each(|value| *value = (true, true));
        assert_eq!(
            component_bom_flags(&component, &flags).unwrap(),
            (true, true)
        );
        flags.remove(&key);
        assert!(component_bom_flags(&component, &flags).is_err());
        flags.insert(key, (true, true));
        let mut foreign = component.clone();
        foreign.units[0].file = "other.kicad_sch".into();
        assert!(component_bom_flags(&foreign, &flags).is_err());
    }

    #[test]
    fn export_worker_preferences_are_exactly_scoped_and_every_final_input_is_sealed() {
        let owner = tempfile::tempdir().unwrap();
        let original = owner.path().join("original");
        fs::create_dir(&original).unwrap();
        let source = original.join("design.kicad_sch");
        let board = original.join("design.kicad_pcb");
        let preferences = original.join("design.kicad_prl");
        fs::write(&source, b"source").unwrap();
        fs::write(&board, b"board").unwrap();
        fs::write(&preferences, br#"{"native":0}"#).unwrap();
        let mut originals = Inputs::default();
        let project = ExportProject::copy(
            &original,
            &source,
            &owner.path().join("worker"),
            &mut originals,
        )
        .unwrap();
        fs::write(&project.preferences, br#"{"native":1}"#).unwrap();
        project.verify().unwrap();
        originals.verify().unwrap();
        assert_eq!(fs::read(&preferences).unwrap(), br#"{"native":0}"#);
        let worker_board = project.root.join("design.kicad_pcb");
        fs::write(&worker_board, b"changed").unwrap();
        assert!(project.verify().is_err());
        fs::write(&worker_board, b"board").unwrap();
        let unrelated = project.root.join("other.kicad_prl");
        fs::write(&unrelated, b"{}").unwrap();
        assert!(project.verify().is_err());
        fs::remove_file(unrelated).unwrap();
        fs::write(&project.preferences, b"[]").unwrap();
        assert!(project.verify().is_err());
        fs::write(&project.preferences, b"{}").unwrap();
        #[cfg(unix)]
        {
            fs::remove_file(&project.preferences).unwrap();
            std::os::unix::fs::symlink(&preferences, &project.preferences).unwrap();
            assert!(project.verify().is_err());
            fs::remove_file(&project.preferences).unwrap();
            fs::write(&project.preferences, b"{}").unwrap();
        }
        let worker_preferences = project.preferences.clone();
        let sealed = project.freeze().unwrap();
        sealed.verify().unwrap();
        fs::write(worker_preferences, br#"{"later":true}"#).unwrap();
        assert!(sealed.verify().is_err());
        fs::write(&board, b"later source edit").unwrap();
        assert!(originals.verify().is_err());
        fs::remove_file(&preferences).unwrap();
        let mut absent_originals = Inputs::default();
        let initialized = ExportProject::copy(
            &original,
            &source,
            &owner.path().join("initialized"),
            &mut absent_originals,
        )
        .unwrap();
        assert!(!initialized.preferences.exists());
        fs::write(&initialized.preferences, b"{}").unwrap();
        initialized.verify().unwrap();
        initialized.freeze().unwrap().verify().unwrap();
        absent_originals.verify().unwrap();
    }
}
