//! Bounded identity binding for generated zones whose native serialization omits UUIDs.
mod correspondence;
use super::filled_copper::{FillNode, canonical, point};
use super::*;
pub use correspondence::{
    ClosedGeneratedDrcSnapshot, GeneratedDrcSnapshot, GeneratedZoneCorrespondence,
};
use kicad_ipc::{base_types, board_types};
use protobuf::{Enum, Message};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Signature {
    kind: String,
    layer: i32,
    net: String,
    priority: u32,
    outline: Vec<FillNode>,
}

/// A bijective observation identity, not authorization to ignore generated content changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TeardropIdentity {
    native_uuid: String,
    correspondence_key: String,
    kind: String,
    layer: i32,
    net: String,
    text_definition_digest: String,
    text_content_digest: String,
    native_definition_digest: String,
    native_content_digest: String,
}
impl TeardropIdentity {
    pub fn native_uuid(&self) -> &str {
        &self.native_uuid
    }
    pub fn correspondence_key(&self) -> &str {
        &self.correspondence_key
    }
}

#[derive(Clone, Debug)]
struct GeneratedText {
    correspondence_key: String,
    definition_digest: String,
    content_digest: String,
    locked: bool,
    default_generated_hatch: bool,
    span: Span,
}

/// Keeps generated text separate from the complete remaining authored board.
/// Ordinary-zone parsing remains strict; this partition alone proves neither refill nor preservation.
#[derive(Clone, Debug)]
pub struct TeardropTextPartition {
    source: CstDocument,
    authored: LiveBoardText,
    generated: BTreeMap<Signature, GeneratedText>,
    full_text_digest: String,
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn layer_number(name: &str) -> Result<i32, String> {
    match name {
        "F.Mask" => Ok(board_types::BoardLayer::BL_F_Mask.value()),
        "B.Mask" => Ok(board_types::BoardLayer::BL_B_Mask.value()),
        _ => Ok(crate::native_copper_layer(
            name.parse::<kicad_model::CopperLayer>()
                .map_err(|e| e.to_string())?,
        )
        .native()
        .value()),
    }
}
fn teardrop_kind(zone: &Node) -> Result<Option<&str>, String> {
    let Some(attr) = unique_form(zone, "attr")? else {
        return Ok(None);
    };
    let Some(drop) = unique_form(attr, "teardrop")? else {
        return Ok(None);
    };
    if items(attr)?.len() != 2 || items(drop)?.len() != 2 {
        return Err("unclassified teardrop attributes".into());
    }
    let kind = scalar(unique_form(drop, "type")?.ok_or("teardrop type missing")?)?;
    match kind {
        "padvia" | "track_end" => Ok(Some(kind)),
        _ => Err("unknown generated teardrop type".into()),
    }
}
fn text_outline(zone: &Node) -> Result<Vec<FillNode>, String> {
    let polygon = unique_form(zone, "polygon")?.ok_or("teardrop outline missing")?;
    if items(polygon)?.len() != 2 {
        return Err("unclassified teardrop outline fields".into());
    }
    let pts = unique_form(polygon, "pts")?.ok_or("teardrop points missing")?;
    let mut nodes = Vec::new();
    for p in &items(pts)?[1..] {
        nodes.push(match tag(p)? {
            "xy" => FillNode::Point(point(p)?),
            "arc" => {
                if items(p)?.len() != 4 {
                    return Err("invalid teardrop arc".into());
                }
                FillNode::Arc(
                    point(unique_form(p, "start")?.ok_or("arc start")?)?,
                    point(unique_form(p, "mid")?.ok_or("arc mid")?)?,
                    point(unique_form(p, "end")?.ok_or("arc end")?)?,
                )
            }
            _ => return Err("unclassified teardrop point".into()),
        });
        if nodes.len() > 500_000 {
            return Err("teardrop point bound exceeded".into());
        }
    }
    if nodes.len() < 3 {
        return Err("teardrop outline has too few nodes".into());
    }
    Ok(canonical(nodes))
}

fn native_signature(zone: &board_types::Zone) -> Result<Signature, String> {
    if zone.type_.enum_value() != Ok(board_types::ZoneType::ZT_TEARDROP) || zone.layers.len() != 1 {
        return Err("native generated zone type or layers differ".into());
    }
    let Some(board_types::zone::Settings::CopperSettings(settings)) = &zone.settings else {
        return Err("native teardrop settings missing".into());
    };
    let kind = match settings
        .teardrop
        .as_ref()
        .ok_or("native teardrop classification missing")?
        .type_
        .enum_value()
    {
        Ok(board_types::ZoneTeardropType::ZTDT_VIA_PAD) => "padvia",
        Ok(board_types::ZoneTeardropType::ZTDT_TRACK_END) => "track_end",
        _ => return Err("native teardrop type unknown".into()),
    };
    let outline = zone
        .outline
        .as_ref()
        .ok_or("native teardrop outline missing")?;
    if outline.polygons.len() != 1 || !outline.polygons[0].holes.is_empty() {
        return Err("native teardrop outline is not a single boundary".into());
    }
    let line = outline.polygons[0]
        .outline
        .as_ref()
        .ok_or("native teardrop boundary missing")?;
    if !line.closed || line.nodes.len() < 3 || line.nodes.len() > 500_000 {
        return Err("native teardrop boundary invalid".into());
    }
    let point = |p: Option<&base_types::Vector2>| -> Result<(i64, i64), String> {
        let p = p.ok_or("native teardrop point missing")?;
        if p.x_nm.unsigned_abs() > 2_000_000_000 || p.y_nm.unsigned_abs() > 2_000_000_000 {
            return Err("native teardrop coordinate exceeds bound".into());
        }
        Ok((p.x_nm, p.y_nm))
    };
    let nodes = line
        .nodes
        .iter()
        .map(|n| match &n.geometry {
            Some(base_types::poly_line_node::Geometry::Point(p)) => {
                Ok(FillNode::Point(point(Some(p))?))
            }
            Some(base_types::poly_line_node::Geometry::Arc(a)) => Ok(FillNode::Arc(
                point(a.start.as_ref())?,
                point(a.mid.as_ref())?,
                point(a.end.as_ref())?,
            )),
            _ => Err("native teardrop node missing".into()),
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Signature {
        kind: kind.into(),
        layer: zone.layers[0].value(),
        net: settings.net.as_ref().map_or("", |n| n.name.as_str()).into(),
        priority: zone.priority,
        outline: canonical(nodes),
    })
}

impl TeardropTextPartition {
    pub fn parse(text: &str) -> Result<Self, String> {
        preflight(text)?;
        let cst = parse_one(text).map_err(|e| e.to_string())?;
        let root = cst.nodes.first().ok_or("missing board root")?;
        if tag(root)? != "kicad_pcb" {
            return Err("expected native board root".into());
        }
        let mut generated = BTreeMap::new();
        let mut authored = Vec::new();
        let mut total_points = 0usize;
        for node in &items(root)?[1..] {
            let Node::List { span, .. } = node else {
                return Err("unexpected board scalar".into());
            };
            let kind = if tag(node)? == "zone" {
                teardrop_kind(node)?
            } else {
                None
            };
            if let Some(kind) = kind {
                if unique_form(node, "uuid")?.is_some()
                    || unique_form(node, "name")?.is_some()
                    || unique_form(node, "layers")?.is_some()
                {
                    return Err("unexpected generated zone identity, name or layers".into());
                }
                let layer = layer_number(scalar(
                    unique_form(node, "layer")?.ok_or("generated zone layer missing")?,
                )?)?;
                let net = unique_form(node, "net")?
                    .map(scalar)
                    .transpose()?
                    .unwrap_or("");
                let priority = unique_form(node, "priority")?
                    .map(scalar)
                    .transpose()?
                    .unwrap_or("0")
                    .parse::<u32>()
                    .map_err(|_| "invalid teardrop priority")?;
                let locked = match unique_form(node, "locked")?.map(scalar).transpose()? {
                    None | Some("no") => false,
                    Some("yes") => true,
                    _ => return Err("invalid generated lock".into()),
                };
                let signature = Signature {
                    kind: kind.into(),
                    layer,
                    net: net.into(),
                    priority,
                    outline: text_outline(node)?,
                };
                total_points += signature.outline.len();
                if total_points > 500_000 {
                    return Err("generated outline point bound exceeded".into());
                }
                let mut hash = Sha256::new();
                feed_zone_definition(&mut hash, text, node, &mut total_points)?;
                let evidence = GeneratedText {
                    correspondence_key: hash_bytes(
                        &serde_json::to_vec(&signature).map_err(|e| e.to_string())?,
                    ),
                    definition_digest: hex::encode(hash.finalize()),
                    content_digest: structural_digest(text, node),
                    locked,
                    span: *span,
                    default_generated_hatch: match unique_form(node, "hatch")? {
                        Some(hatch) => {
                            let fields = items(hatch)?;
                            fields.len() == 3
                                && atom(&fields[1])? == "none"
                                && atom(&fields[2])? == "0.1"
                        }
                        None => false,
                    },
                };
                if generated.insert(signature, evidence).is_some() {
                    return Err("ambiguous generated teardrop correspondence".into());
                }
                if generated.len() > MAX_ZONES {
                    return Err("generated zone count exceeds bound".into());
                }
            } else {
                authored.push(&text[span.start..span.end]);
            }
        }
        let authored = LiveBoardText::parse(&format!("(kicad_pcb {})", authored.join(" ")))?;
        if authored.zones().len() + generated.len() > MAX_ZONES {
            return Err("combined zone count exceeds bound".into());
        }
        Ok(Self {
            authored,
            generated,
            full_text_digest: structural_digest(text, root),
            source: cst,
        })
    }
    /// All non-generated native text, including complete pad/via/track settings and ordinary zones.
    pub fn authored(&self) -> &LiveBoardText {
        &self.authored
    }
    pub fn raw_text(&self) -> &str {
        &self.source.raw
    }

    pub fn generated_count(&self) -> usize {
        self.generated.len()
    }
    pub fn full_text_digest(&self) -> &str {
        &self.full_text_digest
    }

    pub(crate) fn generated_content(&self) -> Result<BTreeMap<String, String>, String> {
        self.generated
            .values()
            .map(|text| Ok((text.correspondence_key.clone(), text.content_digest.clone())))
            .collect()
    }

    pub(crate) fn has_default_generated_hatch(&self, key: &str) -> Result<bool, String> {
        for text in self.generated.values() {
            if text.correspondence_key == key {
                return Ok(text.default_generated_hatch);
            }
        }
        Err("generated hatch correspondence missing".into())
    }

    /// Pair generated text with complete native observations; UUID/parent projection is explicit in each digest.
    pub fn bind(&self, native: &NativeBoardGeometry) -> Result<Vec<TeardropIdentity>, String> {
        self.bind_zones(native.zones())
    }

    fn bind_zones(
        &self,
        zones: &BTreeMap<String, board_types::Zone>,
    ) -> Result<Vec<TeardropIdentity>, String> {
        let mut ordinary = BTreeSet::new();
        let mut used = BTreeSet::new();
        let mut identities = Vec::new();
        for (id, zone) in zones {
            if zone.type_.enum_value() == Ok(board_types::ZoneType::ZT_TEARDROP) {
                let signature = native_signature(zone)?;
                let text = self
                    .generated
                    .get(&signature)
                    .ok_or("native teardrop has no exact text correspondence")?;
                if !used.insert(signature.clone()) {
                    return Err("native teardrop correspondence is ambiguous".into());
                }
                validate_zone_lock(text.locked, zone)?;
                let mut payload = zone.clone();
                payload.id = None.into();
                payload.parent = None.into();
                let native_content_digest =
                    hash_bytes(&payload.write_to_bytes().map_err(|e| e.to_string())?);
                payload.filled = false;
                payload.filled_polygons.clear();
                identities.push(TeardropIdentity {
                    native_uuid: id.clone(),
                    correspondence_key: text.correspondence_key.clone(),
                    kind: signature.kind,
                    layer: signature.layer,
                    net: signature.net,
                    text_definition_digest: text.definition_digest.clone(),
                    text_content_digest: text.content_digest.clone(),
                    native_definition_digest: hash_bytes(
                        &payload.write_to_bytes().map_err(|e| e.to_string())?,
                    ),
                    native_content_digest,
                });
            } else {
                let text = self
                    .authored
                    .zones()
                    .get(id)
                    .ok_or("ordinary native zone missing from authored text")?;
                validate_zone_lock(text.locked, zone)?;
                ordinary.insert(id);
            }
        }
        if ordinary.len() != self.authored.zones().len() || used.len() != self.generated.len() {
            return Err("native and text generated/ordinary sets differ".into());
        }
        identities.sort_by(|a, b| a.correspondence_key.cmp(&b.correspondence_key));
        Ok(identities)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const DROP: &str = r#"(zone (layer "F.Cu") (net "GND") (priority 30000) (attr (teardrop (type padvia))) (polygon (pts (xy 0 0) (xy 2 0) (xy 1 1))) (fill yes) (future 7))"#;
    fn board(drop: &str) -> String {
        format!(
            "(kicad_pcb (version 20260206) (future_board 8) (pad (future_pad 9)) (zone (uuid ordinary) (future_zone 10)) {drop})"
        )
    }
    #[test]
    fn partition_keeps_authored_unknowns_and_all_generated_content() {
        let text = board(DROP);
        assert!(LiveBoardText::parse(&text).is_err());
        let partition = TeardropTextPartition::parse(&text).unwrap();
        assert_eq!(partition.generated_count(), 1);
        assert_eq!(partition.raw_text(), text);
        assert_eq!(partition.authored().zones().len(), 1);
        assert!(partition.authored().raw_text().contains("future_pad 9"));
        for (from, to) in [
            ("future_board 8", "future_board 80"),
            ("future_pad 9", "future_pad 90"),
            ("future_zone 10", "future_zone 100"),
        ] {
            let changed = TeardropTextPartition::parse(&text.replace(from, to)).unwrap();
            assert_ne!(
                partition.authored().structure_digest(),
                changed.authored().structure_digest()
            );
            assert_ne!(partition.full_text_digest(), changed.full_text_digest());
        }
        let changed = TeardropTextPartition::parse(&text.replace("future 7", "future 70")).unwrap();
        assert_eq!(
            partition.generated.keys().collect::<Vec<_>>(),
            changed.generated.keys().collect::<Vec<_>>()
        );
        assert_ne!(
            partition
                .generated
                .values()
                .next()
                .unwrap()
                .definition_digest,
            changed.generated.values().next().unwrap().definition_digest
        );
        assert_ne!(
            partition.generated.values().next().unwrap().content_digest,
            changed.generated.values().next().unwrap().content_digest
        );
    }
    #[test]
    fn full_generated_result_keeps_mask_fill_flags_points_and_unknown_fields() {
        let fill = "(filled_polygon (layer F.Mask) (island yes) (pts (xy 0 0) (xy 2 0) (xy 1 1)))";
        let drop = DROP
            .replace("F.Cu", "F.Mask")
            .replace("(future 7)", &format!("(future 7) {fill}"));
        let original = TeardropTextPartition::parse(&board(&drop)).unwrap();
        for (from, to) in [
            ("future 7", "future 8"),
            ("(island yes)", ""),
            ("(layer F.Mask)", "(layer B.Mask)"),
            ("xy 2 0", "xy 2.000001 0"),
        ] {
            let changed = TeardropTextPartition::parse(&board(&drop.replace(from, to))).unwrap();
            assert_ne!(original.generated_content(), changed.generated_content());
        }
        let moved = board(&drop)
            .replace(&drop, "")
            .replace("(version 20260206)", &format!("(version 20260206) {drop}"));
        assert_eq!(
            original.generated_content(),
            TeardropTextPartition::parse(&moved)
                .unwrap()
                .generated_content()
        );
    }
    #[test]
    fn only_unambiguous_classified_generated_zones_can_omit_ids() {
        for drop in [
            format!("{DROP} {DROP}"),
            DROP.replace("padvia", "future"),
            DROP.replace("(attr (teardrop (type padvia)))", ""),
            DROP.replace("(zone ", "(zone (uuid forged) "),
            DROP.replace("F.Cu", "Dwgs.User"),
            DROP.replace("(type padvia)", "(type padvia) (future 1)"),
            DROP.replace("(layer", "(name custom) (layer"),
        ] {
            assert!(
                TeardropTextPartition::parse(&board(&drop)).is_err(),
                "{drop}"
            );
        }
        assert!(TeardropTextPartition::parse(&board(DROP).replace("(uuid ordinary)", "")).is_err());
        for layer in ["F.Cu", "In1.Cu", "B.Cu", "F.Mask", "B.Mask"] {
            assert_eq!(
                TeardropTextPartition::parse(&board(&DROP.replace("F.Cu", layer)))
                    .unwrap()
                    .generated_count(),
                1
            );
        }
    }
    #[test]
    fn generated_signature_is_exact_and_does_not_grant_content_equivalence() {
        let a = TeardropTextPartition::parse(&board(DROP)).unwrap();
        let reversed = DROP.replace("(xy 0 0) (xy 2 0) (xy 1 1)", "(xy 1 1) (xy 2 0) (xy 0 0)");
        let b = TeardropTextPartition::parse(&board(&reversed)).unwrap();
        assert_eq!(
            a.generated.keys().collect::<Vec<_>>(),
            b.generated.keys().collect::<Vec<_>>()
        );
        assert_ne!(a.full_text_digest(), b.full_text_digest());
        for (from, to) in [
            ("xy 2 0", "xy 2.000001 0"),
            ("GND", "VCC"),
            ("30000", "30001"),
            ("F.Cu", "B.Cu"),
            ("padvia", "track_end"),
        ] {
            let b = TeardropTextPartition::parse(&board(&DROP.replace(from, to))).unwrap();
            assert_ne!(
                a.generated.keys().collect::<Vec<_>>(),
                b.generated.keys().collect::<Vec<_>>()
            );
        }
    }
}

#[cfg(test)]
mod native_binding_tests {
    use super::*;
    use protobuf::{EnumOrUnknown, MessageField};
    fn fixture() -> (TeardropTextPartition, BTreeMap<String, board_types::Zone>) {
        let text = "(kicad_pcb (version 20260206) (zone (uuid ordinary)) (zone (layer F.Cu) (net GND) (attr (teardrop (type padvia))) (polygon (pts (xy 0 0) (xy 2 0) (xy 1 1)))))";
        let zone = board_types::Zone {
            id: MessageField::some(base_types::KIID {
                value: "generated".into(),
                ..Default::default()
            }),
            type_: EnumOrUnknown::new(board_types::ZoneType::ZT_TEARDROP),
            layers: vec![EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu)],
            outline: MessageField::some(base_types::PolySet {
                polygons: vec![base_types::PolygonWithHoles {
                    outline: MessageField::some(base_types::PolyLine {
                        closed: true,
                        nodes: [(0, 0), (2000000, 0), (1000000, 1000000)]
                            .map(|(x_nm, y_nm)| base_types::PolyLineNode {
                                geometry: Some(base_types::poly_line_node::Geometry::Point(
                                    base_types::Vector2 {
                                        x_nm,
                                        y_nm,
                                        ..Default::default()
                                    },
                                )),
                                ..Default::default()
                            })
                            .to_vec(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            settings: Some(board_types::zone::Settings::CopperSettings(
                board_types::CopperZoneSettings {
                    net: MessageField::some(board_types::Net {
                        name: "GND".into(),
                        ..Default::default()
                    }),
                    teardrop: MessageField::some(board_types::ZoneTeardropSettings {
                        type_: EnumOrUnknown::new(board_types::ZoneTeardropType::ZTDT_VIA_PAD),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        (
            TeardropTextPartition::parse(text).unwrap(),
            BTreeMap::from([
                ("ordinary".into(), board_types::Zone::default()),
                ("generated".into(), zone),
            ]),
        )
    }
    #[test]
    fn native_binding_is_bijective_and_retains_unknown_fields() {
        let (partition, zones) = fixture();
        let baseline = partition.bind_zones(&zones).unwrap();
        for mode in 0..5 {
            let mut changed = zones.clone();
            match mode {
                0 => {
                    changed.remove("generated");
                }
                1 => {
                    changed.insert("duplicate".into(), changed["generated"].clone());
                }
                2 => {
                    changed.remove("ordinary");
                }
                3 => {
                    changed.get_mut("generated").unwrap().layers[0] =
                        EnumOrUnknown::new(board_types::BoardLayer::BL_B_Cu)
                }
                _ => {
                    changed.get_mut("ordinary").unwrap().locked =
                        EnumOrUnknown::new(LockedState::LS_LOCKED)
                }
            }
            assert!(partition.bind_zones(&changed).is_err());
        }
        let mut changed = zones.clone();
        changed
            .get_mut("generated")
            .unwrap()
            .special_fields
            .mut_unknown_fields()
            .add_varint(999, 42);
        let unknown = partition.bind_zones(&changed).unwrap();
        assert_eq!(
            baseline[0].correspondence_key,
            unknown[0].correspondence_key
        );
        assert_ne!(
            baseline[0].native_content_digest,
            unknown[0].native_content_digest
        );
        assert_ne!(
            baseline[0].native_definition_digest,
            unknown[0].native_definition_digest
        );
        let mut changed = zones;
        changed.get_mut("generated").unwrap().filled = true;
        let fill = partition.bind_zones(&changed).unwrap();
        assert_eq!(
            baseline[0].native_definition_digest,
            fill[0].native_definition_digest
        );
        assert_ne!(
            baseline[0].native_content_digest,
            fill[0].native_content_digest
        );
    }
}
