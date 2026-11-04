//! Exact inheritance and physical pad correspondence for source-bound replacement templates.
use super::*;

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ReplacementBinding {
    pub before_reference: String,
    pub pad_mapping: BTreeMap<String, String>,
    pub original_footprint_sha256: String,
    pub inherited_footprint_sha256: String,
    pub manual_field_ids: Vec<String>,
}

const PRESERVED: &[&str] = &[
    "layer",
    "at",
    "uuid",
    "path",
    "sheetname",
    "sheetfile",
    "units",
    "locked",
    "clearance",
    "solder_mask_margin",
    "solder_paste_margin",
    "solder_paste_ratio",
    "thermal_width",
    "thermal_gap",
    "zone_connect",
    "autoplace_cost90",
    "autoplace_cost180",
];
const SOURCE_FORMS: &[&str] = &[
    "descr",
    "tags",
    "fp_text",
    "fp_text_box",
    "fp_line",
    "fp_rect",
    "fp_circle",
    "fp_arc",
    "fp_poly",
    "fp_curve",
    "pad",
    "model",
    "embedded_fonts",
    "duplicate_pad_numbers_are_jumpers",
];
const MANUAL_ATTRIBUTES: &[&str] = &[
    "exclude_from_pos_files",
    "exclude_from_bom",
    "dnp",
    "allow_missing_courtyard",
];

fn raw<'a>(board: &'a SavedSynchronizationBoard, node: &Node) -> &'a str {
    let s = span(node);
    &board.text()[s.start..s.end]
}
fn rewritten(mut text: String, mut patches: Vec<(Span, String)>) -> Result<String, String> {
    patches.sort_by_key(|(span, _)| (span.start, span.end));
    if patches
        .windows(2)
        .any(|pair| pair[0].0.end > pair[1].0.start)
    {
        return Err("overlapping replacement inheritance spans".into());
    }
    for (span, value) in patches.into_iter().rev() {
        text.replace_range(span.start..span.end, &value);
    }
    Ok(text)
}
fn field_with_identity(
    board: &SavedSynchronizationBoard,
    field: &Node,
    id: &str,
) -> Result<String, String> {
    let full = span(field);
    let old = span(required(field, "uuid")?);
    rewritten(
        raw(board, field).into(),
        vec![(
            Span {
                start: old.start - full.start,
                end: old.end - full.start,
            },
            format!("(uuid {})", quoted(id)?),
        )],
    )
}

fn map_pads(
    original: &SavedSynchronizationBoard,
    replacement: &SavedSynchronizationBoard,
    root_id: &str,
    connected: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let old_node = original.node(root_id)?;
    let new_node = replacement.node(root_id)?;
    let mut available = children(new_node, "pad")?;
    available.sort_by_key(|pad| identity(pad).unwrap_or_default());
    let mut old = children(old_node, "pad")?;
    // Reserve exact matches for connected pads before disconnected contacts consume any slot.
    old.sort_by_key(|pad| {
        (
            !identity(pad).is_ok_and(|id| connected.contains(&id)),
            identity(pad).unwrap_or_default(),
        )
    });
    if old.len() != available.len() {
        return Err("replacement_physical_pad_count_requires_separate_qualification".into());
    }
    let mut mapping = BTreeMap::new();
    for pad in old {
        let id = identity(pad)?;
        let number = atom(&items(pad)?[1])?;
        let signature = templates::physical_pad_digest(pad)?;
        let exact = available.iter().position(|candidate| {
            items(candidate)
                .ok()
                .and_then(|v| v.get(1))
                .and_then(|n| atom(n).ok())
                == Some(number)
                && templates::physical_pad_digest(candidate).is_ok_and(|v| v == signature)
        });
        let index = if let Some(index) = exact {
            index
        } else {
            if connected.contains(&id) {
                return Err(format!(
                    "replacement_connected_contact_requires_reroute: {id}"
                ));
            }
            available
                .iter()
                .position(|candidate| {
                    items(candidate)
                        .ok()
                        .and_then(|v| v.get(1))
                        .and_then(|n| atom(n).ok())
                        == Some(number)
                })
                .ok_or("replacement_physical_pad_numbers_require_separate_qualification")?
        };
        let candidate = available.remove(index);
        mapping.insert(id, identity(candidate)?);
    }
    if !available.is_empty() {
        return Err("replacement pad bijection is incomplete".into());
    }
    Ok(mapping)
}

