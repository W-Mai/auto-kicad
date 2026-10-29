//! Server-derived footprint compatibility from exact native library pad numbers.
use super::*;
use kicad_schematic::editing::FootprintCompatibilityEvidence;
use kiutils_sexpr::{Atom, Node, parse_one};

impl SchematicAudit {
    pub fn footprint_assignment_evidence(
        &self,
        file: &str,
        uuid: &str,
        identifier: &str,
        mut pin_pad_map: BTreeMap<String, String>,
    ) -> Result<FootprintCompatibilityEvidence, String> {
        self.verify()?;
        let (nickname, entry_name) = identifier
            .split_once(':')
            .ok_or("invalid footprint identifier")?;
        let identifier = FootprintLibraryIdentifier {
            nickname: nickname.into(),
            entry_name: entry_name.into(),
        };
        let resolved = self.inner.footprints.resolve(&identifier)?;
        let bytes = fs::read(&resolved.canonical_source_path).map_err(|error| error.to_string())?;
        if bytes.len() as u64 != resolved.source_size || hash(&bytes) != resolved.source_digest {
            return Err("footprint source changed while reading compatibility".into());
        }
        let pads = numbered_pads(&bytes)?;
        let pins = self
            .pins()
            .pins()
            .iter()
            .filter(|pin| {
                pin.key
                    .component_units
                    .iter()
                    .any(|unit| unit.file == file && unit.symbol_uuid == uuid)
            })
            .map(|pin| pin.key.number.clone())
            .collect::<BTreeSet<_>>();
        if pins.is_empty() {
            return Err("footprint target has no complete observed component pins".into());
        }
        if pin_pad_map.is_empty() {
            pin_pad_map = pins
                .iter()
                .map(|number| (number.clone(), number.clone()))
                .collect();
        }
        if pin_pad_map.keys().cloned().collect::<BTreeSet<_>>() != pins
            || pin_pad_map.values().cloned().collect::<BTreeSet<_>>() != pads
            || pin_pad_map.len() != pads.len()
        {
            return Err("footprint mapping must bijectively match actual symbol pins and numbered library pads".into());
        }
        self.verify()?;
        if self.inner.footprints.resolve(&identifier)? != resolved {
            return Err("footprint selection changed during compatibility".into());
        }
        Ok(FootprintCompatibilityEvidence {
            catalog_digest: resolved.catalog_digest,
            source_digest: resolved.source_digest,
            pad_numbers: pads.into_iter().collect(),
            pin_pad_map,
        })
    }
}

fn numbered_pads(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("footprint source exceeds bound".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    crate::library_catalog::symbols::validate_raw_nesting(bytes)?;
    let root = parse_one(text).map_err(|error| error.to_string())?;
    let Node::List { items, .. } = root
        .nodes
        .into_iter()
        .next()
        .ok_or("missing footprint root")?
    else {
        return Err("invalid native footprint definition".into());
    };
    let atom = |node: &Node| match node {
        Node::Atom {
            atom: Atom::Symbol(value) | Atom::Quoted(value),
            ..
        } => Some(value.clone()),
        _ => None,
    };
    if items.first().and_then(atom).as_deref() != Some("footprint") {
        return Err("native footprint header is required".into());
    }
    let mut pads = BTreeSet::new();
    let mut count = 0;
    for node in &items[1..] {
        let Node::List { items, .. } = node else {
            continue;
        };
        if items.first().and_then(atom).as_deref() != Some("pad") {
            continue;
        }
        count += 1;
        if count > 10_000 {
            return Err("footprint pad count exceeds bound".into());
        }
        let Some(Node::Atom { span, .. }) = items.get(1) else {
            return Err("footprint pad number is missing".into());
        };
        let raw = text
            .get(span.start..span.end)
            .ok_or("invalid footprint pad span")?;
        let number = items
            .get(1)
            .and_then(atom)
            .ok_or("invalid footprint pad number")?;
        if raw.contains('\\') || number.len() > 128 || number.chars().any(char::is_control) {
            return Err("escaped or unbounded footprint pad numbers are unsupported".into());
        }
        if !number.is_empty() {
            pads.insert(number);
        }
    }
    if pads.is_empty() {
        return Err("footprint has no numbered pads".into());
    }
    Ok(pads)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compatibility_uses_actual_distinct_numbered_pads_and_rejects_escape_ambiguity() {
        assert_eq!(numbered_pads(br#"(footprint "x" (pad "1" smd rect) (pad "2" smd rect) (pad "2" smd rect) (pad "" np_thru_hole circle))"#).unwrap(), BTreeSet::from(["1".into(), "2".into()]));
        for raw in [
            br#"(footprint "x" (pad "\x31" smd rect))"#.as_slice(),
            br#"(footprint "x" (pad "" np_thru_hole circle))"#,
            br#"(symbol "x" (pad "1"))"#,
        ] {
            assert!(numbered_pads(raw).is_err());
        }
    }
}
