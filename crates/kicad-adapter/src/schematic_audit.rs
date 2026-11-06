//! Private native schematic observations with bound source and library inputs.
pub mod artifact_export;
mod assignment;
mod preview;
mod source_transition;
pub mod synchronization;
use crate::library_catalog::{
    FootprintCatalog, LibraryCatalogPolicy, SchematicFootprintContext,
    symbols::{SymbolCatalog, SymbolLibraryContext},
};
use crate::library_table::{LibraryTableEntry, TableKind, render_library_rows};
use kicad_checks::erc::CompleteErcReport;
use kicad_model::FootprintLibraryIdentifier;
use kicad_schematic::{
    Limits, NativeExecutionContext, NetlistDocument, NetlistRunner, ParsedSchematicProject,
    identities::SchematicIdentityIndex, load_schematic_project,
};
pub use preview::{
    ConnectivityExpectation, ErcExpectation, MatchedSchematicPreview, NoConnectMarker,
    SchematicEditExpectation, SchematicEditPreview, SchematicPublicationCheck,
    SchematicPublicationWrite, VerifiedSchematicPublication,
};
use serde::Serialize;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const MAX_FILE: u64 = 32 * 1024 * 1024;
const MAX_TOTAL: u64 = 128 * 1024 * 1024;
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn canonical_dir(path: &Path) -> Result<PathBuf, String> {
    let p = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !path.is_absolute() || p != path || !p.is_dir() {
        return Err("audit directories must be canonical".into());
    }
    Ok(p)
}