fn observed_font_defaults(
    inherited: &SavedSynchronizationBoard,
    native_original: &SavedSynchronizationBoard,
    id: &str,
) -> Result<String, String> {
    let old_properties = properties(native_original.node(id)?)?;
    let inherited_properties = properties(inherited.node(id)?)?;
    let mut patches = Vec::new();
    for (name, before) in old_properties {
        let Some(field) = inherited_properties.get(&name) else {
            continue;
        };
        let font = |node: &Node| -> Result<Option<Node>, String> {
            let Some(effects) = optional(node, "effects")? else {
                return Ok(None);
            };
            optional(effects, "font").map(|node| node.cloned())
        };
        if let (Some(old_font), Some(inherited_font)) = (font(before)?, font(field)?)
            && optional(&inherited_font, "thickness")?.is_none()
            && let Some(observed) = optional(&old_font, "thickness")?
        {
            // Only an omitted stroke is materialized from the original native field observation.
            scalar(observed)?;
            let at = span(&inherited_font).end - 1;
            patches.push((
                Span { start: at, end: at },
                format!(" {}", raw(native_original, observed)),
            ));
        }
    }
    let normalized =
        SavedSynchronizationBoard::parse(&rewritten(inherited.text().into(), patches)?)?;
    Ok(normalized.footprint_text(id)?.into())
}

