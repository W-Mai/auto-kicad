use crate::cst::parse_one;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use kicad_model::{
    ModelError, ObservedSchematicPin, ObservedSchematicSymbol, ReferenceResolution, RevisionDigest,
    SchematicArtifactFile, SchematicArtifactRevision, StableId, schematic_pin_id,
    schematic_symbol_id,
};
use kiutils_sexpr::{Atom, Node, Span};
use sha2::{Digest, Sha256};
use thiserror::Error;

mod cst;
pub mod editing;
mod execution;
pub use execution::NativeExecutionContext;
pub mod geometry;
pub mod identities;
mod netlist;
mod patch;
pub mod pins;
pub mod probes;

pub use netlist::{
    NetlistComponent, NetlistDocument, NetlistLimits, NetlistNet, NetlistNode, NetlistRun,
    NetlistRunner, NetlistRunnerError, NetlistSheet, NetlistUnit, parse_netlist_xml,
};
pub use patch::{
    SchematicPatchOutcome, SchematicPatchRejection, SchematicPreviewReceipt,
    TWO_RESISTOR_OPERATION_IDENTITY, commit_two_resistor_circuit, create_two_resistor_circuit,
    preview_two_resistor_circuit,
};

pub fn schematic_change_available() -> bool {
    NetlistRunner::production().is_ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_files: usize,
    pub max_depth: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_files: 128,
            max_depth: 32,
            max_file_bytes: 32 * 1024 * 1024,
            max_total_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSchematicProject {
    pub root_relative_path: String,
    pub files: Vec<ParsedSchematic>,
    pub revision: SchematicArtifactRevision,
    pub editor_lock_present: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSchematic {
    pub format_version: String,
    pub generator: String,
    pub generator_version: Option<String>,
    pub file_uuid: String,
    pub sheets: Vec<RawSheet>,
    pub symbols: Vec<RawSymbol>,
    pub diagnostics: Vec<StructuralDiagnostic>,
    pub artifact: SchematicArtifactFile,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawSheet {
    pub source_uuid: String,
    pub name: String,
    pub file: String,
    pub pins: Vec<RawSheetPin>,
    pub instance_paths: Vec<InstancePath>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawSheetPin {
    pub source_uuid: String,
    pub name: String,
    pub electrical_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawSymbol {
    pub source_uuid: String,
    pub lib_id: String,
    pub lib_name: Option<String>,
    pub position: Position,
    pub unit: u32,
    pub body_style: Option<u32>,
    pub reference: String,
    pub value: String,
    pub footprint: Option<String>,
    pub on_board: Option<bool>,
    pub in_bom: Option<bool>,
    pub dnp: Option<bool>,
    pub pins: Vec<RawSymbolPin>,
    pub instance_paths: Vec<InstancePath>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawSymbolPin {
    pub number: String,
    pub source_uuid: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub orientation_degrees: Option<f64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstancePath {
    pub project: String,
    pub path: String,
    pub reference: Option<String>,
    pub unit: Option<u32>,
    pub page: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JoinedSchematicComponent {
    pub symbol: ObservedSchematicSymbol,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum JoinError {
    #[error("invalid schematic revision")]
    InvalidRevision,
    #[error("invalid stable UUID in {field}")]
    InvalidUuid { field: &'static str },
    #[error("{kind} `{reference}` has no unique symbol at sheet path `{sheet_path}`")]
    MissingOrAmbiguousSymbol {
        kind: &'static str,
        reference: String,
        sheet_path: String,
    },
    #[error("symbol UUID mismatch for `{reference}` at sheet path `{sheet_path}`")]
    SymbolUuidMismatch {
        reference: String,
        sheet_path: String,
    },
    #[error("symbol `{reference}` has duplicate pin number `{number}`")]
    DuplicatePinNumber { reference: String, number: String },
    #[error("symbol `{reference}` has a pin without a UUID")]
    MissingPinUuid { reference: String },
}

pub fn join_netlist_connectivity(
    project: &ParsedSchematicProject,
    netlist: &NetlistDocument,
    project_id: &StableId,
) -> Result<Vec<JoinedSchematicComponent>, ConnectivityJoinError> {
    let mut joined = join_netlist_components(project, netlist, project_id)
        .map_err(ConnectivityJoinError::Component)?;
    let mut pin_locations = BTreeMap::new();
    for (component_index, component) in joined.iter().enumerate() {
        for (pin_index, pin) in component.symbol.pins.iter().enumerate() {
            let key = (component.symbol.reference.clone(), pin.number.clone());
            if pin_locations
                .insert(key.clone(), (component_index, pin_index))
                .is_some()
            {
                return Err(ConnectivityJoinError::DuplicatePinIdentity {
                    reference: key.0,
                    pin: key.1,
                });
            }
        }
    }

    let mut assignments = BTreeMap::new();
    let mut net_names = BTreeSet::new();
    for net in &netlist.nets {
        let net_name = net.name.trim();
        if net_name.is_empty() || !net_names.insert(net_name.to_owned()) {
            return Err(ConnectivityJoinError::InvalidNetIdentity);
        }
        let net_id = StableId::new(format!(
            "netlist-net-v1:{}:{}:{}",
            project_id,
            project.revision.digest.as_str(),
            net.name.trim()
        ))
        .map_err(|_| ConnectivityJoinError::InvalidNetIdentity)?;
        for node in &net.nodes {
            let key = (node.reference.clone(), node.pin.clone());
            let Some(&(component_index, pin_index)) = pin_locations.get(&key) else {
                return Err(ConnectivityJoinError::UnknownNode {
                    reference: node.reference.clone(),
                    pin: node.pin.clone(),
                });
            };
            if assignments.insert(key.clone(), net_id.clone()).is_some() {
                return Err(ConnectivityJoinError::DuplicateNode {
                    reference: node.reference.clone(),
                    pin: node.pin.clone(),
                });
            }
            joined[component_index].symbol.pins[pin_index].net =
                ReferenceResolution::Resolved(net_id.clone());
        }
    }
    Ok(joined)
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConnectivityJoinError {
    #[error(transparent)]
    Component(#[from] JoinError),
    #[error("net has no canonical identity")]
    InvalidNetIdentity,
    #[error("net node `{reference}:{pin}` does not match a joined pin")]
    UnknownNode { reference: String, pin: String },
    #[error("net node `{reference}:{pin}` is assigned more than once")]
    DuplicateNode { reference: String, pin: String },
    #[error("pin identity `{reference}:{pin}` is ambiguous across sheet instances")]
    DuplicatePinIdentity { reference: String, pin: String },
}

pub fn join_netlist_components(
    project: &ParsedSchematicProject,
    netlist: &NetlistDocument,
    project_id: &StableId,
) -> Result<Vec<JoinedSchematicComponent>, JoinError> {
    if project.files.is_empty()
        || project.revision.digest.as_str().is_empty()
        || project.revision.kicad_cli_version.is_empty()
        || project.revision.extractor_schema_version == 0
    {
        return Err(JoinError::InvalidRevision);
    }

    let root_uuid = project
        .files
        .iter()
        .find(|file| file.artifact.relative_path == project.root_relative_path)
        .map(|file| file.file_uuid.as_str())
        .ok_or(JoinError::InvalidRevision)?;
    let mut joined = Vec::with_capacity(netlist.components.len());
    for component in &netlist.components {
        let Some(sheet_path) =
            normalize_netlist_instance_path(&component.sheet_uuid_path, root_uuid)
        else {
            return Err(JoinError::MissingOrAmbiguousSymbol {
                kind: "component",
                reference: component.reference.clone(),
                sheet_path: component.sheet_uuid_path.clone(),
            });
        };
        let matches = project
            .files
            .iter()
            .flat_map(|file| file.symbols.iter())
            .filter(|symbol| {
                symbol.reference == component.reference
                    && symbol.instance_paths.iter().any(|instance| {
                        instance.reference.as_deref() == Some(component.reference.as_str())
                            && normalize_cst_instance_path(&instance.path) == sheet_path
                    })
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(JoinError::MissingOrAmbiguousSymbol {
                kind: if matches.is_empty() {
                    "component"
                } else {
                    "ambiguous component"
                },
                reference: component.reference.clone(),
                sheet_path,
            });
        }
        let raw = matches[0];
        if let Some(source_uuid) = &component.symbol_uuid
            && source_uuid != &raw.source_uuid
        {
            return Err(JoinError::SymbolUuidMismatch {
                reference: component.reference.clone(),
                sheet_path,
            });
        }
        let source_uuid =
            StableId::new(raw.source_uuid.clone()).map_err(|_| JoinError::InvalidUuid {
                field: "symbol source UUID",
            })?;
        let symbol_id = schematic_symbol_id(project_id, &sheet_path, &source_uuid);
        let mut pin_numbers = BTreeSet::new();
        let mut pins = Vec::with_capacity(raw.pins.len());
        for pin in &raw.pins {
            if !pin_numbers.insert(pin.number.clone()) {
                return Err(JoinError::DuplicatePinNumber {
                    reference: component.reference.clone(),
                    number: pin.number.clone(),
                });
            }
            let pin_uuid =
                StableId::new(pin.source_uuid.clone()).map_err(|_| JoinError::MissingPinUuid {
                    reference: component.reference.clone(),
                })?;
            pins.push(ObservedSchematicPin {
                pin_id: schematic_pin_id(&symbol_id, &pin_uuid),
                source_uuid: pin_uuid,
                number: pin.number.clone(),
                name: None,
                electrical_type: None,
                net: ReferenceResolution::Unresolved {
                    query: format!("{}:{}", component.reference, pin.number),
                },
            });
        }
        pins.sort_by(|left, right| {
            (&left.number, &left.source_uuid).cmp(&(&right.number, &right.source_uuid))
        });
        joined.push(JoinedSchematicComponent {
            symbol: ObservedSchematicSymbol {
                symbol_id,
                source_uuid,
                sheet_instance_path: sheet_path,
                reference: component.reference.clone(),
                value: component.value.clone(),
                lib_identifier: component.library_part.clone(),
                footprint_field: component.footprint.clone(),
                unit: raw.unit,
                pins,
                observed_revision: project.revision.digest.clone(),
            },
        });
    }
    joined.sort_by(|left, right| {
        (
            &left.symbol.sheet_instance_path,
            &left.symbol.reference,
            &left.symbol.source_uuid,
        )
            .cmp(&(
                &right.symbol.sheet_instance_path,
                &right.symbol.reference,
                &right.symbol.source_uuid,
            ))
    });
    Ok(joined)
}

/// Normalize native sheet UUID paths against a captured root identity.
pub fn normalize_netlist_instance_path(path: &str, root_uuid: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains("//") {
        return None;
    }
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return Some(format!("/{root_uuid}"));
    }
    let segments = path[1..].split('/').collect::<Vec<_>>();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return None;
    }
    if segments.first() == Some(&root_uuid) {
        return None;
    }
    Some(format!("/{root_uuid}{path}"))
}

fn normalize_cst_instance_path(path: &str) -> String {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        "/".to_owned()
    } else {
        path.to_owned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuralDiagnostic {
    pub token: String,
    pub span: Span,
}

#[derive(Debug, Error)]
pub enum SchematicParseError {
    #[error("schematic bytes are not valid UTF-8")]
    InvalidUtf8,
    #[error("malformed schematic CST: {0}")]
    MalformedCst(#[from] kiutils_sexpr::ParseError),
    #[error("expected kicad_sch root")]
    InvalidRoot,
    #[error("missing required {0}")]
    MissingField(&'static str),
    #[error("invalid {0}")]
    InvalidField(&'static str),
    #[error("unsupported top-level token `{token}` at bytes {start}..{end}")]
    UnsupportedTopLevelToken {
        token: String,
        start: usize,
        end: usize,
    },
    #[error("invalid schematic artifact metadata: {0}")]
    InvalidArtifact(ModelError),
}

#[derive(Debug, Error)]
pub enum SchematicProjectError {
    #[error("project root is not a canonical directory")]
    InvalidProjectRoot,
    #[error("schematic path is missing or unreadable: {path}")]
    Unreadable { path: String },
    #[error("schematic path escapes the project root: {path}")]
    PathEscape { path: String },
    #[error("schematic path is not a regular .kicad_sch file: {path}")]
    InvalidSchematicFile { path: String },
    #[error("schematic hierarchy contains a cycle at {path}")]
    Cycle { path: String },
    #[error("schematic hierarchy exceeds maximum depth {limit} at {path}")]
    DepthLimit { path: String, limit: usize },
    #[error("schematic hierarchy exceeds maximum file count {limit}")]
    FileLimit { limit: usize },
    #[error("schematic file {path} exceeds maximum size {limit} bytes")]
    FileSizeLimit { path: String, limit: u64 },
    #[error("schematic hierarchy exceeds maximum total size {limit} bytes")]
    TotalSizeLimit { limit: u64 },
    #[error("failed to parse schematic {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: SchematicParseError,
    },
    #[error("invalid aggregate schematic revision: {0}")]
    InvalidRevision(ModelError),
}

pub fn load_schematic_project(
    project_root: &Path,
    root_schematic: &Path,
    kicad_cli_version: impl Into<String>,
    extractor_schema_version: u32,
    limits: Limits,
) -> Result<ParsedSchematicProject, SchematicProjectError> {
    let canonical_root =
        fs::canonicalize(project_root).map_err(|_| SchematicProjectError::InvalidProjectRoot)?;
    if canonical_root != project_root || !canonical_root.is_dir() {
        return Err(SchematicProjectError::InvalidProjectRoot);
    }

    let canonical_schematic = confined_schematic(&canonical_root, root_schematic)?;
    let root_relative_path = relative_path(&canonical_root, &canonical_schematic)?;
    let mut loader = ProjectLoader {
        project_root: &canonical_root,
        limits,
        files: BTreeMap::new(),
        active: BTreeSet::new(),
        total_bytes: 0,
        editor_lock_present: false,
    };
    loader.visit(canonical_schematic, 0)?;

    let files = loader.files.into_values().collect::<Vec<_>>();
    let artifacts = files.iter().map(|file| file.artifact.clone()).collect();
    let revision = SchematicArtifactRevision::new(
        artifacts,
        kicad_cli_version,
        extractor_schema_version,
        loader.editor_lock_present,
    )
    .map_err(SchematicProjectError::InvalidRevision)?;

    Ok(ParsedSchematicProject {
        root_relative_path,
        files,
        revision,
        editor_lock_present: loader.editor_lock_present,
    })
}

struct ProjectLoader<'a> {
    project_root: &'a Path,
    limits: Limits,
    files: BTreeMap<String, ParsedSchematic>,
    active: BTreeSet<PathBuf>,
    total_bytes: u64,
    editor_lock_present: bool,
}

impl ProjectLoader<'_> {
    fn visit(&mut self, path: PathBuf, depth: usize) -> Result<(), SchematicProjectError> {
        let relative = relative_path(self.project_root, &path)?;
        if depth > self.limits.max_depth {
            return Err(SchematicProjectError::DepthLimit {
                path: relative,
                limit: self.limits.max_depth,
            });
        }
        if self.active.contains(&path) {
            return Err(SchematicProjectError::Cycle { path: relative });
        }
        if self.files.contains_key(&relative) {
            return Ok(());
        }
        if self.files.len() >= self.limits.max_files {
            return Err(SchematicProjectError::FileLimit {
                limit: self.limits.max_files,
            });
        }

        let metadata = fs::metadata(&path).map_err(|_| SchematicProjectError::Unreadable {
            path: relative.clone(),
        })?;
        if !metadata.is_file() {
            return Err(SchematicProjectError::InvalidSchematicFile { path: relative });
        }
        if metadata.len() > self.limits.max_file_bytes {
            return Err(SchematicProjectError::FileSizeLimit {
                path: relative,
                limit: self.limits.max_file_bytes,
            });
        }
        let bytes = fs::read(&path).map_err(|_| SchematicProjectError::Unreadable {
            path: relative.clone(),
        })?;
        let size = bytes.len() as u64;
        if size > self.limits.max_file_bytes {
            return Err(SchematicProjectError::FileSizeLimit {
                path: relative,
                limit: self.limits.max_file_bytes,
            });
        }
        let total =
            self.total_bytes
                .checked_add(size)
                .ok_or(SchematicProjectError::TotalSizeLimit {
                    limit: self.limits.max_total_bytes,
                })?;
        if total > self.limits.max_total_bytes {
            return Err(SchematicProjectError::TotalSizeLimit {
                limit: self.limits.max_total_bytes,
            });
        }

        let parsed = parse_schematic(relative.clone(), &bytes).map_err(|source| {
            SchematicProjectError::Parse {
                path: relative.clone(),
                source,
            }
        })?;
        self.total_bytes = total;
        self.editor_lock_present |= lock_path(&path).is_file();
        let children = parsed
            .sheets
            .iter()
            .map(|sheet| sheet.file.clone())
            .collect::<Vec<_>>();
        self.files.insert(relative, parsed);
        self.active.insert(path.clone());
        for child in children {
            let candidate = path.parent().unwrap_or(self.project_root).join(&child);
            let canonical =
                fs::canonicalize(&candidate).map_err(|_| SchematicProjectError::Unreadable {
                    path: display_candidate(self.project_root, &candidate),
                })?;
            let canonical = confined_schematic(self.project_root, &canonical)?;
            self.visit(canonical, depth + 1)?;
        }
        self.active.remove(&path);
        Ok(())
    }
}

fn confined_schematic(project_root: &Path, path: &Path) -> Result<PathBuf, SchematicProjectError> {
    let canonical = fs::canonicalize(path).map_err(|_| SchematicProjectError::Unreadable {
        path: display_candidate(project_root, path),
    })?;
    if !canonical.starts_with(project_root) {
        return Err(SchematicProjectError::PathEscape {
            path: display_candidate(project_root, path),
        });
    }
    if canonical.extension().and_then(|value| value.to_str()) != Some("kicad_sch")
        || !canonical.is_file()
    {
        return Err(SchematicProjectError::InvalidSchematicFile {
            path: relative_path(project_root, &canonical)
                .unwrap_or_else(|_| display_candidate(project_root, path)),
        });
    }
    Ok(canonical)
}

fn relative_path(project_root: &Path, path: &Path) -> Result<String, SchematicProjectError> {
    let relative =
        path.strip_prefix(project_root)
            .map_err(|_| SchematicProjectError::PathEscape {
                path: display_candidate(project_root, path),
            })?;
    let value = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| SchematicProjectError::InvalidSchematicFile {
            path: "<non-utf8>".to_owned(),
        })?
        .join("/");
    if value.is_empty() {
        return Err(SchematicProjectError::InvalidSchematicFile { path: value });
    }
    Ok(value)
}

fn display_candidate(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .ok()
        .and_then(|relative| relative.to_str())
        .unwrap_or("<outside-project>")
        .replace('\\', "/")
}

fn lock_path(schematic: &Path) -> PathBuf {
    let file_name = schematic
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    schematic.with_file_name(format!("~{file_name}.lck"))
}

pub fn parse_schematic(
    relative_path: impl Into<String>,
    bytes: &[u8],
) -> Result<ParsedSchematic, SchematicParseError> {
    let text = std::str::from_utf8(bytes).map_err(|_| SchematicParseError::InvalidUtf8)?;
    let document = parse_one(text)?;
    let root = document
        .nodes
        .first()
        .ok_or(SchematicParseError::InvalidRoot)?;
    let root_items = list_items(root).ok_or(SchematicParseError::InvalidRoot)?;
    if list_tag(root_items) != Some("kicad_sch") {
        return Err(SchematicParseError::InvalidRoot);
    }

    let mut format_version = None;
    let mut generator = None;
    let mut generator_version = None;
    let mut file_uuid = None;
    let mut sheets = Vec::new();
    let mut symbols = Vec::new();
    let mut diagnostics = Vec::new();

    for node in &root_items[1..] {
        let Some(items) = list_items(node) else {
            return Err(SchematicParseError::InvalidField("top-level form"));
        };
        let tag = list_tag(items).ok_or(SchematicParseError::InvalidField("top-level tag"))?;
        match tag {
            "version" => format_version = Some(required_value(items, "version")?.to_owned()),
            "generator" => generator = Some(required_value(items, "generator")?.to_owned()),
            "generator_version" => {
                generator_version = Some(required_value(items, "generator_version")?.to_owned())
            }
            "uuid" => file_uuid = Some(required_value(items, "uuid")?.to_owned()),
            "sheet" => sheets.push(parse_sheet(items)?),
            "symbol" => symbols.push(parse_symbol(items)?),
            token if is_allowed_unparsed_top_level(token) => {
                diagnostics.push(StructuralDiagnostic {
                    token: token.to_owned(),
                    span: node_span(node),
                })
            }
            token => {
                let span = node_span(node);
                return Err(SchematicParseError::UnsupportedTopLevelToken {
                    token: token.to_owned(),
                    start: span.start,
                    end: span.end,
                });
            }
        }
    }

    sheets.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
    symbols.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
    diagnostics.sort_by_key(|diagnostic| diagnostic.span.start);

    let digest = RevisionDigest::new(hex::encode(Sha256::digest(bytes)))
        .map_err(SchematicParseError::InvalidArtifact)?;
    let artifact = SchematicArtifactFile::new(relative_path, bytes.len() as u64, digest)
        .map_err(SchematicParseError::InvalidArtifact)?;

    Ok(ParsedSchematic {
        format_version: format_version.ok_or(SchematicParseError::MissingField("version"))?,
        generator: generator.ok_or(SchematicParseError::MissingField("generator"))?,
        generator_version,
        file_uuid: file_uuid.ok_or(SchematicParseError::MissingField("uuid"))?,
        sheets,
        symbols,
        diagnostics,
        artifact,
    })
}

fn parse_sheet(items: &[Node]) -> Result<RawSheet, SchematicParseError> {
    let mut source_uuid = None;
    let mut name = None;
    let mut file = None;
    let mut pins = Vec::new();
    let mut instance_paths = Vec::new();

    for child in &items[1..] {
        let Some(values) = list_items(child) else {
            continue;
        };
        match list_tag(values) {
            Some("uuid") => source_uuid = Some(required_value(values, "sheet uuid")?.to_owned()),
            Some("property") => match atom_text(values.get(1)) {
                Some("Sheetname") => name = Some(required_nth(values, 2, "Sheetname")?.to_owned()),
                Some("Sheetfile") => file = Some(required_nth(values, 2, "Sheetfile")?.to_owned()),
                _ => {}
            },
            Some("pin") => pins.push(parse_sheet_pin(values)?),
            Some("instances") => instance_paths.extend(parse_instances(values)?),
            _ => {}
        }
    }
    pins.sort_by(|left, right| {
        (&left.name, &left.source_uuid).cmp(&(&right.name, &right.source_uuid))
    });
    instance_paths
        .sort_by(|left, right| (&left.project, &left.path).cmp(&(&right.project, &right.path)));
    Ok(RawSheet {
        source_uuid: source_uuid.ok_or(SchematicParseError::MissingField("sheet uuid"))?,
        name: name.ok_or(SchematicParseError::MissingField("Sheetname"))?,
        file: file.ok_or(SchematicParseError::MissingField("Sheetfile"))?,
        pins,
        instance_paths,
    })
}

fn parse_sheet_pin(items: &[Node]) -> Result<RawSheetPin, SchematicParseError> {
    let name = required_nth(items, 1, "sheet pin name")?.to_owned();
    let electrical_type = atom_text(items.get(2)).map(str::to_owned);
    let source_uuid = child_value(items, "uuid", "sheet pin uuid")?.to_owned();
    Ok(RawSheetPin {
        source_uuid,
        name,
        electrical_type,
    })
}

fn parse_symbol(items: &[Node]) -> Result<RawSymbol, SchematicParseError> {
    let mut source_uuid = None;
    let mut lib_id = None;
    let mut lib_name = None;
    let mut position = None;
    let mut unit = None;
    let mut body_style = None;
    let mut reference = None;
    let mut value = None;
    let mut footprint = None;
    let mut footprint_seen = false;
    let mut on_board = None;
    let mut in_bom = None;
    let mut dnp = None;
    let mut pins = Vec::new();
    let mut instance_paths = Vec::new();

    for child in &items[1..] {
        let Some(values) = list_items(child) else {
            continue;
        };
        match list_tag(values) {
            Some("uuid") => source_uuid = Some(required_value(values, "symbol uuid")?.to_owned()),
            Some("lib_id") => lib_id = Some(required_value(values, "lib_id")?.to_owned()),
            Some("lib_name") => lib_name = Some(required_value(values, "lib_name")?.to_owned()),
            Some("at") => position = Some(parse_position(values)?),
            Some("unit") => unit = Some(parse_u32(required_value(values, "unit")?, "unit")?),
            Some("body_style" | "convert") => {
                if body_style.is_some() {
                    return Err(SchematicParseError::InvalidField("duplicate body style"));
                }
                body_style = Some(parse_u32(
                    required_value(values, "body_style")?,
                    "body_style",
                )?)
            }
            Some("property") => match atom_text(values.get(1)) {
                Some("Reference") => {
                    reference = Some(required_nth(values, 2, "Reference")?.to_owned())
                }
                Some("Value") => value = Some(required_nth(values, 2, "Value")?.to_owned()),
                Some("Footprint") => {
                    footprint_seen = true;
                    footprint = Some(required_nth(values, 2, "Footprint")?.to_owned());
                }
                _ => {}
            },
            Some("on_board") => {
                on_board = Some(parse_bool(required_value(values, "on_board")?, "on_board")?)
            }
            Some("in_bom") => {
                in_bom = Some(parse_bool(required_value(values, "in_bom")?, "in_bom")?)
            }
            Some("dnp") => dnp = Some(parse_bool(required_value(values, "dnp")?, "dnp")?),
            Some("pin") => pins.push(parse_symbol_pin(values)?),
            Some("instances") => instance_paths.extend(parse_instances(values)?),
            _ => {}
        }
    }
    pins.sort_by(|left, right| {
        (&left.number, &left.source_uuid).cmp(&(&right.number, &right.source_uuid))
    });
    instance_paths
        .sort_by(|left, right| (&left.project, &left.path).cmp(&(&right.project, &right.path)));
    Ok(RawSymbol {
        source_uuid: source_uuid.ok_or(SchematicParseError::MissingField("symbol uuid"))?,
        lib_id: lib_id.ok_or(SchematicParseError::MissingField("lib_id"))?,
        lib_name,
        position: position.ok_or(SchematicParseError::MissingField("at"))?,
        unit: unit.ok_or(SchematicParseError::MissingField("unit"))?,
        body_style,
        reference: reference.ok_or(SchematicParseError::MissingField("Reference"))?,
        value: value.ok_or(SchematicParseError::MissingField("Value"))?,
        footprint: footprint_seen.then_some(footprint.unwrap_or_default()),
        on_board,
        in_bom,
        dnp,
        pins,
        instance_paths,
    })
}

fn parse_symbol_pin(items: &[Node]) -> Result<RawSymbolPin, SchematicParseError> {
    Ok(RawSymbolPin {
        number: required_nth(items, 1, "symbol pin number")?.to_owned(),
        source_uuid: child_value(items, "uuid", "symbol pin uuid")?.to_owned(),
    })
}

fn parse_position(items: &[Node]) -> Result<Position, SchematicParseError> {
    Ok(Position {
        x: parse_f64(required_nth(items, 1, "at x")?, "at x")?,
        y: parse_f64(required_nth(items, 2, "at y")?, "at y")?,
        orientation_degrees: atom_text(items.get(3))
            .map(|value| parse_f64(value, "at orientation"))
            .transpose()?,
    })
}

fn parse_instances(items: &[Node]) -> Result<Vec<InstancePath>, SchematicParseError> {
    let mut result = Vec::new();
    for project_node in &items[1..] {
        let Some(project_items) = list_items(project_node) else {
            continue;
        };
        if list_tag(project_items) != Some("project") {
            continue;
        }
        let project = required_nth(project_items, 1, "instance project")?.to_owned();
        for path_node in &project_items[2..] {
            let Some(path_items) = list_items(path_node) else {
                continue;
            };
            if list_tag(path_items) != Some("path") {
                continue;
            }
            let mut instance = InstancePath {
                project: project.clone(),
                path: required_nth(path_items, 1, "instance path")?.to_owned(),
                reference: None,
                unit: None,
                page: None,
            };
            for field in &path_items[2..] {
                let Some(field_items) = list_items(field) else {
                    continue;
                };
                match list_tag(field_items) {
                    Some("reference") => {
                        instance.reference =
                            Some(required_value(field_items, "instance reference")?.to_owned())
                    }
                    Some("unit") => {
                        instance.unit = Some(parse_u32(
                            required_value(field_items, "instance unit")?,
                            "instance unit",
                        )?)
                    }
                    Some("page") => {
                        instance.page =
                            Some(required_value(field_items, "instance page")?.to_owned())
                    }
                    _ => {}
                }
            }
            result.push(instance);
        }
    }
    Ok(result)
}

fn is_allowed_unparsed_top_level(tag: &str) -> bool {
    matches!(
        tag,
        "paper"
            | "title_block"
            | "lib_symbols"
            | "junction"
            | "no_connect"
            | "bus_entry"
            | "wire"
            | "bus"
            | "polyline"
            | "text"
            | "text_box"
            | "label"
            | "hierarchical_label"
            | "global_label"
            | "sheet_instances"
            | "embedded_fonts"
    )
}

fn list_items(node: &Node) -> Option<&[Node]> {
    match node {
        Node::List { items, .. } => Some(items),
        _ => None,
    }
}

fn list_tag(items: &[Node]) -> Option<&str> {
    atom_text(items.first())
}

fn atom_text(node: Option<&Node>) -> Option<&str> {
    match node {
        Some(Node::Atom {
            atom: Atom::Symbol(value) | Atom::Quoted(value),
            ..
        }) => Some(value),
        _ => None,
    }
}

fn required_value<'a>(
    items: &'a [Node],
    field: &'static str,
) -> Result<&'a str, SchematicParseError> {
    required_nth(items, 1, field)
}

fn required_nth<'a>(
    items: &'a [Node],
    index: usize,
    field: &'static str,
) -> Result<&'a str, SchematicParseError> {
    atom_text(items.get(index)).ok_or(SchematicParseError::MissingField(field))
}

fn child_value<'a>(
    items: &'a [Node],
    tag: &str,
    field: &'static str,
) -> Result<&'a str, SchematicParseError> {
    items[1..]
        .iter()
        .find_map(|node| {
            let child = list_items(node)?;
            (list_tag(child) == Some(tag))
                .then(|| atom_text(child.get(1)))
                .flatten()
        })
        .ok_or(SchematicParseError::MissingField(field))
}

fn parse_u32(value: &str, field: &'static str) -> Result<u32, SchematicParseError> {
    value
        .parse()
        .map_err(|_| SchematicParseError::InvalidField(field))
}

fn parse_f64(value: &str, field: &'static str) -> Result<f64, SchematicParseError> {
    let value = value
        .parse::<f64>()
        .map_err(|_| SchematicParseError::InvalidField(field))?;
    value
        .is_finite()
        .then_some(value)
        .ok_or(SchematicParseError::InvalidField(field))
}

fn parse_bool(value: &str, field: &'static str) -> Result<bool, SchematicParseError> {
    match value {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(SchematicParseError::InvalidField(field)),
    }
}

fn node_span(node: &Node) -> Span {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => *span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLETE: &str = r#"
(kicad_sch
  (version 20231120)
  (generator eeschema)
  (generator_version "8.0")
  (uuid root-uuid)
  (paper "A4")
  (lib_symbols (symbol "Device:R" (symbol "R_1_1")))
  (wire (pts (xy 1 2) (xy 3 4)))
  (sheet
    (at 10 20)
    (size 30 40)
    (property "Sheetname" "Power")
    (property "Sheetfile" "power.kicad_sch")
    (pin "ENABLE" input (at 10 10 0) (uuid sheet-pin-uuid))
    (uuid sheet-uuid)
    (instances (project "demo" (path "/root-uuid/sheet-uuid" (page "2")))))
  (symbol
    (lib_id "Device:R")
    (at 12.5 25.4 90)
    (unit 1)
    (body_style 2)
    (in_bom yes)
    (on_board no)
    (dnp yes)
    (uuid symbol-uuid)
    (property "Reference" "R1")
    (property "Value" "10k")
    (property "Footprint" "")
    (pin "2" (uuid pin-2-uuid))
    (pin "1" (uuid pin-1-uuid))
    (instances (project "demo" (path "/root-uuid/symbol-uuid" (reference "R1") (unit 1)))))
  (symbol
    (lib_id "Device:C")
    (at 1 2)
    (unit 1)
    (uuid symbol-2-uuid)
    (property "Reference" "C1")
    (property "Value" "1u")))
"#;

    #[test]
    fn extracts_structural_identity_and_excludes_library_symbols() {
        let parsed = parse_schematic("root.kicad_sch", COMPLETE.as_bytes()).unwrap();

        assert_eq!(parsed.format_version, "20231120");
        assert_eq!(parsed.generator, "eeschema");
        assert_eq!(parsed.generator_version.as_deref(), Some("8.0"));
        assert_eq!(parsed.file_uuid, "root-uuid");
        assert_eq!(parsed.sheets.len(), 1);
        assert_eq!(parsed.sheets[0].source_uuid, "sheet-uuid");
        assert_eq!(parsed.sheets[0].name, "Power");
        assert_eq!(parsed.sheets[0].file, "power.kicad_sch");
        assert_eq!(parsed.sheets[0].pins[0].source_uuid, "sheet-pin-uuid");
        assert_eq!(
            parsed.sheets[0].pins[0].electrical_type.as_deref(),
            Some("input")
        );
        assert_eq!(
            parsed.sheets[0].instance_paths[0].page.as_deref(),
            Some("2")
        );

        assert_eq!(parsed.symbols.len(), 2);
        let resistor = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.reference == "R1")
            .unwrap();
        assert_eq!(resistor.source_uuid, "symbol-uuid");
        assert_eq!(resistor.lib_id, "Device:R");
        assert_eq!(
            resistor.position,
            Position {
                x: 12.5,
                y: 25.4,
                orientation_degrees: Some(90.0)
            }
        );
        assert_eq!(resistor.body_style, Some(2));
        assert_eq!(
            (resistor.in_bom, resistor.on_board, resistor.dnp),
            (Some(true), Some(false), Some(true))
        );
        assert_eq!(
            resistor.pins[0],
            RawSymbolPin {
                number: "1".into(),
                source_uuid: "pin-1-uuid".into()
            }
        );
        assert_eq!(resistor.instance_paths[0].reference.as_deref(), Some("R1"));
        assert_eq!(resistor.instance_paths[0].unit, Some(1));
        assert!(
            parsed
                .symbols
                .iter()
                .all(|symbol| symbol.source_uuid != "R_1_1")
        );
    }

    #[test]
    fn preserves_explicit_empty_and_absent_footprint() {
        let parsed = parse_schematic("root.kicad_sch", COMPLETE.as_bytes()).unwrap();
        let resistor = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.reference == "R1")
            .unwrap();
        let capacitor = parsed
            .symbols
            .iter()
            .find(|symbol| symbol.reference == "C1")
            .unwrap();
        assert_eq!(resistor.footprint.as_deref(), Some(""));
        assert_eq!(capacitor.footprint, None);
    }

    #[test]
    fn retains_diagnostics_for_explicitly_allowed_unparsed_tokens() {
        let parsed = parse_schematic("root.kicad_sch", COMPLETE.as_bytes()).unwrap();
        let tokens = parsed
            .diagnostics
            .iter()
            .map(|item| item.token.as_str())
            .collect::<Vec<_>>();
        assert_eq!(tokens, vec!["paper", "lib_symbols", "wire"]);
        assert!(
            parsed
                .diagnostics
                .iter()
                .all(|item| item.span.end > item.span.start)
        );
    }

    #[test]
    fn rejects_unknown_top_level_token_fail_closed() {
        let input = "(kicad_sch (version 1) (generator test) (uuid root) (future_connectivity 1))";
        let error = parse_schematic("root.kicad_sch", input.as_bytes()).unwrap_err();
        assert!(
            matches!(error, SchematicParseError::UnsupportedTopLevelToken { ref token, .. } if token == "future_connectivity")
        );
    }

    #[test]
    fn rejects_malformed_cst_wrong_root_and_missing_metadata() {
        assert!(matches!(
            parse_schematic("root.kicad_sch", &[0xff]),
            Err(SchematicParseError::InvalidUtf8)
        ));
        assert!(matches!(
            parse_schematic("root.kicad_sch", b"(kicad_sch"),
            Err(SchematicParseError::MalformedCst(_))
        ));
        assert!(matches!(
            parse_schematic("root.kicad_sch", b"(kicad_pcb (version 1))"),
            Err(SchematicParseError::InvalidRoot)
        ));
        assert!(matches!(
            parse_schematic(
                "root.kicad_sch",
                b"(kicad_sch (version 1) (generator test))"
            ),
            Err(SchematicParseError::MissingField("uuid"))
        ));
    }

    #[test]
    fn artifact_digest_is_deterministic_over_exact_bytes() {
        let first = parse_schematic("root.kicad_sch", COMPLETE.as_bytes()).unwrap();
        let second = parse_schematic("root.kicad_sch", COMPLETE.as_bytes()).unwrap();
        assert_eq!(first.artifact, second.artifact);
        assert_eq!(first.artifact.size, COMPLETE.len() as u64);

        let changed = COMPLETE.replace("10k", "11k");
        let changed = parse_schematic("root.kicad_sch", changed.as_bytes()).unwrap();
        assert_ne!(
            first.artifact.content_digest,
            changed.artifact.content_digest
        );
    }

    struct TestProject {
        path: PathBuf,
    }

    impl TestProject {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "auto-kicad-schematic-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self {
                path: fs::canonicalize(path).unwrap(),
            }
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.path.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, contents).unwrap();
            path
        }

        fn load(&self, root: &Path) -> Result<ParsedSchematicProject, SchematicProjectError> {
            load_schematic_project(&self.path, root, "10.0.5", 1, Limits::default())
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    fn schematic(uuid: &str, sheets: &[(&str, &str)]) -> String {
        let sheets = sheets.iter().map(|(sheet_uuid, file)| format!(
            "(sheet (property \"Sheetname\" \"Child\") (property \"Sheetfile\" \"{file}\") (uuid {sheet_uuid}))"
        )).collect::<Vec<_>>().join("\n");
        format!("(kicad_sch (version 20260306) (generator eeschema) (uuid {uuid}) {sheets})")
    }

    #[test]
    fn loads_transitive_files_once_and_preserves_shared_sheet_references() {
        let project = TestProject::new();
        let root = project.write(
            "root.kicad_sch",
            &schematic(
                "root",
                &[
                    ("sheet-b", "child.kicad_sch"),
                    ("sheet-a", "child.kicad_sch"),
                ],
            ),
        );
        project.write("child.kicad_sch", &schematic("child", &[]));

        let loaded = project.load(&root).unwrap();
        assert_eq!(loaded.root_relative_path, "root.kicad_sch");
        assert_eq!(loaded.files.len(), 2);
        assert_eq!(loaded.files[0].artifact.relative_path, "child.kicad_sch");
        assert_eq!(loaded.files[1].artifact.relative_path, "root.kicad_sch");
        let root_file = loaded
            .files
            .iter()
            .find(|file| file.file_uuid == "root")
            .unwrap();
        assert_eq!(root_file.sheets.len(), 2);
        assert_ne!(
            root_file.sheets[0].source_uuid,
            root_file.sheets[1].source_uuid
        );
        assert!(loaded.revision.validate().is_ok());
    }

    #[test]
    fn rejects_root_and_sheet_paths_outside_project() {
        let project = TestProject::new();
        let outside = TestProject::new();
        let outside_root = outside.write("outside.kicad_sch", &schematic("outside", &[]));
        assert!(matches!(
            project.load(&outside_root),
            Err(SchematicProjectError::PathEscape { .. })
        ));

        let root = project.write(
            "root.kicad_sch",
            &schematic("root", &[("sheet", "../outside.kicad_sch")]),
        );
        let escaped = project.path.parent().unwrap().join("outside.kicad_sch");
        fs::write(&escaped, schematic("escaped", &[])).unwrap();
        assert!(matches!(
            project.load(&root),
            Err(SchematicProjectError::PathEscape { .. })
        ));
        fs::remove_file(escaped).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let project = TestProject::new();
        let outside = TestProject::new();
        let outside_child = outside.write("child.kicad_sch", &schematic("child", &[]));
        let root = project.write(
            "root.kicad_sch",
            &schematic("root", &[("sheet", "child.kicad_sch")]),
        );
        symlink(outside_child, project.path.join("child.kicad_sch")).unwrap();
        assert!(matches!(
            project.load(&root),
            Err(SchematicProjectError::PathEscape { .. })
        ));
    }

    #[test]
    fn rejects_missing_child_cycle_and_malformed_child() {
        let missing = TestProject::new();
        let root = missing.write(
            "root.kicad_sch",
            &schematic("root", &[("sheet", "missing.kicad_sch")]),
        );
        assert!(matches!(
            missing.load(&root),
            Err(SchematicProjectError::Unreadable { .. })
        ));

        let cycle = TestProject::new();
        let root = cycle.write(
            "root.kicad_sch",
            &schematic("root", &[("a", "child.kicad_sch")]),
        );
        cycle.write(
            "child.kicad_sch",
            &schematic("child", &[("b", "root.kicad_sch")]),
        );
        assert!(
            matches!(cycle.load(&root), Err(SchematicProjectError::Cycle { ref path }) if path == "root.kicad_sch")
        );

        let malformed = TestProject::new();
        let root = malformed.write(
            "root.kicad_sch",
            &schematic("root", &[("sheet", "child.kicad_sch")]),
        );
        malformed.write("child.kicad_sch", "(kicad_sch");
        assert!(
            matches!(malformed.load(&root), Err(SchematicProjectError::Parse { ref path, .. }) if path == "child.kicad_sch")
        );
    }

    #[test]
    fn enforces_depth_file_and_byte_limits() {
        let project = TestProject::new();
        let root_contents = schematic("root", &[("sheet", "child.kicad_sch")]);
        let child_contents = schematic("child", &[]);
        let root = project.write("root.kicad_sch", &root_contents);
        project.write("child.kicad_sch", &child_contents);

        for (limits, expected) in [
            (
                Limits {
                    max_depth: 0,
                    ..Limits::default()
                },
                "depth",
            ),
            (
                Limits {
                    max_files: 1,
                    ..Limits::default()
                },
                "files",
            ),
            (
                Limits {
                    max_file_bytes: root_contents.len() as u64 - 1,
                    ..Limits::default()
                },
                "file-bytes",
            ),
            (
                Limits {
                    max_total_bytes: (root_contents.len() + child_contents.len() - 1) as u64,
                    ..Limits::default()
                },
                "total-bytes",
            ),
        ] {
            let error = load_schematic_project(&project.path, &root, "10", 1, limits).unwrap_err();
            assert!(match expected {
                "depth" => matches!(error, SchematicProjectError::DepthLimit { .. }),
                "files" => matches!(error, SchematicProjectError::FileLimit { limit: 1 }),
                "file-bytes" => matches!(error, SchematicProjectError::FileSizeLimit { .. }),
                "total-bytes" => matches!(error, SchematicProjectError::TotalSizeLimit { .. }),
                _ => false,
            });
        }
    }

    fn test_netlist_component(
        reference: &str,
        symbol_uuid: Option<&str>,
        sheet_uuid_path: &str,
        footprint: Option<&str>,
    ) -> NetlistComponent {
        NetlistComponent {
            units: None,
            reference: reference.into(),
            value: "10k".into(),
            footprint: footprint.map(str::to_owned),
            symbol_uuid: symbol_uuid.map(str::to_owned),
            sheet_path: "root.kicad_sch".into(),
            sheet_uuid_path: sheet_uuid_path.into(),
            library: "Device".into(),
            library_part: "R".into(),
        }
    }

    fn test_netlist(components: Vec<NetlistComponent>) -> NetlistDocument {
        NetlistDocument {
            export_version: "D".into(),
            sheets: Vec::new(),
            components,
            nets: Vec::new(),
        }
    }

    #[test]
    fn joins_component_to_unique_symbol_and_preserves_empty_footprint() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let parsed = project.load(&root).unwrap();
        let id = StableId::new("project").unwrap();
        let netlist = test_netlist(vec![test_netlist_component(
            "R1",
            Some("symbol-uuid"),
            "/symbol-uuid/",
            Some(""),
        )]);

        let joined = join_netlist_components(&parsed, &netlist, &id).unwrap();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].symbol.reference, "R1");
        assert_eq!(joined[0].symbol.footprint_field.as_deref(), Some(""));
        assert_eq!(
            joined[0].symbol.sheet_instance_path,
            "/root-uuid/symbol-uuid"
        );
        assert_eq!(joined[0].symbol.pins.len(), 2);
        assert_eq!(joined[0].symbol.pins[0].number, "1");
    }

    #[test]
    fn normalizes_netlist_root_path_to_root_schematic_uuid() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let mut parsed = project.load(&root).unwrap();
        let id = StableId::new("project").unwrap();
        let root_symbol = parsed
            .files
            .iter_mut()
            .find(|file| file.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference == "R1")
            .unwrap();
        root_symbol.instance_paths[0].path = "/root-uuid".into();

        let joined = join_netlist_components(
            &parsed,
            &test_netlist(vec![test_netlist_component(
                "R1",
                Some("symbol-uuid"),
                "/",
                Some(""),
            )]),
            &id,
        )
        .unwrap();
        assert_eq!(joined[0].symbol.sheet_instance_path, "/root-uuid");

        parsed
            .files
            .iter_mut()
            .find(|file| file.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference == "R1")
            .unwrap()
            .instance_paths[0]
            .path = "/unrelated-root-uuid".into();
        assert!(matches!(
            join_netlist_components(
                &parsed,
                &test_netlist(vec![test_netlist_component(
                    "R1",
                    Some("symbol-uuid"),
                    "/",
                    Some("")
                )]),
                &id
            ),
            Err(JoinError::MissingOrAmbiguousSymbol {
                kind: "component",
                ..
            })
        ));
    }

    #[test]
    fn canonicalizes_real_netlist_instance_paths() {
        let root = "11111111-1111-1111-1111-111111111111";
        let child = "22222222-2222-2222-2222-222222222222";

        assert_eq!(
            normalize_netlist_instance_path("/", root),
            Some(format!("/{root}"))
        );
        assert_eq!(
            normalize_netlist_instance_path(&format!("/{child}/"), root),
            Some(format!("/{root}/{child}"))
        );
        assert_eq!(
            normalize_netlist_instance_path(&format!("/{root}/{child}"), root),
            None
        );
        assert_eq!(
            normalize_netlist_instance_path("/root/symbol-uuid", root),
            Some(format!("/{root}/root/symbol-uuid"))
        );
        assert_eq!(normalize_netlist_instance_path("/../child", root), None);
    }

    #[test]
    fn rejects_duplicate_reference_and_missing_or_mismatched_component() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let parsed = project.load(&root).unwrap();
        let id = StableId::new("project").unwrap();
        let duplicate = RawSymbol {
            instance_paths: vec![InstancePath {
                project: "demo".into(),
                path: "/root-uuid/symbol-uuid".into(),
                reference: Some("R1".into()),
                unit: Some(1),
                page: None,
            }],
            ..parsed
                .files
                .iter()
                .find(|f| f.file_uuid == "root-uuid")
                .unwrap()
                .symbols
                .iter()
                .find(|s| s.reference == "R1")
                .unwrap()
                .clone()
        };
        let mut duplicate_project = parsed.clone();
        duplicate_project
            .files
            .iter_mut()
            .find(|f| f.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .push(duplicate);
        assert!(matches!(
            join_netlist_components(
                &duplicate_project,
                &test_netlist(vec![test_netlist_component(
                    "R1",
                    Some("symbol-uuid"),
                    "/symbol-uuid/",
                    Some("")
                )]),
                &id
            ),
            Err(JoinError::MissingOrAmbiguousSymbol {
                kind: "ambiguous component",
                ..
            })
        ));
        assert!(matches!(
            join_netlist_components(
                &parsed,
                &test_netlist(vec![test_netlist_component("X1", None, "/x", None)]),
                &id
            ),
            Err(JoinError::MissingOrAmbiguousSymbol {
                kind: "component",
                ..
            })
        ));
        assert!(matches!(
            join_netlist_components(
                &parsed,
                &test_netlist(vec![test_netlist_component(
                    "R1",
                    Some("symbol-uuid"),
                    "/other",
                    Some("")
                )]),
                &id
            ),
            Err(JoinError::MissingOrAmbiguousSymbol {
                kind: "component",
                ..
            })
        ));
    }

    #[test]
    fn rejects_ambiguous_pin_identity_across_sheet_instances() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let parsed = project.load(&root).unwrap();
        let id = StableId::new("project").unwrap();

        let original = parsed
            .files
            .iter()
            .find(|file| file.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .iter()
            .find(|symbol| symbol.reference == "R1")
            .unwrap()
            .clone();
        let mut second = original.clone();
        second.source_uuid = "symbol-uuid-2".into();
        second.instance_paths[0].path = "/root-uuid/symbol-uuid-2".into();
        let mut duplicate_project = parsed.clone();
        duplicate_project
            .files
            .iter_mut()
            .find(|file| file.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .push(second);

        let netlist = test_netlist(vec![
            test_netlist_component("R1", Some("symbol-uuid"), "/symbol-uuid/", None),
            test_netlist_component("R1", Some("symbol-uuid-2"), "/symbol-uuid-2/", None),
        ]);
        assert!(matches!(
            join_netlist_connectivity(&duplicate_project, &netlist, &id),
            Err(ConnectivityJoinError::DuplicatePinIdentity {
                reference,
                pin
            }) if reference == "R1" && pin == "1"
        ));
    }

    #[test]
    fn rejects_symbol_uuid_mismatch_and_duplicate_pin_number() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let parsed = project.load(&root).unwrap();
        let id = StableId::new("project").unwrap();
        assert!(matches!(
            join_netlist_components(
                &parsed,
                &test_netlist(vec![test_netlist_component(
                    "R1",
                    Some("wrong"),
                    "/symbol-uuid/",
                    Some("")
                )]),
                &id
            ),
            Err(JoinError::SymbolUuidMismatch { .. })
        ));
        let mut duplicate_project = parsed.clone();
        duplicate_project
            .files
            .iter_mut()
            .find(|f| f.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .iter_mut()
            .find(|s| s.reference == "R1")
            .unwrap()
            .pins
            .push(RawSymbolPin {
                number: "1".into(),
                source_uuid: "another-pin".into(),
            });
        assert!(matches!(
            join_netlist_components(&duplicate_project, &test_netlist(vec![test_netlist_component("R1", Some("symbol-uuid"), "/symbol-uuid/", Some(""))]), &id),
            Err(JoinError::DuplicatePinNumber { number, .. }) if number == "1"
        ));
    }

    #[test]
    fn joins_in_deterministic_order_and_derives_stable_ids() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", COMPLETE);
        project.write("power.kicad_sch", &schematic("power", &[]));
        let mut parsed = project.load(&root).unwrap();
        parsed
            .files
            .iter_mut()
            .find(|file| file.file_uuid == "root-uuid")
            .unwrap()
            .symbols
            .iter_mut()
            .find(|symbol| symbol.reference == "C1")
            .unwrap()
            .instance_paths
            .push(InstancePath {
                project: "demo".into(),
                path: "/root-uuid/symbol-2-uuid".into(),
                reference: Some("C1".into()),
                unit: Some(1),
                page: None,
            });
        let id = StableId::new("project").unwrap();
        let r1 = test_netlist_component("R1", Some("symbol-uuid"), "/symbol-uuid/", Some(""));
        let c1 = test_netlist_component("C1", Some("symbol-2-uuid"), "/symbol-2-uuid/", None);
        let first =
            join_netlist_components(&parsed, &test_netlist(vec![r1.clone(), c1.clone()]), &id)
                .unwrap();
        let second = join_netlist_components(&parsed, &test_netlist(vec![c1, r1]), &id).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].symbol.reference, "C1");
        assert_eq!(first[0].symbol.observed_revision, parsed.revision.digest);
        assert_ne!(first[0].symbol.symbol_id, first[1].symbol.symbol_id);
    }

    #[test]
    fn aggregate_revision_is_deterministic_and_lock_is_only_gating_metadata() {
        let project = TestProject::new();
        let root = project.write(
            "root.kicad_sch",
            &schematic("root", &[("sheet", "child.kicad_sch")]),
        );
        project.write("child.kicad_sch", &schematic("child", &[]));

        let first = project.load(&root).unwrap();
        let second = project.load(&root).unwrap();
        assert_eq!(first, second);
        fs::write(project.path.join("~child.kicad_sch.lck"), "not read").unwrap();
        let locked = project.load(&root).unwrap();
        assert!(!first.editor_lock_present);
        assert!(locked.editor_lock_present);
        assert_eq!(first.revision.digest, locked.revision.digest);
        assert_eq!(first.revision.files, locked.revision.files);
    }

    #[test]
    fn rejects_invalid_revision_metadata() {
        let project = TestProject::new();
        let root = project.write("root.kicad_sch", &schematic("root", &[]));
        assert!(matches!(
            load_schematic_project(&project.path, &root, "", 1, Limits::default()),
            Err(SchematicProjectError::InvalidRevision(_))
        ));
        assert!(matches!(
            load_schematic_project(&project.path, &root, "10", 0, Limits::default()),
            Err(SchematicProjectError::InvalidRevision(_))
        ));
    }
}
