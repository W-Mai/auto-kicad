//! Exact authored-span preservation when composing independently matched refill results.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClosedRegionCopper {
    pub digest: String,
    pub polygon_count: usize,
    pub point_count: usize,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClosedRegionCopperChange {
    pub before: ClosedRegionCopper,
    pub after: ClosedRegionCopper,
}
/// Complete authored fill changes and complete before/after generated content, without transient UUIDs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClosedRefillDelta {
    pub authored: BTreeMap<String, ClosedRegionCopperChange>,
    pub generated_before: BTreeMap<String, String>,
    pub generated_after: BTreeMap<String, String>,
}

/// Matched refill content. This supplies no electrical, source, human-approval or write authority.
pub struct ClosedRefillCandidate {
    text: String,
    delta: ClosedRefillDelta,
    evidence: serde_json::Value,
    digest: String,
}

fn span(node: &Node) -> Span {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => *span,
    }
}
fn fields<'a>(node: &'a Node, name: &str) -> Result<Vec<&'a Node>, String> {
    items(node)?[1..]
        .iter()
        .filter_map(|child| match tag(child) {
            Ok(value) if value == name => Some(Ok(child)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}
fn one<'a>(node: &'a Node, name: &str) -> Result<Option<&'a Node>, String> {
    let matches = fields(node, name)?;
    if matches.len() > 1 {
        return Err("duplicate closed-refill field".into());
    }
    Ok(matches.first().copied())
}
fn generated(node: &Node) -> Result<bool, String> {
    let Some(attr) = one(node, "attr")? else {
        return Ok(false);
    };
    Ok(one(attr, "teardrop")?.is_some())
}
fn zone_id(node: &Node) -> Result<String, String> {
    one(node, "uuid")?
        .map(scalar)
        .transpose()?
        .map(str::to_owned)
        .ok_or_else(|| "authored refill zone has no identity".into())
}
fn zone_nodes(document: &CstDocument) -> Result<BTreeMap<String, &Node>, String> {
    let root = document.nodes.first().ok_or("missing refill board root")?;
    let mut result = BTreeMap::new();
    for node in fields(root, "zone")? {
        if !generated(node)? && result.insert(zone_id(node)?, node).is_some() {
            return Err("duplicate authored refill zone".into());
        }
    }
    Ok(result)
}
fn raw<'a>(text: &'a str, node: &Node) -> &'a str {
    let value = span(node);
    &text[value.start..value.end]
}

/// Comparison-only ordering for complete independent direct routing records.
/// Every nested token/order and every non-route source span remains significant.
pub(crate) fn canonical_direct_routes(text: &str) -> Result<String, String> {
    preflight_profile(
        text,
        kicad_checks::connectivity::SavedBoardProfile::Synchronization,
    )?;
    let document = parse_one(text).map_err(|e| e.to_string())?;
    let root = document
        .nodes
        .first()
        .ok_or("missing routing comparison root")?;
    if tag(root)? != "kicad_pcb" {
        return Err("routing comparison requires a board".into());
    }
    let mut slots = Vec::new();
    let mut routes = BTreeMap::new();
    for node in &items(root)?[1..] {
        if !matches!(tag(node)?, "segment" | "arc" | "via") {
            continue;
        }
        let id = one(node, "uuid")?
            .map(scalar)
            .transpose()?
            .ok_or("route lacks an exact UUID")?;
        if routes.insert(id, raw(text, node)).is_some() {
            return Err("duplicate direct route identity".into());
        }
        slots.push(span(node));
    }
    let mut projected = text.to_owned();
    for (span, value) in slots.into_iter().zip(routes.into_values()).rev() {
        projected.replace_range(span.start..span.end, value);
    }
    Ok(projected)
}

