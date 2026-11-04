//! Exact saved-board candidates; models and candidates carry no publication approval.
use super::*;
mod disposition;
pub(super) mod replacement;
use kicad_checks::connectivity::{ClosedConnectivityEvidence, ConnectivityKind};
use kiutils_sexpr::{Atom, CstDocument, Node, Span};
use serde::{Deserialize, Serialize};

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_ITEMS: usize = 100_000;

pub(super) fn items(node: &Node) -> Result<&[Node], String> {
    match node {
        Node::List { items, .. } => Ok(items),
        _ => Err("expected board form".into()),
    }
}
pub(super) fn atom(node: &Node) -> Result<&str, String> {
    match node {
        Node::Atom {
            atom: Atom::Quoted(v) | Atom::Symbol(v),
            ..
        } => Ok(v),
        _ => Err("expected board atom".into()),
    }
}
fn span(node: &Node) -> Span {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => *span,
    }
}
fn tag(node: &Node) -> Result<&str, String> {
    atom(items(node)?.first().ok_or("empty board form")?)
}
fn scalar(node: &Node) -> Result<&str, String> {
    let fields = items(node)?;
    if fields.len() != 2 {
        return Err("expected one board field value".into());
    }
    atom(&fields[1])
}
fn children<'a>(node: &'a Node, name: &str) -> Result<Vec<&'a Node>, String> {
    Ok(items(node)?
        .iter()
        .filter(|n| matches!(n, Node::List { .. }) && tag(n).is_ok_and(|t| t == name))
        .collect())
}
fn optional<'a>(node: &'a Node, name: &str) -> Result<Option<&'a Node>, String> {
    let nodes = children(node, name)?;
    if nodes.len() > 1 {
        return Err(format!("duplicate board field: {name}"));
    }
    Ok(nodes.first().copied())
}
fn required<'a>(node: &'a Node, name: &str) -> Result<&'a Node, String> {
    optional(node, name)?.ok_or_else(|| format!("missing board field: {name}"))
}
fn locked(node: &Node) -> Result<bool, String> {
    match optional(node, "locked")? {
        None => Ok(false),
        Some(v) => match items(v)? {
            [_] => Ok(true),
            [_, n] => match atom(n)? {
                "yes" => Ok(true),
                "no" => Ok(false),
                _ => Err("unknown board lock".into()),
            },
            _ => Err("invalid board lock".into()),
        },
    }
}
fn quoted(text: &str) -> Result<String, String> {
    if text.len() > 64 * 1024 || text.contains('\0') {
        return Err("board value exceeds bound or contains NUL".into());
    }
    let mut quoted = String::from("\"");
    for value in text.chars() {
        match value {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{7}' => quoted.push_str("\\a"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{b}' => quoted.push_str("\\v"),
            '\u{c}' => quoted.push_str("\\f"),
            value if value.is_ascii_control() => {
                quoted.push_str(&format!("\\x{:02x}", value as u32))
            }
            value => quoted.push(value),
        }
    }
    quoted.push('"');
    Ok(quoted)
}
pub(super) fn uuid(value: &str) -> bool {
    value.len() == 36
        && value != "00000000-0000-0000-0000-000000000000"
        && value.bytes().enumerate().all(|(i, v)| {
            if [8, 13, 18, 23].contains(&i) {
                v == b'-'
            } else {
                v.is_ascii_digit() || (b'a'..=b'f').contains(&v)
            }
        })
}
fn identity(node: &Node) -> Result<String, String> {
    let value = scalar(required(node, "uuid")?)?;
    if !uuid(value) {
        return Err("invalid native board UUID".into());
    }
    Ok(value.into())
}
fn properties(node: &Node) -> Result<BTreeMap<String, &Node>, String> {
    let mut result = BTreeMap::new();
    for property in children(node, "property")? {
        let fields = items(property)?;
        let name = atom(fields.get(1).ok_or("missing board property name")?)?;
        atom(fields.get(2).ok_or("missing board property value")?)?;
        if result.insert(name.into(), property).is_some() {
            return Err("duplicate board property".into());
        }
    }
    Ok(result)
}
fn property(node: &Node, name: &str) -> Result<String, String> {
    let props = properties(node)?;
    atom(
        items(
            props
                .get(name)
                .ok_or_else(|| format!("missing board property: {name}"))?,
        )?
        .get(2)
        .ok_or("missing board property value")?,
    )
    .map(str::to_owned)
}
fn net(node: &Node, nets: &BTreeMap<String, String>) -> Result<String, String> {
    let Some(net) = optional(node, "net")? else {
        return Ok(String::new());
    };
    match items(net)? {
        [
            _,
            Node::Atom {
                atom: Atom::Quoted(value),
                ..
            },
        ] => Ok(value.clone()),
        [_, number] => nets
            .get(atom(number)?)
            .cloned()
            .ok_or_else(|| "unresolved numeric board net".into()),
        [_, code, name] if nets.get(atom(code)?) == Some(&atom(name)?.to_owned()) => {
            Ok(atom(name)?.into())
        }
        _ => Err("unsupported board net form".into()),
    }
}

