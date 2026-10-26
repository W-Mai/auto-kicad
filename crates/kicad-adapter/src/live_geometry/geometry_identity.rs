//! Identity projection is private to receipt-bound candidate comparison.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

impl LiveBoardText {
    pub(crate) fn project_created_geometry(
        &self,
        identities: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        let mut rest = Vec::new();
        let mut geometry = BTreeMap::new();
        let mut seen = BTreeSet::new();
        let mut original_ids = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        for alias in identities.values() {
            if alias.is_empty() || alias.len() > 128 || !aliases.insert(alias) {
                return Err("candidate aliases must be bounded and unique".into());
            }
        }
        for node in &items(root)?[1..] {
            let Node::List { span, .. } = node else {
                return Err("unexpected direct board scalar".into());
            };
            let mut text = self.raw_text()[span.start..span.end].to_owned();
            if matches!(
                tag(node)?,
                "zone" | "gr_line" | "gr_arc" | "gr_circle" | "gr_rect" | "gr_poly"
            ) {
                let uuid = unique_form(node, "uuid")?.ok_or("candidate geometry lacks UUID")?;
                let original = scalar(uuid)?;
                if !original_ids.insert(original) {
                    return Err("candidate geometry identity repeats".into());
                }
                let id = if let Some(alias) = identities.get(original) {
                    if tag(node)? != "zone"
                        && scalar(
                            unique_form(node, "layer")?.ok_or("created outline lacks layer")?,
                        )? != "Edge.Cuts"
                    {
                        return Err(
                            "candidate creation mapping targets a non-outline graphic".into()
                        );
                    }
                    let Node::Atom { span: value, .. } = &items(uuid)?[1] else {
                        return Err("candidate UUID is not scalar".into());
                    };
                    text.replace_range(
                        value.start - span.start..value.end - span.start,
                        &serde_json::to_string(alias).map_err(|e| e.to_string())?,
                    );
                    seen.insert(original.to_owned());
                    alias.as_str()
                } else {
                    original
                };
                if geometry.insert(id.to_owned(), text).is_some() {
                    return Err("candidate geometry identity mapping collides".into());
                }
            } else {
                rest.push(text);
            }
        }
        if seen.len() != identities.len()
            || aliases
                .iter()
                .any(|alias| original_ids.contains(alias.as_str()))
        {
            return Err("candidate mapping is missing or aliases an original identity".into());
        }
        rest.extend(geometry.into_values());
        Self::parse(&format!("(kicad_pcb {})", rest.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(s: &str) -> LiveBoardText {
        LiveBoardText::parse(&format!("(kicad_pcb (version 20260206) {s})")).unwrap()
    }
    #[test]
    fn candidate_projection_maps_only_explicit_direct_identities() {
        let first = r#"(gr_line (uuid old) (layer "Edge.Cuts") (future 1))
            (footprint (uuid footprint) (fp_line (uuid a) (future 2)))
            (zone (uuid a) (name a) (future 3) (fill yes) (filled_polygon (layer F.Cu) (pts (xy 0 0) (xy 1 0) (xy 0 1))))
            (gr_arc (uuid b) (layer "Edge.Cuts") (future 4))"#;
        let second = first
            .replace("(uuid a) (name", "(uuid c) (name")
            .replace("(uuid b)", "(uuid d)");
        let map_a = BTreeMap::from([
            ("a".into(), "zone:new".into()),
            ("b".into(), "outline:new".into()),
        ]);
        let map_b = BTreeMap::from([
            ("c".into(), "zone:new".into()),
            ("d".into(), "outline:new".into()),
        ]);
        let left = parse(first).project_created_geometry(&map_a).unwrap();
        let right = parse(&second).project_created_geometry(&map_b).unwrap();
        assert_eq!(
            left.refill_definition_digest().unwrap(),
            right.refill_definition_digest().unwrap()
        );
        assert_eq!(
            left.filled_copper().unwrap(),
            right.filled_copper().unwrap()
        );
        for (from, to) in [
            ("future 1", "future 9"),
            ("future 2", "future 9"),
            ("future 3", "future 9"),
            ("future 4", "future 9"),
            ("uuid old", "uuid other"),
            ("name a", "name c"),
        ] {
            let changed = parse(&second.replace(from, to))
                .project_created_geometry(&map_b)
                .unwrap();
            assert_ne!(
                left.refill_definition_digest().unwrap(),
                changed.refill_definition_digest().unwrap()
            );
        }
        let changed = parse(&second.replace("xy 1 0", "xy 1.000001 0"))
            .project_created_geometry(&map_b)
            .unwrap();
        assert_ne!(
            left.filled_copper().unwrap(),
            changed.filled_copper().unwrap()
        );
        for map in [
            BTreeMap::from([("missing".into(), "new".into())]),
            BTreeMap::from([("a".into(), "old".into())]),
            BTreeMap::from([("a".into(), "new".into()), ("b".into(), "new".into())]),
        ] {
            assert!(parse(first).project_created_geometry(&map).is_err());
        }
        assert!(
            parse(&first.replace(
                "(uuid b) (layer \"Edge.Cuts\")",
                "(uuid b) (layer \"Dwgs.User\")"
            ))
            .project_created_geometry(&map_a)
            .is_err()
        );
        assert!(
            parse(&format!("{first} (gr_line (uuid b) (layer Edge.Cuts))"))
                .project_created_geometry(&map_a)
                .is_err()
        );
    }
}
