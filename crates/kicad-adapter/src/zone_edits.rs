//! Bind native zone definition mutations to complete live evidence and independently observed locks.
use crate::live_geometry::{LiveGeometryEvidence, observe_live_geometry};
use crate::outline::outline_revision;
use kicad_ipc::{
    DeleteItemsReceipt, KiCadClient, KiCadTransport, SolidZonePatch, SolidZonePayload,
    SolidZoneReceipt, board_types,
};
use kicad_model::RevisionDigest;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct SolidZoneEdits {
    pub creates: Vec<SolidZonePayload>,
    pub updates: Vec<(String, SolidZonePatch)>,
    pub delete_ids: Vec<String>,
}

/// Validated definition edits; this is neither an approved source plan nor a refill result.
#[derive(Clone, Debug)]
pub struct PreparedSolidZoneEdits {
    before: LiveGeometryEvidence,
    edits: SolidZoneEdits,
}

#[derive(Clone, Debug)]
pub struct SolidZoneMutationReceipts {
    pub created: Vec<SolidZoneReceipt>,
    pub updated: Vec<SolidZoneReceipt>,
    pub deleted: Option<DeleteItemsReceipt>,
}

impl PreparedSolidZoneEdits {
    pub fn new(
        before: &LiveGeometryEvidence,
        expected_revision: &RevisionDigest,
        edits: SolidZoneEdits,
    ) -> Result<Self, String> {
        before.text.validate_native_zones(&before.native)?;
        if &outline_revision(before)? != expected_revision {
            return Err("zone observation revision changed".into());
        }
        let count = edits.creates.len() + edits.updates.len() + edits.delete_ids.len();
        if count == 0 || count > kicad_ipc::MAX_ZONE_MUTATIONS {
            return Err("zone definition batch must contain 1..32 mutations".into());
        }
        let mut targets = BTreeSet::new();
        for id in edits
            .updates
            .iter()
            .map(|(id, _)| id)
            .chain(&edits.delete_ids)
        {
            if !targets.insert(id) {
                return Err("zone mutation target repeats".into());
            }
            let text = before
                .text
                .zones()
                .get(id)
                .ok_or("zone live-text target missing")?;
            let zone = before
                .native
                .zones()
                .get(id)
                .ok_or("zone native target missing")?;
            if text.locked {
                return Err("zone mutation target is locked".into());
            }
            if zone.type_.enum_value() != Ok(board_types::ZoneType::ZT_COPPER)
                || !matches!(&zone.settings,Some(board_types::zone::Settings::CopperSettings(c)) if c.fill_mode.enum_value()==Ok(board_types::ZoneFillMode::ZFM_SOLID))
            {
                return Err("zone target is not ordinary solid copper".into());
            }
        }
        if before.native.zones().len() + edits.creates.len() - edits.delete_ids.len() > 256 {
            return Err("resulting zone count exceeds observation bound".into());
        }
        for creation in &edits.creates {
            if creation.native().id.is_some() || creation.native().parent.is_some() {
                return Err("zone creation contains an existing identity".into());
            }
        }
        for (id, patch) in &edits.updates {
            SolidZonePayload::patched(&before.native.zones()[id], patch)
                .map_err(|e| e.to_string())?;
        }
        Ok(Self {
            before: before.clone(),
            edits,
        })
    }

    /// Verify all native records and every unedited live-text field after commit or same-process reload. This does not certify refill, DRC, connectivity or source approval.
    pub fn verify_definitions(
        &self,
        after: &LiveGeometryEvidence,
        receipts: &SolidZoneMutationReceipts,
    ) -> Result<(), String> {
        self.verify_geometry_definitions(after, receipts, &BTreeSet::new())
    }

    pub(crate) fn unchanged(before: &LiveGeometryEvidence) -> Self {
        Self {
            before: before.clone(),
            edits: SolidZoneEdits::default(),
        }
    }

