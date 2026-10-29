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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditScope {
    SingleInstance(String),
    FileAllInstances,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectKind {
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
    CreateSymbol {
        key: String,
        symbol: SymbolCreation,
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
    let mut pins = BTreeSet::new();
    for body in children(root, "symbol") {
        let body_name = atom(values(body)?, 1)?;
        let mut parts = body_name.rsplitn(3, '_');
        let body_number = parts
            .next()
            .ok_or("missing embedded body style")?
            .parse::<u32>()
            .map_err(|_| "invalid embedded body style")?;
        let unit_number = parts
            .next()
            .ok_or("missing embedded unit")?
            .parse::<u32>()
            .map_err(|_| "invalid embedded unit")?;
        if parts.next().is_none()
            || (unit_number != 0 && unit_number != unit)
            || (body_number != 0 && body_number != body_style.unwrap_or(1))
        {
            continue;
        }
        for pin in children(values(body)?, "pin") {
            let number = atom(values(one(values(pin)?, "number")?)?, 1)?.to_owned();
            if number.is_empty() || number.contains(['[', ']', '$']) || !pins.insert(number) {
                return Err("symbol definition has unsupported or repeated pin number".into());
            }
        }
    }
    if pins.is_empty() {
        return Err("symbol definition has no usable pins".into());
    }
    let _ = nickname;
    Ok((name, normalized, pins.into_iter().collect()))
}

struct CreationAllocation {
    symbol_uuid: String,
    pin_uuids: Vec<String>,
    definition: String,
    pin_numbers: Vec<String>,
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
            quote(&symbol.reference),
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
) -> Result<(String, ObjectKind), String> {
    let (id, kind) = match action {
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
        EditAction::Create { .. } | EditAction::CreateSymbol { .. } => {
            return Err("creation requires allocation".into());
        }
    };
    let node = object(root, id, kind)?;
    let items = values(node)?;
    match action {
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
            let instances = one(items, "instances")?;
            let project_nodes = values(instances)?
                .iter()
                .filter_map(list_items)
                .filter(|v| list_tag(v) == Some("project"))
                .collect::<Vec<_>>();
            if project_nodes.len() != 1 {
                return Err("symbol must have one instances project".into());
            }
            let paths = project_nodes[0]
                .iter()
                .skip(2)
                .filter_map(list_items)
                .filter(|v| list_tag(v) == Some("path"))
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return Err("symbol has no instance paths".into());
            }
            if let Some(target) = instance_path {
                let matches = paths
                    .iter()
                    .filter(|path| atom_text(path.get(1)) == Some(target.as_str()))
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
        EditAction::Create { .. } | EditAction::CreateSymbol { .. } => unreachable!(),
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

fn validate_footprint_edits(
    project: &ParsedSchematicProject,
    edits: &[SchematicEdit],
) -> Result<(), String> {
    for edit in edits {
        let EditAction::AssignFootprint {
            uuid,
            footprint,
            evidence,
        } = &edit.action
        else {
            continue;
        };
        validate_footprint_identifier(footprint)?;
        if !validate_digest(&evidence.catalog_digest) || !validate_digest(&evidence.source_digest) {
            return Err("footprint assignment evidence digest is invalid".into());
        }
        if evidence.pad_numbers.is_empty() || evidence.pad_numbers.len() > 512 {
            return Err("footprint assignment pad evidence is empty or oversized".into());
        }
        let pads = evidence.pad_numbers.iter().collect::<BTreeSet<_>>();
        if pads.len() != evidence.pad_numbers.len()
            || pads.iter().any(|pad| {
                pad.is_empty()
                    || pad.len() > 32
                    || !pad.is_ascii()
                    || pad
                        .chars()
                        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
            })
        {
            return Err("footprint assignment pad evidence is invalid".into());
        }
        let symbol = project
            .files
            .iter()
            .find_map(|file| {
                file.symbols
                    .iter()
                    .find(|symbol| symbol.source_uuid == *uuid)
            })
            .ok_or("footprint assignment target symbol is not observed")?;
        let pins = symbol
            .pins
            .iter()
            .map(|pin| pin.number.as_str())
            .collect::<BTreeSet<_>>();
        let mapped_pins = evidence
            .pin_pad_map
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mapped_pads = evidence.pin_pad_map.values().collect::<BTreeSet<_>>();
        if pins.is_empty()
            || mapped_pins != pins
            || mapped_pads.len() != evidence.pin_pad_map.len()
            || mapped_pads.iter().any(|pad| !pads.contains(pad))
            || evidence.pin_pad_map.len() != evidence.pad_numbers.len()
        {
            return Err("footprint assignment pin-pad evidence is not a complete bijection".into());
        }
    }
    Ok(())
}

fn reference_entries(project: &ParsedSchematicProject) -> Vec<(String, String, String)> {
    project
        .files
        .iter()
        .flat_map(|file| {
            file.symbols.iter().flat_map(move |symbol| {
                symbol.instance_paths.iter().filter_map(move |instance| {
                    instance.reference.as_ref().map(|reference| {
                        (
                            instance.path.clone(),
                            reference.clone(),
                            symbol.source_uuid.clone(),
                        )
                    })
                })
            })
        })
        .collect()
}

/// Allocate deterministic references against the complete expanded hierarchy.
pub fn allocate_references(
    project: &ParsedSchematicProject,
    native: &NetlistDocument,
    requests: &[ReferenceRequest],
) -> Result<Vec<SchematicEdit>, String> {
    if requests.is_empty() || requests.len() > 128 {
        return Err("reference allocation requires 1..128 requests".into());
    }
    let _ = SchematicIdentityIndex::resolve(project, native).map_err(|e| e.to_string())?;
    let mut occupied = reference_entries(project);
    let mut edits = Vec::new();
    let mut assigned = BTreeSet::new();
    for request in requests {
        if !uuid(&request.uuid) || request.prefix.is_empty() || !request.prefix.is_ascii() {
            return Err("invalid reference allocation request".into());
        }
        let file = project
            .files
            .iter()
            .find(|file| {
                file.symbols
                    .iter()
                    .any(|symbol| symbol.source_uuid == request.uuid)
            })
            .ok_or("reference request symbol is not observed")?;
        let target_paths = file
            .symbols
            .iter()
            .find(|symbol| symbol.source_uuid == request.uuid)
            .map(|symbol| {
                symbol
                    .instance_paths
                    .iter()
                    .filter(|instance| {
                        request
                            .instance_path
                            .as_deref()
                            .is_none_or(|path| path == instance.path)
                    })
                    .map(|instance| instance.path.clone())
                    .collect::<Vec<_>>()
            })
            .ok_or("reference request symbol is not observed")?;
        if target_paths.is_empty() {
            return Err("reference request instance path is not observed".into());
        }
        let mut number = request.minimum.max(1);
        let reference = loop {
            let candidate = format!("{}{number}", request.prefix);
            if !occupied.iter().any(|(path, value, _source)| {
                target_paths.iter().any(|target| target == path) && value == &candidate
            }) && target_paths
                .iter()
                .all(|path| !assigned.contains(&(path.clone(), candidate.clone())))
            {
                break candidate;
            }
            number = number
                .checked_add(1)
                .ok_or("reference allocation overflow")?;
        };
        for path in &target_paths {
            occupied.push((path.clone(), reference.clone(), request.uuid.clone()));
            assigned.insert((path.clone(), reference.clone()));
        }
        edits.push(SchematicEdit {
            file: file.artifact.relative_path.clone(),
            scope: if request.instance_path.is_some() {
                EditScope::SingleInstance(request.instance_path.clone().unwrap())
            } else {
                EditScope::FileAllInstances
            },
            action: EditAction::SetReference {
                uuid: request.uuid.clone(),
                reference,
                instance_path: request.instance_path.clone(),
            },
        });
    }
    Ok(edits)
}

fn validate_reference_edits(
    project: &ParsedSchematicProject,
    edits: &[SchematicEdit],
) -> Result<(), String> {
    let mut occupied = BTreeMap::<(String, String), String>::new();
    for (path, reference, source_uuid) in reference_entries(project) {
        occupied.entry((path, reference)).or_insert(source_uuid);
    }
    for edit in edits {
        let EditAction::SetReference {
            uuid,
            reference,
            instance_path,
        } = &edit.action
        else {
            continue;
        };
        validate_reference(reference)?;
        let file = project
            .files
            .iter()
            .find(|file| file.artifact.relative_path == edit.file)
            .ok_or("reference target file is not observed")?;
        let symbol = file
            .symbols
            .iter()
            .find(|symbol| symbol.source_uuid == *uuid)
            .ok_or("reference target symbol is not observed")?;
        let paths = symbol
            .instance_paths
            .iter()
            .filter(|instance| {
                instance_path
                    .as_deref()
                    .is_none_or(|target| target == instance.path)
            })
            .map(|instance| instance.path.clone())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Err("reference target instance path is not observed".into());
        }
        for path in paths {
            if let Some(existing) = occupied.get(&(path.clone(), reference.clone()))
                && existing != uuid
            {
                return Err(format!(
                    "reference `{reference}` is already used at instance path `{path}`"
                ));
            }
            occupied.insert((path, reference.clone()), uuid.clone());
        }
    }
    Ok(())
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
    validate_reference_edits(project, edits)?;
    validate_footprint_edits(project, edits)?;
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
    let mut allocations = BTreeMap::new();
    let mut symbol_allocations = BTreeMap::new();
    let mut keys = BTreeSet::new();
    let mut created_references = BTreeSet::new();
    for (ordinal, edit) in edits.iter().enumerate() {
        let key = match &edit.action {
            EditAction::Create { key, .. } | EditAction::CreateSymbol { key, .. } => key,
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
                let occupied = reference_entries(project);
                if occupied.iter().any(|(path, reference, _)| {
                    supplied_paths.contains(path) && reference == &symbol.reference
                }) {
                    return Err("created symbol reference is already occupied".into());
                }
                if supplied_paths.iter().any(|path| {
                    !created_references.insert((path.clone(), symbol.reference.clone()))
                }) {
                    return Err("created symbol reference is repeated in the edit set".into());
                }
                let _ = file;
                symbol_allocations.insert(
                    ordinal,
                    CreationAllocation {
                        symbol_uuid,
                        pin_uuids,
                        definition,
                        pin_numbers,
                    },
                );
            }
            _ => {}
        }
    }
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
                EditAction::Create { object, .. } => {
                    let id = &allocations[&ordinal];
                    appended.push_str(&render(object, id)?);
                    (id.clone(), object.kind())
                }
                EditAction::CreateSymbol { symbol, .. } => {
                    let allocation = &symbol_allocations[&ordinal];
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
                _ => edit_existing(root, &edit.action, &mut patches, &file.format_version)?,
            };
            changes.push(ObjectChange {
                file: name.clone(),
                uuid: id,
                kind,
                instance_paths: instances.clone(),
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
