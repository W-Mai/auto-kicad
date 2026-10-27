//! Receipt-bound additive definitions for complete geometry refill verification.
use crate::{
    BoardGraphicsExecutor, NativePcbBatch, NativePcbBatchReceipt,
    geometry::{GeometryMutationReceipts, GeometryObservation, PreparedGeometryChange},
    teardrop_refill::TeardropRefillObservation,
};
use kicad_ipc::{KiCadClient, KiCadTransport};
use kicad_model::geometry::GeometryRequest;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct PreparedGeometryAdditions {
    pub(crate) batch: NativePcbBatch,
    original_items: kicad_ipc::ExistingBoardItemSet,
    pub(crate) identities: Option<crate::additive_identities::AdditiveCreationIdentities>,
}

/// Complete definition proof constructed only after stable native readback and preservation checks.
#[derive(Clone, Debug)]
pub struct VerifiedAdditiveGeometryDefinitions {
    request: GeometryRequest,
    batch: NativePcbBatch,
    receipts: NativePcbBatchReceipt,
    before_revision: kicad_model::RevisionDigest,
    observed: TeardropRefillObservation,
}

impl PreparedGeometryAdditions {
    pub(crate) fn new<T: KiCadTransport>(
        client: &mut KiCadClient<T>,
        observed: &GeometryObservation,
        filename: &str,
        batch: NativePcbBatch,
    ) -> Result<Self, String> {
        if batch.graphics.len() > 100
            || batch.tracks.len() + batch.vias.len() > kicad_model::copper_route::MAX_BATCH_COPPER
        {
            return Err("geometry additions exceed graphic or copper limits".into());
        }
        executor().validate_native_batch(filename, &batch.graphics, &batch.tracks, &batch.vias)?;
        let enabled = observed
            .context()
            .enabled_layers
            .iter()
            .copied()
            .map(crate::native_copper_layer)
            .collect::<Vec<_>>();
        let known_net = |net: &Option<kicad_ipc::CreateNetIdentity>| {
            net.as_ref().is_none_or(|n| {
                observed
                    .context()
                    .nets
                    .iter()
                    .any(|known| known.name == n.name)
            })
        };
        if batch
            .tracks
            .iter()
            .any(|t| !enabled.contains(&t.layer) || !known_net(&t.net))
            || batch
                .vias
                .iter()
                .any(|v| v.copper_layers != enabled || !known_net(&v.net))
        {
            return Err(
                "additive geometry net or layer context differs from observed board".into(),
            );
        }
        let original_items = client
            .capture_existing_board_items(filename)
            .map_err(|e| e.to_string())?;
        observed.verify_current(client, filename)?;
        Ok(Self {
            batch,
            original_items,
            identities: None,
        })
    }

    pub(crate) fn apply<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<NativePcbBatchReceipt, String> {
        executor().create_native_batch_with_identities(
            client,
            filename,
            &self.batch.graphics,
            &self.batch.tracks,
            &self.batch.vias,
            self.identities.as_ref(),
        )
    }

    pub(crate) fn intent(&self) -> serde_json::Value {
        serde_json::json!({"batch": batch_intent(&self.batch), "creation_identities": self.identities})
    }
}

fn executor() -> BoardGraphicsExecutor {
    BoardGraphicsExecutor {
        max_segments: kicad_model::copper_route::MAX_BATCH_COPPER + 100,
    }
}

pub(crate) fn identities(
    receipts: &NativePcbBatchReceipt,
) -> Result<BTreeMap<String, &'static str>, String> {
    let mut result = BTreeMap::new();
    for (id, tag) in receipts
        .graphics
        .iter()
        .map(|g| (&g.graphic_id, "gr_line"))
        .chain(receipts.tracks.iter().map(|t| (&t.source_uuid, "segment")))
        .chain(receipts.vias.iter().map(|v| (&v.source_uuid, "via")))
    {
        if id.is_empty() || result.insert(id.clone(), tag).is_some() {
            return Err("additive geometry receipt identities are empty or repeated".into());
        }
    }
    Ok(result)
}