    pub(crate) fn verify_geometry_definitions(
        &self,
        after: &LiveGeometryEvidence,
        receipts: &SolidZoneMutationReceipts,
        outlines: &BTreeSet<String>,
    ) -> Result<(), String> {
        after.text.validate_native_zones(&after.native)?;
        let mut zones = self.before.native.zones().clone();
        let mut excluded = self
            .edits
            .delete_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for id in &self.edits.delete_ids {
            zones.remove(id);
        }
        if receipts.created.len() != self.edits.creates.len()
            || receipts.updated.len() != self.edits.updates.len()
        {
            return Err("zone mutation receipt count differs from intent".into());
        }
        match (&receipts.deleted, self.edits.delete_ids.is_empty()) {
            (None, true) => {}
            (
                Some(
                    DeleteItemsReceipt::ExactItemStatuses { ids }
                    | DeleteItemsReceipt::NeedsSavedVerification { ids },
                ),
                false,
            ) if ids.iter().collect::<BTreeSet<_>>()
                == self.edits.delete_ids.iter().collect::<BTreeSet<_>>()
                && ids.len() == self.edits.delete_ids.len() => {}
            _ => return Err("zone deletion receipt differs from intent".into()),
        }
        let mut updated_ids = BTreeSet::new();
        for receipt in &receipts.updated {
            if !self
                .edits
                .updates
                .iter()
                .any(|(id, _)| id == receipt.source_uuid())
                || !updated_ids.insert(receipt.source_uuid())
            {
                return Err("zone update receipt target differs".into());
            }
            let (_, patch) = self
                .edits
                .updates
                .iter()
                .find(|(id, _)| id == receipt.source_uuid())
                .ok_or("unrequested zone receipt")?;
            let expected = SolidZonePayload::patched(
                &self.before.native.zones()[receipt.source_uuid()],
                patch,
            )
            .map_err(|e| e.to_string())?;
            if !receipt.matches_definition(&expected) {
                return Err("zone update receipt differs from requested patch".into());
            }
            zones.insert(receipt.source_uuid().into(), receipt.native().clone());
        }
        for (receipt, expected) in receipts.created.iter().zip(&self.edits.creates) {
            if !receipt.matches_definition(expected) {
                return Err("zone creation receipt differs from requested definition".into());
            }
            if self
                .before
                .native
                .zones()
                .contains_key(receipt.source_uuid())
                || zones
                    .insert(receipt.source_uuid().into(), receipt.native().clone())
                    .is_some()
            {
                return Err("zone creation receipt identity collides".into());
            }
            excluded.insert(receipt.source_uuid().into());
        }
        if !zones.keys().eq(after.native.zones().keys()) {
            return Err("saved zone identity set differs".into());
        }
        for (id, expected) in &zones {
            let mut expected = expected.clone();
            let mut actual = after.native.zones()[id].clone();
            expected.parent = None.into();
            actual.parent = None.into();
            if expected != actual {
                return Err(format!("complete native zone changed: {id}"));
            }
        }
        let mut shapes = self.before.native.shapes().clone();
        let mut after_shapes = after.native.shapes().clone();
        shapes.retain(|id, _| !outlines.contains(id));
        after_shapes.retain(|id, _| !outlines.contains(id));
        for shape in shapes.values_mut().chain(after_shapes.values_mut()) {
            shape.parent = None.into();
        }
        if shapes != after_shapes {
            return Err("zone edit changed board graphics".into());
        }
        let patches = self.edits.updates.iter().cloned().collect();
        let digest = |text: &crate::live_geometry::LiveBoardText| {
            if outlines.is_empty() {
                text.zone_edit_digest(&excluded, &patches)
            } else {
                text.geometry_edit_digest(&excluded, &patches, outlines)
            }
        };
        if digest(&self.before.text)? != digest(&after.text)? {
            return Err("zone mutation changed unedited live-text fields".into());
        }
        Ok(())
    }

    /// Apply inside a caller-owned native transaction. Any error requires cancellation and unchanged-state verification; successful definitions still require commit, refill, save and independent verification.
    pub fn apply_in_transaction<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<SolidZoneMutationReceipts, String> {
        let actual = observe_live_geometry(client, filename)?;
        if actual.native != self.before.native
            || actual.text.raw_text() != self.before.text.raw_text()
        {
            return Err("complete zone observation changed before mutation".into());
        }
        let updated = if self.edits.updates.is_empty() {
            vec![]
        } else {
            client
                .update_solid_zones(filename, &actual.native, &self.edits.updates)
                .map_err(|e| e.to_string())?
        };
        let deleted = if self.edits.delete_ids.is_empty() {
            None
        } else {
            Some(
                client
                    .delete_board_items(filename, &self.edits.delete_ids)
                    .map_err(|e| e.to_string())?,
            )
        };
        let created = if self.edits.creates.is_empty() {
            vec![]
        } else {
            let current = client
                .capture_board_geometry(filename)
                .map_err(|e| e.to_string())?;
            client
                .create_solid_zones(filename, &current, &self.edits.creates)
                .map_err(|e| e.to_string())?
        };
        Ok(SolidZoneMutationReceipts {
            created,
            updated,
            deleted,
        })
    }
}
