//! Typed schematic edits compiled to immutable candidate bytes without source publication.
use crate::cst::parse_one;
use crate::geometry::{Coordinate, Mirror, Point, QuarterTurn, SymbolTransform};
use crate::identities::{SchematicIdentityIndex, uuid};
use crate::{
    NetlistDocument, ParsedSchematicProject, atom_text, list_items, list_tag, node_span,
    parse_schematic,
};
use kicad_model::SchematicArtifactRevision;
use kiutils_sexpr::{Node, Span};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
mod components;
mod creation;
mod sheets;
pub use sheets::{SheetPin, SheetPinEdit, SheetRectangle, SheetUpdate};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditScope {
    SingleInstance(String),
    FileAllInstances,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
    Sheet,
    Symbol,
    Wire,
    Junction,
    NoConnect,
    LocalLabel,
    GlobalLabel,
    HierarchicalLabel,
}
impl ObjectKind {
    fn tag(self) -> &'static str {
        match self {
            Self::Sheet => "sheet",
            Self::Symbol => "symbol",
            Self::Wire => "wire",
            Self::Junction => "junction",
            Self::NoConnect => "no_connect",
            Self::LocalLabel => "label",
            Self::GlobalLabel => "global_label",
            Self::HierarchicalLabel => "hierarchical_label",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ElectricalShape {
    Input,
    Output,
    Bidirectional,
    TriState,
    Passive,
}
impl ElectricalShape {
    fn atom(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::Bidirectional => "bidirectional",
            Self::TriState => "tri_state",
            Self::Passive => "passive",
        }
    }
}

/// Local labels have no electrical shape; global and hierarchical labels require one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LabelKind {
    Local,
    Global(ElectricalShape),
    Hierarchical(ElectricalShape),
}
impl LabelKind {
    fn object(&self) -> ObjectKind {
        match self {
            Self::Local => ObjectKind::LocalLabel,
            Self::Global(_) => ObjectKind::GlobalLabel,
            Self::Hierarchical(_) => ObjectKind::HierarchicalLabel,
        }
    }
    fn shape(&self) -> Option<ElectricalShape> {
        match self {
            Self::Local => None,
            Self::Global(s) | Self::Hierarchical(s) => Some(*s),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Label {
    pub kind: LabelKind,
    pub text: String,
    pub position: Point,
    pub rotation: QuarterTurn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NewObject {
    Wire { start: Point, end: Point },
    Junction(Point),
    NoConnect(Point),
    Label(Label),
}
impl NewObject {
    fn kind(&self) -> ObjectKind {
        match self {
            Self::Wire { .. } => ObjectKind::Wire,
            Self::Junction(_) => ObjectKind::Junction,
            Self::NoConnect(_) => ObjectKind::NoConnect,
            Self::Label(l) => l.kind.object(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SymbolProperty {
    Value,
    Named(String),
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SymbolFlags {
    pub in_bom: Option<bool>,
    pub on_board: Option<bool>,
    pub dnp: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditAction {
    UpdateSheet {
        uuid: String,
        update: SheetUpdate,
    },
    CreateSymbol {
        key: String,
        symbol: SymbolCreation,
    },
    AddSymbolUnit {
        key: String,
        uuid: String,
        unit: u32,
        transform: SymbolTransform,
    },
    SetReference {
        uuid: String,
        reference: String,
        instance_path: Option<String>,
    },
    AssignFootprint {
        uuid: String,
        footprint: String,
        evidence: FootprintCompatibilityEvidence,
    },
    SetProperty {
        uuid: String,
        property: SymbolProperty,
        value: String,
    },
    SetFlags {
        uuid: String,
        flags: SymbolFlags,
    },
    TransformSymbol {
        uuid: String,
        transform: SymbolTransform,
    },
    UpdateWire {
        uuid: String,
        start: Point,
        end: Point,
    },
    MoveMarker {
        uuid: String,
        kind: ObjectKind,
        position: Point,
    },
    UpdateLabel {
        uuid: String,
        label: Label,
    },
    Delete {
        uuid: String,
        kind: ObjectKind,
    },
    Create {
        key: String,
        object: NewObject,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceRequest {
    pub uuid: String,
    pub instance_path: Option<String>,
    pub prefix: String,
    pub minimum: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintCompatibilityEvidence {
    pub catalog_digest: String,
    pub source_digest: String,
    pub pad_numbers: Vec<String>,
    pub pin_pad_map: BTreeMap<String, String>,
}

/// Exact library definition bound to an approved symbol catalog observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolDefinitionEvidence {
    pub lib_id: String,
    pub catalog_digest: String,
    pub source_digest: String,
    pub definition: String,
}

/// One physical symbol instance path in the expanded schematic hierarchy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolInstancePath {
    pub path: String,
    pub reference: Option<String>,
}

/// Typed creation request for one placed symbol and its embedded definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolCreation {
    pub evidence: SymbolDefinitionEvidence,
    pub transform: SymbolTransform,
    pub unit: u32,
    pub body_style: Option<u32>,
    pub reference: String,
    pub value: String,
    pub footprint: Option<String>,
    pub flags: SymbolFlags,
    pub instance_paths: Vec<SymbolInstancePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicEdit {
    pub file: String,
    pub scope: EditScope,
    pub action: EditAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectChange {
    pub file: String,
    pub uuid: String,
    pub kind: ObjectKind,
    pub instance_paths: Vec<String>,
    pub action: EditAction,
}

/// Exact source spans and replacement text; unchanged intervals stay byte-identical.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceReplacement {
    pub start: usize,
    pub end: usize,
    pub before: String,
    pub after: String,
}

/// A source-bound candidate only; native validation and publication approval are separate.
pub struct SchematicCandidate {
    source_revision: SchematicArtifactRevision,
    parsed: ParsedSchematicProject,
    files: BTreeMap<String, Vec<u8>>,
    changes: Vec<ObjectChange>,
    replacements: BTreeMap<String, Vec<SourceReplacement>>,
}
impl SchematicCandidate {
    pub fn source_revision(&self) -> &SchematicArtifactRevision {
        &self.source_revision
    }
    pub fn parsed(&self) -> &ParsedSchematicProject {
        &self.parsed
    }
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    pub fn changes(&self) -> &[ObjectChange] {
        &self.changes
    }
    pub fn replacements(&self) -> &BTreeMap<String, Vec<SourceReplacement>> {
        &self.replacements
    }
}

fn children<'a>(items: &'a [Node], tag: &'a str) -> impl Iterator<Item = &'a Node> {
    items
        .iter()
        .filter(move |n| list_items(n).is_some_and(|v| list_tag(v) == Some(tag)))
}
fn optional<'a>(items: &'a [Node], tag: &str) -> Result<Option<&'a Node>, String> {
    let found = items
        .iter()
        .filter(|n| list_items(n).is_some_and(|v| list_tag(v) == Some(tag)))
        .collect::<Vec<_>>();
    if found.len() > 1 {
        return Err(format!("duplicate {tag} field"));
    }
    Ok(found.first().copied())
}
fn one<'a>(items: &'a [Node], tag: &str) -> Result<&'a Node, String> {
    optional(items, tag)?.ok_or_else(|| format!("missing {tag} field"))
}
fn values(node: &Node) -> Result<&[Node], String> {
    list_items(node).ok_or_else(|| "expected list".into())
}
fn atom(items: &[Node], i: usize) -> Result<&str, String> {
    atom_text(items.get(i)).ok_or_else(|| "expected atom".into())
}
fn text(value: &str, empty: bool) -> Result<(), String> {
    if value.len() > 65_536
        || (!empty && value.is_empty())
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err("invalid or excessive schematic text".into());
    }
    Ok(())
}
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
fn degrees(rotation: QuarterTurn) -> u16 {
    match rotation {
        QuarterTurn::Zero => 0,
        QuarterTurn::Counterclockwise90 => 90,
        QuarterTurn::Half => 180,
        QuarterTurn::Counterclockwise270 => 270,
    }
}
fn at(position: Point, rotation: Option<QuarterTurn>) -> String {
    format!(
        "(at {} {}{})",
        position.x.mm(),
        position.y.mm(),
        rotation
            .map(|r| format!(" {}", degrees(r)))
            .unwrap_or_default()
    )
}
fn points(start: Point, end: Point) -> Result<String, String> {
    if start == end {
        return Err("zero-length wire".into());
    }
    Ok(format!(
        "(pts (xy {} {}) (xy {} {}))",
        start.x.mm(),
        start.y.mm(),
        end.x.mm(),
        end.y.mm()
    ))
}
fn generated_uuid(revision: &str, ordinal: usize) -> String {
    generated_uuid_seed(&format!("schematic-edit-v1:{revision}:{ordinal}"))
}
fn generated_uuid_seed(seed: &str) -> String {
    let mut hash = Sha256::digest(seed);
    hash[6] = (hash[6] & 15) | 64;
    hash[8] = (hash[8] & 63) | 128;
    let s = hex::encode(&hash[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bool_atom(value: Option<bool>, default: bool) -> &'static str {
    if value.unwrap_or(default) {
        "yes"
    } else {
        "no"
    }
}

fn validate_symbol_creation(symbol: &SymbolCreation) -> Result<(), String> {
    validate_reference(&symbol.reference)?;
    text(&symbol.value, true)?;
    if !valid_digest(&symbol.evidence.catalog_digest)
        || !valid_digest(&symbol.evidence.source_digest)
    {
        return Err("symbol catalog/source evidence is incomplete".into());
    }
    validate_library_identifier(&symbol.evidence.lib_id)?;
    if symbol.evidence.definition.len() > 32 * 1024 * 1024 {
        return Err("embedded symbol definition exceeds bound".into());
    }
    if symbol.unit == 0 || symbol.unit > 1024 {
        return Err("invalid symbol unit".into());
    }
    if symbol.body_style == Some(0) || symbol.body_style.is_some_and(|style| style > 1024) {
        return Err("invalid symbol body style".into());
    }
    if let Some(footprint) = &symbol.footprint {
        validate_footprint_identifier(footprint)?;
    }
    if symbol.instance_paths.is_empty() || symbol.instance_paths.len() > 4096 {
        return Err("symbol creation requires 1..4096 instance paths".into());
    }
    let mut paths = BTreeSet::new();
    for instance in &symbol.instance_paths {
        if instance.path.len() > 4096
            || !instance.path.starts_with('/')
            || instance.path.contains(['\\', '\n', '\r', '\t'])
            || !paths.insert(instance.path.clone())
        {
            return Err("invalid or repeated symbol instance path".into());
        }
        validate_reference(instance.reference.as_deref().unwrap_or(&symbol.reference))?;
    }
    if symbol.instance_paths.len() > 1
        && symbol
            .instance_paths
            .iter()
            .any(|instance| instance.reference.is_none())
    {
        return Err("reused-sheet symbol creation requires explicit instance references".into());
    }
    if symbol
        .instance_paths
        .iter()
        .min_by_key(|instance| &instance.path)
        .is_some_and(|instance| {
            instance.reference.as_deref().unwrap_or(&symbol.reference) != symbol.reference
        })
    {
        return Err("displayed reference must match the first observed instance".into());
    }
    Ok(())
}

fn validate_library_identifier(value: &str) -> Result<(), String> {
    let (nickname, entry) = value
        .split_once(':')
        .ok_or("symbol identifier must be nickname:entry")?;
    if nickname.is_empty()
        || entry.is_empty()
        || nickname.len() > 128
        || entry.len() > 256
        || !nickname
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || !entry
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'~'))
    {
        return Err("invalid symbol library identifier".into());
    }
    Ok(())
}

fn parse_symbol_definition(
    definition: &str,
    expected: &str,
    unit: u32,
    body_style: Option<u32>,
) -> Result<(String, String, Vec<String>), String> {
    let document = parse_one(definition.trim()).map_err(|e| e.to_string())?;
    if document.nodes.len() != 1 {
        return Err("symbol definition must contain one node".into());
    }
    let root = values(&document.nodes[0])?;
    if list_tag(root) != Some("symbol") {
        return Err("symbol definition root must be symbol".into());
    }
    let (nickname, entry) = expected
        .split_once(':')
        .ok_or("symbol identifier must be nickname:entry")?;
    let name = atom(root, 1)?.to_owned();
    if name != expected && name != entry {
        return Err("symbol definition identifier differs from catalog binding".into());
    }
    let normalized = if name == expected {
        definition.trim().to_owned()
    } else {
        let root_items = list_items(&document.nodes[0]).ok_or("invalid symbol definition")?;
        let atom_span = node_span(root_items.get(1).ok_or("missing symbol identifier")?);
        let base = definition.trim();
        let start = atom_span.start;
        let end = atom_span.end;
        if end > base.len() || start >= end {
            return Err("invalid symbol definition identifier span".into());
        }
        let mut rendered = base.to_owned();
        rendered.replace_range(start..end, &quote(expected));
        rendered
    };
    let pins = creation::selected_pins(root, unit, body_style)?;
    let _ = nickname;
    Ok((name, normalized, pins))
}

struct CreationAllocation {
    symbol_uuid: String,
    pin_uuids: Vec<String>,
    definition: String,
    pin_numbers: Vec<String>,
}
enum SymbolAllocation {
    Catalog(CreationAllocation),
    Unit {
        symbol_uuid: String,
        rendered: String,
    },
}

fn render_created_symbol(
    symbol: &SymbolCreation,
    allocation: &CreationAllocation,
    format: &str,
    project: &str,
) -> Result<String, String> {
    let mut result = String::new();
    result.push_str("\n(symbol\n");
    result.push_str(&format!("\t(lib_id {})\n", quote(&symbol.evidence.lib_id)));
    result.push_str(&format!(
        "\t{}\n",
        at(symbol.transform.origin, Some(symbol.transform.rotation))
    ));
    if symbol.transform.mirror != Mirror::None {
        result.push_str(&format!(
            "\t(mirror {})\n",
            if symbol.transform.mirror == Mirror::X {
                "x"
            } else {
                "y"
            }
        ));
    }
    result.push_str(&format!("\t(unit {})\n", symbol.unit));
    if let Some(style) = symbol.body_style {
        result.push_str(&format!(
            "\t({} {})\n",
            if format == "20250114" {
                "convert"
            } else {
                "body_style"
            },
            style
        ));
    }
    result.push_str("\t(exclude_from_sim no)\n");
    result.push_str(&format!(
        "\t(in_bom {})\n\t(on_board {})\n\t(dnp {})\n",
        bool_atom(symbol.flags.in_bom, true),
        bool_atom(symbol.flags.on_board, true),
        bool_atom(symbol.flags.dnp, false)
    ));
    result.push_str(&format!("\t(uuid {})\n", quote(&allocation.symbol_uuid)));
    result.push_str(&format!(
        "\t(property \"Reference\" {} {} (effects (font (size 1.27 1.27))))\n",
        quote(&symbol.reference),
        at(symbol.transform.origin, Some(QuarterTurn::Zero))
    ));
    result.push_str(&format!(
        "\t(property \"Value\" {} {} (effects (font (size 1.27 1.27))))\n",
        quote(&symbol.value),
        at(symbol.transform.origin, Some(QuarterTurn::Zero))
    ));
    if let Some(footprint) = &symbol.footprint {
        result.push_str(&format!(
            "\t(property \"Footprint\" {} {} (effects (font (size 1.27 1.27)) (hide yes)))\n",
            quote(footprint),
            at(symbol.transform.origin, Some(QuarterTurn::Zero))
        ));
    }
    for (number, uuid) in allocation.pin_numbers.iter().zip(&allocation.pin_uuids) {
        result.push_str(&format!(
            "\t(pin {} (uuid {}))\n",
            quote(number),
            quote(uuid)
        ));
    }
    result.push_str("\t(instances\n\t\t(project ");
    result.push_str(&quote(project));
    result.push('\n');
    for instance in &symbol.instance_paths {
        result.push_str(&format!(
            "\t\t\t(path {} (reference {}) (unit {}))\n",
            quote(&instance.path),
            quote(instance.reference.as_deref().unwrap_or(&symbol.reference)),
            symbol.unit
        ));
    }
    result.push_str("\t\t)\n\t)\n)\n");
    Ok(result)
}
fn render(object: &NewObject, id: &str) -> Result<String, String> {
    let body = match object {
        NewObject::Wire { start, end } => {
            format!("{} (stroke (width 0) (type solid))", points(*start, *end)?)
        }
        NewObject::Junction(p) => format!("{} (diameter 0) (color 0 0 0 0)", at(*p, None)),
        NewObject::NoConnect(p) => at(*p, None),
        NewObject::Label(l) => {
            text(&l.text, false)?;
            format!(
                "{} {} {} (effects (font (size 1.27 1.27)) (justify left bottom))",
                quote(&l.text),
                at(l.position, Some(l.rotation)),
                l.kind
                    .shape()
                    .map(|s| format!("(shape {})", s.atom()))
                    .unwrap_or_default()
            )
        }
    };
    Ok(format!(
        "\n({} {body} (uuid {}))\n",
        object.kind().tag(),
        quote(id)
    ))
}

struct Patches<'a> {
    text: &'a str,
    replacements: Vec<SourceReplacement>,
    claims: BTreeSet<String>,
}
impl Patches<'_> {
    fn span(&mut self, span: Span, after: String) -> Result<(), String> {
        let before = self
            .text
            .get(span.start..span.end)
            .ok_or("invalid UTF-8 source span")?;
        self.replacements.push(SourceReplacement {
            start: span.start,
            end: span.end,
            before: before.into(),
            after,
        });
        Ok(())
    }
    fn node(&mut self, node: &Node, after: String) -> Result<(), String> {
        self.span(node_span(node), after)
    }
    fn claim(&mut self, id: &str, field: &str) -> Result<(), String> {
        if !self.claims.insert(format!("{id}:{field}")) {
            return Err("repeated field write".into());
        }
        Ok(())
    }
    fn insert(&mut self, offset: usize, after: String) -> Result<(), String> {
        if let Some(existing) = self
            .replacements
            .iter_mut()
            .find(|p| p.start == offset && p.end == offset)
        {
            existing.after.push_str(&after);
            return Ok(());
        }
        self.span(
            Span {
                start: offset,
                end: offset,
            },
            after,
        )
    }
    fn finish(mut self) -> Result<(Vec<u8>, Vec<SourceReplacement>), String> {
        self.replacements.sort_by_key(|p| (p.start, p.end));
        for pair in self.replacements.windows(2) {
            if pair[1].start < pair[0].end || pair[1].start == pair[0].start {
                return Err("overlapping schematic edits".into());
            }
        }
        self.replacements.retain(|p| p.before != p.after);
        let mut candidate = self.text.to_owned();
        for replacement in self.replacements.iter().rev() {
            candidate.replace_range(replacement.start..replacement.end, &replacement.after);
        }
        if candidate.len() > 32 * 1024 * 1024 {
            return Err("candidate file exceeds bound".into());
        }
        Ok((candidate.into_bytes(), self.replacements))
    }
}

fn object<'a>(root: &'a [Node], id: &str, kind: ObjectKind) -> Result<&'a Node, String> {
    if !uuid(id) {
        return Err("invalid target UUID".into());
    }
    let mut found = None;
    for node in children(root, kind.tag()) {
        let items = values(node)?;
        if atom(values(one(items, "uuid")?)?, 1)? == id && found.replace(node).is_some() {
            return Err("ambiguous target UUID".into());
        }
    }
    found.ok_or_else(|| format!("missing {} target {id}", kind.tag()))
}

// Apply the relative orthogonal transform without floating-point coordinates.
fn linear(transform: SymbolTransform, x: i64, y: i64) -> (i64, i64) {
    let (mut x, mut y) = match transform.rotation {
        QuarterTurn::Zero => (x, y),
        QuarterTurn::Counterclockwise90 => (y, -x),
        QuarterTurn::Half => (-x, -y),
        QuarterTurn::Counterclockwise270 => (-y, x),
    };
    match transform.mirror {
        Mirror::None => {}
        Mirror::X => y = -y,
        Mirror::Y => x = -x,
    }
    (x, y)
}
fn map_offset(old: SymbolTransform, new: SymbolTransform, x: i64, y: i64) -> (i64, i64) {
    let a = linear(old, 1, 0);
    let b = linear(old, 0, 1);
    linear(new, a.0 * x + a.1 * y, b.0 * x + b.1 * y)
}
fn transform_fields(
    items: &[Node],
    old: SymbolTransform,
    new: SymbolTransform,
    patches: &mut Patches<'_>,
) -> Result<(), String> {
    for field in children(items, "property") {
        let field = values(field)?;
        let node = one(field, "at")?;
        let coordinates = values(node)?;
        if coordinates.len() != 4 {
            return Err("unsupported property position".into());
        }
        let p = Point::from_at(coordinates)?;
        let (dx, dy) = map_offset(
            old,
            new,
            p.x.nm() - old.origin.x.nm(),
            p.y.nm() - old.origin.y.nm(),
        );
        let position = Point::from_nm(new.origin.x.nm() + dx, new.origin.y.nm() + dy)?;
        let angle = Coordinate::parse_mm(atom(coordinates, 3)?)?.nm();
        let vector = match angle {
            0 | 180_000_000 => (1, 0),
            90_000_000 | 270_000_000 => (0, 1),
            _ => return Err("unsupported property text angle".into()),
        };
        let vector = map_offset(old, new, vector.0, vector.1);
        let rotation = if vector.0 == 0 {
            QuarterTurn::Counterclockwise90
        } else {
            QuarterTurn::Zero
        };
        patches.node(node, at(position, Some(rotation)))?;
    }
    Ok(())
}

fn edit_existing(
    root: &[Node],
    action: &EditAction,
    patches: &mut Patches<'_>,
    format: &str,
    annotation_scope: Option<(&str, &[String])>,
) -> Result<(String, ObjectKind), String> {
    let (id, kind) = match action {
        EditAction::UpdateSheet { .. } => {
            return Err("sheet update requires complete projection".into());
        }
        EditAction::SetReference { uuid, .. } => (uuid, ObjectKind::Symbol),
        EditAction::AssignFootprint { uuid, .. } => (uuid, ObjectKind::Symbol),
        EditAction::SetProperty { uuid, .. }
        | EditAction::SetFlags { uuid, .. }
        | EditAction::TransformSymbol { uuid, .. } => (uuid, ObjectKind::Symbol),
        EditAction::UpdateWire { uuid, .. } => (uuid, ObjectKind::Wire),
        EditAction::MoveMarker { uuid, kind, .. } | EditAction::Delete { uuid, kind } => {
            (uuid, *kind)
        }
        EditAction::UpdateLabel { uuid, label } => (uuid, label.kind.object()),
        EditAction::Create { .. }
        | EditAction::CreateSymbol { .. }
        | EditAction::AddSymbolUnit { .. } => {
            return Err("creation requires allocation".into());
        }
    };
    if kind == ObjectKind::Sheet {
        return Err("sheet placement deletion requires its own hierarchy contract".into());
    }
    let node = object(root, id, kind)?;
    let items = values(node)?;
    match action {
        EditAction::UpdateSheet { .. } => unreachable!(),
        EditAction::SetReference {
            reference,
            instance_path,
            ..
        } => {
            validate_reference(reference)?;
            let property = children(items, "property")
                .filter(|p| list_items(p).is_some_and(|v| atom_text(v.get(1)) == Some("Reference")))
                .collect::<Vec<_>>();
            if property.len() != 1 {
                return Err("symbol must have one Reference property".into());
            }
            let (project_name, observed_paths) =
                annotation_scope.ok_or("reference edit requires observed annotation scope")?;
            let instances = one(items, "instances")?;
            let project_nodes = values(instances)?
                .iter()
                .filter_map(list_items)
                .filter(|v| {
                    list_tag(v) == Some("project") && atom_text(v.get(1)) == Some(project_name)
                })
                .collect::<Vec<_>>();
            if project_nodes.len() != 1 {
                return Err("symbol must have one current instances project".into());
            }
            let paths = project_nodes[0]
                .iter()
                .skip(2)
                .filter_map(list_items)
                .filter(|v| {
                    list_tag(v) == Some("path")
                        && atom_text(v.get(1)).is_some_and(|path| {
                            observed_paths
                                .iter()
                                .any(|observed| observed == path.trim_end_matches('/'))
                        })
                })
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return Err("symbol has no instance paths".into());
            }
            if let Some(target) = instance_path {
                let matches = paths
                    .iter()
                    .filter(|path| {
                        atom_text(path.get(1))
                            .is_some_and(|path| path.trim_end_matches('/') == target)
                    })
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err("reference target instance path is missing or ambiguous".into());
                }
                patches.claim(id, &format!("reference:{target}"))?;
                let reference_node = one(matches[0], "reference")?;
                patches.node(reference_node, format!("(reference {})", quote(reference)))?;
            } else {
                patches.claim(id, "reference")?;
                let property = values(property[0])?;
                patches.node(
                    property.get(2).ok_or("missing Reference value")?,
                    quote(reference),
                )?;
                for path in paths {
                    let path_id = atom(path, 1)?;
                    patches.claim(id, &format!("reference:{path_id}"))?;
                    let reference_node = one(path, "reference")?;
                    patches.node(reference_node, format!("(reference {})", quote(reference)))?;
                }
            }
        }
        EditAction::AssignFootprint {
            footprint,
            evidence,
            ..
        } => {
            validate_footprint_identifier(footprint)?;
            let property = children(items, "property")
                .filter(|p| list_items(p).is_some_and(|v| atom_text(v.get(1)) == Some("Footprint")))
                .collect::<Vec<_>>();
            if property.len() > 1 {
                return Err("symbol must have at most one Footprint property".into());
            }
            patches.claim(id, "property:Footprint")?;
            if let Some(property) = property.first() {
                let property = values(property)?;
                patches.node(
                    property.get(2).ok_or("missing Footprint value")?,
                    quote(footprint),
                )?;
            } else {
                let transform = SymbolTransform::from_cst(items)?;
                patches.insert(
                    node_span(node).end - 1,
                    format!(
                        "\n(property \"Footprint\" {} {} (effects (font (size 1.27 1.27)) (hide yes)))\n",
                        quote(footprint),
                        at(transform.origin, Some(QuarterTurn::Zero))
                    ),
                )?;
            }
            if evidence.catalog_digest.is_empty() || evidence.source_digest.is_empty() {
                return Err("footprint assignment evidence is incomplete".into());
            }
        }
        EditAction::SetProperty {
            property, value, ..
        } => {
            text(value, true)?;
            if format == "20250114" && value == "~" {
                return Err("legacy literal tilde property cannot be represented".into());
            }
            let name = match property {
                SymbolProperty::Value => "Value",
                SymbolProperty::Named(name) => {
                    text(name, false)?;
                    if ["reference", "value", "footprint"]
                        .contains(&name.to_ascii_lowercase().as_str())
                    {
                        return Err("reserved property requires its typed operation".into());
                    }
                    name
                }
            };
            patches.claim(id, &format!("property:{name}"))?;
            let fields = children(items, "property")
                .filter(|p| list_items(p).is_some_and(|v| atom_text(v.get(1)) == Some(name)))
                .collect::<Vec<_>>();
            if fields.len() > 1 {
                return Err("ambiguous property".into());
            }
            if let Some(field) = fields.first() {
                let field = values(field)?;
                let value_node = field.get(2).ok_or("missing property value")?;
                atom(field, 2)?;
                patches.node(value_node, quote(value))?;
            } else if matches!(property, SymbolProperty::Value) {
                return Err("missing mandatory value property".into());
            } else {
                let transform = SymbolTransform::from_cst(items)?;
                patches.insert(
                    node_span(node).end - 1,
                    format!(
                        "\n(property {} {} {} (effects (font (size 1.27 1.27)) (hide yes)))\n",
                        quote(name),
                        quote(value),
                        at(transform.origin, Some(QuarterTurn::Zero))
                    ),
                )?;
            }
        }
        EditAction::SetFlags { flags, .. } => {
            if *flags == SymbolFlags::default() {
                return Err("empty flag edit".into());
            }
            let mut insertion = String::new();
            for (tag, flag) in [
                ("in_bom", flags.in_bom),
                ("on_board", flags.on_board),
                ("dnp", flags.dnp),
            ] {
                if let Some(flag) = flag {
                    patches.claim(id, tag)?;
                    let rendered = format!("({tag} {})", if flag { "yes" } else { "no" });
                    if let Some(field) = optional(items, tag)? {
                        let old = values(field)?;
                        if old.len() != 2 || !matches!(atom(old, 1)?, "yes" | "no") {
                            return Err("invalid source flag".into());
                        }
                        patches.node(field, rendered)?;
                    } else {
                        insertion.push_str(&rendered);
                    }
                }
            }
            if !insertion.is_empty() {
                patches.insert(node_span(node).end - 1, insertion)?;
            }
        }
        EditAction::TransformSymbol { transform, .. } => {
            let old = SymbolTransform::from_cst(items)?;
            transform_fields(items, old, *transform, patches)?;
            let at_node = one(items, "at")?;
            let mirror = optional(items, "mirror")?;
            let rendered = match transform.mirror {
                Mirror::None => None,
                Mirror::X => Some("(mirror x)"),
                Mirror::Y => Some("(mirror y)"),
            };
            let mut position = at(transform.origin, Some(transform.rotation));
            if let Some(mirror) = mirror {
                patches.node(mirror, rendered.unwrap_or("").into())?;
            } else if let Some(rendered) = rendered {
                position.push(' ');
                position.push_str(rendered);
            }
            patches.node(at_node, position)?;
        }
        EditAction::UpdateWire { start, end, .. } => {
            let pts = one(items, "pts")?;
            let values = values(pts)?;
            if values.len() != 3
                || values[1..].iter().any(|n| {
                    list_items(n).is_none_or(|v| list_tag(v) != Some("xy") || v.len() != 3)
                })
            {
                return Err("wire must contain exactly two endpoints".into());
            }
            patches.node(pts, points(*start, *end)?)?;
        }
        EditAction::MoveMarker { kind, position, .. } => {
            if !matches!(kind, ObjectKind::Junction | ObjectKind::NoConnect) {
                return Err("marker operation requires marker kind".into());
            }
            let node = one(items, "at")?;
            if values(node)?.len() != 3 {
                return Err("invalid marker position".into());
            }
            patches.node(node, at(*position, None))?;
        }
        EditAction::UpdateLabel { label, .. } => {
            let old = SymbolTransform::from_cst(items)?;
            let new = SymbolTransform {
                origin: label.position,
                rotation: label.rotation,
                mirror: Mirror::None,
            };
            if optional(items, "iref")?.is_some() {
                return Err("legacy intersheet field position requires explicit handling".into());
            }
            transform_fields(items, old, new, patches)?;
            text(&label.text, false)?;
            atom(items, 1)?;
            patches.node(&items[1], quote(&label.text))?;
            let node = one(items, "at")?;
            if values(node)?.len() != 4 {
                return Err("invalid label position".into());
            }
            patches.node(node, at(label.position, Some(label.rotation)))?;
            if let Some(shape) = label.kind.shape() {
                patches.node(one(items, "shape")?, format!("(shape {})", shape.atom()))?;
            } else if optional(items, "shape")?.is_some() {
                return Err("local label cannot have shape".into());
            }
        }
        EditAction::Delete { .. } => patches.node(node, String::new())?,
        EditAction::Create { .. }
        | EditAction::CreateSymbol { .. }
        | EditAction::AddSymbolUnit { .. } => unreachable!(),
    }
    Ok((id.clone(), kind))
}