impl PreparedGeometryChange {
    /// Read back new objects and bind their exact identities to complete retained definitions.
    /// The caller establishes any saved reload before requesting reload correspondence.
    pub fn verify_additive_definitions<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
        observed: &TeardropRefillObservation,
        receipts: &GeometryMutationReceipts,
        reloaded: bool,
    ) -> Result<VerifiedAdditiveGeometryDefinitions, String> {
        let additions = self
            .additions
            .as_ref()
            .ok_or("no prepared geometry additions")?;
        let native_receipts = receipts
            .additions
            .as_ref()
            .ok_or("missing additive receipts")?;
        if receipts.additive_batch.as_ref() != Some(&additions.batch)
            || &receipts.request != self.compiled().request()
        {
            return Err("additive receipts belong to a different compiled intent".into());
        }
        let before = self
            .generated_before()
            .ok_or("additions require complete generated observation")?;
        if !before.same_binding(observed) {
            return Err("additive definitions changed editor or document binding".into());
        }
        let ids = identities(native_receipts)?;
        if native_receipts.graphics.len() != additions.batch.graphics.len()
            || native_receipts.tracks.len() != additions.batch.tracks.len()
            || native_receipts.vias.len() != additions.batch.vias.len()
        {
            return Err("additive receipt counts differ from compiled batch".into());
        }
        // Both reads are tied to the exact complete text/native observation, including generated regions.
        let current = TeardropRefillObservation::capture(client, filename)?;
        require_same_observation(observed, &current)?;
        crate::verify_native_pcb_batch(client, filename, native_receipts)?;
        if native_receipts
            .graphics
            .iter()
            .any(|r| r.lock_state != kicad_ipc::ObservedLockState::Unlocked)
            || native_receipts
                .tracks
                .iter()
                .any(|r| r.lock_state != kicad_ipc::ObservedLockState::Unlocked)
            || native_receipts
                .vias
                .iter()
                .any(|r| r.lock_state != kicad_ipc::ObservedLockState::Unlocked)
        {
            return Err("created geometry has an unexpected lock state".into());
        }
        for receipt in &native_receipts.graphics {
            let graphic = observed
                .native()
                .shapes()
                .get(&receipt.graphic_id)
                .ok_or("created native graphic is missing")?;
            if graphic.locked.enum_value() != Ok(kicad_ipc::base_types::LockedState::LS_UNLOCKED) {
                return Err("created geometry graphic lock changed during readback".into());
            }
        }

        let after_items = client
            .capture_existing_board_items(filename)
            .map_err(|e| e.to_string())?;
        if !additions
            .original_items
            .matches_after_additions(&after_items, &ids.keys().cloned().collect::<Vec<_>>())
            .map_err(|e| e.to_string())?
        {
            return Err("additive definitions changed original complete native items".into());
        }
        let current = TeardropRefillObservation::capture(client, filename)?;
        require_same_observation(observed, &current)?;
        // Copper edits can regenerate teardrops during CommitChanges. This proof only admits
        // complete authored preservation; independent refill and final candidate checks remain mandatory.
        if additions.batch.tracks.is_empty() && additions.batch.vias.is_empty() {
            if !reloaded && before.generated_correspondence() != observed.generated_correspondence()
            {
                return Err(
                    "generated identities changed during additive graphics creation".into(),
                );
            }
            before
                .generated_correspondence()
                .equivalent_to(&observed.generated_correspondence())?;
        }
        crate::live_geometry::LiveBoardText::verify_additive_identity_namespace(
            before.text().raw_text(),
            observed.text().raw_text(),
            &ids,
            before.generated_ids(),
            observed.generated_ids(),
        )?;
        let before = before.authored_geometry()?;
        let after = observed.authored_geometry()?;
        let remaining = after.text.without_created_additions(&before.text, &ids)?;
        if before.text.refill_definition_digest()? != remaining.refill_definition_digest()?
            || before.text.filled_copper()? != remaining.filled_copper()?
        {
            return Err(
                "additive definitions changed original complete board text or fills".into(),
            );
        }
        let mut expected = before.native.shapes().clone();
        let mut actual = after.native.shapes().clone();
        for receipt in &native_receipts.graphics {
            if expected.contains_key(&receipt.graphic_id)
                || actual.remove(&receipt.graphic_id).is_none()
            {
                return Err("additive native graphic identity is missing or preexisting".into());
            }
        }
        for shape in expected.values_mut().chain(actual.values_mut()) {
            shape.parent = None.into();
        }
        if expected != actual
            || before
                .native
                .zone_definition_bytes()
                .map_err(|e| e.to_string())?
                != after
                    .native
                    .zone_definition_bytes()
                    .map_err(|e| e.to_string())?
        {
            return Err(
                "additive definitions changed complete native zones or retained graphics".into(),
            );
        }
        Ok(VerifiedAdditiveGeometryDefinitions {
            request: self.compiled().request().clone(),
            batch: additions.batch.clone(),
            receipts: native_receipts.clone(),
            before_revision: self.compiled().request().observed_revision.clone(),
            observed: observed.clone(),
        })
    }
}

