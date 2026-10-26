//! Complete live board text and independently observed zone locks.
mod filled_copper;
pub use filled_copper::FilledCopperEvidence;
use kicad_ipc::{
    DocumentQuery, KiCadClient, KiCadTransport, MAX_NATIVE_GEOMETRY_BYTES, NativeBoardGeometry,
    base_types::LockedState,
};
use kiutils_sexpr::{Atom, CstDocument, Node, Span, parse_one};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 3_000_000;
const MAX_ZONES: usize = 256;

/// Exact native-written zone evidence. Missing `locked` means false only in the supported format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneTextEvidence {
    pub locked: bool,
    pub structure_digest: String,
    span: Span,
}

/// The complete CST and source remain available; no unknown or derived field is discarded.
#[derive(Clone, Debug)]
pub struct LiveBoardText {
    cst: CstDocument,
    zones: BTreeMap<String, ZoneTextEvidence>,
    structure_digest: String,
}

fn items(node: &Node) -> Result<&[Node], String> {
    match node {
        Node::List { items, .. } => Ok(items),
        _ => Err("expected native board form".into()),
    }
}
fn atom(node: &Node) -> Result<&str, String> {
    match node {
        Node::Atom {
            atom: Atom::Symbol(value) | Atom::Quoted(value),
            ..
        } => Ok(value),
        _ => Err("expected native board scalar".into()),
    }
}
fn tag(node: &Node) -> Result<&str, String> {
    atom(items(node)?.first().ok_or("empty native board form")?)
}
fn scalar(node: &Node) -> Result<&str, String> {
    let fields = items(node)?;
    if fields.len() != 2 {
        return Err("native board field must have one value".into());
    }
    atom(&fields[1])
}

// Bound allocation and recursion before invoking the CST parser. Match its token boundaries,
// including quotes embedded in unquoted symbols, rather than counting whitespace as nodes.
fn preflight(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_NATIVE_GEOMETRY_BYTES || text.contains('\0') {
        return Err("native board text is empty, contains NUL or exceeds byte bound".into());
    }
    let bytes = text.as_bytes();
    let (mut pos, mut depth, mut nodes) = (0, 0usize, 0);
    while pos < bytes.len() {
        match bytes[pos] {
            b' ' | b'\t' | b'\n' | b'\r' => {
                pos += 1;
                continue;
            }
            b')' => {
                depth = depth.checked_sub(1).ok_or("unbalanced native board form")?;
                pos += 1;
                continue;
            }
            b'(' => {
                depth += 1;
                pos += 1;
                if depth > MAX_DEPTH {
                    return Err("native board nesting exceeds bound".into());
                }
            }
            b'"' => {
                pos += 1;
                let mut closed = false;
                while pos < bytes.len() {
                    match bytes[pos] {
                        b'\\' => pos += 2,
                        b'"' => {
                            pos += 1;
                            closed = true;
                            break;
                        }
                        _ => pos += 1,
                    }
                }
                if !closed {
                    return Err("unterminated native board string".into());
                }
            }
            _ => {
                while pos < bytes.len()
                    && !matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r' | b'(' | b')')
                {
                    pos += 1;
                }
            }
        }
        nodes += 1;
        if nodes > MAX_NODES {
            return Err("native board node count exceeds bound".into());
        }
    }
    if depth != 0 {
        return Err("unterminated native board form".into());
    }
    Ok(())
}

// Hash original lexical atoms, including quotes and escapes. Only inter-token whitespace is
// ignored; numeric spelling, list order, unknown fields and every filled polygon remain exact.
fn feed_structure(hash: &mut Sha256, text: &str, node: &Node) {
    match node {
        Node::List { items, .. } => {
            hash.update([0]);
            hash.update((items.len() as u64).to_be_bytes());
            for child in items {
                feed_structure(hash, text, child);
            }
        }
        Node::Atom { span, .. } => {
            hash.update([1]);
            hash.update(((span.end - span.start) as u64).to_be_bytes());
            hash.update(&text.as_bytes()[span.start..span.end]);
        }
    }
}
fn structural_digest(text: &str, node: &Node) -> String {
    let mut hash = Sha256::new();
    feed_structure(&mut hash, text, node);
    hex::encode(hash.finalize())
}