fn validate_reference(reference: &str) -> Result<(), String> {
    if reference.is_empty()
        || reference.len() > 128
        || !reference.is_ascii()
        || reference
            .chars()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return Err("invalid schematic reference".into());
    }
    if reference.starts_with('#') {
        if reference.len() == 1 {
            return Err("empty private reference".into());
        }
    } else if !reference.chars().any(|c| c.is_ascii_digit())
        || !reference
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '?')
    {
        return Err("reference must contain an alphanumeric annotation or # prefix".into());
    }
    Ok(())
}

fn validate_footprint_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 512
        || !value.is_ascii()
        || value
            .chars()
            .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
        || value.matches(':').count() != 1
    {
        return Err("invalid schematic footprint identifier".into());
    }
    Ok(())
}

fn validate_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Allocate deterministic references against the complete expanded hierarchy.
pub fn allocate_references(
    project: &ParsedSchematicProject,
    native: &NetlistDocument,
    requests: &[ReferenceRequest],
) -> Result<Vec<SchematicEdit>, String> {
    components::allocate(project, native, requests)
}

pub fn compile_edits(
    project: &ParsedSchematicProject,
    native: &NetlistDocument,
    sources: &BTreeMap<String, &[u8]>,
    edits: &[SchematicEdit],
) -> Result<SchematicCandidate, String> {
    if edits.is_empty() || edits.len() > 128 {
        return Err("schematic edits require 1..128 operations".into());
    }
    if project.editor_lock_present || project.revision.editor_lock_present {
        return Err("schematic editor lock is present".into());
    }
    let identities = SchematicIdentityIndex::resolve(project, native).map_err(|e| e.to_string())?;
    if sources.len() != project.files.len() {
        return Err("source file set differs from observation".into());
    }
    let changed = edits
        .iter()
        .map(|e| e.file.as_str())
        .collect::<BTreeSet<_>>();
    if changed.len() > 32 {
        return Err("changed schematic file bound exceeded".into());
    }
    if changed.iter().any(|f| !sources.contains_key(*f)) {
        return Err("edit targets unobserved file".into());
    }
    let mut seen_uuids = BTreeSet::new();
    let mut documents = BTreeMap::new();
    let mut total = 0;
    for file in &project.files {
        let name = &file.artifact.relative_path;
        let bytes = *sources.get(name).ok_or("missing observed source")?;
        total += bytes.len();
        if bytes.len() > 32 * 1024 * 1024 || total > 128 * 1024 * 1024 {
            return Err("source byte bound exceeded".into());
        }
        if parse_schematic(name, bytes).map_err(|e| e.to_string())? != *file {
            return Err("source bytes differ from observation".into());
        }
        if !matches!(file.format_version.as_str(), "20250114" | "20260306") {
            return Err("unsupported source format".into());
        }
        let document = parse_one(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut file_ids = BTreeSet::new();
        let mut stack = document.nodes.iter().collect::<Vec<_>>();
        while let Some(node) = stack.pop() {
            if let Some(items) = list_items(node) {
                if list_tag(items) == Some("uuid") {
                    let id = atom(items, 1)?;
                    if items.len() != 2 || !uuid(id) || !file_ids.insert(id.to_owned()) {
                        return Err("invalid or repeated source UUID".into());
                    }
                    seen_uuids.insert(id.to_owned());
                }
                stack.extend(items);
            }
        }
        documents.insert(name.clone(), document);
    }
    components::validate(project, &identities, sources, edits)?;
    let mut allocations = BTreeMap::new();
    let mut symbol_allocations = BTreeMap::new();
    let mut keys = BTreeSet::new();
    creation::validate_groups(project, &identities, edits)?;
    for (ordinal, edit) in edits.iter().enumerate() {
        let key = match &edit.action {
            EditAction::Create { key, .. }
            | EditAction::CreateSymbol { key, .. }
            | EditAction::AddSymbolUnit { key, .. } => key,
            _ => continue,
        };
        text(key, false)?;
        if !keys.insert(key) {
            return Err("duplicate creation key".into());
        }
        match &edit.action {
            EditAction::Create { .. } => {
                let id = generated_uuid(project.revision.digest.as_str(), ordinal);
                if !seen_uuids.insert(id.clone()) {
                    return Err("allocated UUID collision".into());
                }
                allocations.insert(ordinal, id);
            }
            EditAction::CreateSymbol { symbol, .. } => {
                if !matches!(edit.scope, EditScope::FileAllInstances) {
                    return Err("symbol creation requires file_all_instances scope".into());
                }
                validate_symbol_creation(symbol)?;
                let file = project
                    .files
                    .iter()
                    .find(|file| file.artifact.relative_path == edit.file)
                    .ok_or("symbol creation target file is not observed")?;
                let expected_paths = identities
                    .sheets()
                    .iter()
                    .filter(|instance| instance.file == edit.file)
                    .map(|instance| instance.path.clone())
                    .collect::<BTreeSet<_>>();
                let supplied_paths = symbol
                    .instance_paths
                    .iter()
                    .map(|instance| instance.path.clone())
                    .collect::<BTreeSet<_>>();
                if expected_paths.is_empty() || expected_paths != supplied_paths {
                    return Err("symbol instance paths do not cover the physical file".into());
                }
                let (name, definition, pin_numbers) = parse_symbol_definition(
                    &symbol.evidence.definition,
                    &symbol.evidence.lib_id,
                    symbol.unit,
                    symbol.body_style,
                )?;
                let _ = name;
                let symbol_uuid = generated_uuid_seed(&format!(
                    "schematic-edit-v1:{}:{}:symbol",
                    project.revision.digest, ordinal
                ));
                if !seen_uuids.insert(symbol_uuid.clone()) {
                    return Err("allocated symbol UUID collision".into());
                }
                let mut pin_uuids = Vec::with_capacity(pin_numbers.len());
                for (index, _) in pin_numbers.iter().enumerate() {
                    let pin_uuid = generated_uuid_seed(&format!(
                        "schematic-edit-v1:{}:{}:pin:{index}",
                        project.revision.digest, ordinal
                    ));
                    if !seen_uuids.insert(pin_uuid.clone()) {
                        return Err("allocated pin UUID collision".into());
                    }
                    pin_uuids.push(pin_uuid);
                }
                let _ = file;
                symbol_allocations.insert(
                    ordinal,
                    SymbolAllocation::Catalog(CreationAllocation {
                        symbol_uuid,
                        pin_uuids,
                        definition,
                        pin_numbers,
                    }),
                );
            }
            EditAction::AddSymbolUnit {
                uuid,
                unit,
                transform,
                ..
            } => {
                let file = project
                    .files
                    .iter()
                    .find(|file| file.artifact.relative_path == edit.file)
                    .ok_or("unit target file is not observed")?;
                let bytes = sources[&edit.file];
                let document = &documents[&edit.file];
                let symbol_uuid = generated_uuid_seed(&format!(
                    "schematic-edit-v1:{}:{}:symbol",
                    project.revision.digest, ordinal
                ));
                if !seen_uuids.insert(symbol_uuid.clone()) {
                    return Err("allocated symbol UUID collision".into());
                }
                let inserted = creation::insert_unit(
                    creation::Insertion {
                        project,
                        identities: &identities,
                        file,
                        document,
                        bytes,
                        anchor_uuid: uuid,
                        unit: *unit,
                        transform: *transform,
                        symbol_uuid: &symbol_uuid,
                        ordinal,
                    },
                    &mut seen_uuids,
                )?;
                symbol_allocations.insert(
                    ordinal,
                    SymbolAllocation::Unit {
                        symbol_uuid,
                        rendered: inserted,
                    },
                );
            }
            _ => {}
        }
    }
    let sheet_allocations = sheets::allocate(
        project.revision.digest.as_str(),
        edits,
        &mut seen_uuids,
        &mut keys,
    )?;
    let mut files = BTreeMap::new();
    let mut parsed = Vec::new();
    let mut changes = Vec::new();
    let mut replacements = BTreeMap::new();
    let mut total = 0;
    for file in &project.files {
        let name = &file.artifact.relative_path;
        let original = std::str::from_utf8(sources[name]).map_err(|e| e.to_string())?;
        let document = &documents[name];
        let root_node = document.nodes.first().ok_or("missing root")?;
        let root = values(root_node)?;
        if edits
            .iter()
            .any(|edit| edit.file == *name && matches!(edit.action, EditAction::UpdateSheet { .. }))
        {
            sheets::validate_names(root, name, edits)?;
        }
        let instances = identities
            .sheets()
            .iter()
            .filter(|i| i.file == *name)
            .map(|i| i.path.clone())
            .collect::<Vec<_>>();
        let mut patches = Patches {
            text: original,
            replacements: Vec::new(),
            claims: BTreeSet::new(),
        };
        let mut appended = String::new();
        let mut embedded = String::new();
        let lib_symbols = one(root, "lib_symbols")?;
        let mut embedded_definitions = BTreeMap::new();
        for node in children(values(lib_symbols)?, "symbol") {
            let items = values(node)?;
            let id =
                atom_text(items.get(1)).ok_or("embedded symbol definition has no identifier")?;
            let raw = original
                .get(node_span(node).start..node_span(node).end)
                .ok_or("invalid embedded symbol definition span")?
                .trim()
                .to_owned();
            embedded_definitions.insert(id.to_owned(), raw);
        }
        for (ordinal, edit) in edits.iter().enumerate().filter(|(_, e)| e.file == *name) {
            let allows_instance_reference = matches!(
                (&edit.scope, &edit.action),
                (
                    EditScope::SingleInstance(scope_path),
                    EditAction::SetReference {
                        instance_path: Some(target),
                        ..
                    }
                ) if scope_path == target
            );
            if let EditScope::SingleInstance(path) = &edit.scope
                && !allows_instance_reference
                && (instances.len() != 1 || &instances[0] != path)
            {
                return Err(format!(
                    "physical file edit requires file_all_instances; affected instances: {instances:?}"
                ));
            }
            let (id, kind) = match &edit.action {
                EditAction::UpdateSheet { uuid, update } => sheets::edit(
                    root,
                    uuid,
                    update,
                    ordinal,
                    &sheet_allocations,
                    &mut patches,
                )?,
                EditAction::Create { object, .. } => {
                    let id = &allocations[&ordinal];
                    appended.push_str(&render(object, id)?);
                    (id.clone(), object.kind())
                }
                EditAction::CreateSymbol { symbol, .. } => {
                    let SymbolAllocation::Catalog(allocation) = &symbol_allocations[&ordinal]
                    else {
                        unreachable!("catalog allocation")
                    };
                    match embedded_definitions.get(symbol.evidence.lib_id.as_str()) {
                        Some(existing) if existing != &allocation.definition => {
                            return Err(
                                "embedded symbol definition differs from catalog evidence".into()
                            );
                        }
                        Some(_) => {}
                        None => {
                            embedded_definitions.insert(
                                symbol.evidence.lib_id.clone(),
                                allocation.definition.clone(),
                            );
                            if !embedded.is_empty() {
                                embedded.push('\n');
                            }
                            embedded.push_str(&allocation.definition);
                        }
                    }
                    let project_name = Path::new(&project.root_relative_path)
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .ok_or("invalid schematic project name")?;
                    appended.push_str(&render_created_symbol(
                        symbol,
                        allocation,
                        &file.format_version,
                        project_name,
                    )?);
                    (allocation.symbol_uuid.clone(), ObjectKind::Symbol)
                }
                EditAction::AddSymbolUnit { .. } => {
                    let SymbolAllocation::Unit {
                        symbol_uuid,
                        rendered,
                    } = &symbol_allocations[&ordinal]
                    else {
                        unreachable!("unit allocation")
                    };
                    appended.push_str(rendered);
                    (symbol_uuid.clone(), ObjectKind::Symbol)
                }
                _ => edit_existing(
                    root,
                    &edit.action,
                    &mut patches,
                    &file.format_version,
                    Some((
                        Path::new(&project.root_relative_path)
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .ok_or("invalid project basename")?,
                        &instances,
                    )),
                )?,
            };
            changes.push(ObjectChange {
                file: name.clone(),
                uuid: id,
                kind,
                instance_paths: match &edit.action {
                    EditAction::SetReference {
                        instance_path: Some(path),
                        ..
                    } => vec![path.clone()],
                    _ => instances.clone(),
                },
                action: edit.action.clone(),
            });
        }
        if !embedded.is_empty() {
            patches.insert(node_span(lib_symbols).end - 1, format!("\n{}\n", embedded))?;
        }
        if !appended.is_empty() {
            patches.insert(node_span(root_node).end - 1, appended)?;
        }
        let (bytes, spans) = patches.finish()?;
        total += bytes.len();
        if total > 128 * 1024 * 1024 {
            return Err("candidate total byte bound exceeded".into());
        }
        parsed.push(parse_schematic(name, &bytes).map_err(|e| e.to_string())?);
        if !spans.is_empty() {
            replacements.insert(name.clone(), spans);
        }
        files.insert(name.clone(), bytes);
    }
    if replacements.is_empty() {
        return Err("schematic edits have no effect".into());
    }
    let revision = SchematicArtifactRevision::new(
        parsed.iter().map(|f| f.artifact.clone()).collect(),
        &project.revision.kicad_cli_version,
        project.revision.extractor_schema_version,
        false,
    )
    .map_err(|e| e.to_string())?;
    Ok(SchematicCandidate {
        source_revision: project.revision.clone(),
        parsed: ParsedSchematicProject {
            root_relative_path: project.root_relative_path.clone(),
            files: parsed,
            revision,
            editor_lock_present: false,
        },
        files,
        changes,
        replacements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetlistComponent, NetlistSheet, NetlistUnit};
    const ROOT: &str = "11111111-1111-4111-8111-111111111111";
    const SYMBOL: &str = "22222222-2222-4222-8222-222222222222";
    fn fixture() -> (ParsedSchematicProject, NetlistDocument, Vec<u8>) {
        let text = format!(
            r#"(kicad_sch (version 20260306) (generator eeschema) (uuid "{ROOT}")
          (lib_symbols (symbol "Test:R" (symbol "R_1_1" (pin passive line (at 0 0 0) (name "") (number "1")))))
          (paper "A4")
          (symbol (lib_id "Test:R") (at 10 20 0) (unit 1) (in_bom yes) (on_board yes) (dnp no) (uuid "{SYMBOL}")
            (property "Reference" "R1" (at 11 21 0) (effects (font (size 1 1))))
            (property "Value" "10k" (at 12 22 90) (effects (font (size 1 1)) (hide yes)))
            (property "Custom" "keep" (at 9 19 0) (effects (font (size 1 1)) (justify left)))
            (pin "1" (uuid "33333333-3333-4333-8333-333333333333"))
            (instances (project "test" (path "/{ROOT}" (reference "R1") (unit 1)))))
          (text "unrelated 中文" (at 40 40 0) (uuid "44444444-4444-4444-8444-444444444444")))"#
        );
        let parsed = parse_schematic("test.kicad_sch", text.as_bytes()).unwrap();
        let project = ParsedSchematicProject {
            root_relative_path: "test.kicad_sch".into(),
            revision: SchematicArtifactRevision::new(
                vec![parsed.artifact.clone()],
                "10.0.7",
                1,
                false,
            )
            .unwrap(),
            files: vec![parsed],
            editor_lock_present: false,
        };
        let native = NetlistDocument {
            export_version: "E".into(),
            sheets: vec![NetlistSheet {
                number: "1".into(),
                name: "/".into(),
                uuid_path: "/".into(),
            }],
            components: vec![NetlistComponent {
                reference: "R1".into(),
                value: "10k".into(),
                footprint: None,
                symbol_uuid: Some(SYMBOL.into()),
                sheet_path: "/".into(),
                sheet_uuid_path: "/".into(),
                library: "Test".into(),
                library_part: "R".into(),
                units: Some(vec![NetlistUnit {
                    name: "A".into(),
                    pins: vec!["1".into()],
                }]),
            }],
            nets: vec![],
        };
        (project, native, text.into_bytes())
    }
    fn edit(action: EditAction) -> SchematicEdit {
        SchematicEdit {
            file: "test.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action,
        }
    }
    fn compile(
        p: &ParsedSchematicProject,
        n: &NetlistDocument,
        b: &[u8],
        edits: &[SchematicEdit],
    ) -> Result<SchematicCandidate, String> {
        compile_edits(p, n, &BTreeMap::from([("test.kicad_sch".into(), b)]), edits)
    }
    fn property(value: &str) -> EditAction {
        EditAction::SetProperty {
            uuid: SYMBOL.into(),
            property: SymbolProperty::Value,
            value: value.into(),
        }
    }
    fn footprint() -> EditAction {
        EditAction::AssignFootprint {
            uuid: SYMBOL.into(),
            footprint: "Resistor_SMD:R_0603_1608Metric".into(),
            evidence: FootprintCompatibilityEvidence {
                catalog_digest: "a".repeat(64),
                source_digest: "b".repeat(64),
                pad_numbers: vec!["1".into()],
                pin_pad_map: BTreeMap::from([("1".into(), "1".into())]),
            },
        }
    }
    fn point(x: i64, y: i64) -> Point {
        Point::from_nm(x * 1_000_000, y * 1_000_000).unwrap()
    }

    #[test]
    fn creates_catalog_bound_symbol_definition_and_instances() {
        let (project, native, source) = fixture();
        let symbol = SymbolCreation {
            evidence: SymbolDefinitionEvidence {
                lib_id: "Test:New".into(),
                catalog_digest: "a".repeat(64),
                source_digest: "b".repeat(64),
                definition: r#"(symbol "New"
                    (property "Reference" "R")
                    (property "Value" "New")
                    (symbol "New_0_1")
                    (symbol "New_1_1"
                        (pin passive line (at 0 0 0) (name "") (number "1")))
                )"#
                .into(),
            },
            transform: SymbolTransform {
                origin: point(50, 60),
                rotation: QuarterTurn::Zero,
                mirror: Mirror::None,
            },
            unit: 1,
            body_style: None,
            reference: "R2".into(),
            value: "1k".into(),
            footprint: None,
            flags: SymbolFlags::default(),
            instance_paths: vec![SymbolInstancePath {
                path: format!("/{ROOT}"),
                reference: None,
            }],
        };
        let candidate = compile(
            &project,
            &native,
            &source,
            &[edit(EditAction::CreateSymbol {
                key: "new-resistor".into(),
                symbol,
            })],
        )
        .unwrap();
        let bytes = &candidate.files()["test.kicad_sch"];
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.contains("(symbol \"Test:New\""));
        assert!(text.contains("(lib_id \"Test:New\")"));
        assert!(text.contains("(reference \"R2\")"));
        assert_eq!(candidate.parsed().files[0].symbols.len(), 2);
        assert_eq!(candidate.changes()[0].kind, ObjectKind::Symbol);
        assert!(candidate.replacements()["test.kicad_sch"].len() >= 2);
    }

    fn multiunit_creation(unit: u32) -> SymbolCreation {
        SymbolCreation {
            evidence: SymbolDefinitionEvidence {
                lib_id: "Test:Pair".into(),
                catalog_digest: "a".repeat(64),
                source_digest: "b".repeat(64),
                definition: r#"(symbol "Pair"
                    (property "Reference" "U") (property "Value" "Pair")
                    (symbol "Pair_0_1" (pin passive line (at 0 0 0) (name "COM") (number "3")))
                    (symbol "Pair_1_1" (pin passive line (at 1 0 0) (name "A") (number "1")))
                    (symbol "Pair_2_1" (pin passive line (at -1 0 0) (name "B") (number "2"))))"#
                    .into(),
            },
            transform: SymbolTransform {
                origin: point(50 + 10 * i64::from(unit), 60),
                rotation: QuarterTurn::Zero,
                mirror: Mirror::None,
            },
            unit,
            body_style: None,
            reference: "U2".into(),
            value: "Pair".into(),
            footprint: None,
            flags: SymbolFlags::default(),
            instance_paths: vec![SymbolInstancePath {
                path: format!("/{ROOT}"),
                reference: None,
            }],
        }
    }

    #[test]
    fn complete_components_reserve_compatible_unique_unit_slots() {
        let (project, native, bytes) = fixture();
        let action = |symbol: SymbolCreation| {
            edit(EditAction::CreateSymbol {
                key: format!("unit-{}", symbol.unit),
                symbol,
            })
        };
        let first = multiunit_creation(1);
        let second = multiunit_creation(2);
        let candidate = compile(
            &project,
            &native,
            &bytes,
            &[action(first.clone()), action(second.clone())],
        )
        .unwrap();
        assert_preserved(&bytes, &candidate);
        assert_eq!(
            candidate.parsed().files[0].symbols[0],
            project.files[0].symbols[0]
        );
        let members = &candidate.parsed().files[0].symbols[1..];
        assert_eq!(
            members.iter().map(|s| s.unit).collect::<BTreeSet<_>>(),
            BTreeSet::from([1, 2])
        );
        assert_ne!(members[0].source_uuid, members[1].source_uuid);
        assert_eq!(members[0].pins.len(), 2);
        assert_eq!(members[1].pins.len(), 2);
        let ids = members
            .iter()
            .flat_map(|s| s.pins.iter().map(|p| &p.source_uuid))
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 4);
        let mut conflicts = vec![first.clone()];
        let mut changed = second.clone();
        changed.value = "different".into();
        conflicts.push(changed);
        let mut changed = second.clone();
        changed.flags.dnp = Some(true);
        conflicts.push(changed);
        let mut changed = second.clone();
        changed.evidence.source_digest = "c".repeat(64);
        conflicts.push(changed);
        let mut changed = second.clone();
        changed.evidence.definition.push(' ');
        conflicts.push(changed);
        let mut changed = second.clone();
        changed.unit = 3;
        conflicts.push(changed);
        for conflict in conflicts {
            let mut conflicting_action = action(conflict);
            if let EditAction::CreateSymbol { key, .. } = &mut conflicting_action.action {
                *key = "conflict".into();
            }
            assert!(
                compile(
                    &project,
                    &native,
                    &bytes,
                    &[action(first.clone()), conflicting_action]
                )
                .is_err()
            );
        }
        assert!(
            compile(
                &project,
                &native,
                &bytes,
                &[
                    action(first),
                    edit(EditAction::SetReference {
                        uuid: SYMBOL.into(),
                        reference: "U2".into(),
                        instance_path: None,
                    })
                ]
            )
            .is_err()
        );
    }

    #[test]
    fn inserted_units_preserve_source_metadata_and_component_authority() {
        let (_, mut native, bytes) = fixture();
        let text = String::from_utf8(bytes).unwrap().replace(
            "(symbol \"R_1_1\" (pin passive line (at 0 0 0) (name \"\") (number \"1\")))",
            "(symbol \"R_1_1\" (pin passive line (at 0 0 0) (name \"\") (number \"1\"))) (symbol \"R_2_1\" (pin passive line (at 1 0 0) (name \"\") (number \"2\")))",
        ).replace("(dnp no)", "(dnp yes)").replace(
            "(property \"Custom\"",
            "(property \"Footprint\" \"Test:Assigned\" (at 10 20 0) (effects (font (size 1 1)))) (property \"Custom\"",
        );
        let bytes = text.into_bytes();
        let parsed = parse_schematic("test.kicad_sch", &bytes).unwrap();
        let project = ParsedSchematicProject {
            root_relative_path: "test.kicad_sch".into(),
            revision: SchematicArtifactRevision::new(
                vec![parsed.artifact.clone()],
                "10.0.7",
                1,
                false,
            )
            .unwrap(),
            files: vec![parsed],
            editor_lock_present: false,
        };
        native.components[0].footprint = Some("Test:Assigned".into());
        let insertion = edit(EditAction::AddSymbolUnit {
            key: "missing-unit".into(),
            uuid: SYMBOL.into(),
            unit: 2,
            transform: SymbolTransform {
                origin: point(50, 60),
                rotation: QuarterTurn::Zero,
                mirror: Mirror::None,
            },
        });
        let candidate =
            compile(&project, &native, &bytes, std::slice::from_ref(&insertion)).unwrap();
        assert_preserved(&bytes, &candidate);
        assert_eq!(
            candidate.parsed().files[0].symbols[0],
            project.files[0].symbols[0]
        );
        let added = &candidate.parsed().files[0].symbols[1];
        assert_eq!(added.unit, 2);
        assert_eq!(added.footprint.as_deref(), Some("Test:Assigned"));
        assert_eq!(added.dnp, Some(true));
        assert_eq!(added.pins.len(), 1);
        assert_eq!(added.pins[0].number, "2");
        assert_ne!(
            added.pins[0].source_uuid,
            project.files[0].symbols[0].pins[0].source_uuid
        );
        let doc =
            parse_one(std::str::from_utf8(&candidate.files()["test.kicad_sch"]).unwrap()).unwrap();
        let inserted = values(
            object(
                values(&doc.nodes[0]).unwrap(),
                &added.source_uuid,
                ObjectKind::Symbol,
            )
            .unwrap(),
        )
        .unwrap();
        let custom = children(inserted, "property")
            .find(|p| atom(values(p).unwrap(), 1).unwrap() == "Custom")
            .unwrap();
        assert_eq!(atom(values(custom).unwrap(), 2).unwrap(), "keep");
        assert_eq!(
            Point::from_at(values(one(values(custom).unwrap(), "at").unwrap()).unwrap()).unwrap(),
            point(49, 59)
        );
        assert_eq!(
            added.instance_paths,
            project.files[0].symbols[0]
                .instance_paths
                .iter()
                .cloned()
                .map(|mut i| {
                    i.unit = Some(2);
                    i
                })
                .collect::<Vec<_>>()
        );
        assert!(
            compile(
                &project,
                &native,
                &bytes,
                &[insertion.clone(), edit(property("changed"))]
            )
            .is_err()
        );
        let mut duplicate = insertion.clone();
        if let EditAction::AddSymbolUnit { key, .. } = &mut duplicate.action {
            *key = "duplicate".into();
        }
        assert!(compile(&project, &native, &bytes, &[insertion.clone(), duplicate]).is_err());
        let mut occupied = insertion;
        if let EditAction::AddSymbolUnit { unit, .. } = &mut occupied.action {
            *unit = 1;
        }
        assert!(compile(&project, &native, &bytes, &[occupied]).is_err());
    }

    fn component_fixture() -> (
        ParsedSchematicProject,
        NetlistDocument,
        Vec<u8>,
        Vec<String>,
    ) {
        let (project, mut native, bytes) = fixture();
        let edits = (1..=2)
            .map(|unit| {
                edit(EditAction::CreateSymbol {
                    key: format!("member-{unit}"),
                    symbol: multiunit_creation(unit),
                })
            })
            .collect::<Vec<_>>();
        let created = compile(&project, &native, &bytes, &edits).unwrap();
        let ids = created.parsed().files[0]
            .symbols
            .iter()
            .filter(|s| s.reference == "U2")
            .map(|s| s.source_uuid.clone())
            .collect::<Vec<_>>();
        native.components.push(NetlistComponent {
            reference: "U2".into(),
            value: "Pair".into(),
            footprint: None,
            symbol_uuid: Some(ids.join(" ")),
            sheet_path: "/".into(),
            sheet_uuid_path: "/".into(),
            library: "Test".into(),
            library_part: "Pair".into(),
            units: Some(vec![
                NetlistUnit {
                    name: "A".into(),
                    pins: vec!["1".into(), "3".into()],
                },
                NetlistUnit {
                    name: "B".into(),
                    pins: vec!["2".into(), "3".into()],
                },
            ]),
        });
        (
            created.parsed().clone(),
            native,
            created.files()["test.kicad_sch"].clone(),
            ids,
        )
    }
    fn reparsed(project: &ParsedSchematicProject, bytes: &[u8]) -> ParsedSchematicProject {
        let mut project = project.clone();
        project.files[0] = parse_schematic("test.kicad_sch", bytes).unwrap();
        project.revision = SchematicArtifactRevision::new(
            vec![project.files[0].artifact.clone()],
            "10.0.7",
            1,
            false,
        )
        .unwrap();
        project
    }
    fn component_assignment(uuid: &str, numbers: &[&str]) -> SchematicEdit {
        edit(EditAction::AssignFootprint {
            uuid: uuid.into(),
            footprint: "Test:Assigned".into(),
            evidence: FootprintCompatibilityEvidence {
                catalog_digest: "a".repeat(64),
                source_digest: "b".repeat(64),
                pad_numbers: numbers.iter().map(|n| (*n).into()).collect(),
                pin_pad_map: numbers.iter().map(|n| ((*n).into(), (*n).into())).collect(),
            },
        })
    }

    #[test]
    fn component_references_preserve_membership_and_reject_partial_or_merged_groups() {
        let (project, native, bytes, ids) = component_fixture();
        let rename = |id: &str, reference: &str| {
            edit(EditAction::SetReference {
                uuid: id.into(),
                reference: reference.into(),
                instance_path: None,
            })
        };
        let edits = ids.iter().map(|id| rename(id, "U20")).collect::<Vec<_>>();
        let candidate = compile(&project, &native, &bytes, &edits).unwrap();
        assert_preserved(&bytes, &candidate);
        assert_eq!(
            candidate.parsed().files[0]
                .symbols
                .iter()
                .filter(|s| s.reference == "U20")
                .map(|s| &s.source_uuid)
                .collect::<BTreeSet<_>>(),
            ids.iter().collect()
        );
        assert!(compile(&project, &native, &bytes, &edits[..1]).is_err());
        assert!(
            compile(
                &project,
                &native,
                &bytes,
                &[rename(&ids[0], "U20"), rename(&ids[1], "U21")]
            )
            .is_err()
        );
        assert!(
            compile(
                &project,
                &native,
                &bytes,
                &ids.iter().map(|id| rename(id, "R1")).collect::<Vec<_>>()
            )
            .is_err()
        );
        let swapped = compile(
            &project,
            &native,
            &bytes,
            &[
                rename(SYMBOL, "U2"),
                rename(&ids[0], "R1"),
                rename(&ids[1], "R1"),
            ],
        )
        .unwrap();
        assert_preserved(&bytes, &swapped);
        assert_eq!(
            swapped.parsed().files[0]
                .symbols
                .iter()
                .filter(|s| s.reference == "R1")
                .count(),
            2
        );
    }

    #[test]
    fn component_metadata_and_assignment_require_consistent_complete_members() {
        let (project, native, bytes, ids) = component_fixture();
        let assignments = ids
            .iter()
            .map(|id| component_assignment(id, &["1", "2", "3"]))
            .collect::<Vec<_>>();
        let candidate = compile(&project, &native, &bytes, &assignments).unwrap();
        assert_preserved(&bytes, &candidate);
        assert!(
            candidate.parsed().files[0]
                .symbols
                .iter()
                .filter(|s| ids.contains(&s.source_uuid))
                .all(|s| s.footprint.as_deref() == Some("Test:Assigned"))
        );
        assert!(compile(&project, &native, &bytes, &assignments[..1]).is_err());
        let mut remapped = assignments.clone();
        for change in &mut remapped {
            if let EditAction::AssignFootprint { evidence, .. } = &mut change.action {
                evidence.pin_pad_map.insert("1".into(), "2".into());
                evidence.pin_pad_map.insert("2".into(), "1".into());
            }
        }
        assert!(compile(&project, &native, &bytes, &remapped).is_err());
        let values = ids
            .iter()
            .map(|id| {
                edit(EditAction::SetProperty {
                    uuid: id.clone(),
                    property: SymbolProperty::Value,
                    value: "changed".into(),
                })
            })
            .collect::<Vec<_>>();
        assert!(compile(&project, &native, &bytes, &values[..1]).is_err());
        assert!(compile(&project, &native, &bytes, &values).is_ok());
        let flags = ids
            .iter()
            .map(|id| {
                edit(EditAction::SetFlags {
                    uuid: id.clone(),
                    flags: SymbolFlags {
                        dnp: Some(true),
                        ..Default::default()
                    },
                })
            })
            .collect::<Vec<_>>();
        assert!(compile(&project, &native, &bytes, &flags[..1]).is_err());
        assert!(compile(&project, &native, &bytes, &flags).is_ok());
        let mut removed = assignments;
        removed.push(edit(EditAction::Delete {
            uuid: ids[0].clone(),
            kind: ObjectKind::Symbol,
        }));
        assert!(compile(&project, &native, &bytes, &removed).is_err());
    }

    #[test]
    fn component_assignment_uses_active_expanded_pins_and_ignores_inactive_records() {
        let (project, mut native, bytes, ids) = component_fixture();
        let unit_one_id = &project.files[0]
            .symbols
            .iter()
            .find(|symbol| symbol.reference == "U2" && symbol.unit == 1)
            .unwrap()
            .source_uuid;
        let text = std::str::from_utf8(&bytes).unwrap();
        let doc = parse_one(text).unwrap();
        let root = values(&doc.nodes[0]).unwrap();
        let cache = values(one(root, "lib_symbols").unwrap()).unwrap();
        let definition = children(cache, "symbol")
            .find(|node| atom(values(node).unwrap(), 1).unwrap() == "Test:Pair")
            .unwrap();
        let body = children(values(definition).unwrap(), "symbol")
            .find(|node| atom(values(node).unwrap(), 1).unwrap() == "Pair_1_1")
            .unwrap();
        let pin = children(values(body).unwrap(), "pin").next().unwrap();
        let unit = values(object(root, unit_one_id, ObjectKind::Symbol).unwrap()).unwrap();
        let record = children(unit, "pin")
            .find(|p| atom(values(p).unwrap(), 1).unwrap() == "1")
            .unwrap();
        let mut patches = Patches {
            text,
            replacements: Vec::new(),
            claims: BTreeSet::new(),
        };
        patches
            .node(
                one(values(pin).unwrap(), "number").unwrap(),
                "(number \"[1,4]\")".into(),
            )
            .unwrap();
        patches
            .node(&values(record).unwrap()[1], "\"[1,4]\"".into())
            .unwrap();
        patches.insert(node_span(definition).end - 1, "(symbol \"Pair_1_2\" (pin passive line (at 0 0 0) (name \"inactive\") (number \"9\")))".into()).unwrap();
        patches
            .insert(
                node_span(object(root, unit_one_id, ObjectKind::Symbol).unwrap()).end - 1,
                "(pin \"9\" (uuid \"99999999-9999-4999-8999-999999999999\"))".into(),
            )
            .unwrap();
        let (bytes, _) = patches.finish().unwrap();
        let project = reparsed(&project, &bytes);
        native.components[1].units.as_mut().unwrap()[0]
            .pins
            .push("4".into());
        let edits = ids
            .iter()
            .map(|id| component_assignment(id, &["1", "2", "3", "4"]))
            .collect::<Vec<_>>();
        let candidate = compile(&project, &native, &bytes, &edits).unwrap();
        assert_preserved(&bytes, &candidate);
        assert!(
            compile(
                &project,
                &native,
                &bytes,
                &ids.iter()
                    .map(|id| component_assignment(id, &["1", "2", "3", "4", "9"]))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
    }

    #[test]
    fn reference_changes_preserve_foreign_and_inactive_annotations() {
        let (project, native, bytes, ids) = component_fixture();
        let text = std::str::from_utf8(&bytes).unwrap();
        let doc = parse_one(text).unwrap();
        let root = values(&doc.nodes[0]).unwrap();
        let mut patches = Patches {
            text,
            replacements: Vec::new(),
            claims: BTreeSet::new(),
        };
        for id in &ids {
            let unit = values(object(root, id, ObjectKind::Symbol).unwrap()).unwrap();
            let number = atom(values(one(unit, "unit").unwrap()).unwrap(), 1).unwrap();
            let instances = one(unit, "instances").unwrap();
            let project_node = children(values(instances).unwrap(), "project")
                .next()
                .unwrap();
            patches
                .insert(
                    node_span(project_node).end - 1,
                    format!("(path \"/obsolete\" (reference \"U90\") (unit {number}))"),
                )
                .unwrap();
            patches.insert(node_span(instances).end - 1, format!("(project \"foreign\" (path \"/elsewhere\" (reference \"U80\") (unit {number})))")).unwrap();
        }
        let (bytes, _) = patches.finish().unwrap();
        let bytes = String::from_utf8(bytes)
            .unwrap()
            .replace(
                &format!("(path \"/{ROOT}\""),
                &format!("(path \"/{ROOT}/\""),
            )
            .into_bytes();
        let project = reparsed(&project, &bytes);
        let candidate = compile(
            &project,
            &native,
            &bytes,
            &ids.iter()
                .map(|id| {
                    edit(EditAction::SetReference {
                        uuid: id.clone(),
                        reference: "U20".into(),
                        instance_path: None,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_preserved(&bytes, &candidate);
        for unit in candidate.parsed().files[0]
            .symbols
            .iter()
            .filter(|s| ids.contains(&s.source_uuid))
        {
            assert_eq!(unit.reference, "U20");
            assert_eq!(
                unit.instance_paths
                    .iter()
                    .find(|i| i.path == "/obsolete")
                    .unwrap()
                    .reference
                    .as_deref(),
                Some("U90")
            );
            assert_eq!(
                unit.instance_paths
                    .iter()
                    .find(|i| i.project == "foreign")
                    .unwrap()
                    .reference
                    .as_deref(),
                Some("U80")
            );
        }
    }

    #[test]
    fn component_annotation_allocator_coalesces_members_and_reserves_global_names() {
        let (project, native, bytes, ids) = component_fixture();
        let requests = ids
            .iter()
            .map(|id| ReferenceRequest {
                uuid: id.clone(),
                instance_path: None,
                prefix: "R".into(),
                minimum: 1,
            })
            .collect::<Vec<_>>();
        let edits = allocate_references(&project, &native, &requests).unwrap();
        assert_eq!(edits.len(), 2);
        assert!(edits.iter().all(
            |e| matches!(&e.action, EditAction::SetReference { reference, .. } if reference == "R2")
        ));
        assert!(compile(&project, &native, &bytes, &edits).is_ok());
        assert_eq!(
            edits,
            allocate_references(
                &project,
                &native,
                &requests.iter().rev().cloned().collect::<Vec<_>>()
            )
            .unwrap()
        );
        let mut invalid = requests;
        invalid[1].prefix = "U".into();
        assert!(allocate_references(&project, &native, &invalid).is_err());
    }

    fn assert_preserved(before: &[u8], candidate: &SchematicCandidate) {
        let before = std::str::from_utf8(before).unwrap();
        let after = std::str::from_utf8(&candidate.files()["test.kicad_sch"]).unwrap();
        let mut old = 0;
        let mut new = 0;
        for span in &candidate.replacements()["test.kicad_sch"] {
            let size = span.start - old;
            assert_eq!(&before[old..span.start], &after[new..new + size]);
            assert_eq!(&before[span.start..span.end], span.before);
            new += size;
            assert_eq!(&after[new..new + span.after.len()], span.after);
            new += span.after.len();
            old = span.end;
        }
        assert_eq!(&before[old..], &after[new..]);
    }
    #[test]
    fn candidate_binds_exact_sources_and_preserves_untouched_intervals() {
        let (p, n, b) = fixture();
        let value = "中文 \"quoted\" \\ path\nnext\tvalue";
        let candidate = compile(
            &p,
            &n,
            &b,
            &[
                edit(property(value)),
                edit(EditAction::SetFlags {
                    uuid: SYMBOL.into(),
                    flags: SymbolFlags {
                        dnp: Some(true),
                        in_bom: Some(false),
                        on_board: None,
                    },
                }),
            ],
        )
        .unwrap();
        assert_eq!(candidate.parsed().files[0].symbols[0].value, value);
        assert_eq!(candidate.parsed().files[0].symbols[0].dnp, Some(true));
        assert_eq!(candidate.source_revision(), &p.revision);
        assert_ne!(candidate.parsed().revision, p.revision);
        assert_preserved(&b, &candidate);
        let mut stale = b.clone();
        stale.push(b' ');
        assert!(compile(&p, &n, &stale, &[edit(property("changed"))]).is_err());
        let mut locked = p.clone();
        locked.editor_lock_present = true;
        assert!(compile(&locked, &n, &b, &[edit(property("changed"))]).is_err());
        let mut wrong = edit(property("changed"));
        wrong.scope = EditScope::SingleInstance("wrong".into());
        assert!(compile(&p, &n, &b, &[wrong]).is_err());
    }

    #[test]
    fn typed_reference_edit_updates_property_and_instance_path() {
        let (p, n, b) = fixture();
        let candidate = compile(
            &p,
            &n,
            &b,
            &[edit(EditAction::SetReference {
                uuid: SYMBOL.into(),
                reference: "R101".into(),
                instance_path: None,
            })],
        )
        .unwrap();
        let symbol = &candidate.parsed().files[0].symbols[0];
        assert_eq!(symbol.reference, "R101");
        assert_eq!(symbol.instance_paths[0].reference.as_deref(), Some("R101"));
        assert_preserved(&b, &candidate);
        let path = symbol.instance_paths[0].path.clone();
        let scoped = compile(
            &p,
            &n,
            &b,
            &[SchematicEdit {
                file: p.files[0].artifact.relative_path.clone(),
                scope: EditScope::SingleInstance(path.clone()),
                action: EditAction::SetReference {
                    uuid: SYMBOL.into(),
                    reference: "R102".into(),
                    instance_path: Some(path.clone()),
                },
            }],
        )
        .unwrap();
        assert_eq!(scoped.changes()[0].instance_paths, vec![path]);
        assert_eq!(scoped.parsed().files[0].symbols[0].reference, "R1");
        assert_eq!(
            scoped.parsed().files[0].symbols[0].instance_paths[0]
                .reference
                .as_deref(),
            Some("R102")
        );
        assert_preserved(&b, &scoped);
    }

    #[test]
    fn footprint_assignment_requires_complete_pin_pad_evidence() {
        let (p, n, b) = fixture();
        let candidate = compile(&p, &n, &b, &[edit(footprint())]).unwrap();
        assert_eq!(
            candidate.parsed().files[0].symbols[0].footprint.as_deref(),
            Some("Resistor_SMD:R_0603_1608Metric")
        );
        assert_preserved(&b, &candidate);
        let mut invalid = footprint();
        if let EditAction::AssignFootprint { evidence, .. } = &mut invalid {
            evidence.pin_pad_map.clear();
        }
        assert!(compile(&p, &n, &b, &[edit(invalid)]).is_err());
    }

    #[test]
    fn reference_allocator_is_deterministic_and_rejects_invalid_values() {
        let (p, n, b) = fixture();
        let request = ReferenceRequest {
            uuid: SYMBOL.into(),
            instance_path: None,
            prefix: "RA".into(),
            minimum: 1,
        };
        let first = allocate_references(&p, &n, std::slice::from_ref(&request)).unwrap();
        let second = allocate_references(&p, &n, std::slice::from_ref(&request)).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first[0].action,
            EditAction::SetReference {
                uuid: SYMBOL.into(),
                reference: "RA1".into(),
                instance_path: None,
            }
        );
        let unchanged = edit(EditAction::SetReference {
            uuid: SYMBOL.into(),
            reference: "R1".into(),
            instance_path: None,
        });
        assert!(compile(&p, &n, &b, &[unchanged]).is_err());
        let invalid = edit(EditAction::SetReference {
            uuid: SYMBOL.into(),
            reference: "R 2".into(),
            instance_path: None,
        });
        assert!(compile(&p, &n, &b, &[invalid]).is_err());
    }
    #[test]
    fn overlapping_and_reserved_writes_reject_even_when_one_is_a_noop() {
        let (p, n, b) = fixture();
        for actions in [
            vec![property("a"), property("b")],
            vec![property("a"), property("10k")],
            vec![
                property("10k"),
                EditAction::Delete {
                    uuid: SYMBOL.into(),
                    kind: ObjectKind::Symbol,
                },
            ],
        ] {
            assert!(
                compile(
                    &p,
                    &n,
                    &b,
                    &actions.into_iter().map(edit).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        for name in ["Reference", "Footprint", "reference", "VALUE"] {
            assert!(
                compile(
                    &p,
                    &n,
                    &b,
                    &[edit(EditAction::SetProperty {
                        uuid: SYMBOL.into(),
                        property: SymbolProperty::Named(name.into()),
                        value: "bad".into()
                    })]
                )
                .is_err()
            );
        }
        assert!(compile(&p, &n, &b, &[edit(property("10k"))]).is_err());
        let custom = |name: &str| {
            edit(EditAction::SetProperty {
                uuid: SYMBOL.into(),
                property: SymbolProperty::Named(name.into()),
                value: "new".into(),
            })
        };
        assert!(compile(&p, &n, &b, &[custom("New"), custom("New")]).is_err());
        let candidate = compile(&p, &n, &b, &[custom("New"), custom("Other")]).unwrap();
        assert_preserved(&b, &candidate);
    }
    #[test]
    fn primitive_creation_update_and_deletion_share_exact_source_boundaries() {
        let (p, n, b) = fixture();
        let objects = [
            NewObject::Wire {
                start: point(30, 30),
                end: point(40, 30),
            },
            NewObject::Junction(point(30, 30)),
            NewObject::NoConnect(point(40, 30)),
            NewObject::Label(Label {
                kind: LabelKind::Local,
                text: "本地".into(),
                position: point(30, 30),
                rotation: QuarterTurn::Zero,
            }),
            NewObject::Label(Label {
                kind: LabelKind::Global(ElectricalShape::Input),
                text: "GLOBAL".into(),
                position: point(40, 30),
                rotation: QuarterTurn::Half,
            }),
            NewObject::Label(Label {
                kind: LabelKind::Hierarchical(ElectricalShape::Output),
                text: "HIER".into(),
                position: point(40, 40),
                rotation: QuarterTurn::Counterclockwise90,
            }),
        ];
        let edits = objects
            .iter()
            .enumerate()
            .map(|(i, o)| {
                edit(EditAction::Create {
                    key: i.to_string(),
                    object: o.clone(),
                })
            })
            .collect::<Vec<_>>();
        let created = compile(&p, &n, &b, &edits).unwrap();
        assert_preserved(&b, &created);
        let again = compile(&p, &n, &b, &edits).unwrap();
        assert_eq!(again.files(), created.files());
        assert_eq!(
            created
                .changes()
                .iter()
                .map(|c| &c.uuid)
                .collect::<BTreeSet<_>>()
                .len(),
            6
        );
        let mutations = created
            .changes()
            .iter()
            .map(|c| {
                edit(match c.kind {
                    ObjectKind::Wire => EditAction::UpdateWire {
                        uuid: c.uuid.clone(),
                        start: point(31, 30),
                        end: point(41, 30),
                    },
                    ObjectKind::Junction | ObjectKind::NoConnect => EditAction::MoveMarker {
                        uuid: c.uuid.clone(),
                        kind: c.kind,
                        position: point(31, 30),
                    },
                    _ => EditAction::UpdateLabel {
                        uuid: c.uuid.clone(),
                        label: Label {
                            kind: match c.kind {
                                ObjectKind::LocalLabel => LabelKind::Local,
                                ObjectKind::GlobalLabel => {
                                    LabelKind::Global(ElectricalShape::Passive)
                                }
                                _ => LabelKind::Hierarchical(ElectricalShape::Bidirectional),
                            },
                            text: "changed".into(),
                            position: point(31, 31),
                            rotation: QuarterTurn::Counterclockwise270,
                        },
                    },
                })
            })
            .collect::<Vec<_>>();
        let updated = compile(
            created.parsed(),
            &n,
            &created.files()["test.kicad_sch"],
            &mutations,
        )
        .unwrap();
        assert_preserved(&created.files()["test.kicad_sch"], &updated);
        let deletes = created
            .changes()
            .iter()
            .map(|c| {
                edit(EditAction::Delete {
                    uuid: c.uuid.clone(),
                    kind: c.kind,
                })
            })
            .collect::<Vec<_>>();
        let deleted = compile(
            updated.parsed(),
            &n,
            &updated.files()["test.kicad_sch"],
            &deletes,
        )
        .unwrap();
        assert_preserved(&updated.files()["test.kicad_sch"], &deleted);
        let parsed = deleted.parsed();
        assert_eq!(parsed.files[0].symbols, p.files[0].symbols);
    }
    #[test]
    fn symbol_transforms_move_fields_and_preserve_unrelated_content() {
        let (p, n, b) = fixture();
        for rotation in [
            QuarterTurn::Zero,
            QuarterTurn::Counterclockwise90,
            QuarterTurn::Half,
            QuarterTurn::Counterclockwise270,
        ] {
            for mirror in [Mirror::None, Mirror::X, Mirror::Y] {
                let target = SymbolTransform {
                    origin: point(50, 60),
                    rotation,
                    mirror,
                };
                let c = compile(
                    &p,
                    &n,
                    &b,
                    &[edit(EditAction::TransformSymbol {
                        uuid: SYMBOL.into(),
                        transform: target,
                    })],
                )
                .unwrap();
                assert_preserved(&b, &c);
                let doc =
                    parse_one(std::str::from_utf8(&c.files()["test.kicad_sch"]).unwrap()).unwrap();
                let root = values(&doc.nodes[0]).unwrap();
                let symbol = values(object(root, SYMBOL, ObjectKind::Symbol).unwrap()).unwrap();
                assert_eq!(SymbolTransform::from_cst(symbol).unwrap(), target);
                let field = values(children(symbol, "property").next().unwrap()).unwrap();
                let p = Point::from_at(values(one(field, "at").unwrap()).unwrap()).unwrap();
                // Original reference offset is (1,1) mm in sheet coordinates.
                let expected = target.contact(point(1, -1)).unwrap();
                assert_eq!(p, expected);
            }
        }
    }
}
