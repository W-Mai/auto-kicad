//! Complete preview/source correspondence with receipt-bound creation identities.
use crate::{
    geometry::{GeometryMutationReceipts, PreparedGeometryChange},
    live_geometry::{FilledCopperEvidence, LiveGeometryEvidence},
    refill_verification::VerifiedRefillContent,
};
use kicad_checks::refill::ClosedRefillResult;
use kicad_model::{
    StableId,
    geometry::{GeometryRequest, ZoneEdit},
};
use protobuf::Message;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryCreationIdentity {
    pub kind: String,
    pub logical_id: String,
    pub native_uuid: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct GeometryContent {
    native_definitions: String,
    board_definitions: String,
    copper: FilledCopperEvidence,
}

/// Complete baseline content across editor instances; source/rule bindings remain separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryBaselineEvidence(GeometryContent);
impl GeometryBaselineEvidence {
    pub fn capture(observed: &crate::geometry::GeometryObservation) -> Result<Self, String> {
        Ok(Self(content(observed.live(), &BTreeMap::new())?))
    }
}

/// Verified content correspondence, not topology, electrical acceptance or source approval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryCandidateEvidence {
    intent_digest: String,
    before: GeometryContent,
    after: GeometryContent,
    creations: Vec<GeometryCreationIdentity>,
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn canonical_intent(request: &GeometryRequest) -> Result<String, String> {
    let mut value = request.clone();
    value.observed_revision =
        kicad_model::RevisionDigest::new("0".repeat(64)).map_err(|e| e.to_string())?;
    let board = StableId::new("candidate-board").map_err(|e| e.to_string())?;
    for edit in &mut value.zones {
        let net = match edit {
            ZoneEdit::Create { definition, .. } => Some(&mut definition.net),
            ZoneEdit::Update { patch, .. } => patch.net.as_mut(),
            ZoneEdit::Delete { .. } => None,
        };
        if let Some(net) = net {
            net.net_id = kicad_model::board_net_id(&board, &net.name);
        }
    }
    Ok(digest(
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    ))
}

fn content(
    observed: &LiveGeometryEvidence,
    identities: &BTreeMap<String, String>,
) -> Result<GeometryContent, String> {
    observed.text.validate_native_zones(&observed.native)?;
    let projected = observed.text.project_created_geometry(identities)?;
    let mut native = BTreeMap::new();
    for (id, zone) in observed.native.zones() {
        let mapped = identities.get(id).unwrap_or(id);
        let mut zone = zone.clone();
        zone.id.as_mut().ok_or("zone UUID missing")?.value = mapped.clone();
        zone.parent = None.into();
        zone.filled = false;
        zone.filled_polygons.clear();
        native.insert(
            ("zone", mapped),
            zone.write_to_bytes().map_err(|e| e.to_string())?,
        );
    }
    for (id, shape) in observed.native.shapes() {
        let mapped = identities.get(id).unwrap_or(id);
        let mut shape = shape.clone();
        shape.id.as_mut().ok_or("shape UUID missing")?.value = mapped.clone();
        shape.parent = None.into();
        native.insert(
            ("shape", mapped),
            shape.write_to_bytes().map_err(|e| e.to_string())?,
        );
    }
    let bytes =
        serde_json::to_vec(&native.into_iter().collect::<Vec<_>>()).map_err(|e| e.to_string())?;
    Ok(GeometryContent {
        native_definitions: digest(&bytes),
        board_definitions: projected.refill_definition_digest()?,
        copper: projected.filled_copper()?,
    })
}

impl GeometryCandidateEvidence {
    pub fn baseline(&self) -> GeometryBaselineEvidence {
        GeometryBaselineEvidence(self.before.clone())
    }

    /// Each caller must establish saved/live reconciliation and bind all observations to its editor/document.
    /// This checks definition receipts and independent refill content before constructing comparable evidence.
    pub fn capture(
        prepared: &PreparedGeometryChange,
        receipts: &GeometryMutationReceipts,
        before_fill: &LiveGeometryEvidence,
        after: &LiveGeometryEvidence,
        independent: &ClosedRefillResult,
    ) -> Result<Self, String> {
        prepared.verify_definitions(before_fill, receipts)?;
        VerifiedRefillContent::verify(before_fill, after, independent)?;
        let mut creations = Vec::new();
        let zone_ids = prepared
            .compiled()
            .request()
            .zones
            .iter()
            .filter_map(|z| match z {
                ZoneEdit::Create { zone_id, .. } => Some(zone_id),
                _ => None,
            });
        for (id, receipt) in zone_ids.zip(&receipts.zones().created) {
            creations.push(GeometryCreationIdentity {
                kind: "zone".into(),
                logical_id: id.to_string(),
                native_uuid: receipt.source_uuid().into(),
            });
        }
        if let Some(outline) = prepared.compiled().outline() {
            for (intent, receipt) in outline.primitives().iter().zip(receipts.outlines()) {
                creations.push(GeometryCreationIdentity {
                    kind: "outline".into(),
                    logical_id: intent.target_id.to_string(),
                    native_uuid: receipt.source_uuid.clone(),
                });
            }
        }
        let identities = creation_map(&creations)?;
        Ok(Self {
            intent_digest: canonical_intent(prepared.compiled().request())?,
            before: content(prepared.before(), &BTreeMap::new())?,
            after: content(after, &identities)?,
            creations,
        })
    }

    /// Return exact preview UUID to source UUID correspondence only after all complete content agrees.
    pub fn equivalent_to(&self, source: &Self) -> Result<BTreeMap<String, String>, String> {
        if self.intent_digest != source.intent_digest
            || self.before != source.before
            || self.after != source.after
        {
            return Err(
                "geometry candidate intent, original content or complete result differs".into(),
            );
        }
        let keyed = |items: &[GeometryCreationIdentity]| {
            items
                .iter()
                .map(|c| {
                    (
                        (c.kind.clone(), c.logical_id.clone()),
                        c.native_uuid.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let preview = keyed(&self.creations);
        let actual = keyed(&source.creations);
        if preview.len() != self.creations.len()
            || actual.len() != source.creations.len()
            || !preview.keys().eq(actual.keys())
        {
            return Err("geometry creation correspondence is ambiguous".into());
        }
        Ok(preview
            .into_iter()
            .map(|(key, id)| (id, actual[&key].clone()))
            .collect())
    }
}

fn creation_map(
    creations: &[GeometryCreationIdentity],
) -> Result<BTreeMap<String, String>, String> {
    let mut ids = BTreeMap::new();
    let mut aliases = std::collections::BTreeSet::new();
    for c in creations {
        let key = serde_json::to_vec(&(&c.kind, &c.logical_id)).map_err(|e| e.to_string())?;
        let alias = format!("created:{}", digest(&key));
        if ids.insert(c.native_uuid.clone(), alias.clone()).is_some() || !aliases.insert(alias) {
            return Err("geometry creation identities repeat".into());
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(id: &str) -> GeometryCandidateEvidence {
        let copper = crate::live_geometry::LiveBoardText::parse(
            "(kicad_pcb (version 20260206) (zone (uuid retained)))",
        )
        .unwrap()
        .filled_copper()
        .unwrap();
        let content = GeometryContent {
            native_definitions: "native".into(),
            board_definitions: "text".into(),
            copper,
        };
        GeometryCandidateEvidence {
            intent_digest: "intent".into(),
            before: content.clone(),
            after: content,
            creations: vec![GeometryCreationIdentity {
                kind: "zone".into(),
                logical_id: "new".into(),
                native_uuid: id.into(),
            }],
        }
    }

    #[test]
    fn candidate_equivalence_requires_complete_baseline_intent_and_result() {
        let preview = evidence("preview");
        let source = evidence("source");
        assert_eq!(
            preview.equivalent_to(&source).unwrap(),
            BTreeMap::from([("preview".into(), "source".into())])
        );
        for mode in 0..8 {
            let mut changed = source.clone();
            match mode {
                0 => changed.intent_digest.push('x'),
                1 => changed.before.native_definitions.push('x'),
                2 => changed.before.board_definitions.push('x'),
                3 => changed.after.native_definitions.push('x'),
                4 => changed.after.board_definitions.push('x'),
                5 => changed.creations[0].kind = "outline".into(),
                6 => changed.creations[0].logical_id = "other".into(),
                _ => changed.creations.push(changed.creations[0].clone()),
            }
            assert!(preview.equivalent_to(&changed).is_err(), "{mode}");
        }
        let mut changed = source.clone();
        changed.after.copper = crate::live_geometry::LiveBoardText::parse(
            "(kicad_pcb (version 20260206) (zone (uuid retained) (fill yes)))",
        )
        .unwrap()
        .filled_copper()
        .unwrap();
        assert!(preview.equivalent_to(&changed).is_err());
        assert!(creation_map(&[source.creations[0].clone(), source.creations[0].clone()]).is_err());
    }

    #[test]
    fn canonical_intent_changes_only_editor_revision_and_board_scoped_net_ids() {
        let mut json: serde_json::Value = serde_json::from_str(include_str!(
            "../../../scripts/fixtures/geometry-candidate-request.json"
        ))
        .unwrap();
        let v = &mut json[0];
        v["observed_revision"] = serde_json::json!("1".repeat(64));
        v["outline"]["remove_ids"] = serde_json::json!(["old"]);
        v["zones"][0]["definition"]["net"]["net_id"] = serde_json::json!("source-board-net");
        let request: GeometryRequest = serde_json::from_value(v.clone()).unwrap();
        let expected = canonical_intent(&request).unwrap();
        v["observed_revision"] = serde_json::json!("2".repeat(64));
        v["zones"][0]["definition"]["net"]["net_id"] = serde_json::json!("shadow-board-net");
        assert_eq!(
            expected,
            canonical_intent(&serde_json::from_value(v.clone()).unwrap()).unwrap()
        );
        for (path, value) in [
            ("/zones/0/definition/net/name", serde_json::json!("other")),
            (
                "/zones/0/definition/clearance_nm",
                serde_json::json!(200001),
            ),
            (
                "/zones/0/definition/polygon/exterior/points/0/x_nm",
                serde_json::json!(10000001),
            ),
            ("/zones/0/zone_id", serde_json::json!("other")),
            ("/outline/remove_ids/0", serde_json::json!("other")),
            ("/refill", serde_json::json!("none")),
        ] {
            let mut changed = v.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert_ne!(
                expected,
                canonical_intent(&serde_json::from_value(changed).unwrap()).unwrap(),
                "{path}"
            );
        }
    }
}