// KiCad serializes project text variables as direct board properties. Only the exact
// synthetic variable established by the private project may be restored to source text.
fn restore_project_root_property(
    original: &str,
    native: &str,
    resource_root: &str,
) -> Result<String, String> {
    fn property(document: &CstDocument) -> Result<Option<&Node>, String> {
        let root = document
            .nodes
            .first()
            .ok_or("missing resource-bound board root")?;
        let mut found = None;
        for node in fields(root, "property")? {
            let values = items(node)?;
            if atom(values.get(1).ok_or("missing board property name")?)? == "KIPRJMOD" {
                if values.len() != 3 || found.replace(node).is_some() {
                    return Err("ambiguous board project-root property".into());
                }
                atom(&values[2])?;
            }
        }
        Ok(found)
    }
    let source = parse_one(original).map_err(|e| e.to_string())?;
    let output = parse_one(native).map_err(|e| e.to_string())?;
    let injected =
        property(&output)?.ok_or("native refill lacks the exact private project-root property")?;
    if atom(&items(injected)?[2])? != resource_root {
        return Err("native refill project-root property differs from its frozen context".into());
    }
    let replacement = property(&source)?
        .map(|node| raw(original, node))
        .unwrap_or("");
    let mut text = native.to_owned();
    let at = span(injected);
    text.replace_range(at.start..at.end, replacement);
    Ok(text)
}

fn region_copper(text: &str, node: &Node) -> Result<ClosedRegionCopper, String> {
    let wrapped = LiveBoardText::parse_synchronization(&format!(
        "(kicad_pcb (version 20260206) {})",
        raw(text, node)
    ))?;
    let fill = wrapped.filled_copper()?;
    Ok(ClosedRegionCopper {
        digest: fill.digest().into(),
        polygon_count: fill.polygon_count(),
        point_count: fill.point_count(),
    })
}

fn pad_metadata_definition(text: &str, pad: &Node) -> Result<String, String> {
    let values = items(pad)?;
    if values.len() < 4 {
        return Err("missing native pad header".into());
    }
    for value in &values[..4] {
        atom(value)?;
    }
    let mut metadata = BTreeMap::new();
    let mut kept = Vec::new();
    for field in &values[4..] {
        let name = tag(field)?;
        if matches!(name, "uuid" | "net" | "pinfunction" | "pintype") {
            if metadata.insert(name, raw(text, field)).is_some() {
                return Err("duplicate synchronization pad metadata".into());
            }
        } else {
            kept.push(raw(text, field));
        }
    }
    Ok(format!(
        "({} {} {})",
        values[..4]
            .iter()
            .map(|value| raw(text, value))
            .collect::<Vec<_>>()
            .join(" "),
        kept.join(" "),
        metadata.values().copied().collect::<Vec<_>>().join(" ")
    ))
}

/// Comparison-only set grammar for direct footprints and their known metadata forms.
/// Unknown fields, all geometry order, numeric tokens and unit/pin contents remain exact.
pub(crate) fn synchronization_definitions(
    board: &LiveBoardText,
) -> Result<ClosedRefillDefinitionEvidence, String> {
    let text = board.raw_text();
    let document = parse_one(text).map_err(|e| e.to_string())?;
    let root = document
        .nodes
        .first()
        .ok_or("missing synchronization definition root")?;
    let mut footprints = BTreeMap::new();
    let mut patches = Vec::new();
    for footprint in fields(root, "footprint")? {
        let values = items(footprint)?;
        if values.len() < 2 {
            return Err("missing footprint identifier".into());
        }
        atom(&values[1])?;
        let mut id = None;
        let mut metadata = BTreeMap::new();
        let mut kept = Vec::new();
        for field in &values[2..] {
            let name = tag(field)?;
            if name == "uuid" {
                id = Some(scalar(field)?.to_owned());
            }
            if matches!(
                name,
                "uuid" | "path" | "sheetname" | "sheetfile" | "units" | "attr" | "locked"
            ) {
                if metadata.insert(name.to_owned(), raw(text, field)).is_some() {
                    return Err("duplicate synchronization footprint metadata".into());
                }
            } else if name == "property" {
                let values = items(field)?;
                let property = atom(values.get(1).ok_or("missing footprint property name")?)?;
                atom(values.get(2).ok_or("missing footprint property value")?)?;
                if metadata
                    .insert(format!("property:{property}"), raw(text, field))
                    .is_some()
                {
                    return Err("duplicate synchronization footprint property".into());
                }
            } else if name == "pad" {
                kept.push(pad_metadata_definition(text, field)?);
            } else {
                kept.push(raw(text, field).to_owned());
            }
        }
        let projected = format!(
            "(footprint {} {} {})",
            raw(text, &values[1]),
            kept.join(" "),
            metadata.values().copied().collect::<Vec<_>>().join(" ")
        );
        if footprints
            .insert(id.ok_or("missing native footprint identity")?, projected)
            .is_some()
        {
            return Err("duplicate synchronization footprint identity".into());
        }
        patches.push((span(footprint), String::new()));
    }
    let at = span(root).end - 1;
    patches.push((
        Span { start: at, end: at },
        footprints.into_values().collect::<Vec<_>>().join(" "),
    ));
    patches.sort_by_key(|(span, _)| (span.start, span.end));
    let mut projected = text.to_owned();
    for (span, value) in patches.into_iter().rev() {
        projected.replace_range(span.start..span.end, &value);
    }
    LiveBoardText::parse_synchronization(&canonical_direct_routes(&projected)?)?
        .closed_refill_definitions()
}