fn require_same_observation(
    a: &TeardropRefillObservation,
    b: &TeardropRefillObservation,
) -> Result<(), String> {
    if !a.same_binding(b) || a.native() != b.native() || a.text().raw_text() != b.text().raw_text()
    {
        return Err("complete additive observation changed during receipt verification".into());
    }
    Ok(())
}

impl VerifiedAdditiveGeometryDefinitions {
    pub(crate) fn verify_binding(
        &self,
        prepared: &PreparedGeometryChange,
        receipts: &GeometryMutationReceipts,
    ) -> Result<&TeardropRefillObservation, String> {
        if &self.request != prepared.compiled().request()
            || self.before_revision != prepared.compiled().request().observed_revision
            || prepared.additions.as_ref().map(|a| &a.batch) != Some(&self.batch)
            || receipts.additions.as_ref() != Some(&self.receipts)
            || receipts.additive_batch.as_ref() != Some(&self.batch)
        {
            return Err(
                "additive definition proof belongs to a different prepared mutation".into(),
            );
        }
        let before = prepared
            .generated_before()
            .ok_or("missing complete additive baseline")?;
        if !before.same_binding(&self.observed) {
            return Err("additive definition proof changed editor binding".into());
        }
        Ok(&self.observed)
    }
}

/// Canonical physical additions, independent of board scope and native creation UUIDs.
pub fn batch_intent(batch: &NativePcbBatch) -> serde_json::Value {
    use serde_json::json;
    json!({
        "graphics": batch.graphics.iter().map(|g| json!([
            match g.layer { kicad_ipc::BoardGraphicLayer::DwgsUser => "Dwgs.User", kicad_ipc::BoardGraphicLayer::EdgeCuts => "Edge.Cuts", _ => "unsupported" },
            g.start_x_nm, g.start_y_nm, g.end_x_nm, g.end_y_nm, g.width_nm,
        ])).collect::<Vec<_>>(),
        "tracks": batch.tracks.iter().map(|t| json!([
            t.layer.native() as i32, t.start_x_nm, t.start_y_nm, t.end_x_nm, t.end_y_nm,
            t.width_nm, t.net.as_ref().map(|n| &n.name),
        ])).collect::<Vec<_>>(),
        "vias": batch.vias.iter().map(|v| json!([
            v.copper_layers.iter().map(|l| l.native() as i32).collect::<Vec<_>>(),
            v.position_x_nm, v.position_y_nm, v.pad_diameter_nm, v.drill_diameter_nm,
            v.net.as_ref().map(|n| &n.name),
        ])).collect::<Vec<_>>(),
    })
}