/// Server-owned locations; request payloads must not supply native resource roots.
#[derive(Clone, Debug)]
pub struct SchematicAuditPolicy {
    project: PathBuf,
    schematic: PathBuf,
    configuration: PathBuf,
    symbols: PathBuf,
    footprints: PathBuf,
    extra_roots: Vec<PathBuf>,
}
impl SchematicAuditPolicy {
    pub fn project_dir(&self) -> &Path {
        &self.project
    }
    pub fn new(
        project: &Path,
        schematic: &Path,
        configuration: &Path,
        symbols: &Path,
        footprints: &Path,
        extra_roots: &[PathBuf],
    ) -> Result<Self, String> {
        let project = canonical_dir(project)?;
        let source = fs::canonicalize(schematic).map_err(|e| e.to_string())?;
        if source != schematic
            || !source.starts_with(&project)
            || source.extension().and_then(|s| s.to_str()) != Some("kicad_sch")
        {
            return Err("root schematic must be canonical and confined".into());
        }
        Ok(Self {
            project,
            schematic: source,
            configuration: canonical_dir(configuration)?,
            symbols: canonical_dir(symbols)?,
            footprints: canonical_dir(footprints)?,
            extra_roots: extra_roots
                .iter()
                .map(|p| canonical_dir(p))
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Clone, Serialize)]
struct InputFile {
    requested: PathBuf,
    canonical: Option<PathBuf>,
    digest: Option<String>,
    size: u64,
    #[serde(skip)]
    bytes: Option<Vec<u8>>,
    #[serde(skip)]
    limit: u64,
}
#[derive(Clone, Default, Serialize)]
struct Inputs {
    files: BTreeMap<PathBuf, InputFile>,
    total: u64,
    #[serde(skip)]
    synchronization: bool,
}
impl Inputs {
    fn synchronization() -> Self {
        Self {
            synchronization: true,
            ..Self::default()
        }
    }
    fn read(path: &Path) -> Result<InputFile, String> {
        Self::read_bounded(path, MAX_FILE)
    }
    fn read_bounded(path: &Path, limit: u64) -> Result<InputFile, String> {
        if !path.is_absolute() {
            return Err("audit input path must be absolute".into());
        }
        let canonical = match fs::canonicalize(path) {
            Ok(p) => p,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && fs::symlink_metadata(path).is_err() =>
            {
                return Ok(InputFile {
                    requested: path.into(),
                    canonical: None,
                    digest: None,
                    size: 0,
                    bytes: None,
                    limit,
                });
            }
            Err(e) => return Err(e.to_string()),
        };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options.open(&canonical).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > limit {
            return Err("audit input exceeds regular-file bounds".into());
        }
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != metadata.len()
            || bytes.len() as u64 > limit
            || fs::canonicalize(path).map_err(|e| e.to_string())? != canonical
        {
            return Err("audit input changed while reading".into());
        }
        Ok(InputFile {
            requested: path.into(),
            canonical: Some(canonical),
            digest: Some(hash(&bytes)),
            size: bytes.len() as u64,
            bytes: Some(bytes),
            limit,
        })
    }
    fn track(&mut self, path: &Path) -> Result<&InputFile, String> {
        if !self.files.contains_key(path) {
            if self.files.len() >= 8192 {
                return Err("audit input count exceeds bound".into());
            }
            let limit = if self.synchronization {
                match path.extension().and_then(|value| value.to_str()) {
                    Some("kicad_pcb") => {
                        kicad_checks::connectivity::SavedBoardProfile::Synchronization.board_bytes()
                    }
                    Some("json") => kicad_checks::connectivity::SavedBoardProfile::Synchronization
                        .report_bytes(),
                    _ => MAX_FILE,
                }
            } else {
                MAX_FILE
            };
            let file = Self::read_bounded(path, limit)?;
            let total = self
                .total
                .checked_add(file.size)
                .ok_or("audit byte count overflow")?;
            let max_total = if self.synchronization {
                1024 * 1024 * 1024
            } else {
                MAX_TOTAL
            };
            if total > max_total {
                return Err("audit input bytes exceed bound".into());
            }
            self.total = total;
            self.files.insert(path.into(), file);
        }
        Ok(&self.files[path])
    }
    fn copy(&mut self, source: &Path, target: &Path) -> Result<(), String> {
        if let Some(bytes) = &self.track(source)?.bytes {
            write_new(target, bytes)?;
        }
        Ok(())
    }
    fn copy_expected(
        &mut self,
        source: &Path,
        target: &Path,
        size: u64,
        digest: Option<&str>,
    ) -> Result<(), String> {
        let file = self.track(source)?;
        if file.size != size || file.digest.as_deref() != digest {
            return Err("resource changed between resolution and snapshot".into());
        }
        self.copy(source, target)
    }
    fn verify(&self) -> Result<(), String> {
        for (path, expected) in &self.files {
            let actual = Self::read_bounded(path, expected.limit)?;
            if actual.canonical != expected.canonical
                || actual.digest != expected.digest
                || actual.size != expected.size
            {
                return Err(format!("audit input changed: {}", path.display()));
            }
        }
        Ok(())
    }
    fn digest(&self) -> Result<String, String> {
        Ok(hash(&serde_json::to_vec(self).map_err(|e| e.to_string())?))
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("missing destination parent")?)
        .map_err(|e| e.to_string())?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| e.to_string())
}
fn config_names(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json")
            || path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.ends_with("lib-table"))
        {
            names.push(path);
        }
        if names.len() > 64 {
            return Err("native configuration file count exceeds bound".into());
        }
    }
    names.sort();
    Ok(names)
}
fn english_config(bytes: &[u8]) -> Result<Vec<u8>, String> {
    kicad_checks::validate_json_structure(bytes).map_err(|e| e.to_string())?;
    let mut root: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let mut system: BTreeMap<String, Box<RawValue>> = root
        .get("system")
        .map(|v| serde_json::from_str(v.get()))
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    system.insert(
        "language".into(),
        RawValue::from_string("\"English\"".into()).map_err(|e| e.to_string())?,
    );
    root.insert(
        "system".into(),
        RawValue::from_string(serde_json::to_string(&system).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    );
    serde_json::to_vec_pretty(&root).map_err(|e| e.to_string())
}
fn variables(
    context: &NativeExecutionContext,
    common: &Value,
    project: &Value,
    policy: &SchematicAuditPolicy,
) -> Result<BTreeMap<String, String>, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::from([
        (
            "KICAD10_SYMBOL_DIR".into(),
            policy
                .symbols
                .to_str()
                .ok_or("non-UTF8 symbol path")?
                .into(),
        ),
        (
            "KICAD10_FOOTPRINT_DIR".into(),
            policy
                .footprints
                .to_str()
                .ok_or("non-UTF8 footprint path")?
                .into(),
        ),
    ]);
    if let Some(map) = common
        .pointer("/environment/vars")
        .and_then(Value::as_object)
    {
        for (k, v) in map {
            if let Some(v) = v.as_str() {
                result.insert(k.clone(), v.into());
            }
        }
    }
    for name in context.environment_names() {
        if let Some(name) = name.to_str()
            && let Some(value) = context
                .environment_value(OsStr::new(name))
                .and_then(OsStr::to_str)
        {
            result.insert(name.into(), value.into());
        }
    }
    if let Some(map) = project.get("text_variables").and_then(Value::as_object) {
        for (k, v) in map {
            if let Some(v) = v.as_str() {
                result.insert(k.clone(), v.into());
            }
        }
    }
    result.remove("KIPRJMOD");
    result.retain(|k, v| {
        !k.is_empty()
            && !k.as_bytes()[0].is_ascii_digit()
            && k.len() <= 128
            && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            && Path::new(v).is_absolute()
            && !v.chars().any(|c| c.is_control() || matches!(c, '$' | '`'))
            && !v.contains("://")
    });
    Ok(result)
}
fn parse_json(bytes: Option<&Vec<u8>>) -> Result<Value, String> {
    bytes
        .map(|b| {
            kicad_checks::validate_json_structure(b).map_err(|e| e.to_string())?;
            serde_json::from_slice(b).map_err(|e| e.to_string())
        })
        .unwrap_or(Ok(serde_json::json!({})))
}
fn lexical(path: &Path) -> Result<PathBuf, String> {
    let mut result = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err("path escapes root".into());
                }
            }
            x => result.push(x.as_os_str()),
        }
    }
    Ok(result)
}
fn relocate(
    rows: Vec<LibraryTableEntry>,
    used: &BTreeMap<String, PathBuf>,
    project: &Path,
    kind: TableKind,
) -> Result<String, String> {
    let mut result = Vec::new();
    let project = project.to_str().ok_or("non-UTF8 project path")?;
    for mut row in rows {
        if let Some(path) = used.get(&row.nickname) {
            row.uri = path.to_str().ok_or("non-UTF8 copied resource")?.into();
        } else {
            row.uri = row
                .uri
                .replace("${KIPRJMOD}", project)
                .replace("$(KIPRJMOD)", project);
        }
        result.push(row);
    }
    render_library_rows(&result, kind)
}