impl ClosedRefillCandidate {
    /// Match two independently executed refill outputs and transplant only fill/generated spans.
    /// Callers must separately prove both native executions use the exact intended inputs/context.
    pub fn compose(original: &str, native: &str, independent: &str) -> Result<Self, String> {
        Self::compose_profile(original, native, independent, None)
    }

    /// Project-root rebasing creates one native board property; keep its exact origin explicit.
    pub(crate) fn compose_for_project_root(
        original: &str,
        native: &str,
        independent: &str,
        resource_root: &str,
    ) -> Result<Self, String> {
        Self::compose_profile(original, native, independent, Some(resource_root))
    }

    fn compose_profile(
        original: &str,
        raw_native: &str,
        raw_independent: &str,
        resource_root: Option<&str>,
    ) -> Result<Self, String> {
        let normalized_native;
        let normalized_independent;
        let (native, independent) = if let Some(resource_root) = resource_root {
            normalized_native = restore_project_root_property(original, raw_native, resource_root)?;
            normalized_independent =
                restore_project_root_property(original, raw_independent, resource_root)?;
            (normalized_native.as_str(), normalized_independent.as_str())
        } else {
            (raw_native, raw_independent)
        };
        let before = TeardropTextPartition::parse_synchronization(original)?;
        let first = TeardropTextPartition::parse_synchronization(native)?;
        let second = TeardropTextPartition::parse_synchronization(independent)?;
        let definitions = before.authored().closed_refill_definitions()?;
        let native_definitions = synchronization_definitions(before.authored())?;
        if native_definitions != synchronization_definitions(first.authored())?
            || native_definitions != synchronization_definitions(second.authored())?
        {
            return Err("native refill changed authored definitions".into());
        }
        let copper = first.authored().filled_copper()?;
        let generated_content = first.generated_content()?;
        if copper != second.authored().filled_copper()?
            || generated_content != second.generated_content()?
        {
            return Err("independent refill copper or complete generated content differs".into());
        }
        let source = parse_one(original).map_err(|e| e.to_string())?;
        let result = parse_one(native).map_err(|e| e.to_string())?;
        let source_zones = zone_nodes(&source)?;
        let result_zones = zone_nodes(&result)?;
        if source_zones.keys().ne(result_zones.keys()) {
            return Err("refill changed the authored zone identity set".into());
        }
        let mut patches = Vec::<(Span, String)>::new();
        let mut changed_zones = BTreeSet::new();
        for (id, zone) in &source_zones {
            let after = result_zones[id];
            if region_copper(original, zone)? == region_copper(native, after)? {
                continue;
            }
            if one(zone, "locked")?.map(scalar).transpose()? == Some("yes") {
                return Err("native refill changed a locked authored region".into());
            }
            let old_polygons = fields(zone, "filled_polygon")?;
            let new_polygons = fields(after, "filled_polygon")?;
            for polygon in old_polygons {
                patches.push((span(polygon), String::new()));
            }
            let mut insertion = new_polygons
                .iter()
                .map(|node| raw(native, node))
                .collect::<Vec<_>>()
                .join("\n");
            if !insertion.is_empty() {
                insertion.insert(0, '\n');
            }
            let position = span(zone).end - 1;
            patches.push((
                Span {
                    start: position,
                    end: position,
                },
                insertion,
            ));
            let old_fill = one(zone, "fill")?;
            let new_fill = one(after, "fill")?;
            let filled = |node: &Node| -> Result<bool, String> {
                Ok(items(node)?
                    .get(1)
                    .is_some_and(|value| atom(value) == Ok("yes")))
            };
            if let Some(fill) = old_fill {
                if filled(fill)? {
                    patches.push((span(&items(fill)?[1]), String::new()));
                }
                if new_fill.map(filled).transpose()?.unwrap_or(false) {
                    let position = span(&items(fill)?[0]).end;
                    patches.push((
                        Span {
                            start: position,
                            end: position,
                        },
                        " yes".into(),
                    ));
                }
            } else if new_fill.is_some() {
                return Err("refill introduced unapproved fill settings".into());
            }
            changed_zones.insert(id.clone());
        }
        let source_root = source.nodes.first().ok_or("missing refill source root")?;
        if fields(source_root, "zone")?.iter().any(|zone| {
            generated(zone).unwrap_or(false)
                && one(zone, "locked")
                    .ok()
                    .flatten()
                    .and_then(|node| scalar(node).ok())
                    == Some("yes")
        }) && before.generated_content()? != generated_content
        {
            return Err("refill changed locked generated regions".into());
        }
        if before.generated_content()? != generated_content {
            for zone in fields(source_root, "zone")? {
                if generated(zone)? {
                    patches.push((span(zone), String::new()));
                }
            }
            let result_root = result.nodes.first().ok_or("missing refill result root")?;
            let generated_text = fields(result_root, "zone")?
                .into_iter()
                .filter_map(|zone| match generated(zone) {
                    Ok(true) => Some(Ok(raw(native, zone))),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect::<Result<Vec<_>, String>>()?
                .join("\n");
            if !generated_text.is_empty() {
                let position = span(source_root).end - 1;
                patches.push((
                    Span {
                        start: position,
                        end: position,
                    },
                    format!("\n{generated_text}\n"),
                ));
            }
        }
        patches.sort_by_key(|(span, _)| (span.start, span.end));
        if patches
            .windows(2)
            .any(|pair| pair[0].0.end > pair[1].0.start)
        {
            return Err("overlapping closed-refill spans".into());
        }
        let mut text = original.to_owned();
        for (span, value) in patches.into_iter().rev() {
            text.replace_range(span.start..span.end, &value);
        }
        let candidate = TeardropTextPartition::parse_synchronization(&text)?;
        if candidate.authored().closed_refill_definitions()? != definitions
            || candidate.authored().filled_copper()? != copper
            || candidate.generated_content()? != generated_content
        {
            return Err(
                "composed refill differs from matched native content or authored definitions"
                    .into(),
            );
        }
        let final_cst = parse_one(&text).map_err(|e| e.to_string())?;
        let final_zones = zone_nodes(&final_cst)?;
        let mut authored = BTreeMap::new();
        for (id, zone) in &source_zones {
            let before = region_copper(original, zone)?;
            let after = region_copper(&text, final_zones[id])?;
            if before != after {
                authored.insert(id.clone(), ClosedRegionCopperChange { before, after });
            }
        }
        let delta = ClosedRefillDelta {
            authored,
            generated_before: before.generated_content()?,
            generated_after: generated_content.clone(),
        };
        let evidence = serde_json::json!({"schema_version":1,"input_sha256":hash_bytes(original.as_bytes()),
            "native_sha256":hash_bytes(raw_native.as_bytes()),"independent_sha256":hash_bytes(raw_independent.as_bytes()),"private_project_root":resource_root,
            "candidate_sha256":hash_bytes(text.as_bytes()),"authored_definitions":definitions,"native_authored_definitions":native_definitions,"footprint_definition_grammar":"source-span-metadata-set-v1","copper":copper,
            "generated":generated_content,"changed_zone_ids":changed_zones,"delta":delta,"publication_authorized":false});
        let digest = hash_bytes(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?);
        Ok(Self {
            text,
            delta,
            evidence,
            digest,
        })
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn delta(&self) -> &ClosedRefillDelta {
        &self.delta
    }
    pub fn evidence(&self) -> &serde_json::Value {
        &self.evidence
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    const INPUT: &str = r#"(kicad_pcb (version 20260206) (future_board "keep exactly")
      (zone (uuid "z") (layer "F.Cu") (fill (thermal_gap 0.2) (future_fill 7))
      (polygon (pts (xy 0 0) (xy 1 0) (xy 0 1)))))"#;
    fn filled() -> String {
        INPUT.replace("(fill (", "(fill yes (").replace(
            "(polygon (pts",
            "(filled_polygon (layer \"F.Cu\") (pts (xy 0 0) (xy 1 0) (xy 0 1))) (polygon (pts",
        )
    }
    #[test]
    fn exact_composition_preserves_authored_spans_and_requires_complete_independent_content() {
        let native = filled();
        let candidate = ClosedRefillCandidate::compose(INPUT, &native, &native).unwrap();
        assert_eq!(candidate.delta().authored.len(), 1);
        assert!(candidate.text().contains("(future_board \"keep exactly\")"));
        assert_eq!(
            ClosedRefillCandidate::compose(&native, &native, &native)
                .unwrap()
                .text(),
            native
        );
        for changed in [
            native.replace("future_fill 7", "future_fill 8"),
            native.replace("xy 1 0", "xy 2 0"),
        ] {
            assert!(ClosedRefillCandidate::compose(INPUT, &native, &changed).is_err());
        }
        let locked = INPUT.replace("(uuid \"z\")", "(uuid \"z\") (locked yes)");
        assert!(
            ClosedRefillCandidate::compose(
                &locked,
                &filled().replace("(uuid \"z\")", "(uuid \"z\") (locked yes)"),
                &filled().replace("(uuid \"z\")", "(uuid \"z\") (locked yes)")
            )
            .is_err()
        );
    }
    #[test]
    fn only_the_exact_private_project_property_can_be_projected() {
        let native = filled().replace(
            "(future_board",
            "(property \"KIPRJMOD\" \"/private/owned/resources\") (future_board",
        );
        let composed = ClosedRefillCandidate::compose_for_project_root(
            INPUT,
            &native,
            &native,
            "/private/owned/resources",
        )
        .unwrap();
        assert!(!composed.text().contains("KIPRJMOD"));
        assert_eq!(
            composed.evidence()["native_sha256"],
            hash_bytes(native.as_bytes())
        );
        assert!(ClosedRefillCandidate::compose(INPUT, &native, &native).is_err());
        assert!(
            ClosedRefillCandidate::compose_for_project_root(INPUT, &native, &native, "/different")
                .is_err()
        );
        let changed = native.replace("future_board", "changed_board");
        assert!(
            ClosedRefillCandidate::compose_for_project_root(
                INPUT,
                &changed,
                &changed,
                "/private/owned/resources"
            )
            .is_err()
        );
    }
    #[test]
    fn native_footprint_order_preserves_source_spans_and_every_unknown_child() {
        let footprint = r#"(footprint "Part:Exact" (uuid "aaaaaaaa-0000-4000-8000-000000000001") (future_geometry (xy 1 2) (xy 3 4)) (sheetname "/") (units (unit (name "A") (pins "2" "1"))))"#;
        let source = INPUT.replace("(future_board", &format!("{footprint} (future_board"));
        let native = filled();
        let at = native.rfind(')').unwrap();
        let native = format!("{} {}{}", &native[..at], footprint, &native[at..]);
        let candidate = ClosedRefillCandidate::compose(&source, &native, &native).unwrap();
        assert!(candidate.text().contains(footprint));
        assert!(
            candidate.text().find(footprint).unwrap()
                < candidate.text().find("future_board").unwrap()
        );
        let changed = native.replace("xy 3 4", "xy 4 3");
        assert!(ClosedRefillCandidate::compose(&source, &changed, &changed).is_err());
        let changed = native.replace("pins \"2\" \"1\"", "pins \"1\" \"2\"");
        assert!(ClosedRefillCandidate::compose(&source, &changed, &changed).is_err());
    }
    #[test]
    fn only_pad_identity_and_electrical_metadata_order_is_a_set() {
        let pad = r#"(pad "1" smd rect (at 1 2) (size 1 1) (layers "F.Cu") (uuid "aaaaaaaa-0000-4000-8000-000000000002") (net "GND") (pinfunction "one") (pintype "passive") (future_geometry (xy 1 2) (xy 3 4)))"#;
        let reordered = pad.replace(
            r#"(uuid "aaaaaaaa-0000-4000-8000-000000000002") (net "GND")"#,
            r#"(net "GND") (uuid "aaaaaaaa-0000-4000-8000-000000000002")"#,
        );
        let board = |p: &str| {
            LiveBoardText::parse_synchronization(&format!(r#"(kicad_pcb (version 20260206) (footprint "Part:Exact" (uuid "aaaaaaaa-0000-4000-8000-000000000001") {p}))"#)).unwrap()
        };
        let expected = synchronization_definitions(&board(pad)).unwrap();
        assert_eq!(
            expected,
            synchronization_definitions(&board(&reordered)).unwrap()
        );
        for changed in [
            reordered.replace("GND", "OTHER"),
            reordered.replace("xy 3 4", "xy 4 3"),
            reordered.replace("size 1 1", "size 1 2"),
        ] {
            assert_ne!(
                expected,
                synchronization_definitions(&board(&changed)).unwrap()
            );
        }
    }
    #[test]
    fn named_footprint_property_order_preserves_complete_values_and_geometry() {
        let property = r#"(property "Custom" "retain" (at 1 2) (future_style (opaque 7)))"#;
        let a = format!(
            r#"(footprint "Part:Exact" (uuid "aaaaaaaa-0000-4000-8000-000000000001") {property} (future_geometry (xy 1 2)))"#
        );
        let b = format!(
            r#"(footprint "Part:Exact" (uuid "aaaaaaaa-0000-4000-8000-000000000001") (future_geometry (xy 1 2)) {property})"#
        );
        let board = |p: &str| {
            LiveBoardText::parse_synchronization(&format!("(kicad_pcb (version 20260206) {p})"))
                .unwrap()
        };
        let expected = synchronization_definitions(&board(&a)).unwrap();
        assert_eq!(expected, synchronization_definitions(&board(&b)).unwrap());
        for changed in [
            b.replace("retain", "changed"),
            b.replace("at 1 2", "at 2 1"),
            b.replace("opaque 7", "opaque 8"),
        ] {
            assert_ne!(
                expected,
                synchronization_definitions(&board(&changed)).unwrap()
            );
        }
    }
}

#[cfg(test)]
mod routing_comparison_tests {
    use super::*;
    #[test]
    fn routing_permutations_keep_complete_tokens_and_nonroute_order() {
        let a = r#"(segment (start 0 0) (end 1 0) (width 0.2) (layer F.Cu) (net OLD) (uuid a) (future (ordered 1 2)))"#;
        let b =
            r#"(arc (start 1 0) (mid 2 1) (end 3 0) (width 0.3) (layer F.Cu) (net OLD) (uuid b))"#;
        let wrap = |left: &str, right: &str| {
            format!("(kicad_pcb (version 20260206) {left} (future_root 7) {right})")
        };
        let source = canonical_direct_routes(&wrap(a, b)).unwrap();
        assert_eq!(source, canonical_direct_routes(&wrap(b, a)).unwrap());
        for changed in [
            a.replace("width 0.2", "width 0.3"),
            a.replace("end 1 0", "end 2 0"),
            a.replace("net OLD", "net NEW"),
            a.replace("ordered 1 2", "ordered 2 1"),
        ] {
            assert_ne!(source, canonical_direct_routes(&wrap(&changed, b)).unwrap());
        }
        assert_ne!(
            source,
            canonical_direct_routes(&wrap(a, b).replace("future_root 7", "future_root 8")).unwrap()
        );
        assert!(canonical_direct_routes(&wrap(a, a)).is_err());
    }
}