// Only the exact native derived grammar may disappear from the definition fingerprint.
fn filled_polygon(node: &Node, points: &mut usize) -> Result<(), String> {
    let (mut layer, mut island, mut pts) = (false, false, false);
    for field in &items(node)?[1..] {
        match tag(field)? {
            "layer" if !layer => {
                scalar(field)?;
                layer = true;
            }
            "island" if !island && scalar(field)? == "yes" => {
                island = true;
            }
            "pts" if !pts => {
                pts = true;
                for point in &items(field)?[1..] {
                    let xy = |node: &Node| -> Result<(), String> {
                        let values = items(node)?;
                        if values.len() != 3 {
                            return Err("invalid derived fill point".into());
                        }
                        atom(&values[1])?;
                        atom(&values[2])?;
                        Ok(())
                    };
                    match tag(point)? {
                        "xy" => {
                            xy(point)?;
                            *points += 1;
                        }
                        "arc" => {
                            let fields = items(point)?;
                            if fields.len() != 4 {
                                return Err("invalid derived fill arc".into());
                            }
                            for (field, expected) in fields[1..].iter().zip(["start", "mid", "end"])
                            {
                                if tag(field)? != expected {
                                    return Err("invalid derived fill arc control".into());
                                }
                                xy(field)?;
                            }
                            *points += 3;
                        }
                        _ => return Err("unknown derived fill point field".into()),
                    }
                    if *points > 500_000 {
                        return Err("derived fill points exceed bound".into());
                    }
                }
            }
            _ => return Err("unknown or duplicate derived fill field".into()),
        }
    }
    if !layer || !pts {
        return Err("incomplete derived fill polygon".into());
    }
    Ok(())
}

// KiCad 10.0.7 can materialize an empty defaults map while filling hatched zones.
// Only this direct empty container has no settings; every nonempty form remains exact.
fn feed_refill_setup(hash: &mut Sha256, text: &str, setup: &Node) -> Result<(), String> {
    let fields = items(setup)?;
    let mut kept = Vec::new();
    let mut seen = false;
    for field in &fields[1..] {
        if tag(field)? == "zone_defaults" {
            if seen {
                return Err("duplicate setup zone defaults".into());
            }
            seen = true;
            if items(field)?.len() == 1 {
                continue;
            }
        }
        kept.push(field);
    }
    hash.update([0]);
    hash.update(((kept.len() + 1) as u64).to_be_bytes());
    feed_structure(hash, text, &fields[0]);
    for field in kept {
        feed_structure(hash, text, field);
    }
    Ok(())
}

fn require_unique_setup(fields: &[Node]) -> Result<(), String> {
    if fields
        .iter()
        .filter(|n| tag(n).ok() == Some("setup"))
        .count()
        > 1
    {
        return Err("duplicate board setup".into());
    }
    Ok(())
}

fn feed_zone_definition(
    hash: &mut Sha256,
    text: &str,
    zone: &Node,
    points: &mut usize,
) -> Result<(), String> {
    let mut kept = Vec::new();
    let mut fill_seen = false;
    for field in &items(zone)?[1..] {
        match tag(field)? {
            "filled_polygon" => filled_polygon(field, points)?,
            "fill" => {
                if fill_seen {
                    return Err("duplicate zone fill definition".into());
                }
                fill_seen = true;
                let fields = items(field)?;
                for (index, item) in fields[1..].iter().enumerate() {
                    if matches!(item, Node::Atom { .. }) && (index != 0 || atom(item)? != "yes") {
                        return Err("ambiguous zone filled marker".into());
                    }
                }
                kept.push(field);
            }
            _ => kept.push(field),
        }
    }
    hash.update([0]);
    hash.update(((kept.len() + 1) as u64).to_be_bytes());
    feed_structure(hash, text, &items(zone)?[0]);
    for field in kept {
        if tag(field)? != "fill" {
            feed_structure(hash, text, field);
            continue;
        }
        let fields = items(field)?;
        let filled = fields
            .get(1)
            .is_some_and(|node| matches!(node, Node::Atom { .. }));
        hash.update([0]);
        hash.update(((fields.len() - usize::from(filled)) as u64).to_be_bytes());
        feed_structure(hash, text, &fields[0]);
        for item in &fields[1 + usize::from(filled)..] {
            feed_structure(hash, text, item);
        }
    }
    Ok(())
}

