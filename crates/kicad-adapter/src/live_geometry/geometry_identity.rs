//! Identity projection is private to receipt-bound candidate comparison.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// Dimension text serializes the same native BOARD_ITEM; all independent identities stay unique.
pub(crate) fn board_identity_set(root: &Node) -> Result<BTreeSet<String>, String> {
    fn identity_fields<'a>(node: &'a Node, name: &str) -> Result<Vec<&'a Node>, String> {
        Ok(items(node)?
            .iter()
            .filter(|node| {
                matches!(node, Node::List { .. }) && tag(node).is_ok_and(|tag| tag == name)
            })
            .collect())
    }
    fn optional_identity_field<'a>(node: &'a Node, name: &str) -> Result<Option<&'a Node>, String> {
        let fields = identity_fields(node, name)?;
        if fields.len() > 1 {
            return Err(format!("duplicate board field: {name}"));
        }
        Ok(fields.first().copied())
    }
    fn required_identity_field<'a>(node: &'a Node, name: &str) -> Result<&'a Node, String> {
        optional_identity_field(node, name)?.ok_or_else(|| format!("missing board field: {name}"))
    }
    let mut reserved = BTreeSet::new();
    let mut pending = vec![(root, None::<&str>)];
    while let Some((node, owned_alias)) = pending.pop() {
        let Node::List { items: fields, .. } = node else {
            continue;
        };
        let name = fields.first().and_then(|n| atom(n).ok());
        if name == Some("uuid") {
            let id = scalar(node)?;
            if owned_alias.is_some_and(|owner| owner != id)
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
            && optional_identity_field(node, "type")?
                .map(scalar)
                .transpose()?
                .is_some_and(|kind| matches!(kind, "aligned" | "orthogonal" | "leader" | "radial"))
        {
            let id = scalar(required_identity_field(node, "uuid")?)?;
            let labels = identity_fields(node, "gr_text")?;
            if labels.len() != 1 || scalar(required_identity_field(labels[0], "uuid")?)? != id {
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

impl LiveBoardText {
    pub(crate) fn identity_namespace<'a>(
        text: &str,
        derived: impl Iterator<Item = &'a str>,
    ) -> Result<BTreeMap<String, usize>, String> {
        preflight(text)?;
        let cst = parse_one(text).map_err(|e| e.to_string())?;
        let mut result: BTreeMap<String, usize> =
            board_identity_set(cst.nodes.first().ok_or("missing board root")?)?
                .into_iter()
                .map(|id| (id, 1))
                .collect();
        for id in derived {
            *result.entry(id.to_owned()).or_default() += 1;
        }
        if result.values().any(|count| *count != 1) {
            return Err("complete native identity namespace contains duplicates".into());
        }
        Ok(result)
    }

    /// Check the complete native identity namespace, including generated IDs absent from text.
    pub(crate) fn verify_additive_identity_namespace<'a>(
        before: &str,
        after: &str,
        identities: &BTreeMap<String, &'static str>,
        before_derived: impl Iterator<Item = &'a str>,
        after_derived: impl Iterator<Item = &'a str>,
    ) -> Result<(), String> {
        let old_ids = Self::identity_namespace(before, before_derived)?;
        let new_ids = Self::identity_namespace(after, after_derived)?;
        for id in identities.keys() {
            if old_ids.contains_key(id) || new_ids.get(id) != Some(&1) {
                return Err("created identity is preexisting, duplicated or absent".into());
            }
        }
        Ok(())
    }

    /// Remove only receipt-proven new direct objects; every retained field stays byte-lexically exact.
    pub(crate) fn without_created_additions(
        &self,
        before: &Self,
        identities: &BTreeMap<String, &'static str>,
    ) -> Result<Self, String> {
        Self::verify_additive_identity_namespace(
            before.raw_text(),
            self.raw_text(),
            identities,
            std::iter::empty(),
            std::iter::empty(),
        )?;
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        let mut kept = Vec::new();
        let mut seen = BTreeSet::new();
        for node in &items(root)?[1..] {
            let Node::List { span, .. } = node else {
                return Err("unexpected board scalar".into());
            };
            if matches!(tag(node)?, "segment" | "via" | "gr_line")
                && let Some(uuid) = unique_form(node, "uuid")?
            {
                let id = scalar(uuid)?;
                if let Some(kind) = identities.get(id) {
                    if tag(node)? != *kind
                        || !matches!(*kind, "segment" | "via" | "gr_line")
                        || !seen.insert(id.to_owned())
                    {
                        return Err(
                            "creation projection targets an unexpected direct record".into()
                        );
                    }
                    if *kind == "gr_line"
                        && !matches!(
                            scalar(unique_form(node, "layer")?.ok_or("graphic layer missing")?)?,
                            "Dwgs.User" | "Edge.Cuts"
                        )
                    {
                        return Err(
                            "creation projection targets an unsupported graphic layer".into()
                        );
                    }
                    continue;
                }
            }
            kept.push(&self.raw_text()[span.start..span.end]);
        }
        if seen.len() != identities.len() {
            return Err("creation projection did not find every direct record".into());
        }
        Self::parse(&format!("(kicad_pcb {})", kept.join(" ")))
    }

    pub(crate) fn project_created_geometry(
        &self,
        identities: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let root = self.cst.nodes.first().ok_or("missing board root")?;
        fn collect_ids(node: &Node, ids: &mut BTreeSet<String>) -> Result<(), String> {
            if let Node::List { items: fields, .. } = node {
                if fields.first().and_then(|n| atom(n).ok()) == Some("uuid") {
                    ids.insert(scalar(node)?.into());
                } else {
                    for field in fields {
                        collect_ids(field, ids)?;
                    }
                }
            }
            Ok(())
        }
        let mut all_original_ids = BTreeSet::new();
        collect_ids(root, &mut all_original_ids)?;
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
                "zone"
                    | "gr_line"
                    | "gr_arc"
                    | "gr_circle"
                    | "gr_rect"
                    | "gr_poly"
                    | "segment"
                    | "via"
            ) {
                let uuid = unique_form(node, "uuid")?.ok_or("candidate geometry lacks UUID")?;
                let original = scalar(uuid)?;
                if !original_ids.insert(original) {
                    return Err("candidate geometry identity repeats".into());
                }
                let id = if let Some(alias) = identities.get(original) {
                    if !matches!(tag(node)?, "zone" | "segment" | "via")
                        && !matches!(
                            (
                                tag(node)?,
                                scalar(
                                    unique_form(node, "layer")?
                                        .ok_or("created graphic lacks layer")?
                                )?
                            ),
                            (_, "Edge.Cuts") | ("gr_line", "Dwgs.User")
                        )
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
                .any(|alias| all_original_ids.contains(alias.as_str()))
        {
            return Err("candidate mapping is missing or aliases an original identity".into());
        }
        rest.extend(geometry.into_values());
        Self::parse(&format!("(kicad_pcb {})", rest.join(" ")))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn native_dimension_text_identity_is_owned_and_cannot_authorize_other_collisions() {
        let owner = "77777777-7777-4777-8777-777777777777";
        let other = "88888888-8888-4888-8888-888888888888";
        let dimension = format!(
            "(dimension (type leader) (uuid {owner}) (gr_text note (uuid {owner}) (at 2 2)))"
        );
        let raw = format!("(kicad_pcb {dimension})");
        let namespace = LiveBoardText::identity_namespace(&raw, std::iter::empty()).unwrap();
        assert_eq!(namespace, BTreeMap::from([(owner.to_owned(), 1)]));
        for corrupted in [
            format!("(kicad_pcb {dimension} (gr_text extra (uuid {owner})))"),
            format!("(kicad_pcb {dimension} {dimension})"),
            raw.replace("(type leader)", "(type future)"),
            raw.replace(
                "gr_text note",
                &format!("gr_text note (future (uuid {owner}))"),
            ),
            raw.replace("gr_text note", &format!("gr_text note (uuid {other})")),
        ] {
            assert!(LiveBoardText::identity_namespace(&corrupted, std::iter::empty()).is_err());
        }
        assert!(LiveBoardText::identity_namespace(&raw, std::iter::once(owner)).is_err());
        assert!(LiveBoardText::identity_namespace(&raw, std::iter::once(other)).is_ok());
    }
    use super::*;
    fn parse(s: &str) -> LiveBoardText {
        LiveBoardText::parse(&format!("(kicad_pcb (version 20260206) {s})")).unwrap()
    }
    #[test]
    fn additive_projection_preserves_every_unedited_field_and_rejects_identity_substitution() {
        let original = r#"(footprint (uuid fp) (pad (uuid pad) (future 7)))
            (segment (uuid old) (future 8)) (zone (uuid zone) (future 9))"#;
        let created = r#"(segment (uuid new-track) (width 0.25))
            (via (uuid new-via) (size 0.6)) (gr_line (uuid new-graphic) (layer "Dwgs.User"))"#;
        let before = parse(original);
        let after = parse(&format!("{original} {created}"));
        let ids = BTreeMap::from([
            ("new-track".into(), "segment"),
            ("new-via".into(), "via"),
            ("new-graphic".into(), "gr_line"),
        ]);
        let full_before = format!(
            "(kicad_pcb (version 20260206) {original} (zone (attr (teardrop (type padvia)))))"
        );
        let full_after = format!(
            "(kicad_pcb (version 20260206) {original} {created} (zone (attr (teardrop (type padvia)))))"
        );
        assert!(
            LiveBoardText::verify_additive_identity_namespace(
                &full_before,
                &full_after,
                &ids,
                ["generated-before"].into_iter(),
                ["generated-after"].into_iter(),
            )
            .is_ok()
        );
        for (old, new) in [
            ("new-track", "generated-after"),
            ("generated-before", "new-track"),
            ("generated-before", "pad"),
        ] {
            assert!(
                LiveBoardText::verify_additive_identity_namespace(
                    &full_before,
                    &full_after,
                    &ids,
                    [old].into_iter(),
                    [new].into_iter(),
                )
                .is_err()
            );
        }
        assert_eq!(
            before.structure_digest(),
            after
                .without_created_additions(&before, &ids)
                .unwrap()
                .structure_digest()
        );
        let changed = parse(&format!(
            "{} {created}",
            original.replace("future 7", "future 70")
        ));
        assert_ne!(
            before.structure_digest(),
            changed
                .without_created_additions(&before, &ids)
                .unwrap()
                .structure_digest()
        );
        for bad in [
            format!("{original} {created} (footprint (uuid other) (pad (uuid new-via)))"),
            format!(
                "{original} {}",
                created.replace("(via (uuid", "(zone (uuid")
            ),
            format!("{original} {}", created.replace("Dwgs.User", "F.Cu")),
            format!("{original} {}", created.replace("new-track", "missing")),
        ] {
            assert!(
                parse(&bad)
                    .without_created_additions(&before, &ids)
                    .is_err()
            );
        }
        for id in ["pad", "fp", "old", "zone"] {
            assert!(
                after
                    .without_created_additions(&before, &BTreeMap::from([(id.into(), "segment")]))
                    .is_err()
            );
        }
        let aliases = ids
            .keys()
            .enumerate()
            .map(|(i, id)| (id.clone(), format!("created:{i}")))
            .collect();
        let projected = after.project_created_geometry(&aliases).unwrap();
        let changed = parse(&format!(
            "{original} {}",
            created.replace("width 0.25", "width 0.26")
        ))
        .project_created_geometry(&aliases)
        .unwrap();
        assert_ne!(
            projected.refill_definition_digest().unwrap(),
            changed.refill_definition_digest().unwrap()
        );
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
