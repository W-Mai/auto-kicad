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
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_digest: Option<String>,
}

/// Complete baseline content across editor instances; source/rule bindings remain separate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryBaselineEvidence(GeometryContent);
impl GeometryBaselineEvidence {
    pub fn capture(observed: &crate::geometry::GeometryObservation) -> Result<Self, String> {
        let mut value = content(observed.live(), &BTreeMap::new())?;
        if let Some(generated) = observed.generated().filter(|g| g.generated_count() > 0) {
            value.generated_digest = Some(generated.generated_correspondence().content_digest()?);
        }
        Ok(Self(value))
    }
}

/// Verified content correspondence, not topology, electrical acceptance or source approval.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeometryCandidateEvidence {
    intent_digest: String,
    before: GeometryContent,
    after: GeometryContent,
    creations: Vec<GeometryCreationIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_before: Option<crate::live_geometry::GeneratedZoneCorrespondence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated_after: Option<crate::live_geometry::GeneratedZoneCorrespondence>,
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
        generated_digest: None,
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
        if prepared
            .generated_before()
            .is_some_and(|g| g.generated_count() > 0)
        {
            return Err(
                "generated content requires complete generated candidate verification".into(),
            );
        }
        prepared.verify_definitions(before_fill, receipts)?;
        VerifiedRefillContent::verify(before_fill, after, independent)?;
        Self::from_verified(prepared, receipts, after, None)
    }

    /// Verify authored mutations and all generated content across refill and saved reload.
    /// The caller must establish the actual saved reload preceding before_fill.
    pub fn capture_generated(
        prepared: &PreparedGeometryChange,
        receipts: &GeometryMutationReceipts,
        before_fill: &crate::teardrop_refill::TeardropRefillObservation,
        filled: &crate::teardrop_refill::TeardropRefillObservation,
        reloaded: &crate::teardrop_refill::TeardropRefillObservation,
        independent: &ClosedRefillResult,
    ) -> Result<Self, String> {
        prepared.verify_generated_definitions_after_reload(before_fill, receipts)?;
        crate::teardrop_refill::VerifiedTeardropRefill::verify(
            before_fill,
            filled,
            reloaded,
            independent,
        )?;
        Self::from_verified(
            prepared,
            receipts,
            &reloaded.authored_geometry()?,
            Some(reloaded),
        )
    }

    /// Verify receipt-bound additive definitions and all copper against saved reload and independent refill.
    pub fn capture_generated_additions(
        prepared: &PreparedGeometryChange,
        receipts: &GeometryMutationReceipts,
        before_fill: &crate::geometry_additions::VerifiedAdditiveGeometryDefinitions,
        filled: &crate::teardrop_refill::TeardropRefillObservation,
        reloaded: &crate::teardrop_refill::TeardropRefillObservation,
        independent: &ClosedRefillResult,
    ) -> Result<Self, String> {
        let before_fill = before_fill.verify_binding(prepared, receipts)?;
        crate::teardrop_refill::VerifiedTeardropRefill::verify(
            before_fill,
            filled,
            reloaded,
            independent,
        )?;
        Self::from_verified(
            prepared,
            receipts,
            &reloaded.authored_geometry()?,
            Some(reloaded),
        )
    }

    fn from_verified(
        prepared: &PreparedGeometryChange,
        receipts: &GeometryMutationReceipts,
        after: &LiveGeometryEvidence,
        generated: Option<&crate::teardrop_refill::TeardropRefillObservation>,
    ) -> Result<Self, String> {
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
        if let Some(additions) = &receipts.additions {
            for (kind, ids) in [
                (
                    "graphic",
                    additions
                        .graphics
                        .iter()
                        .map(|g| g.graphic_id.clone())
                        .collect::<Vec<_>>(),
                ),
                (
                    "track",
                    additions
                        .tracks
                        .iter()
                        .map(|t| t.source_uuid.clone())
                        .collect::<Vec<_>>(),
                ),
                (
                    "via",
                    additions
                        .vias
                        .iter()
                        .map(|v| v.source_uuid.clone())
                        .collect::<Vec<_>>(),
                ),
            ] {
                for (index, native_uuid) in ids.into_iter().enumerate() {
                    creations.push(GeometryCreationIdentity {
                        kind: kind.into(),
                        logical_id: index.to_string(),
                        native_uuid,
                    });
                }
            }
        }
        let identities = creation_map(&creations)?;
        let generated_before = prepared
            .generated_before()
            .filter(|g| g.generated_count() > 0)
            .map(|g| g.generated_correspondence());
        let generated_after = generated
            .filter(|g| g.generated_count() > 0)
            .map(|g| g.generated_correspondence());
        let mut before = content(prepared.before(), &BTreeMap::new())?;
        let mut after = content(after, &identities)?;
        before.generated_digest = generated_before
            .as_ref()
            .map(|g| g.content_digest())
            .transpose()?;
        after.generated_digest = generated_after
            .as_ref()
            .map(|g| g.content_digest())
            .transpose()?;
        Ok(Self {
            intent_digest: match &prepared.additions {
                Some(additions) => digest(
                    &serde_json::to_vec(&(
                        canonical_intent(prepared.compiled().request())?,
                        additions.intent(),
                    ))
                    .map_err(|e| e.to_string())?,
                ),
                None => canonical_intent(prepared.compiled().request())?,
            },
            before,
            after,
            creations,
            generated_before,
            generated_after,
        })
    }

    pub fn generated_baseline(&self) -> Option<&crate::live_geometry::GeneratedZoneCorrespondence> {
        self.generated_before.as_ref()
    }

    /// Return generated diagnostic identities separately from explicit creation receipts.
    pub fn generated_mapping(&self, source: &Self) -> Result<BTreeMap<String, String>, String> {
        self.equivalent_to(source)?;
        match (&self.generated_after, &source.generated_after) {
            (Some(a), Some(b)) => a.equivalent_to(b),
            (None, None) => Ok(BTreeMap::new()),
            _ => Err("generated candidate partitions differ".into()),
        }
    }

    /// Return exact preview UUID to source UUID correspondence only after all complete content agrees.
    pub fn equivalent_to(&self, source: &Self) -> Result<BTreeMap<String, String>, String> {
        if self.intent_digest != source.intent_digest
            || self.before != source.before
            || self.after != source.after
        {
            return Err(format!(
                "geometry candidate differs: intent={}, baseline={}, native_definitions={}, board_definitions={}, copper={}, generated={}",
                self.intent_digest != source.intent_digest,
                self.before != source.before,
                self.after.native_definitions != source.after.native_definitions,
                self.after.board_definitions != source.after.board_definitions,
                self.after.copper != source.after.copper,
                self.after.generated_digest != source.after.generated_digest,
            ));
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
            generated_digest: None,
        };
        GeometryCandidateEvidence {
            intent_digest: "intent".into(),
            before: content.clone(),
            after: content,
            generated_before: None,
            generated_after: None,
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
        for mode in 0..10 {
            let mut changed = source.clone();
            match mode {
                0 => changed.intent_digest.push('x'),
                1 => changed.before.native_definitions.push('x'),
                2 => changed.before.board_definitions.push('x'),
                3 => changed.after.native_definitions.push('x'),
                4 => changed.after.board_definitions.push('x'),
                5 => changed.creations[0].kind = "outline".into(),
                6 => changed.creations[0].logical_id = "other".into(),
                7 => changed.creations.push(changed.creations[0].clone()),
                8 => changed.before.generated_digest = Some("changed".into()),
                _ => changed.after.generated_digest = Some("changed".into()),
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