fn feed_patched_zone(
    hash: &mut Sha256,
    text: &str,
    zone: &Node,
    patch: &kicad_ipc::SolidZonePatch,
) -> Result<(), String> {
    fn feed_fields(
        hash: &mut Sha256,
        text: &str,
        node: &Node,
        skip: &[&str],
        skip_bare: bool,
        fill: bool,
    ) -> Result<(), String> {
        let fields = items(node)?;
        let mut kept = vec![&fields[0]];
        let mut seen = std::collections::BTreeSet::new();
        for (index, field) in fields[1..].iter().enumerate() {
            if matches!(field, Node::Atom { .. }) {
                if index != 0 || (fill && atom(field)? != "yes") {
                    return Err("ambiguous zone scalar".into());
                }
                if skip_bare {
                    continue;
                }
            } else if skip.contains(&tag(field)?) {
                if !seen.insert(tag(field)?) {
                    return Err("duplicate edited zone field".into());
                }
                continue;
            }
            kept.push(field);
        }
        hash.update([0]);
        hash.update((kept.len() as u64).to_be_bytes());
        for field in kept {
            feed_structure(hash, text, field);
        }
        Ok(())
    }
    let mut direct = Vec::new();
    if patch.name.is_some() {
        direct.push("name");
    }
    if patch.net.is_some() {
        direct.push("net");
    }
    if patch.layers.is_some() {
        direct.extend(["layer", "layers"]);
    }
    if patch.polygon.is_some() {
        direct.push("polygon");
    }
    if patch.priority.is_some() {
        direct.push("priority");
    }
    if patch.min_thickness_nm.is_some() {
        direct.push("min_thickness");
    }
    let mut fill_fields = Vec::new();
    if patch.thermal_gap_nm.is_some() {
        fill_fields.push("thermal_gap");
    }
    if patch.thermal_spoke_width_nm.is_some() {
        fill_fields.push("thermal_bridge_width");
    }
    if patch.island_removal.is_some() {
        fill_fields.push("island_removal_mode");
    }
    if patch.min_island_area_nm2.is_some() {
        fill_fields.push("island_area_min");
    }
    let fields = items(zone)?;
    let mut kept = vec![&fields[0]];
    let mut points = 0;
    let mut seen = std::collections::BTreeSet::new();
    for field in &fields[1..] {
        let name = tag(field)?;
        if name == "filled_polygon" {
            filled_polygon(field, &mut points)?;
            continue;
        }
        if name != "polygon"
            && !seen.insert(name)
            && (direct.contains(&name) || ["fill", "connect_pads"].contains(&name))
        {
            return Err("duplicate edited zone definition".into());
        }
        if !direct.contains(&name) {
            kept.push(field);
        }
    }
    hash.update([0]);
    hash.update((kept.len() as u64).to_be_bytes());
    for field in kept {
        if matches!(field, Node::Atom { .. }) {
            feed_structure(hash, text, field);
            continue;
        }
        match tag(field)? {
            "fill" => feed_fields(hash, text, field, &fill_fields, true, true)?,
            "connect_pads" => feed_fields(
                hash,
                text,
                field,
                if patch.clearance_nm.is_some() {
                    &["clearance"]
                } else {
                    &[]
                },
                patch.connection.is_some(),
                false,
            )?,
            _ => feed_structure(hash, text, field),
        }
    }
    Ok(())
}

// Closed-file comparison has a separate type and never weakens source live equality.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClosedRefillDefinitionEvidence {
    definition_digest: String,
    automatic_dimension_ids: Vec<String>,
}

