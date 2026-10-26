//! Complete live board text and independently observed zone locks.
use kicad_ipc::{
    DocumentQuery, KiCadClient, KiCadTransport, MAX_NATIVE_GEOMETRY_BYTES, NativeBoardGeometry,
    base_types::LockedState,
};
use kiutils_sexpr::{Atom, CstDocument, Node, Span, parse_one};
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
fn structural_digest(text: &str, node: &Node) -> String {
    fn feed(hash: &mut Sha256, text: &str, node: &Node) {
        match node {
            Node::List { items, .. } => {
                hash.update([0]);
                hash.update((items.len() as u64).to_be_bytes());
                for child in items {
                    feed(hash, text, child);
                }
            }
            Node::Atom { span, .. } => {
                hash.update([1]);
                hash.update(((span.end - span.start) as u64).to_be_bytes());
                hash.update(&text.as_bytes()[span.start..span.end]);
            }
        }
    }
    let mut hash = Sha256::new();
    feed(&mut hash, text, node);
    hex::encode(hash.finalize())
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