impl SavedSynchronizationBoard {
    pub(in crate::schematic_audit::synchronization) fn inherit_replacement(
        &self,
        template: &mut templates::SavedFootprintTemplate,
        connected: &BTreeSet<String>,
        seed: &str,
        native_original: &SavedSynchronizationBoard,
    ) -> Result<(), String> {
        let id = &template.root_id;
        let fp = self
            .footprints
            .get(id)
            .ok_or("unknown replacement footprint")?;
        if fp.locked || fp.board_only || self.has_zones || self.has_external_references(id)? {
            return Err("locked, board-only, zoned or externally referenced replacement requires separate qualification".into());
        }
        let node = self.node(id)?;
        for field in &items(node)?[2..] {
            if !matches!(field, Node::List { .. }) {
                return Err("unknown replacement footprint scalar grammar".into());
            }
            let name = tag(field)?;
            if !PRESERVED.contains(&name)
                && !SOURCE_FORMS.contains(&name)
                && !["property", "attr"].contains(&name)
            {
                return Err(format!(
                    "unknown affected replacement footprint form: {name}"
                ));
            }
        }
        let fresh = Self::parse(&format!(
            "(kicad_pcb (version 20260206) {})",
            template.footprint
        ))?;
        let fresh_node = fresh.node(id)?;
        let pad_mapping = map_pads(self, &fresh, id, connected)?;
        let old_props = properties(node)?;
        let new_props = properties(fresh_node)?;
        let mut patches = Vec::new();
        let mut append = String::new();
        for name in PRESERVED {
            let before = optional(node, name)?;
            let after = optional(fresh_node, name)?;
            match (before, after) {
                (Some(before), Some(after)) => {
                    patches.push((span(after), raw(self, before).into()))
                }
                (Some(before), None) => append.push_str(&format!("\n{}", raw(self, before))),
                (None, Some(after)) if !["at", "layer", "uuid"].contains(name) => {
                    patches.push((span(after), String::new()))
                }
                _ => {}
            }
        }
        let mut manual_ids = Vec::new();
        for (index, (name, field)) in old_props.iter().enumerate() {
            if let Some(new) = new_props.get(name) {
                patches.push((
                    span(new),
                    field_with_identity(self, field, &identity(new)?)?,
                ));
            } else {
                let allocated = templates::derived(seed, "manual-field", index);
                append.push_str(&format!(
                    "\n{}",
                    field_with_identity(self, field, &allocated)?
                ));
                manual_ids.push(allocated);
            }
        }
        let attr = |node: &Node| -> Result<Vec<String>, String> {
            optional(node, "attr")?
                .map(items)
                .transpose()?
                .unwrap_or(&[])
                .iter()
                .skip(1)
                .map(|n| atom(n).map(str::to_owned))
                .collect()
        };
        let old_attrs = attr(node)?;
        if old_attrs.iter().any(|v| {
            !MANUAL_ATTRIBUTES.contains(&v.as_str())
                && !["smd", "through_hole"].contains(&v.as_str())
        }) {
            return Err("unknown replacement footprint attribute".into());
        }
        let mut attrs = attr(fresh_node)?;
        attrs.retain(|v| !MANUAL_ATTRIBUTES.contains(&v.as_str()));
        attrs.extend(
            old_attrs
                .into_iter()
                .filter(|v| MANUAL_ATTRIBUTES.contains(&v.as_str())),
        );
        let rendered = if attrs.is_empty() {
            String::new()
        } else {
            format!("(attr {})", attrs.join(" "))
        };
        if let Some(old) = optional(fresh_node, "attr")? {
            patches.push((span(old), rendered));
        } else {
            append.push_str(&rendered);
        }
        // Seed intermediate pads with their exact old nets before the source compiler runs.
        for (old_id, new_id) in &pad_mapping {
            let old = fp
                .pads
                .iter()
                .find(|pad| &pad.uuid == old_id)
                .ok_or("missing mapped original pad")?;
            if old.net.is_empty() {
                continue;
            }
            let new = children(fresh_node, "pad")?
                .into_iter()
                .find(|pad| identity(pad).is_ok_and(|id| &id == new_id))
                .ok_or("missing mapped template pad")?;
            let end = span(new).end - 1;
            patches.push((
                Span { start: end, end },
                format!(" (net {})", quoted(&old.net)?),
            ));
        }
        let end = span(fresh_node).end - 1;
        patches.push((Span { start: end, end }, append));
        let inherited = Self::parse(&rewritten(fresh.text().into(), patches)?)?;
        let footprint = inherited.footprint_text(id)?.to_owned();
        let binding = ReplacementBinding {
            before_reference: fp.reference.clone(),
            pad_mapping,
            original_footprint_sha256: hash(self.footprint_text(id)?.as_bytes()),
            inherited_footprint_sha256: hash(footprint.as_bytes()),
            manual_field_ids: manual_ids.clone(),
        };
        let native_definition = observed_font_defaults(&inherited, native_original, id)?;
        template.evidence["native_inherited_sha256"] =
            serde_json::json!(hash(native_definition.as_bytes()));
        template.native_footprint = Some(native_definition);
        template.footprint = footprint;
        template.identities.extend(manual_ids);
        if inherited.identities() != &template.identities {
            return Err(
                "replacement inheritance changed the complete template identity set".into(),
            );
        }
        template.evidence["replacement"] =
            serde_json::to_value(&binding).map_err(|e| e.to_string())?;
        template.replacement = Some(binding);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ROOT: &str = "11111111-1111-4111-8111-111111111111";
    const OLD: &str = "22222222-2222-4222-8222-222222222222";
    const NEW: &str = "33333333-3333-4333-8333-333333333333";
    fn board(id: &str, position: &str, extra: &str) -> SavedSynchronizationBoard {
        SavedSynchronizationBoard::parse(&format!(
            r#"(kicad_pcb (version 20260206)
          (footprint "Test:Part" (uuid "{ROOT}") (layer "F.Cu") (at 10 20)
          (property "Reference" "R1") (property "Value" "manual")
          (pad "1" smd roundrect (at {position} 0) (size 1 2) (layers "F.Cu" "F.Mask")
            (uuid "{id}") {extra})))"#
        ))
        .unwrap()
    }
    #[test]
    fn replaced_identities_cannot_bypass_complete_connected_contact_preservation() {
        let before = board(OLD, "1.00", r#"(net "OLD") (pinfunction "old")"#);
        let after = board(NEW, "1", r#"(net "NEW") (pintype "passive")"#);
        let connected = BTreeSet::from([OLD.into()]);
        let expected = BTreeMap::from([(OLD.into(), NEW.into())]);
        assert_eq!(
            map_pads(&before, &after, ROOT, &connected).unwrap(),
            expected
        );
        let moved = board(NEW, "1.000001", "");
        assert!(
            map_pads(&before, &moved, ROOT, &connected)
                .unwrap_err()
                .contains("reroute")
        );
        assert_eq!(
            map_pads(&before, &moved, ROOT, &BTreeSet::new()).unwrap(),
            expected
        );
        for changed in [
            after.text().replace("(size 1 2)", "(size 2 2)"),
            after.text().replace("F.Cu", "B.Cu"),
            after
                .text()
                .replace("(size 1 2)", "(size 1 2) (unknown physical)"),
        ] {
            let changed = SavedSynchronizationBoard::parse(&changed).unwrap();
            assert!(map_pads(&before, &changed, ROOT, &connected).is_err());
        }
        let renamed =
            SavedSynchronizationBoard::parse(&after.text().replace("(pad \"1\"", "(pad \"2\""))
                .unwrap();
        assert!(map_pads(&before, &renamed, ROOT, &BTreeSet::new()).is_err());
    }
    #[test]
    fn omitted_field_stroke_uses_only_the_original_native_font_value() {
        let original = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{ROOT}")
            (property "Reference" "R1" (effects (font (size 1 1))))
            (property "Value" "manual" (effects (font (size 1 1) (thickness 0.17))))))"#
        );
        let native = original.replace("(size 1 1))", "(size 1 1) (thickness 0.15))");
        let parsed = SavedSynchronizationBoard::parse(&original).unwrap();
        let native = SavedSynchronizationBoard::parse(&native).unwrap();
        let result = observed_font_defaults(&parsed, &native, ROOT).unwrap();
        assert!(result.contains("(thickness 0.15)"));
        assert!(result.contains("(thickness 0.17)"));
        assert_eq!(parsed.text(), original);
        assert_eq!(
            observed_font_defaults(&parsed, &parsed, ROOT).unwrap(),
            parsed.footprint_text(ROOT).unwrap()
        );
    }
    #[test]
    fn manual_property_identity_replacement_retains_every_other_source_byte() {
        let text = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{ROOT}")
          (property "Reference" "R1") (property "Value" "manual")
          (property "Private" "literal" (at 1.234567 -2 37.123) (layer "B.Fab")
            (hide yes) (uuid "{OLD}") (effects (font (size 1.2 3.4)) (justify mirror)) (future "retained"))))"#
        );
        let board = SavedSynchronizationBoard::parse(&text).unwrap();
        let field = properties(board.node(ROOT).unwrap()).unwrap()["Private"];
        assert_eq!(
            field_with_identity(&board, field, NEW).unwrap(),
            raw(&board, field).replace(OLD, NEW)
        );
    }
}