fn unique_form<'a>(node: &'a Node, name: &str) -> Result<Option<&'a Node>, String> {
    let mut result = None;
    for field in &items(node)?[1..] {
        if tag(field)? == name && result.replace(field).is_some() {
            return Err(format!("duplicate dimension {name} field"));
        }
    }
    Ok(result)
}
fn feed_closed_dimension(
    hash: &mut Sha256,
    text: &str,
    node: &Node,
    ids: &mut Vec<String>,
) -> Result<(), String> {
    let kind = unique_form(node, "type")?.map(scalar).transpose()?;
    let format = unique_form(node, "format")?;
    let automatic = if let Some(format) = format {
        unique_form(format, "units")?.map(scalar).transpose()? == Some("3")
            && unique_form(format, "override_value")?.is_none()
    } else {
        false
    };
    if !automatic || !matches!(kind, Some("aligned" | "orthogonal" | "radial")) {
        feed_structure(hash, text, node);
        return Ok(());
    }
    let rendered =
        unique_form(node, "gr_text")?.ok_or("automatic dimension lacks rendered text")?;
    let rendered_fields = items(rendered)?;
    atom(
        rendered_fields
            .get(1)
            .ok_or("automatic dimension lacks rendered value")?,
    )?;
    let id = scalar(unique_form(node, "uuid")?.ok_or("automatic dimension lacks identity")?)?;
    if id.is_empty() || id.len() > 128 || ids.len() >= 4096 || ids.iter().any(|s| s == id) {
        return Err("automatic dimension identities must be bounded and unique".into());
    }
    ids.push(id.into());
    let fields = items(node)?;
    hash.update([0]);
    hash.update((fields.len() as u64).to_be_bytes());
    for field in fields {
        if std::ptr::eq(field, rendered) {
            hash.update([0]);
            hash.update((rendered_fields.len() as u64).to_be_bytes());
            feed_structure(hash, text, &rendered_fields[0]);
            hash.update([2]); // Typed marker for this derived display value, never authored text.
            for child in &rendered_fields[2..] {
                feed_structure(hash, text, child);
            }
        } else {
            feed_structure(hash, text, field);
        }
    }
    Ok(())
}
fn feed_closed_footprint(
    hash: &mut Sha256,
    text: &str,
    node: &Node,
    ids: &mut Vec<String>,
) -> Result<(), String> {
    let fields = items(node)?;
    if fields.len() < 2 {
        return Err("invalid native footprint".into());
    }
    atom(&fields[1])?;
    hash.update([0]);
    hash.update((fields.len() as u64).to_be_bytes());
    for field in &fields[..2] {
        feed_structure(hash, text, field);
    }
    for field in &fields[2..] {
        if tag(field)? == "dimension" {
            feed_closed_dimension(hash, text, field, ids)?;
        } else {
            feed_structure(hash, text, field);
        }
    }
    Ok(())
}

impl LiveBoardText {
    /// Parse KiCad's live 20260206 board serialization, not an arbitrary source-file lock guess.
    pub fn parse(text: &str) -> Result<Self, String> {
        preflight(text)?;
        let cst = parse_one(text).map_err(|e| format!("invalid live board syntax: {e}"))?;
        let root = cst.nodes.first().ok_or("missing native board root")?;
        if tag(root)? != "kicad_pcb" {
            return Err("expected kicad_pcb root".into());
        }
        let mut version = None;
        let mut zones = BTreeMap::new();
        for node in &items(root)?[1..] {
            match tag(node)? {
                "version" => {
                    if version.replace(scalar(node)?).is_some() {
                        return Err("duplicate native board version".into());
                    }
                }
                "zone" => {
                    let (mut id, mut locked) = (None, None);
                    for field in &items(node)?[1..] {
                        match tag(field)? {
                            "uuid" => {
                                let value = scalar(field)?;
                                if value.is_empty()
                                    || value.len() > 128
                                    || id.replace(value).is_some()
                                {
                                    return Err(
                                        "missing, oversized or duplicate zone identity".into()
                                    );
                                }
                            }
                            "locked" => {
                                let value = match scalar(field)? {
                                    "yes" => true,
                                    "no" => false,
                                    _ => return Err("unknown native zone lock value".into()),
                                };
                                if locked.replace(value).is_some() {
                                    return Err("duplicate native zone lock field".into());
                                }
                            }
                            _ => {}
                        }
                    }
                    let id = id.ok_or("zone identity is missing")?.to_owned();
                    let Node::List { span, .. } = node else {
                        unreachable!()
                    };
                    let zone = ZoneTextEvidence {
                        locked: locked.unwrap_or(false),
                        structure_digest: structural_digest(text, node),
                        span: *span,
                    };
                    if zones.insert(id, zone).is_some() || zones.len() > MAX_ZONES {
                        return Err("duplicate zone identity or zone count exceeds bound".into());
                    }
                }
                _ => {}
            }
        }
        if version != Some("20260206") {
            return Err("unverified live board format version".into());
        }
        let structure_digest = structural_digest(text, root);
        Ok(Self {
            cst,
            zones,
            structure_digest,
        })
    }

    /// Hash board definitions excluding direct zone fill results and an empty setup zone-defaults container.
    /// This preserves all unknown definition fields and does not verify the omitted copper.
    pub fn refill_definition_digest(&self) -> Result<String, String> {
        let root = self.cst.nodes.first().ok_or("missing native board root")?;
        let fields = items(root)?;
        require_unique_setup(&fields[1..])?;
        let mut hash = Sha256::new();
        hash.update([0]);
        hash.update((fields.len() as u64).to_be_bytes());
        feed_structure(&mut hash, self.raw_text(), &fields[0]);
        let mut points = 0;
        for node in &fields[1..] {
            if tag(node)? == "zone" {
                feed_zone_definition(&mut hash, self.raw_text(), node, &mut points)?;
            } else if tag(node)? == "setup" {
                feed_refill_setup(&mut hash, self.raw_text(), node)?;
            } else {
                feed_structure(&mut hash, self.raw_text(), node);
            }
        }
        Ok(hex::encode(hash.finalize()))
    }