/// Dimension text serializes the same native BOARD_ITEM; all independent identities stay unique.
fn board_identities(root: &Node) -> Result<BTreeSet<String>, String> {
    let mut reserved = BTreeSet::new();
    let mut pending = vec![(root, None::<&str>)];
    while let Some((node, owned_alias)) = pending.pop() {
        let Node::List { items: fields, .. } = node else {
            continue;
        };
        let name = fields.first().and_then(|n| atom(n).ok());
        if name == Some("uuid") {
            let id = scalar(node)?;
            if !uuid(id)
                || owned_alias.is_some_and(|owner| owner != id)
                || owned_alias.is_none() && !reserved.insert(id.to_owned())
            {
                return Err("invalid or duplicate global board UUID".into());
            }
            if reserved.len() > 1_000_000 {
                return Err("board UUID count exceeds bound".into());
            }
            continue;
        }
        let dimension = if name == Some("dimension")
            && optional(node, "type")?
                .map(scalar)
                .transpose()?
                .is_some_and(|kind| matches!(kind, "aligned" | "orthogonal" | "leader" | "radial"))
        {
            let id = scalar(required(node, "uuid")?)?;
            let labels = children(node, "gr_text")?;
            if labels.len() != 1 || scalar(required(labels[0], "uuid")?)? != id {
                return Err("native dimension must have one exact owned text identity".into());
            }
            Some(id)
        } else {
            None
        };
        for child in fields.iter().filter(|n| matches!(n, Node::List { .. })) {
            let child_name = tag(child)?;
            let alias = if dimension.is_some() && child_name == "gr_text" {
                dimension
            } else if name == Some("gr_text") && child_name == "uuid" {
                owned_alias
            } else {
                None
            };
            pending.push((child, alias));
        }
    }
    Ok(reserved)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationPad {
    pub uuid: String,
    pub number: String,
    pub net: String,
    pub pin_function: String,
    pub pin_type: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SynchronizationFootprint {
    pub uuid: String,
    pub reference: String,
    pub value: String,
    pub library: String,
    pub path: Option<String>,
    pub locked: bool,
    pub board_only: bool,
    pub pads: Vec<SynchronizationPad>,
}

#[derive(Clone, Debug)]
pub struct SavedSynchronizationBoard {
    cst: CstDocument,
    footprints: BTreeMap<String, SynchronizationFootprint>,
    routes: BTreeMap<String, (String, bool)>,
    digest: String,
    has_zones: bool,
    net_names: BTreeSet<String>,
    identities: BTreeSet<String>,
    external_references: BTreeMap<String, BTreeSet<Option<String>>>,
}
impl SavedSynchronizationBoard {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.is_empty() || text.len() > MAX_BYTES || text.contains('\0') {
            return Err("saved synchronization board exceeds bounds".into());
        }
        crate::library_catalog::symbols::validate_raw_nesting(text.as_bytes())?;
        let cst = kicad_schematic::cst::parse_one(text).map_err(|e| e.to_string())?;
        let root = cst.nodes.first().ok_or("missing board root")?;
        if tag(root)? != "kicad_pcb" {
            return Err("expected saved kicad_pcb root".into());
        }
        if scalar(required(root, "version")?)? != "20260206" {
            return Err("unverified saved synchronization board version".into());
        }
        let reserved = board_identities(root)?;
        let mut external_references = BTreeMap::<String, BTreeSet<Option<String>>>::new();
        let mut pending = vec![(root, None::<String>)];
        while let Some((node, owner)) = pending.pop() {
            match node {
                Node::List { items: fields, .. } => {
                    let owner = if fields.first().and_then(|n| atom(n).ok()) == Some("footprint") {
                        Some(identity(node)?)
                    } else {
                        owner
                    };
                    pending.extend(fields.iter().map(|node| (node, owner.clone())));
                }
                Node::Atom { .. } => {
                    // Unknown forms and path-valued references must not retain dangling child IDs.
                    for token in atom(node)?.split(|c: char| !c.is_ascii_hexdigit() && c != '-') {
                        if uuid(token) && reserved.contains(token) {
                            external_references
                                .entry(token.into())
                                .or_default()
                                .insert(owner.clone());
                        }
                    }
                }
            }
        }
        let mut nets = BTreeMap::new();
        nets.insert("0".into(), String::new());
        for node in children(root, "net")? {
            let fields = items(node)?;
            if fields.len() != 3 {
                return Err("unsupported board net table".into());
            }
            let code = atom(&fields[1])?.to_owned();
            let name = atom(&fields[2])?.to_owned();
            if let Some(previous) = nets.insert(code, name.clone())
                && previous != name
            {
                return Err("ambiguous board net table".into());
            }
        }
        let mut footprints = BTreeMap::new();
        for node in children(root, "footprint")? {
            let id = identity(node)?;
            let mut pads = Vec::new();
            for pad in children(node, "pad")? {
                pads.push(SynchronizationPad {
                    uuid: identity(pad)?,
                    number: atom(items(pad)?.get(1).ok_or("missing board pad number")?)?.into(),
                    net: net(pad, &nets)?,
                    pin_function: optional(pad, "pinfunction")?
                        .map(scalar)
                        .transpose()?
                        .unwrap_or("")
                        .into(),
                    pin_type: optional(pad, "pintype")?
                        .map(scalar)
                        .transpose()?
                        .unwrap_or("")
                        .into(),
                });
            }
            pads.sort_by(|a, b| a.uuid.cmp(&b.uuid));
            let attr = optional(node, "attr")?
                .map(items)
                .transpose()?
                .unwrap_or(&[]);
            let fp = SynchronizationFootprint {
                uuid: id.clone(),
                reference: property(node, "Reference")?,
                value: property(node, "Value")?,
                library: atom(items(node)?.get(1).ok_or("missing footprint identifier")?)?.into(),
                path: optional(node, "path")?
                    .map(scalar)
                    .transpose()?
                    .map(str::to_owned),
                locked: locked(node)?,
                board_only: attr
                    .iter()
                    .skip(1)
                    .any(|n| atom(n).is_ok_and(|s| s == "board_only")),
                pads,
            };
            if footprints.insert(id, fp).is_some() || footprints.len() > MAX_ITEMS {
                return Err("duplicate or excessive footprints".into());
            }
        }
        let mut routes = BTreeMap::new();
        for name in ["segment", "arc", "via"] {
            for node in children(root, name)? {
                routes.insert(identity(node)?, (net(node, &nets)?, locked(node)?));
            }
        }
        for name in ["gr_line", "gr_arc", "gr_circle", "gr_rect", "gr_poly"] {
            for node in children(root, name)? {
                let layer = scalar(required(node, "layer")?)?;
                if layer.parse::<kicad_model::CopperLayer>().is_ok() {
                    routes.insert(identity(node)?, (net(node, &nets)?, locked(node)?));
                }
            }
        }
        if routes.len() > MAX_ITEMS {
            return Err("board routing count exceeds bound".into());
        }
        let zones = children(root, "zone")?;
        let has_zones = !zones.is_empty();
        let mut net_names = nets
            .values()
            .filter(|name| !name.is_empty())
            .cloned()
            .collect::<BTreeSet<_>>();
        net_names.extend(
            footprints
                .values()
                .flat_map(|fp| fp.pads.iter().map(|pad| pad.net.clone()))
                .filter(|name| !name.is_empty()),
        );
        net_names.extend(
            routes
                .values()
                .map(|(name, _)| name.clone())
                .filter(|name| !name.is_empty()),
        );
        for zone in zones {
            let name = net(zone, &nets)?;
            if !name.is_empty() {
                net_names.insert(name);
            }
        }

        Ok(Self {
            cst,
            footprints,
            routes,
            digest: hash(text.as_bytes()),
            has_zones,
            net_names,
            identities: reserved,
            external_references,
        })
    }
    fn has_external_references(&self, footprint: &str) -> Result<bool, String> {
        let mut pending = vec![self.node(footprint)?];
        while let Some(node) = pending.pop() {
            if let Node::List { items: fields, .. } = node {
                if fields.first().and_then(|n| atom(n).ok()) == Some("uuid") {
                    let id = scalar(node)?;
                    if self.external_references.get(id).is_some_and(|owners| {
                        owners
                            .iter()
                            .any(|owner| owner.as_deref() != Some(footprint))
                    }) {
                        return Ok(true);
                    }
                }
                pending.extend(
                    fields
                        .iter()
                        .filter(|node| matches!(node, Node::List { .. })),
                );
            }
        }
        Ok(false)
    }
    pub(super) fn identities(&self) -> &BTreeSet<String> {
        &self.identities
    }
    pub(super) fn footprint_text(&self, id: &str) -> Result<&str, String> {
        let span = span(self.node(id)?);
        self.text()
            .get(span.start..span.end)
            .ok_or_else(|| "invalid footprint source span".into())
    }
    pub(super) fn with_templates(
        &self,
        templates: &[templates::SavedFootprintTemplate],
    ) -> Result<Self, String> {
        let mut text = self.text().to_owned();
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        let mut inserted = String::new();
        let mut replacements = Vec::new();
        let mut ids = self.identities.clone();
        let mut replaced = BTreeSet::new();
        for template in templates {
            for id in &template.identities {
                if template.replacement.is_some() && id == &template.root_id {
                    if !self.footprints.contains_key(id) || !replaced.insert(id.clone()) {
                        return Err("unknown or repeated replacement root identity".into());
                    }
                } else if !ids.insert(id.clone()) {
                    return Err("template allocation collides with complete board namespace".into());
                }
            }
            if template.replacement.is_some() {
                replacements.push((
                    span(self.node(&template.root_id)?),
                    template.footprint.clone(),
                ));
            } else {
                inserted.push('\n');
                inserted.push_str(&template.footprint);
            }
        }
        if text.len().saturating_add(inserted.len()) > MAX_BYTES {
            return Err("added templates exceed saved board byte bound".into());
        }
        replacements.push((
            Span {
                start: span(root).end - 1,
                end: span(root).end - 1,
            },
            inserted,
        ));
        replacements.sort_by_key(|(span, _)| span.start);
        for (span, value) in replacements.into_iter().rev() {
            text.replace_range(span.start..span.end, &value);
        }
        if text.len() > MAX_BYTES {
            return Err("replacement templates exceed saved board byte bound".into());
        }
        Self::parse(&text)
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn footprints(&self) -> &BTreeMap<String, SynchronizationFootprint> {
        &self.footprints
    }
    pub fn routes(&self) -> &BTreeMap<String, (String, bool)> {
        &self.routes
    }
    pub fn has_zones(&self) -> bool {
        self.has_zones
    }
    pub fn text(&self) -> &str {
        &self.cst.raw
    }
    pub(super) fn net_names(&self) -> &BTreeSet<String> {
        &self.net_names
    }
    pub(super) fn copper(&self) -> &BTreeMap<String, (String, bool)> {
        &self.routes
    }

    pub(super) fn verify_native_units(&self, native: &Self) -> Result<(), String> {
        for (id, fp) in &self.footprints {
            let after = native
                .footprints
                .get(id)
                .ok_or("native footprint set differs")?;
            let mut normalized = fp.clone();
            normalized.path = after.path.clone();
            if &normalized != after
                || fp.path.as_deref().unwrap_or("").trim_end_matches('/')
                    != after.path.as_deref().unwrap_or("").trim_end_matches('/')
            {
                return Err("native footprint, pad or metadata projection differs".into());
            }
            let before_fields = properties(self.node(id)?)?;
            let after_fields = properties(native.node(id)?)?;
            if before_fields.keys().collect::<BTreeSet<_>>()
                != after_fields.keys().collect::<BTreeSet<_>>()
            {
                return Err("native footprint property set differs".into());
            }
            for name in before_fields.keys() {
                if atom(&items(before_fields[name])?[2])? != atom(&items(after_fields[name])?[2])? {
                    return Err(format!("native footprint property differs: {name}"));
                }
            }
            if read_units(self.node(id)?)? != read_units(native.node(id)?)? {
                return Err("native unit metadata differs from exact candidate".into());
            }
            for name in ["sheetname", "sheetfile"] {
                let value = optional(self.node(id)?, name)?
                    .map(scalar)
                    .transpose()?
                    .unwrap_or("");
                let actual = optional(native.node(id)?, name)?
                    .map(scalar)
                    .transpose()?
                    .unwrap_or("");
                if value != actual {
                    return Err(format!("native footprint {name} differs"));
                }
            }
            let attrs = |board: &Self| -> Result<BTreeSet<String>, String> {
                optional(board.node(id)?, "attr")?
                    .map(items)
                    .transpose()?
                    .unwrap_or(&[])
                    .iter()
                    .skip(1)
                    .map(|n| atom(n).map(str::to_owned))
                    .collect::<Result<_, _>>()
            };
            if attrs(self)? != attrs(native)? {
                return Err("native footprint attributes differ".into());
            }
        }
        Ok(())
    }

    pub(super) fn protected_native_digest(
        &self,
        changes: &[SynchronizationDifference],
    ) -> Result<String, String> {
        self.protected_digest(changes, false)
    }
    pub(super) fn protected_template_digest(
        &self,
        changes: &[SynchronizationDifference],
    ) -> Result<String, String> {
        self.protected_digest(changes, true)
    }
    fn protected_digest(
        &self,
        changes: &[SynchronizationDifference],
        canonical_footprint: bool,
    ) -> Result<String, String> {
        let allowed = changes
            .iter()
            .map(|c| ((c.uuid.as_str(), c.field.as_str()), c))
            .collect::<BTreeMap<_, _>>();
        let mut hash = Sha256::new();
        fn include(
            node: &Node,
            owner: Option<&str>,
            allowed: &BTreeMap<(&str, &str), &SynchronizationDifference>,
        ) -> Result<bool, String> {
            let Node::List { .. } = node else {
                return Ok(true);
            };
            let name = tag(node)?;
            if name == "footprint" {
                let id = scalar(required(node, "uuid")?)?;
                if allowed.contains_key(&(id, "footprint")) {
                    return Ok(false);
                }
            }
            if let Some(id) = owner {
                if [
                    "path",
                    "sheetname",
                    "sheetfile",
                    "units",
                    "net",
                    "pinfunction",
                    "pintype",
                ]
                .contains(&name)
                    && allowed.contains_key(&(id, name))
                {
                    return Ok(false);
                }
                if name == "attr" && allowed.contains_key(&(id, "attr")) {
                    return Ok(items(node)?.iter().skip(1).any(|v| {
                        !atom(v).is_ok_and(|v| ["dnp", "exclude_from_bom"].contains(&v))
                    }));
                }
            }
            Ok(true)
        }
        fn feed(
            hash: &mut Sha256,
            text: &str,
            node: &Node,
            owner: Option<&str>,
            allowed: &BTreeMap<(&str, &str), &SynchronizationDifference>,
            canonical_footprint: bool,
        ) -> Result<(), String> {
            match node {
                Node::Atom { span, .. } => {
                    hash.update([1]);
                    hash.update(((span.end - span.start) as u64).to_le_bytes());
                    hash.update(&text.as_bytes()[span.start..span.end]);
                }
                Node::List { items: fields, .. } => {
                    let name = tag(node)?;
                    let owner = if [
                        "footprint",
                        "pad",
                        "segment",
                        "arc",
                        "via",
                        "gr_line",
                        "gr_arc",
                        "gr_circle",
                        "gr_rect",
                        "gr_poly",
                    ]
                    .contains(&name)
                    {
                        Some(scalar(required(node, "uuid")?)?)
                    } else {
                        owner
                    };
                    let property = if name == "property" {
                        fields.get(1).map(atom).transpose()?
                    } else {
                        None
                    };
                    let property_allowed = owner.zip(property).is_some_and(|(id, name)| {
                        allowed.contains_key(&(id, format!("property:{name}").as_str()))
                    });
                    let attr_allowed = name == "attr"
                        && owner.is_some_and(|id| allowed.contains_key(&(id, "attr")));
                    let children = fields
                        .iter()
                        .enumerate()
                        .filter_map(|(index, child)| {
                            if attr_allowed
                                && index > 0
                                && atom(child)
                                    .is_ok_and(|v| ["dnp", "exclude_from_bom"].contains(&v))
                            {
                                return None;
                            }
                            match include(child, owner, allowed) {
                                Ok(true) => Some(Ok((index, child))),
                                Ok(false) => None,
                                Err(e) => Some(Err(e)),
                            }
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    hash.update([0]);
                    hash.update((children.len() as u64).to_le_bytes());
                    if canonical_footprint && name == "footprint" {
                        // Native SaveBoard relocates inherited properties and local overrides.
                        // Only the root footprint form is a set; nested physical point order stays exact.
                        let mut records = Vec::new();
                        for (_, child) in children {
                            let mut child_hash = Sha256::new();
                            feed(
                                &mut child_hash,
                                text,
                                child,
                                owner,
                                allowed,
                                canonical_footprint,
                            )?;
                            records.push(child_hash.finalize().to_vec());
                        }
                        records.sort();
                        for record in records {
                            hash.update(record);
                        }
                    } else {
                        for (index, child) in children {
                            if property_allowed && index == 2 {
                                hash.update([2]);
                            } else {
                                feed(hash, text, child, owner, allowed, canonical_footprint)?;
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        feed(
            &mut hash,
            &self.cst.raw,
            self.cst.nodes.first().ok_or("missing native board root")?,
            None,
            &allowed,
            canonical_footprint,
        )?;
        Ok(hex::encode(hash.finalize()))
    }
    pub(super) fn node(&self, id: &str) -> Result<&Node, String> {
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        items(root)?
            .iter()
            .filter(|n| matches!(n, Node::List { .. }))
            .find(|n| {
                optional(n, "uuid")
                    .ok()
                    .flatten()
                    .and_then(|v| scalar(v).ok())
                    == Some(id)
            })
            .ok_or_else(|| "missing direct board target".into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SynchronizationSide {
    Front,
    Back,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationPlacement {
    pub position_nm: [i64; 2],
    pub orientation_mdeg: u32,
    pub side: SynchronizationSide,
}
impl SynchronizationPlacement {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self
            .position_nm
            .iter()
            .any(|value| value.unsigned_abs() > 2_000_000_000)
            || self.orientation_mdeg >= 360000
        {
            return Err(
                "synchronization placement exceeds exact coordinate/orientation bounds".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SynchronizationAction {
    Add {
        component_id: String,
        placement: SynchronizationPlacement,
        #[serde(default)]
        fields: Vec<String>,
    },
    Replace {
        component_id: String,
        footprint_uuid: String,
        #[serde(default)]
        fields: Vec<String>,
    },
    Update {
        component_id: String,
        #[serde(default)]
        fields: Vec<String>,
        #[serde(default)]
        update_nets: bool,
    },
    Relink {
        component_id: String,
        footprint_uuid: String,
        #[serde(default)]
        fields: Vec<String>,
        #[serde(default)]
        update_nets: bool,
    },
    Keep {
        footprint_uuid: String,
    },
    Remove {
        footprint_uuid: String,
    },
    Detach {
        footprint_uuid: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationCopperNet {
    pub uuid: String,
    pub net: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationIntent {
    pub schema_version: u32,
    pub actions: Vec<SynchronizationAction>,
    #[serde(default)]
    pub copper_nets: Vec<SynchronizationCopperNet>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SynchronizationDifference {
    pub uuid: String,
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}
#[derive(Clone, Debug)]
pub struct SynchronizationCandidate {
    source_digest: String,
    board_digest: String,
    bytes: Vec<u8>,
    differences: Vec<SynchronizationDifference>,
    digest: String,
    added_pads: BTreeSet<String>,
    replacement_pads: BTreeMap<String, String>,
    template_evidence: Vec<Value>,
}
impl SynchronizationCandidate {
    pub(super) fn added_pads(&self) -> &BTreeSet<String> {
        &self.added_pads
    }
    pub(super) fn replacement_pads(&self) -> &BTreeMap<String, String> {
        &self.replacement_pads
    }
    pub(super) fn template_evidence(&self) -> &[Value] {
        &self.template_evidence
    }
    pub(super) fn bind_templates(
        mut self,
        original: &SavedSynchronizationBoard,
        intent: &SynchronizationIntent,
        templates: &[templates::SavedFootprintTemplate],
    ) -> Result<Self, String> {
        if templates.is_empty() {
            return Ok(self);
        }
        let parsed = SavedSynchronizationBoard::parse(
            std::str::from_utf8(&self.bytes).map_err(|e| e.to_string())?,
        )?;
        self.board_digest = original.digest().into();
        for template in templates {
            let fp = parsed
                .footprints()
                .get(&template.root_id)
                .ok_or("added source footprint disappeared")?;
            self.differences.push(SynchronizationDifference {
                uuid: template.root_id.clone(),
                field: "footprint".into(),
                before: template
                    .replacement
                    .as_ref()
                    .map(|binding| binding.before_reference.clone()),
                after: Some(fp.reference.clone()),
            });
            self.added_pads
                .extend(fp.pads.iter().map(|pad| pad.uuid.clone()));
            if let Some(binding) = &template.replacement {
                for (old, new) in &binding.pad_mapping {
                    if self
                        .replacement_pads
                        .insert(old.clone(), new.clone())
                        .is_some()
                    {
                        return Err("repeated replacement pad correspondence".into());
                    }
                }
            }
            self.template_evidence.push(template.evidence.clone());
        }
        self.digest=hash(&serde_json::to_vec(&serde_json::json!({"source":self.source_digest,"board":self.board_digest,"intent":intent,"candidate":hash(&self.bytes),"differences":self.differences,"templates":self.template_evidence})).map_err(|e| e.to_string())?);
        Ok(self)
    }
    pub fn has_changes(&self) -> bool {
        self.board_digest != hash(&self.bytes)
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn board_digest(&self) -> &str {
        &self.board_digest
    }
    pub fn differences(&self) -> &[SynchronizationDifference] {
        &self.differences
    }
}

struct Patches<'a> {
    text: &'a str,
    spans: Vec<(Span, String)>,
    append: BTreeMap<usize, String>,
    differences: Vec<SynchronizationDifference>,
}
impl<'a> Patches<'a> {
    fn set(&mut self, owner: &Node, id: &str, name: &str, value: &str) -> Result<(), String> {
        let existing = optional(owner, name)?;
        let old = existing.map(scalar).transpose()?;
        if old == Some(value) || (old.is_none() && value.is_empty()) {
            return Ok(());
        }
        let rendered = format!("({name} {})", quoted(value)?);
        if let Some(node) = existing {
            self.spans.push((span(node), rendered));
        } else {
            self.append
                .entry(span(owner).end - 1)
                .or_default()
                .push_str(&format!("\n\t\t{rendered}"));
        }
        self.differences.push(SynchronizationDifference {
            uuid: id.into(),
            field: name.into(),
            before: old.map(str::to_owned),
            after: Some(value.into()),
        });
        Ok(())
    }
    fn prop(&mut self, owner: &Node, id: &str, name: &str, value: &str) -> Result<(), String> {
        let props = properties(owner)?;
        let field = props
            .get(name)
            .ok_or_else(|| format!("new board field requires explicit geometry: {name}"))?;
        let value_node = items(field)?.get(2).ok_or("missing property value")?;
        let old = atom(value_node)?;
        if old != value {
            self.spans.push((span(value_node), quoted(value)?));
            self.differences.push(SynchronizationDifference {
                uuid: id.into(),
                field: format!("property:{name}"),
                before: Some(old.into()),
                after: Some(value.into()),
            });
        }
        Ok(())
    }
    fn unit_metadata(
        &mut self,
        owner: &Node,
        id: &str,
        units: &[SynchronizationUnit],
    ) -> Result<(), String> {
        let expected = format!(
            "(units {})",
            units
                .iter()
                .map(|u| Ok(format!(
                    "(unit (name {}) (pins {}))",
                    quoted(&u.name)?,
                    u.pins
                        .iter()
                        .map(|p| quoted(p))
                        .collect::<Result<Vec<_>, _>>()?
                        .join(" ")
                )))
                .collect::<Result<Vec<_>, String>>()?
                .join(" ")
        );
        let previous = optional(owner, "units")?;
        let mut actual = Vec::new();
        if let Some(node) = previous {
            for unit in &items(node)?[1..] {
                if tag(unit)? != "unit" || items(unit)?.len() != 3 {
                    return Err("unknown native unit metadata cannot be rewritten".into());
                }
                let name = scalar(required(unit, "name")?)?.into();
                let pins = items(required(unit, "pins")?)?
                    .iter()
                    .skip(1)
                    .map(|n| atom(n).map(str::to_owned))
                    .collect::<Result<Vec<_>, _>>()?;
                actual.push(SynchronizationUnit { name, pins });
            }
        }
        if actual == units {
            return Ok(());
        }
        let before = previous.map(|n| self.text[span(n).start..span(n).end].to_owned());
        if let Some(node) = previous {
            self.spans.push((span(node), expected.clone()));
        } else {
            self.append
                .entry(span(owner).end - 1)
                .or_default()
                .push_str(&format!("\n\t\t{expected}"));
        }
        self.differences.push(SynchronizationDifference {
            uuid: id.into(),
            field: "units".into(),
            before,
            after: Some(expected),
        });
        Ok(())
    }
    fn flags(
        &mut self,
        owner: &Node,
        id: &str,
        component: &SynchronizationComponent,
    ) -> Result<(), String> {
        let previous = optional(owner, "attr")?;
        let mut tokens = previous
            .map(items)
            .transpose()?
            .unwrap_or(&[])
            .iter()
            .skip(1)
            .map(|n| atom(n).map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let original = tokens.clone();
        for (name, enabled) in [
            ("dnp", component.dnp),
            ("exclude_from_bom", !component.in_bom),
        ] {
            let count = tokens.iter().filter(|t| t.as_str() == name).count();
            if count > 1 {
                return Err("duplicate footprint attribute".into());
            }
            if enabled && count == 0 {
                tokens.push(name.into());
            } else if !enabled {
                tokens.retain(|t| t != name);
            }
        }
        if original != tokens {
            let rendered = format!("(attr {})", tokens.join(" "));
            if let Some(node) = previous {
                self.spans.push((span(node), rendered.clone()));
            } else {
                self.append
                    .entry(span(owner).end - 1)
                    .or_default()
                    .push_str(&format!("\n\t\t{rendered}"));
            }
            self.differences.push(SynchronizationDifference {
                uuid: id.into(),
                field: "attr".into(),
                before: previous.map(|n| self.text[span(n).start..span(n).end].into()),
                after: Some(rendered),
            });
        }
        Ok(())
    }
    fn finish(mut self) -> Result<(Vec<u8>, Vec<SynchronizationDifference>), String> {
        for (position, text) in self.append {
            self.spans.push((
                Span {
                    start: position,
                    end: position,
                },
                text,
            ));
        }
        self.spans.sort_by_key(|(s, _)| (s.start, s.end));
        if self
            .spans
            .windows(2)
            .any(|p| p[0].0.end > p[1].0.start || p[0].0.start == p[1].0.start)
        {
            return Err("overlapping synchronization source edits".into());
        }
        let mut result = self.text.to_owned();
        for (s, value) in self.spans.into_iter().rev() {
            result.replace_range(s.start..s.end, &value);
        }
        if result.len() > MAX_BYTES {
            return Err("synchronization candidate exceeds bound".into());
        }
        self.differences
            .sort_by(|a, b| (&a.uuid, &a.field).cmp(&(&b.uuid, &b.field)));
        Ok((result.into_bytes(), self.differences))
    }
}

impl SynchronizationSource {
    /// Compile explicit source-derived changes; native preview, approval and publication remain separate.
    pub fn compile_board(
        &self,
        board: &SavedSynchronizationBoard,
        intent: &SynchronizationIntent,
        connectivity: &ClosedConnectivityEvidence,
    ) -> Result<SynchronizationCandidate, String> {
        self.verify()?;
        if intent.schema_version != 1
            || intent.actions.len() + intent.copper_nets.len() == 0
            || intent.actions.len() > 128
            || intent.copper_nets.len() > 4096
        {
            return Err("invalid synchronization version or action bound".into());
        }
        if connectivity.board_sha256() != board.digest() {
            return Err("physical connectivity belongs to different saved board bytes".into());
        }
        let mut endpoints = BTreeMap::new();
        for fp in board.footprints.values() {
            for pad in &fp.pads {
                endpoints.insert(pad.uuid.clone(), (pad.net.clone(), ConnectivityKind::Pad));
            }
        }
        for (id, (net, _)) in &board.routes {
            let kind = match tag(board.node(id)?)? {
                "segment" => ConnectivityKind::Track,
                "arc" => ConnectivityKind::Arc,
                "via" => ConnectivityKind::ThroughVia,
                "gr_line" | "gr_arc" | "gr_circle" | "gr_rect" | "gr_poly" => {
                    ConnectivityKind::CopperGraphic
                }
                _ => return Err("unsupported copper kind".into()),
            };
            endpoints.insert(id.clone(), (net.clone(), kind));
        }
        if endpoints.len() != connectivity.endpoints().len()
            || connectivity
                .endpoints()
                .iter()
                .any(|e| endpoints.get(e.uuid()) != Some(&(e.net().to_owned(), e.kind())))
        {
            return Err("incomplete or mismatched native board endpoint set".into());
        }
        disposition::require_explicit(board, self.ineligible_units(), &intent.actions)?;
        let mut patches = Patches {
            text: board.text(),
            spans: Vec::new(),
            append: BTreeMap::new(),
            differences: Vec::new(),
        };
        let components = self
            .components
            .iter()
            .map(|c| (c.id.as_str(), c))
            .collect::<BTreeMap<_, _>>();
        let mut owners = BTreeMap::new();
        let mut matched = BTreeMap::new();
        for c in self.components.iter() {
            let targets = board
                .footprints
                .values()
                .filter(|f| {
                    f.path
                        .as_deref()
                        .is_some_and(|p| c.aliases.iter().any(|a| a == p.trim_end_matches('/')))
                })
                .collect::<Vec<_>>();
            if targets.len() > 1 {
                return Err(format!("duplicate_board_component_link: {}", c.id));
            }
            if let Some(fp) = targets.first().filter(|_| c.eligible_for_board()) {
                if owners.insert(fp.uuid.as_str(), c.id.as_str()).is_some() {
                    return Err("overlapping source component aliases".into());
                }
                matched.insert(c.id.as_str(), fp.uuid.as_str());
            }
        }
        let mut selected = BTreeSet::new();
        let mut selected_components = BTreeSet::new();
        let mut expected_nets = endpoints
            .iter()
            .map(|(id, (n, _))| (id.clone(), n.clone()))
            .collect::<BTreeMap<_, _>>();
        for action in &intent.actions {
            match action {
                SynchronizationAction::Keep { footprint_uuid } => {
                    if !board.footprints.contains_key(footprint_uuid)
                        || !selected.insert(footprint_uuid.clone())
                    {
                        return Err("unknown or repeated kept footprint target".into());
                    }
                }
                SynchronizationAction::Add { .. } | SynchronizationAction::Replace { .. } => {
                    return Err("source-bound additions require an owned board observation".into());
                }
                SynchronizationAction::Update {
                    component_id,
                    fields,
                    update_nets,
                }
                | SynchronizationAction::Relink {
                    component_id,
                    fields,
                    update_nets,
                    ..
                } => {
                    let c = *components
                        .get(component_id.as_str())
                        .ok_or("unknown source component identity")?;
                    if !c.eligible_for_board() {
                        return Err(
                            "excluded source component requires explicit removal or detachment"
                                .into(),
                        );
                    }
                    if c.placed_units != (1..=c.units.len() as u32).collect() {
                        return Err(format!("incomplete_component_units: {}", c.id));
                    }
                    let id = match action {
                        SynchronizationAction::Relink { footprint_uuid, .. } => {
                            footprint_uuid.as_str()
                        }
                        _ => *matched
                            .get(c.id.as_str())
                            .ok_or("missing_board_link: explicit relink or addition required")?,
                    };
                    let fp = board
                        .footprints
                        .get(id)
                        .ok_or("unknown footprint relink target")?;
                    if !selected.insert(id.to_owned()) || !selected_components.insert(c.id.as_str())
                    {
                        return Err("repeated synchronization component or footprint".into());
                    }
                    if fp.locked || fp.board_only {
                        return Err(
                            "locked or board-only footprint requires a separate explicit policy"
                                .into(),
                        );
                    }
                    if owners.get(id).is_some_and(|owner| *owner != c.id) {
                        return Err("relink target belongs to another source component".into());
                    }
                    if matched.get(c.id.as_str()).is_some_and(|old| *old != id) {
                        return Err("component already has a different board link".into());
                    }
                    let assignment = c
                        .footprint
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .ok_or("missing_source_footprint_assignment")?;
                    if assignment != fp.library {
                        return Err(format!("footprint_replacement_required: {}", fp.uuid));
                    }
                    let numbers = fp
                        .pads
                        .iter()
                        .filter(|p| !p.number.is_empty())
                        .map(|p| p.number.clone())
                        .collect::<BTreeSet<_>>();
                    if numbers != c.pins.keys().cloned().collect() {
                        return Err(format!("complete_pad_set_mismatch: {}", fp.uuid));
                    }
                    let node = board.node(id)?;
                    patches.prop(node, id, "Reference", &c.reference)?;
                    patches.prop(node, id, "Value", &c.value)?;
                    let mut names = BTreeSet::new();
                    for name in fields {
                        if !names.insert(name)
                            || ["Reference", "Value", "Footprint"].contains(&name.as_str())
                            || c.conflicting_fields.contains(name)
                        {
                            return Err(
                                "duplicate, protected or conflicting source field selection".into(),
                            );
                        }
                        patches.prop(
                            node,
                            id,
                            name,
                            c.fields
                                .get(name)
                                .ok_or("selected source field is absent")?,
                        )?;
                    }
                    if matches!(action, SynchronizationAction::Relink { .. }) {
                        patches.set(node, id, "path", &c.canonical_path)?;
                    }
                    patches.set(node, id, "sheetname", &c.sheet_name)?;
                    patches.set(node, id, "sheetfile", &c.sheet_file)?;
                    patches.unit_metadata(node, id, &c.units)?;
                    patches.flags(node, id, c)?;
                    for pad in &fp.pads {
                        if pad.number.is_empty() {
                            continue;
                        }
                        let binding = &c.pins[&pad.number];
                        if (binding.electrical_no_connect || binding.explicit_no_connect)
                            && fp.pads.iter().filter(|p| p.number == pad.number).count() > 1
                        {
                            return Err("repeated physical NC pads require separate native net-name qualification".into());
                        }
                        let pad_node = children(node, "pad")?
                            .into_iter()
                            .find(|p| identity(p).is_ok_and(|i| i == pad.uuid))
                            .ok_or("missing pad node")?;
                        patches.set(pad_node, &pad.uuid, "pinfunction", &binding.pin_function)?;
                        patches.set(pad_node, &pad.uuid, "pintype", &binding.pin_type)?;
                        let desired = binding.name.as_str();
                        if pad.net != desired {
                            if !update_nets {
                                return Err(format!(
                                    "explicit_pad_net_update_required: {}",
                                    pad.uuid
                                ));
                            }
                            if board.has_zones {
                                return Err(
                                    "affected zoned board requires complete refill qualification"
                                        .into(),
                                );
                            }
                            let pad_node = children(node, "pad")?
                                .into_iter()
                                .find(|p| identity(p).is_ok_and(|i| i == pad.uuid))
                                .ok_or("missing pad node")?;
                            patches.set(pad_node, &pad.uuid, "net", desired)?;
                            expected_nets.insert(pad.uuid.clone(), desired.into());
                        }
                    }
                    matched.insert(c.id.as_str(), id);
                    owners.insert(id, c.id.as_str());
                }
                SynchronizationAction::Remove { footprint_uuid }
                | SynchronizationAction::Detach { footprint_uuid } => {
                    let fp = board
                        .footprints
                        .get(footprint_uuid)
                        .ok_or("unknown footprint removal target")?;
                    if !selected.insert(fp.uuid.clone()) || fp.locked {
                        return Err("repeated or locked footprint removal target".into());
                    }
                    if owners.contains_key(fp.uuid.as_str()) {
                        return Err("board-eligible source component cannot be implicitly removed or detached".into());
                    }
                    if matches!(action, SynchronizationAction::Detach { .. }) {
                        patches.set(board.node(&fp.uuid)?, &fp.uuid, "path", "")?;
                        continue;
                    }
                    if fp.board_only
                        || board.has_zones
                        || board.has_external_references(&fp.uuid)?
                    {
                        return Err("board-only, zoned or grouped footprint removal requires separate qualification".into());
                    }
                    for pad in &fp.pads {
                        if connectivity
                            .component(&pad.uuid)
                            .is_none_or(|group| group.len() != 1)
                        {
                            return Err(format!(
                                "connected_footprint_removal_requires_separate_edit: {}",
                                pad.uuid
                            ));
                        }
                        expected_nets.remove(&pad.uuid);
                    }
                    patches
                        .spans
                        .push((span(board.node(&fp.uuid)?), String::new()));
                    patches.differences.push(SynchronizationDifference {
                        uuid: fp.uuid.clone(),
                        field: "footprint".into(),
                        before: Some(fp.reference.clone()),
                        after: None,
                    });
                }
            }
        }
        let desired_names = self
            .components
            .iter()
            .flat_map(|c| c.pins.values())
            .map(|n| n.name.as_str())
            .collect::<BTreeSet<_>>();
        let mut selected_copper = BTreeSet::new();
        for change in &intent.copper_nets {
            let (old, locked) = board
                .routes
                .get(&change.uuid)
                .ok_or("unknown copper net-change target")?;
            if !selected_copper.insert(&change.uuid)
                || *locked
                || old == &change.net
                || !desired_names.contains(change.net.as_str())
            {
                return Err("repeated, locked, redundant or foreign copper net change".into());
            }
            if board.has_zones {
                return Err(
                    "zoned copper net changes require complete refill qualification".into(),
                );
            }
            patches.set(board.node(&change.uuid)?, &change.uuid, "net", &change.net)?;
            expected_nets.insert(change.uuid.clone(), change.net.clone());
        }
        for group in connectivity.components() {
            let desired = group
                .iter()
                .filter_map(|id| expected_nets.get(id))
                .collect::<BTreeSet<_>>();
            if desired.len() > 1 {
                return Err(format!(
                    "connected_copper_has_conflicting_desired_nets: {}",
                    group.join(",")
                ));
            }
        }
        let (bytes, differences) = patches.finish()?;
        let candidate = SavedSynchronizationBoard::parse(
            std::str::from_utf8(&bytes).map_err(|e| e.to_string())?,
        )?;
        for component in self.components.iter().filter(|c| c.eligible_for_board()) {
            let id = matched
                .get(component.id.as_str())
                .ok_or_else(|| format!("missing_board_component: {}", component.id))?;
            let fp = candidate
                .footprints
                .get(*id)
                .ok_or("source component footprint disappeared")?;
            if candidate
                .footprints
                .values()
                .filter(|other| other.reference == component.reference)
                .count()
                != 1
            {
                return Err(format!(
                    "same_reference_board_collision: {}",
                    component.reference
                ));
            }
            if fp.reference != component.reference
                || fp.value != component.value
                || Some(fp.library.as_str()) != component.footprint.as_deref()
            {
                return Err(format!(
                    "remaining_component_metadata_mismatch: {}",
                    component.id
                ));
            }
            for pad in &fp.pads {
                if !pad.number.is_empty() {
                    let expected = component
                        .pins
                        .get(&pad.number)
                        .ok_or("unmapped final board pad")?;
                    if pad.net != expected.name
                        || pad.pin_function != expected.pin_function
                        || pad.pin_type != expected.pin_type
                    {
                        return Err(format!(
                            "remaining_component_pin_metadata_mismatch: {}",
                            pad.uuid
                        ));
                    }
                }
            }
            if fp
                .pads
                .iter()
                .filter(|p| !p.number.is_empty())
                .map(|p| p.number.clone())
                .collect::<BTreeSet<_>>()
                != component.pins.keys().cloned().collect()
            {
                return Err("incomplete final component pad coverage".into());
            }
            let node = candidate.node(id)?;
            if read_units(node)? != component.units {
                return Err(format!(
                    "remaining_component_unit_metadata_mismatch: {}",
                    component.id
                ));
            }
            let attr = optional(node, "attr")?
                .map(items)
                .transpose()?
                .unwrap_or(&[])
                .iter()
                .skip(1)
                .map(atom)
                .collect::<Result<BTreeSet<_>, _>>()?;
            if attr.contains("dnp") != component.dnp
                || attr.contains("exclude_from_bom") == component.in_bom
            {
                return Err(format!(
                    "remaining_component_flag_mismatch: {}",
                    component.id
                ));
            }
            if optional(node, "sheetname")?
                .map(scalar)
                .transpose()?
                .unwrap_or("")
                != component.sheet_name
                || optional(node, "sheetfile")?
                    .map(scalar)
                    .transpose()?
                    .unwrap_or("")
                    != component.sheet_file
            {
                return Err(format!(
                    "remaining_component_sheet_metadata_mismatch: {}",
                    component.id
                ));
            }
        }
        for action in &intent.actions {
            if let SynchronizationAction::Update {
                component_id,
                fields,
                ..
            }
            | SynchronizationAction::Relink {
                component_id,
                fields,
                ..
            } = action
            {
                let component = components
                    .get(component_id.as_str())
                    .ok_or("missing selected component")?;
                let id = matched
                    .get(component_id.as_str())
                    .ok_or("missing final selected footprint")?;
                for name in fields {
                    if property(candidate.node(id)?, name)? != component.fields[name] {
                        return Err(
                            "selected source property differs from compiled candidate".into()
                        );
                    }
                }
            }
        }
        self.verify()?;
        let digest=hash(&serde_json::to_vec(&serde_json::json!({"source":self.digest,"board":board.digest(),"intent":intent,"candidate":hash(&bytes),"differences":differences})).map_err(|e|e.to_string())?);
        Ok(SynchronizationCandidate {
            source_digest: self.digest.clone(),
            board_digest: board.digest().into(),
            added_pads: BTreeSet::new(),
            replacement_pads: BTreeMap::new(),
            template_evidence: Vec::new(),
            bytes,
            differences,
            digest,
        })
    }
}

fn read_units(owner: &Node) -> Result<Vec<SynchronizationUnit>, String> {
    let mut result = Vec::new();
    if let Some(node) = optional(owner, "units")? {
        for unit in &items(node)?[1..] {
            if tag(unit)? != "unit" || items(unit)?.len() != 3 {
                return Err("unknown native unit metadata".into());
            }
            result.push(SynchronizationUnit {
                name: scalar(required(unit, "name")?)?.into(),
                pins: items(required(unit, "pins")?)?
                    .iter()
                    .skip(1)
                    .map(|p| atom(p).map(str::to_owned))
                    .collect::<Result<_, _>>()?,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const FP: &str = "11111111-1111-4111-8111-111111111111";
    fn fixture() -> String {
        format!(
            r#"(kicad_pcb (version 20260206) (unknown "retained")
            (footprint "Test:Pair" (uuid "{FP}") (at 10 20 30) (attr smd)
                (property "Reference" "R1" (at 0 1) (unknown_style "retained"))
                (property "Value" "10k")
                (pad "1" smd rect (uuid "22222222-2222-4222-8222-222222222222")
                    (at 0 0) (net "GND"))))"#
        )
    }

    #[test]
    fn dimension_owned_text_aliases_do_not_relax_global_identity_or_preservation() {
        let id = "77777777-7777-4777-8777-777777777777";
        let dimension = format!(
            "(dimension (type leader) (uuid \"{id}\") (pts (xy 0 0) (xy 1 1)) (gr_text \"note\" (uuid \"{id}\") (at 2 2)))"
        );
        let mut raw = fixture();
        raw.insert_str(raw.len() - 1, &dimension);
        let board = SavedSynchronizationBoard::parse(&raw).unwrap();
        assert!(board.identities().contains(id));
        let changed = raw.replace("note", "changed");
        assert_ne!(
            board.protected_native_digest(&[]).unwrap(),
            SavedSynchronizationBoard::parse(&changed)
                .unwrap()
                .protected_native_digest(&[])
                .unwrap()
        );
        for extra in [
            format!("(gr_text \"other\" (uuid \"{id}\"))"),
            dimension.clone(),
            format!("(future (uuid \"{id}\"))"),
        ] {
            let mut corrupt = raw.clone();
            corrupt.insert_str(corrupt.len() - 1, &extra);
            assert!(SavedSynchronizationBoard::parse(&corrupt).is_err());
        }
        assert!(
            SavedSynchronizationBoard::parse(&raw.replace("(type leader)", "(type future)"))
                .is_err()
        );
        assert!(
            SavedSynchronizationBoard::parse(&raw.replace(
                "(gr_text \"note\"",
                &format!("(gr_text \"note\" (future (uuid \"{FP}\"))")
            ))
            .is_err()
        );
    }

    #[test]
    fn copper_graphic_net_masks_never_authorize_geometry_or_non_copper_changes() {
        let id = "88888888-8888-4888-8888-888888888888";
        let graphic = format!(
            "(gr_poly (uuid \"{id}\") (layer \"F.Cu\") (net \"GND\") (pts (xy 0 0) (xy 2 0) (xy 1 2)))"
        );
        let mut raw = fixture();
        raw.insert_str(raw.len() - 1, &graphic);
        let board = SavedSynchronizationBoard::parse(&raw).unwrap();
        assert_eq!(board.copper().len(), 1);
        let after = raw.replace(&graphic, &graphic.replace("GND", "VCC"));
        let changes = vec![SynchronizationDifference {
            uuid: id.into(),
            field: "net".into(),
            before: Some("GND".into()),
            after: Some("VCC".into()),
        }];
        let expected = board.protected_native_digest(&changes).unwrap();
        assert_eq!(
            expected,
            SavedSynchronizationBoard::parse(&after)
                .unwrap()
                .protected_native_digest(&changes)
                .unwrap()
        );
        assert_ne!(
            expected,
            SavedSynchronizationBoard::parse(&after.replace("(xy 1 2)", "(xy 1 3)"))
                .unwrap()
                .protected_native_digest(&changes)
                .unwrap()
        );
        assert_ne!(
            expected,
            SavedSynchronizationBoard::parse(&after.replace("F.Cu", "B.Cu"))
                .unwrap()
                .protected_native_digest(&changes)
                .unwrap()
        );
        assert!(
            SavedSynchronizationBoard::parse(&raw.replace("F.Cu", "Dwgs.User"))
                .unwrap()
                .copper()
                .is_empty()
        );
    }

    #[test]
    fn exact_spans_and_native_masks_preserve_all_unselected_content() {
        for value in [
            "quote\" slash\\ newline\n tab\t",
            "中文\u{7}\u{8}\u{b}\u{c}\u{1f}\u{7f}",
        ] {
            let raw = format!("(value {})", quoted(value).unwrap());
            let parsed = kicad_schematic::cst::parse_one(&raw).unwrap();
            assert_eq!(scalar(&parsed.nodes[0]).unwrap(), value);
        }
        let original = fixture();
        let before = SavedSynchronizationBoard::parse(&original).unwrap();
        let mut patches = Patches {
            text: before.text(),
            spans: vec![],
            append: BTreeMap::new(),
            differences: vec![],
        };
        patches
            .prop(before.node(FP).unwrap(), FP, "Reference", "中文\\\"\nR9")
            .unwrap();
        let component = SynchronizationComponent {
            id: "source".into(),
            reference: "R9".into(),
            value: "10k".into(),
            footprint: Some("Test:Pair".into()),
            aliases: vec![],
            canonical_path: String::new(),
            sheet_name: String::new(),
            sheet_file: String::new(),
            units: vec![],
            placed_units: BTreeSet::new(),
            pins: BTreeMap::new(),
            in_bom: true,
            on_board: true,
            dnp: true,
            fields: BTreeMap::new(),
            conflicting_fields: BTreeSet::new(),
        };
        patches
            .flags(before.node(FP).unwrap(), FP, &component)
            .unwrap();
        let (bytes, changes) = patches.finish().unwrap();
        let candidate = String::from_utf8(bytes).unwrap();
        assert!(candidate.contains("(unknown_style \"retained\")"));
        assert!(candidate.contains("(unknown \"retained\")"));
        let after = SavedSynchronizationBoard::parse(&candidate).unwrap();
        let protected = before.protected_native_digest(&changes).unwrap();
        assert_eq!(protected, after.protected_native_digest(&changes).unwrap());
        for changed in [
            candidate.replace("(at 10 20 30)", "(at 11 20 30)"),
            candidate.replace("unknown_style \"retained\"", "unknown_style \"lost\""),
            candidate.replace("(attr smd dnp)", "(attr smd dnp board_only)"),
            candidate.replace("(net \"GND\")", "(net \"VCC\")"),
            candidate.replace("(unknown \"retained\")", "(unknown \"changed\")"),
        ] {
            assert_ne!(
                protected,
                SavedSynchronizationBoard::parse(&changed)
                    .unwrap()
                    .protected_native_digest(&changes)
                    .unwrap()
            );
        }
    }
    #[test]
    fn footprint_removal_protects_every_nested_external_identity_reference() {
        let original = fixture();
        let board = SavedSynchronizationBoard::parse(&original).unwrap();
        assert!(!board.has_external_references(FP).unwrap());
        let child = "22222222-2222-4222-8222-222222222222";
        for target in [FP, child] {
            for form in [
                format!("(group (members \"{target}\"))"),
                format!("(future_form (path \"prefix/{target}/suffix\"))"),
                format!("(dimension (reference \"item:{target}\"))"),
            ] {
                let mut text = original.clone();
                text.insert_str(text.len() - 1, &form);
                let board = SavedSynchronizationBoard::parse(&text).unwrap();
                assert!(board.has_external_references(FP).unwrap(), "{form}");
            }
        }
        let internal = original.replace(
            "(at 0 0)",
            &format!("(at 0 0) (future_reference \"{child}\")"),
        );
        assert!(
            !SavedSynchronizationBoard::parse(&internal)
                .unwrap()
                .has_external_references(FP)
                .unwrap()
        );
    }
    #[test]
    fn candidate_identity_and_patch_authority_are_unambiguous() {
        let raw = fixture();
        assert!(
            SavedSynchronizationBoard::parse(
                &raw.replace("22222222-2222-4222-8222-222222222222", FP)
            )
            .is_err()
        );
        assert!(
            SavedSynchronizationBoard::parse(
                &raw.replace("(version 20260206)", "(version 99999999)")
            )
            .is_err()
        );
        let board = SavedSynchronizationBoard::parse(&raw).unwrap();
        let mut patches = Patches {
            text: board.text(),
            spans: vec![],
            append: BTreeMap::new(),
            differences: vec![],
        };
        patches
            .prop(board.node(FP).unwrap(), FP, "Value", "one")
            .unwrap();
        patches
            .prop(board.node(FP).unwrap(), FP, "Value", "two")
            .unwrap();
        assert!(
            patches.finish().is_err(),
            "one source span cannot receive conflicting writes"
        );
    }
}