/// Source-bound connectivity with private native witness evidence, not write approval.
pub struct SchematicConnectivityAudit {
    connectivity: kicad_schematic::probes::PinConnectivity,
    native: NetlistDocument,
    evidence: Value,
}
impl SchematicConnectivityAudit {
    pub fn connectivity(&self) -> &kicad_schematic::probes::PinConnectivity {
        &self.connectivity
    }
    pub fn native(&self) -> &NetlistDocument {
        &self.native
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
}

/// Native observation only. Pin-level proof and write approval are separate contracts.
#[derive(Clone)]
pub struct SchematicAudit {
    inner: Arc<SchematicAuditState>,
}

struct SchematicAuditState {
    owner: runtime::owned_temp::TempDir,
    source: Inputs,
    private_inputs: Inputs,
    source_config: PathBuf,
    source_config_names: Vec<PathBuf>,
    private_config: PathBuf,
    private_config_names: Vec<PathBuf>,
    initialized_local_preferences: Option<PathBuf>,
    policy: SchematicAuditPolicy,
    resource_root: PathBuf,
    symbols: SymbolCatalog,
    symbol_context: SymbolLibraryContext,
    footprints: FootprintCatalog,
    footprint_context: SchematicFootprintContext,
    footprint_ids: Vec<FootprintLibraryIdentifier>,
    private_trees: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    parsed: ParsedSchematicProject,
    runner: NetlistRunner,
    netlist: NetlistDocument,
    erc: CompleteErcReport,
    identities: SchematicIdentityIndex,
    pins: kicad_schematic::pins::SchematicPinIndex,
    revision: String,
    // Keep internally generated source trees until every audit owner is released.
    _source_owner: Option<Arc<runtime::owned_temp::TempDir>>,
}
impl SchematicAudit {
    pub fn revision(&self) -> &str {
        &self.inner.revision
    }
    pub fn netlist(&self) -> &NetlistDocument {
        &self.inner.netlist
    }
    pub fn footprint_catalog_digest(&self) -> &str {
        self.inner.footprints.digest()
    }
    pub fn erc(&self) -> &CompleteErcReport {
        &self.inner.erc
    }
    pub fn identities(&self) -> &SchematicIdentityIndex {
        &self.inner.identities
    }
    pub fn edit_inventory(&self) -> Result<Vec<Value>, String> {
        self.verify()?;
        use kiutils_sexpr::{Atom, Node, parse_one};
        let atom = |node: &Node| match node {
            Node::Atom {
                atom: Atom::Symbol(value) | Atom::Quoted(value),
                ..
            } => Some(value.clone()),
            _ => None,
        };
        let mut result = Vec::new();
        for file in &self.inner.parsed.files {
            let path = self.inner.policy.project.join(&file.artifact.relative_path);
            let bytes = self
                .inner
                .source
                .files
                .get(&path)
                .and_then(|input| input.bytes.as_deref())
                .ok_or("missing inventory source")?;
            let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
            let Node::List { items, .. } = parse_one(text)
                .map_err(|error| error.to_string())?
                .nodes
                .into_iter()
                .next()
                .ok_or("missing schematic root")?
            else {
                return Err("invalid inventory root".into());
            };
            for node in &items {
                let Node::List { items, span, .. } = node else {
                    continue;
                };
                let Some(kind) = items.first().and_then(atom) else {
                    continue;
                };
                if !matches!(
                    kind.as_str(),
                    "symbol"
                        | "wire"
                        | "junction"
                        | "no_connect"
                        | "label"
                        | "global_label"
                        | "hierarchical_label"
                        | "sheet"
                ) {
                    continue;
                }
                let uuid = items
                    .iter()
                    .filter_map(|item| match item {
                        Node::List { items, .. }
                            if items.first().and_then(atom).as_deref() == Some("uuid") =>
                        {
                            items.get(1).and_then(atom)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if uuid.len() != 1 || result.len() >= 100_000 {
                    return Err("ambiguous or excessive source object inventory".into());
                }
                result.push(serde_json::json!({"file":file.artifact.relative_path,"uuid":uuid[0],"kind":kind,"definition":text.get(span.start..span.end).ok_or("invalid inventory span")?}));
            }
        }
        self.verify()?;
        Ok(result)
    }
    pub fn pins(&self) -> &kicad_schematic::pins::SchematicPinIndex {
        &self.inner.pins
    }
    pub fn private_root(&self) -> &Path {
        self.inner.owner.path()
    }
    pub fn verify(&self) -> Result<(), String> {
        self.inner.source.verify()?;
        self.verify_context()?;
        self.verify_hierarchy(&self.inner.parsed)
    }

    fn verify_context(&self) -> Result<(), String> {
        if canonical_dir(&self.inner.resource_root)? != self.inner.resource_root
            || !std::iter::once(&self.inner.policy.project)
                .chain(self.inner.policy.extra_roots.iter())
                .any(|root| self.inner.resource_root.starts_with(root))
        {
            return Err("native resource anchor escaped server policy".into());
        }
        self.inner.private_inputs.verify()?;
        self.inner.symbols.verify(&self.inner.symbol_context)?;
        if self
            .inner
            .footprints
            .schematic_files(&self.inner.footprint_ids)?
            != self.inner.footprint_context
        {
            return Err("schematic footprint context changed".into());
        }
        for (root, expected) in &self.inner.private_trees {
            if &tree_names(root)? != expected {
                return Err("private project or resource file set changed".into());
            }
        }
        if config_names(&self.inner.source_config)? != self.inner.source_config_names
            || config_names(&self.inner.private_config)? != self.inner.private_config_names
        {
            return Err("native configuration file set changed".into());
        }
        self.inner
            .runner
            .execution_context()
            .ok_or("missing fixed execution context")?
            .verify()
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn verify_hierarchy(&self, expected: &ParsedSchematicProject) -> Result<(), String> {
        if load_schematic_project(
            &self.inner.policy.project,
            &self.inner.policy.schematic,
            "10.0.7",
            1,
            Limits::default(),
        )
        .map_err(|e| e.to_string())?
            != *expected
        {
            return Err("source hierarchy or editor locks changed".into());
        }
        Ok(())
    }
    /// Observe omitted source pins in an isolated copy while retaining original ERC evidence.
    pub fn observe_pin_connectivity(&self) -> Result<SchematicConnectivityAudit, String> {
        self.verify()?;
        let sources = self
            .inner
            .parsed
            .files
            .iter()
            .map(|file| {
                let bytes = self
                    .inner
                    .source
                    .files
                    .get(&self.inner.policy.project.join(&file.artifact.relative_path))
                    .and_then(|f| f.bytes.as_deref())
                    .ok_or("missing bound probe source bytes")?;
                Ok((file.artifact.relative_path.clone(), bytes))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let plan = kicad_schematic::probes::PinProbePlan::prepare(
            &self.inner.parsed,
            &self.inner.netlist,
            &sources,
        )?;
        if plan.probe_count() == 0 {
            let connectivity = plan.validate(&self.inner.netlist)?;
            self.verify()?;
            return Ok(SchematicConnectivityAudit {
                connectivity,
                native: self.inner.netlist.clone(),
                evidence: serde_json::json!({"source_revision":self.inner.revision,"probe_count":0,"original_native_reused":true}),
            });
        }
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-pin-probe-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let project = root.join("project");
        let configuration = root.join("config");
        let original_project = fs::canonicalize(self.inner.owner.path())
            .map_err(|e| e.to_string())?
            .join("project");
        let names = self
            .inner
            .private_trees
            .get(&original_project)
            .ok_or("missing original private namespace")?;
        let mut inputs = Inputs::default();
        for path in names {
            if path.is_file() {
                let relative = path
                    .strip_prefix(&original_project)
                    .map_err(|e| e.to_string())?;
                let name = relative.to_str().ok_or("non-UTF8 private path")?;
                let bytes = match plan.files().get(name) {
                    Some(bytes) => bytes.as_slice(),
                    None => self
                        .inner
                        .private_inputs
                        .files
                        .get(path)
                        .and_then(|f| f.bytes.as_deref())
                        .ok_or("missing captured private bytes")?,
                };
                let target = project.join(relative);
                write_new(&target, bytes)?;
                inputs.track(&target)?;
            }
        }
        for path in &self.inner.private_config_names {
            let bytes = self
                .inner
                .private_inputs
                .files
                .get(path)
                .and_then(|f| f.bytes.as_deref())
                .ok_or("missing captured private configuration")?;
            write_new(
                &configuration
                    .join("10.0")
                    .join(path.file_name().ok_or("missing configuration name")?),
                bytes,
            )?;
        }
        let expected_tree = tree_names(&project)?;
        let runner = self
            .inner
            .runner
            .clone()
            .freeze_context(Some(&configuration))
            .map_err(|e| e.to_string())?;
        let schematic = project.join(&self.inner.parsed.root_relative_path);
        let first = runner
            .run(&project, &schematic)
            .map_err(|e| e.to_string())?
            .document;
        inputs.verify()?;
        self.verify()?;
        if tree_names(&project)? != expected_tree {
            return Err("probe initialization changed project namespace".into());
        }
        let config = configuration.join("10.0");
        let config_files = config_names(&config)?;
        for path in &config_files {
            inputs.track(path)?;
        }
        let native = runner
            .run(&project, &schematic)
            .map_err(|e| e.to_string())?
            .document;
        if native != first {
            return Err("native probe output changed after initialization".into());
        }
        let connectivity = plan.validate(&native)?;
        inputs.verify()?;
        self.verify()?;
        if tree_names(&project)? != expected_tree || config_names(&config)? != config_files {
            return Err("probe namespace changed during validation".into());
        }
        let evidence = serde_json::json!({"source_revision":self.inner.revision,"probe_count":plan.probe_count(),"candidate_files":plan.files().iter().map(|(name,bytes)|(name.clone(),hash(bytes))).collect::<BTreeMap<_,_>>(),"native_digest":hash(format!("{native:?}").as_bytes()),"private_inputs":inputs,"execution":runner.execution_context().ok_or("missing probe execution context")?.digest(),"repeated_native_equal":true,"original_erc_retained":true});
        drop(owner);
        if root.exists() {
            return Err("private probe directory was not removed".into());
        }
        Ok(SchematicConnectivityAudit {
            connectivity,
            native,
            evidence,
        })
    }

    pub fn evidence(&self) -> Result<Value, String> {
        Ok(
            serde_json::json!({"revision":self.inner.revision,"source":self.inner.source,"private_inputs":self.inner.private_inputs,"private_trees":self.inner.private_trees,"initialized_local_preferences":self.inner.initialized_local_preferences,"symbols":self.inner.symbol_context,"footprints":self.inner.footprint_context,"execution":self.inner.runner.execution_context().ok_or("missing context")?.digest(),"erc":self.inner.erc,"components":self.inner.identities.components().len(),"sheets":self.inner.identities.sheets().len(),"units":self.inner.identities.units().len(),"nets":self.inner.netlist.nets.len(),"logical_pins":self.inner.pins.pins().len(),"unexported_source_pins":self.inner.pins.unexported().len()}),
        )
    }

    pub fn prepare(policy: &SchematicAuditPolicy) -> Result<Self, String> {
        let parsed = load_schematic_project(
            &policy.project,
            &policy.schematic,
            "10.0.7",
            1,
            Limits::default(),
        )
        .map_err(|e| e.to_string())?;
        if parsed.editor_lock_present {
            return Err("schematic editor lock present".into());
        }
        let source_config = policy.configuration.join("10.0");
        let source_config_names = config_names(&source_config)?;
        for name in ["kicad_common.json", "sym-lib-table", "fp-lib-table"] {
            if !source_config.join(name).is_file() {
                return Err(
                    "native configuration must have initialized common settings and library tables"
                        .into(),
                );
            }
        }
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-schematic-audit-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let owned = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
        let private_config = owned.join("config/10.0");
        fs::create_dir_all(&private_config).map_err(|e| e.to_string())?;
        let mut source = Inputs::default();
        for path in &source_config_names {
            source.copy(
                path,
                &private_config.join(path.file_name().ok_or("missing config name")?),
            )?;
        }
        let common_path = private_config.join("kicad_common.json");
        let common = english_config(&fs::read(&common_path).map_err(|e| e.to_string())?)?;
        fs::write(&common_path, common).map_err(|e| e.to_string())?;
        let runner = NetlistRunner::production()
            .map_err(|e| e.to_string())?
            .freeze_context(Some(&owned.join("config")))
            .map_err(|e| e.to_string())?;
        let init = owned.join("initialize/initialize.kicad_sch");
        write_new(&init,b"(kicad_sch (version 20260306) (generator eeschema) (uuid 11111111-1111-4111-8111-111111111111) (paper A4) (lib_symbols))")?;
        runner
            .run_complete_erc(init.parent().ok_or("missing initialization parent")?, &init)
            .map_err(|e| e.to_string())?;
        source.verify()?;
        let common: Value =
            serde_json::from_slice(&fs::read(&common_path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if common.pointer("/system/language").and_then(Value::as_str) != Some("English") {
            return Err("private native language changed during initialization".into());
        }
        let source_root = policy
            .schematic
            .parent()
            .ok_or("missing schematic parent")?;
        let pro = policy.schematic.with_extension("kicad_pro");
        let project = parse_json(source.track(&pro)?.bytes.as_ref())?;
        // External drawing sheets and simulation resources require additional source evidence.
        if project
            .pointer("/schematic/page_layout_descr_file")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
            || project
                .pointer("/erc/rule_severities/simulation_model_issue")
                .and_then(Value::as_str)
                .is_some_and(|s| s != "ignore")
        {
            return Err("external schematic resources are not yet bound".into());
        }
        let vars = variables(
            runner.execution_context().ok_or("missing context")?,
            &common,
            &project,
            policy,
        )?;
        let mut extras = policy.extra_roots.clone();
        extras.push(policy.project.clone());
        let sym_policy = LibraryCatalogPolicy::new(
            source_root,
            &source_config.join("sym-lib-table"),
            Some(&policy.symbols),
            &extras,
            vars.clone(),
            vec![],
        )?
        .with_native_settings(&source_config.join("kicad_common.json"))?;
        let fp_policy = LibraryCatalogPolicy::new(
            source_root,
            &source_config.join("fp-lib-table"),
            Some(&policy.footprints),
            &extras,
            vars,
            vec![],
        )?
        .with_native_settings(&source_config.join("kicad_common.json"))?;
        let symbols = SymbolCatalog::load(&sym_policy)?;
        let footprints = FootprintCatalog::load(&fp_policy)?;
        let mut sym_names = BTreeSet::new();
        let mut fp_ids = BTreeMap::new();
        let private_project = owned.join("project");
        for file in &parsed.files {
            let path = policy.project.join(&file.artifact.relative_path);
            source.copy(&path, &private_project.join(&file.artifact.relative_path))?;
            for sheet in &file.sheets {
                let child = Path::new(&sheet.file);
                if child.is_absolute() || sheet.file.contains('$') {
                    return Err("absolute or variable child schematic paths are unsupported in private audit".into());
                }
                let requested = path.parent().ok_or("missing file parent")?.join(child);
                if lexical(&requested)?
                    != fs::canonicalize(&requested).map_err(|e| e.to_string())?
                {
                    return Err(
                        "symlinked child schematic paths are unsupported in private audit".into(),
                    );
                }
            }
            for symbol in &file.symbols {
                let (nickname, _) = symbol
                    .lib_id
                    .split_once(':')
                    .ok_or("unqualified symbol library identity")?;
                sym_names.insert(nickname.to_owned());
                if let Some(fp) = symbol.footprint.as_ref().filter(|s| !s.is_empty()) {
                    if fp.contains('$') {
                        return Err(
                            "variable footprint assignments require native expansion evidence"
                                .into(),
                        );
                    }
                    let (nickname, entry_name) =
                        fp.split_once(':').ok_or("unqualified footprint identity")?;
                    fp_ids.insert(
                        fp.clone(),
                        FootprintLibraryIdentifier {
                            nickname: nickname.into(),
                            entry_name: entry_name.into(),
                        },
                    );
                }
            }
        }
        let copied_source = private_project.join(&parsed.root_relative_path);
        for ext in ["kicad_pro", "kicad_prl", "kicad_dru"] {
            source.copy(
                &policy.schematic.with_extension(ext),
                &copied_source.with_extension(ext),
            )?;
        }
        let sym_context = symbols.capture(&sym_names.into_iter().collect::<Vec<_>>())?;
        let ids = fp_ids.into_values().collect::<Vec<_>>();
        let fp_context = footprints.schematic_files(&ids)?;
        for table in symbols
            .table_evidence()
            .iter()
            .chain(footprints.table_evidence())
        {
            source.track(&table.requested_path)?;
        }
        let mut sym_copies = BTreeMap::new();
        for (index, file) in sym_context.files().iter().enumerate() {
            if let Some(path) = file.path() {
                let target = owned.join(format!("resources/symbols/{index}.kicad_sym"));
                source.copy_expected(path, &target, file.size(), file.digest())?;
                sym_copies.insert(file.nickname().to_owned(), target);
            }
        }
        let mut fp_copies = BTreeMap::new();
        for file in fp_context.files() {
            if let Some(path) = &file.path {
                let n = fp_copies.len();
                let root = fp_copies
                    .entry(file.identifier.nickname.clone())
                    .or_insert_with(|| owned.join(format!("resources/footprints/{n}.pretty")))
                    .clone();
                source.copy_expected(
                    path,
                    &root.join(format!("{}.kicad_mod", file.identifier.entry_name)),
                    file.size,
                    file.digest.as_deref(),
                )?;
            }
        }
        let copied_root = copied_source
            .parent()
            .ok_or("missing copied schematic parent")?;
        write_new(
            &copied_root.join("sym-lib-table"),
            relocate(
                symbols.effective_rows()?,
                &sym_copies,
                source_root,
                TableKind::Symbol,
            )?
            .as_bytes(),
        )?;
        write_new(
            &copied_root.join("fp-lib-table"),
            relocate(
                footprints.effective_rows()?,
                &fp_copies,
                source_root,
                TableKind::Footprint,
            )?
            .as_bytes(),
        )?;
        source.verify()?;
        symbols.verify(&sym_context)?;
        if footprints.schematic_files(&ids)? != fp_context {
            return Err("footprint context changed before native validation".into());
        }
        // Protect private project/resources during native initialization; configuration may settle once.
        let mut private_inputs = Inputs::default();
        let mut private_trees = BTreeMap::new();
        for root in [&private_project, &owned.join("resources")] {
            let names = tree_names(root)?;
            for path in &names {
                if path.is_file() {
                    private_inputs.track(path)?;
                }
            }
            private_trees.insert(root.clone(), names);
        }
        let initial_netlist = runner
            .run(&private_project, &copied_source)
            .map_err(|e| e.to_string())?
            .document;
        let initial_erc = runner
            .run_complete_erc(&private_project, &copied_source)
            .map_err(|e| e.to_string())?;
        private_inputs.verify()?;
        source.verify()?;
        let preferences = copied_source.with_extension("kicad_prl");
        let initialized_local_preferences = bind_initialized_preferences(
            &mut private_inputs,
            &mut private_trees,
            &private_project,
            &preferences,
        )?;
        let private_config_names = config_names(&private_config)?;
        for path in &private_config_names {
            private_inputs.track(path)?;
        }
        let netlist = runner
            .run(&private_project, &copied_source)
            .map_err(|e| e.to_string())?
            .document;
        let erc = runner
            .run_complete_erc(&private_project, &copied_source)
            .map_err(|e| e.to_string())?;
        if netlist != initial_netlist
            || !erc.equivalent_to(&initial_erc).map_err(|e| e.to_string())?
        {
            return Err("native output changed after configuration initialization".into());
        }
        let identities =
            SchematicIdentityIndex::resolve(&parsed, &netlist).map_err(|e| e.to_string())?;
        let pin_sources = parsed
            .files
            .iter()
            .map(|file| {
                let bytes = source
                    .files
                    .get(&policy.project.join(&file.artifact.relative_path))
                    .and_then(|f| f.bytes.as_deref())
                    .ok_or("missing pinned schematic bytes")?;
                Ok((file.artifact.relative_path.clone(), bytes))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let pins =
            kicad_schematic::pins::SchematicPinIndex::resolve(&parsed, &netlist, &pin_sources)?;
        let revision = hash(
            &serde_json::to_vec(&(
                source.digest()?,
                private_inputs.digest()?,
                &private_trees,
                &sym_context,
                &fp_context,
                runner
                    .execution_context()
                    .ok_or("missing context")?
                    .digest(),
            ))
            .map_err(|e| e.to_string())?,
        );
        let result = Self {
            inner: Arc::new(SchematicAuditState {
                owner,
                source,
                private_inputs,
                source_config,
                source_config_names,
                private_config,
                private_config_names,
                initialized_local_preferences,
                policy: policy.clone(),
                resource_root: source_root.to_path_buf(),
                symbols,
                symbol_context: sym_context,
                footprints,
                footprint_context: fp_context,
                footprint_ids: ids,
                private_trees,
                parsed,
                runner,
                netlist,
                erc,
                identities,
                pins,
                revision,
                _source_owner: None,
            }),
        };
        result.verify()?;
        Ok(result)
    }
}
fn bind_initialized_preferences(
    inputs: &mut Inputs,
    trees: &mut BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    project: &Path,
    preferences: &Path,
) -> Result<Option<PathBuf>, String> {
    let expected = trees
        .get_mut(project)
        .ok_or("missing private project namespace")?;
    let actual = tree_names(project)?;
    let initialized = !expected.contains(preferences) && actual.contains(preferences);
    if initialized {
        if !preferences.starts_with(project)
            || preferences.extension().and_then(|s| s.to_str()) != Some("kicad_prl")
            || !preferences.is_file()
        {
            return Err("invalid native local-preference initialization".into());
        }
        inputs.track(preferences)?;
        expected.insert(preferences.into());
    }
    if &actual != expected {
        return Err("native initialization changed the private project namespace".into());
    }
    Ok(initialized.then(|| preferences.to_owned()))
}

fn tree_names(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut names = BTreeSet::new();
    let mut stack = vec![root.to_owned()];
    while let Some(path) = stack.pop() {
        let kind = match fs::symlink_metadata(&path) {
            Ok(meta) => meta.file_type(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && path == root => continue,
            Err(e) => return Err(e.to_string()),
        };
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err("private snapshot contains a symlink or special file".into());
        }
        if !names.insert(path.clone()) || names.len() > 8192 {
            return Err("private snapshot namespace exceeds bound".into());
        }
        if kind.is_dir() {
            for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
                if stack.len() + names.len() >= 8192 {
                    return Err("private snapshot namespace exceeds bound".into());
                }
                stack.push(entry.map_err(|e| e.to_string())?.path());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_evidence_binds_absence_content_and_copied_resource_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let path = root.join("source");
        let mut inputs = Inputs::default();
        inputs.track(&path).unwrap();
        inputs.verify().unwrap();
        fs::write(&path, b"original").unwrap();
        assert!(inputs.verify().is_err());
        let mut inputs = Inputs::default();
        inputs.track(&path).unwrap();
        assert!(
            inputs
                .copy_expected(&path, &root.join("bad"), 8, Some("invalid"))
                .is_err()
        );
        assert!(!root.join("bad").exists());
        inputs
            .copy_expected(&path, &root.join("copy"), 8, Some(&hash(b"original")))
            .unwrap();
        assert_eq!(fs::read(root.join("copy")).unwrap(), b"original");
        fs::write(&path, b"modified").unwrap();
        assert!(inputs.verify().is_err());
        fs::remove_file(&path).unwrap();
        assert!(inputs.verify().is_err());
    }

    #[test]
    fn private_namespace_binds_new_entries_and_rejects_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let before = tree_names(&root).unwrap();
        fs::create_dir(root.join("extra")).unwrap();
        assert_ne!(tree_names(&root).unwrap(), before);
        fs::remove_dir(root.join("extra")).unwrap();
        assert_eq!(tree_names(&root).unwrap(), before);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
            assert!(tree_names(&root).is_err());
        }
    }

    #[test]
    fn initialization_only_admits_the_named_absent_native_preferences() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        let preferences = root.join("board.kicad_prl");
        let mut trees = BTreeMap::from([(root.clone(), tree_names(&root).unwrap())]);
        let mut inputs = Inputs::default();
        fs::write(&preferences, b"{}").unwrap();
        assert_eq!(
            bind_initialized_preferences(&mut inputs, &mut trees, &root, &preferences).unwrap(),
            Some(preferences.clone())
        );
        fs::write(&preferences, b"changed").unwrap();
        assert!(inputs.verify().is_err());
        fs::write(root.join("unexpected.kicad_sch"), b"data").unwrap();
        assert!(
            bind_initialized_preferences(&mut inputs, &mut trees, &root, &preferences).is_err()
        );
    }

    #[test]
    fn private_locale_preserves_raw_unknown_values_and_rejects_ambiguity() {
        let raw = br#"{"system":{"language":"Default","precise":123456789012345678901234567890},"unknown":[1.00000000000000000000000000001,1e999]}"#;
        let result = english_config(raw).unwrap();
        let root: BTreeMap<String, Box<RawValue>> = serde_json::from_slice(&result).unwrap();
        let system: BTreeMap<String, Box<RawValue>> =
            serde_json::from_str(root["system"].get()).unwrap();
        assert_eq!(system["language"].get(), "\"English\"");
        assert_eq!(system["precise"].get(), "123456789012345678901234567890");
        assert_eq!(
            root["unknown"].get(),
            "[1.00000000000000000000000000001,1e999]"
        );
        for raw in [
            br#"{"system":{},"system":{}}"#.as_slice(),
            br#"{"system":{"language":"x","language":"y"}}"#,
            br#"{"unknown":[{"a":1,"a":2}]}"#,
        ] {
            assert!(english_config(raw).is_err());
            assert!(parse_json(Some(&raw.to_vec())).is_err());
        }
    }
}

#[cfg(test)]
mod saved_board_capture_tests {
    use super::*;
    #[test]
    fn capture_profiles_keep_small_evidence_bytes_and_revalidation_authority() {
        let owned = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(owned.path())
            .unwrap()
            .join("board.kicad_pcb");
        fs::write(&path, b"captured").unwrap();
        let mut legacy = Inputs::default();
        let mut closed = Inputs::synchronization();
        legacy.track(&path).unwrap();
        closed.track(&path).unwrap();
        assert_eq!(
            serde_json::to_vec(&legacy).unwrap(),
            serde_json::to_vec(&closed).unwrap()
        );
        assert_eq!(legacy.files[&path].limit, MAX_FILE);
        assert_eq!(closed.files[&path].limit, 128 * 1024 * 1024);
        closed.verify().unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(closed.verify().is_err());
        let mut bounded = Inputs {
            total: 128 * 1024 * 1024,
            ..Inputs::default()
        };
        assert!(bounded.track(&path).is_err());
        assert!(bounded.files.is_empty());
        assert_eq!(bounded.total, 128 * 1024 * 1024);
    }
}