    /// Compare closed CLI input/output definitions, separately from strict live preservation.
    /// Only generated automatic dimension labels additionally normalize; authored fields remain exact.
    pub fn closed_refill_definitions(&self) -> Result<ClosedRefillDefinitionEvidence, String> {
        let root = self.cst.nodes.first().ok_or("missing native board root")?;
        let fields = items(root)?;
        require_unique_setup(&fields[1..])?;
        let mut hash = Sha256::new();
        let mut ids = Vec::new();
        let mut points = 0;
        hash.update([0]);
        hash.update((fields.len() as u64).to_be_bytes());
        feed_structure(&mut hash, self.raw_text(), &fields[0]);
        for node in &fields[1..] {
            match tag(node)? {
                "setup" => feed_refill_setup(&mut hash, self.raw_text(), node)?,
                "zone" => feed_zone_definition(&mut hash, self.raw_text(), node, &mut points)?,
                "dimension" => feed_closed_dimension(&mut hash, self.raw_text(), node, &mut ids)?,
                "footprint" => feed_closed_footprint(&mut hash, self.raw_text(), node, &mut ids)?,
                _ => feed_structure(&mut hash, self.raw_text(), node),
            }
        }
        Ok(ClosedRefillDefinitionEvidence {
            definition_digest: hex::encode(hash.finalize()),
            automatic_dimension_ids: ids,
        })
    }

    /// Hash all unedited live fields, including unknown fields of explicitly patched zones.
    pub(crate) fn zone_edit_digest(
        &self,
        excluded: &std::collections::BTreeSet<String>,
        patches: &BTreeMap<String, kicad_ipc::SolidZonePatch>,
    ) -> Result<String, String> {
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        let fields = items(root)?;
        let mut kept = Vec::new();
        for field in fields {
            let zone_id = if matches!(field, Node::List { .. }) && tag(field)? == "zone" {
                self.zones.iter().find_map(|(id, e)| {
                    let Node::List { span, .. } = field else {
                        unreachable!()
                    };
                    (*span == e.span).then_some(id)
                })
            } else {
                None
            };
            if zone_id.is_some_and(|id| excluded.contains(id)) {
                continue;
            }
            kept.push((field, zone_id.and_then(|id| patches.get(id))));
        }
        let mut hash = Sha256::new();
        hash.update([0]);
        hash.update((kept.len() as u64).to_be_bytes());
        for (field, patch) in kept {
            if let Some(patch) = patch {
                feed_patched_zone(&mut hash, self.raw_text(), field, patch)?;
            } else {
                feed_structure(&mut hash, self.raw_text(), field);
            }
        }
        Ok(hex::encode(hash.finalize()))
    }

    pub fn raw_text(&self) -> &str {
        self.cst.to_lossless_string()
    }
    pub fn zones(&self) -> &BTreeMap<String, ZoneTextEvidence> {
        &self.zones
    }
    pub fn structure_digest(&self) -> &str {
        &self.structure_digest
    }
    pub fn zone_text(&self, id: &str) -> Option<&str> {
        let span = self.zones.get(id)?.span;
        Some(&self.cst.raw[span.start..span.end])
    }

    /// Require complete UUID agreement and reject contradictory native lock evidence.
    pub fn validate_native_zones(&self, native: &NativeBoardGeometry) -> Result<(), String> {
        if !self.zones.keys().eq(native.zones().keys()) {
            return Err("live text and native zone identity sets differ".into());
        }
        for (id, zone) in native.zones() {
            let matches = match zone.locked.enum_value() {
                Ok(LockedState::LS_UNKNOWN) => true,
                Ok(LockedState::LS_LOCKED) => self.zones[id].locked,
                Ok(LockedState::LS_UNLOCKED) => !self.zones[id].locked,
                Err(_) => false,
            };
            if !matches {
                return Err("native zone lock contradicts live serialization".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LiveGeometryEvidence {
    pub native: NativeBoardGeometry,
    pub text: LiveBoardText,
}

/// Immutable complete definition evidence, separate from any fill-success claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RefillDefinitionEvidence {
    board_definition_digest: String,
    native_zone_definition_digest: String,
    zone_count: usize,
}
impl RefillDefinitionEvidence {
    pub fn from_observation(observed: &LiveGeometryEvidence) -> Result<Self, String> {
        observed.text.validate_native_zones(&observed.native)?;
        Ok(Self {
            board_definition_digest: observed.text.refill_definition_digest()?,
            native_zone_definition_digest: hex::encode(Sha256::digest(
                observed
                    .native
                    .zone_definition_bytes()
                    .map_err(|e| e.to_string())?,
            )),
            zone_count: observed.native.zones().len(),
        })
    }
    pub fn board_definition_digest(&self) -> &str {
        &self.board_definition_digest
    }
}

/// Bind repeated native/text observations to one exact 10.0.7 editor and document.
/// This is read-only evidence; callers must still bind files, rules, revision and approval.
pub fn observe_live_geometry<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
) -> Result<LiveGeometryEvidence, String> {
    let token = client
        .instance_token()
        .cloned()
        .ok_or("live geometry requires a bound instance")?;
    let version = client.get_version().map_err(|e| e.to_string())?;
    if client.instance_token() != Some(&token) {
        return Err("live geometry instance changed during version query".into());
    }
    if version.full_version != "10.0.7" {
        return Err("live geometry requires verified KiCad 10.0.7".into());
    }
    let documents = client
        .get_open_documents(DocumentQuery::Pcb)
        .map_err(|e| e.to_string())?;
    if client.instance_token() != Some(&token) || documents.len() != 1 {
        return Err("live geometry instance/document changed before capture".into());
    }
    let native = client
        .capture_board_geometry(filename)
        .map_err(|e| e.to_string())?;
    let text = client
        .live_board_text(filename)
        .map_err(|e| e.to_string())?;
    let after = client
        .capture_board_geometry(filename)
        .map_err(|e| e.to_string())?;
    let after_text = client
        .live_board_text(filename)
        .map_err(|e| e.to_string())?;
    let after_documents = client
        .get_open_documents(DocumentQuery::Pcb)
        .map_err(|e| e.to_string())?;
    if documents != after_documents
        || client.instance_token() != Some(&token)
        || native != after
        || text != after_text
    {
        return Err("live geometry, locks or document changed during observation".into());
    }
    let text = LiveBoardText::parse(&text)?;
    text.validate_native_zones(&native)?;
    Ok(LiveGeometryEvidence { native, text })
}

#[cfg(test)]
mod tests {
    #[test]
    fn closed_copy_display_projection_never_weakens_live_or_authored_text() {
        let a = r#"(kicad_pcb (version 20260206) (footprint "x" (dimension (type orthogonal)
          (uuid "dimension") (format (units 3) (precision 4)) (pts (xy 0 0) (xy 30 0))
          (gr_text "1181.1024" (at 15 0) (unknown_style 7)))))"#;
        let b = a.replace("1181.1024", "30");
        let parse = |s: &str| LiveBoardText::parse(s).unwrap();
        let evidence = parse(a).closed_refill_definitions().unwrap();
        assert_eq!(evidence, parse(&b).closed_refill_definitions().unwrap());
        assert_eq!(evidence.automatic_dimension_ids, ["dimension"]);
        assert_ne!(
            parse(a).refill_definition_digest().unwrap(),
            parse(&b).refill_definition_digest().unwrap()
        );
        for (from, to) in [
            ("unknown_style 7", "unknown_style 8"),
            ("precision 4", "precision 5"),
            ("xy 30 0", "xy 31 0"),
            ("at 15 0", "at 16 0"),
            ("dimension\"", "other\""),
        ] {
            assert_ne!(
                evidence,
                parse(&b.replace(from, to))
                    .closed_refill_definitions()
                    .unwrap(),
                "{from}"
            );
        }
        for (from, to) in [
            ("units 3", "units 2"),
            ("(precision 4)", "(precision 4) (override_value \"manual\")"),
            ("type orthogonal", "type leader"),
        ] {
            assert_ne!(
                parse(&a.replace(from, to))
                    .closed_refill_definitions()
                    .unwrap(),
                parse(&b.replace(from, to))
                    .closed_refill_definitions()
                    .unwrap()
            );
        }
        assert!(
            parse(&a.replace("(units 3)", "(units 3) (units 3)"))
                .closed_refill_definitions()
                .is_err()
        );
    }

    #[test]
    fn refill_definition_fingerprint_preserves_all_unclassified_content() {
        let base = r#"(kicad_pcb (version 20260206) (unknown_board "keep")
          (zone (uuid "z") (locked yes) (layers "F.Cu" "In1.Cu")
            (unknown_zone (nested "keep")) (property (layer "F.Cu") (offset 1))
            (fill (thermal_gap 0.2) (unknown_fill 7)) (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1)))))"#;
        let filled = base.replace("(fill (thermal_gap", "(fill yes (thermal_gap").replace("(polygon (pts", "(filled_polygon (layer \"F.Cu\") (island yes) (pts (xy 0 0) (xy 1 0) (xy 0 1))) (polygon (pts");
        let digest = |s: &str| LiveBoardText::parse(s).unwrap().refill_definition_digest();
        let expected = digest(base).unwrap();
        assert_eq!(expected, digest(&filled).unwrap());
        assert_eq!(expected, digest(&filled.replace(" ", "\n")).unwrap());
        for (from, to) in [
            ("unknown_board", "changed_board"),
            ("unknown_zone", "changed_zone"),
            ("unknown_fill 7", "unknown_fill 8"),
            ("locked yes", "locked no"),
            ("thermal_gap 0.2", "thermal_gap 0.3"),
            ("offset 1", "offset 2"),
            ("uuid \"z\"", "uuid \"other\""),
            ("In1.Cu", "B.Cu"),
            ("(polygon (pts (xy 0 0)", "(polygon (pts (xy 0 2)"),
        ] {
            assert_ne!(
                expected,
                digest(&filled.replace(from, to)).unwrap(),
                "{from}"
            );
        }
        for changed in [
            filled.replace("(island yes)", "(island yes) (unknown_derived 1)"),
            filled.replace("(island yes)", "(island yes) (island yes)"),
            filled.replace("(fill yes", "(fill no"),
            filled.replace("(fill yes", "(fill yes yes"),
            filled.replace("(fill yes", "(fill) (fill yes"),
            filled.replace("(island yes)", "(island false)"),
        ] {
            assert!(digest(&changed).is_err());
        }
        let nested = base.replace("(unknown_board \"keep\")", "(footprint (zone (fill yes)))");
        assert_ne!(
            digest(&nested).unwrap(),
            digest(&nested.replace("(fill yes)", "(fill)")).unwrap()
        );
    }

    #[test]
    fn refill_normalizes_only_one_empty_direct_setup_defaults_container() {
        let parse = |body: &str| LiveBoardText::parse(&board(body)).unwrap();
        let base = parse("(setup (unknown keep))");
        let empty = parse("(setup (zone_defaults) (unknown keep))");
        assert_eq!(
            base.refill_definition_digest(),
            empty.refill_definition_digest()
        );
        assert_eq!(
            base.closed_refill_definitions(),
            empty.closed_refill_definitions()
        );
        assert_ne!(base.structure_digest(), empty.structure_digest());
        for body in [
            "(setup (zone_defaults (property (layer F.Cu) (hatch_position (xy 1 2)))) (unknown keep))",
            "(setup (zone_defaults (future value)) (unknown keep))",
            "(setup (zone_defaults) (unknown changed))",
            "(setup (unknown (zone_defaults)))",
            "(setup (unknown keep)) (zone_defaults)",
        ] {
            let changed = parse(body);
            assert_ne!(
                base.refill_definition_digest(),
                changed.refill_definition_digest()
            );
            assert_ne!(
                base.closed_refill_definitions(),
                changed.closed_refill_definitions()
            );
        }
        for body in [
            "(setup (zone_defaults) (zone_defaults))",
            "(setup (zone_defaults (future value)) (zone_defaults))",
            "(setup) (setup (zone_defaults))",
        ] {
            let ambiguous = parse(body);
            assert!(ambiguous.refill_definition_digest().is_err());
            assert!(ambiguous.closed_refill_definitions().is_err());
        }
    }

    use super::*;
    fn board(body: &str) -> String {
        format!("(kicad_pcb (version 20260206) {body})")
    }
    #[test]
    fn live_board_evidence_retains_unknown_and_fill_fields_ignoring_only_whitespace() {
        let source = board(
            r#"(zone (uuid "zone") (locked yes) (future "a\\b") (filled_polygon (layer "F.Cu") (pts (xy 1 2))))"#,
        );
        let original = LiveBoardText::parse(&source).unwrap();
        assert_eq!(original.raw_text(), source);
        assert!(original.zones()["zone"].locked);
        assert!(original.zone_text("zone").unwrap().contains("future"));
        let spaced = source.replace(") (", ")\n\t(");
        assert_eq!(
            original.structure_digest(),
            LiveBoardText::parse(&spaced).unwrap().structure_digest()
        );
        for changed in [
            source.replace("xy 1 2", "xy 1 3"),
            source.replace("locked yes", "locked no"),
            source.replace("future", "other"),
            source.replace("xy 1 2", "xy 1.0 2"),
            source.replace(r#""a\\b""#, r#""a\b""#),
        ] {
            assert_ne!(
                original.structure_digest(),
                LiveBoardText::parse(&changed).unwrap().structure_digest()
            );
        }
    }
    #[test]
    fn zone_lock_observation_rejects_ambiguity_and_unverified_formats() {
        assert!(
            !LiveBoardText::parse(&board("(zone (uuid z))"))
                .unwrap()
                .zones()["z"]
                .locked
        );
        for body in [
            "(zone)",
            "(zone (uuid z) (uuid z))",
            "(zone (uuid z)) (zone (uuid z))",
            "(zone (uuid z) (locked maybe))",
            "(zone (uuid z) (locked yes) (locked no))",
            "(zone (uuid z) (locked))",
            "(version 20260206)",
        ] {
            assert!(LiveBoardText::parse(&board(body)).is_err(), "{body}");
        }
        for invalid in [
            "(kicad_pcb)",
            "(kicad_pcb (version 20260101))",
            "(kicad_pcb (version 20260206)) (extra)",
            "(other (version 20260206))",
        ] {
            assert!(LiveBoardText::parse(invalid).is_err());
        }
    }
    #[test]
    fn preflight_bounds_the_cst_before_recursive_parsing() {
        assert!(
            preflight(&format!(
                "{}x{}",
                "(".repeat(MAX_DEPTH + 1),
                ")".repeat(MAX_DEPTH + 1)
            ))
            .is_err()
        );
        assert!(preflight(&"x ".repeat(MAX_NODES + 1)).is_err());
        assert!(preflight("(x \"unterminated)").is_err());
        assert!(preflight("(x \"escaped\\\" tail\")").is_ok());
        assert!(preflight("(x foo\"bar)").is_ok());
        assert!(preflight("(x)\0").is_err());
    }
}

#[cfg(test)]
mod zone_edit_tests {
    use super::*;
    use kicad_ipc::SolidZonePatch;
    fn board(extra: &str) -> LiveBoardText {
        LiveBoardText::parse(&format!(
            "(kicad_pcb (version 20260206) (generator pcbnew) {extra})"
        ))
        .unwrap()
    }
    #[test]
    fn patch_projection_keeps_unknown_fields_and_unedited_nested_values() {
        let original = r#"(zone (uuid "zone") (name "before") (future_metadata 7) (connect_pads (clearance 0.2) (future_connection 8)) (fill yes (thermal_gap 0.5) (thermal_bridge_width 0.5) (future_fill 9)) (filled_polygon (layer "F.Cu") (pts (xy 0 0) (xy 1 0) (xy 0 1))))"#;
        let changed = original
            .replace("before", "after")
            .replace("clearance 0.2", "clearance 0.3")
            .replace("fill yes", "fill");
        let patches = BTreeMap::from([(
            "zone".into(),
            SolidZonePatch {
                name: Some("after".into()),
                clearance_nm: Some(300_000),
                ..Default::default()
            },
        )]);
        let excluded = std::collections::BTreeSet::new();
        let expected = board(original)
            .zone_edit_digest(&excluded, &patches)
            .unwrap();
        assert_eq!(
            expected,
            board(&changed)
                .zone_edit_digest(&excluded, &patches)
                .unwrap()
        );
        for (a, b) in [
            ("future_metadata 7", "future_metadata 8"),
            ("future_connection 8", "future_connection 9"),
            ("future_fill 9", "future_fill 10"),
            ("thermal_gap 0.5", "thermal_gap 0.6"),
        ] {
            assert_ne!(
                expected,
                board(&changed.replace(a, b))
                    .zone_edit_digest(&excluded, &patches)
                    .unwrap()
            );
        }
        let nested = format!("{original} (footprint (zone (uuid nested) (future_nested 1)))");
        assert_ne!(
            board(&nested)
                .zone_edit_digest(&excluded, &patches)
                .unwrap(),
            board(&nested.replace("future_nested 1", "future_nested 2"))
                .zone_edit_digest(&excluded, &patches)
                .unwrap()
        );
        assert!(
            board(&changed.replace("clearance 0.3", "clearance 0.3) (clearance 0.4"))
                .zone_edit_digest(&excluded, &patches)
                .is_err()
        );
    }
}
