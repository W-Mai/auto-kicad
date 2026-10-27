//! Bind typed geometry requests to complete native observations before any mutation.
use crate::{
    live_geometry::{LiveGeometryEvidence, observe_live_geometry},
    outline::outline_revision,
    zone_edits::{PreparedSolidZoneEdits, SolidZoneEdits},
};
use kicad_engine::geometry::{
    CompiledGeometryChange, GeometryContext, ObservedZoneTarget, compile_geometry_change,
};
use kicad_ipc::{InstanceToken, KiCadClient, KiCadTransport, SolidZonePayload, board_types};
use kicad_model::{
    LockState, RevisionDigest, StableId,
    geometry::{GeometryRequest, ZoneEdit, ZoneNetIdentity},
};

#[derive(Clone, Debug)]
pub struct PreparedGeometryChange {
    compiled: CompiledGeometryChange,
    observation: GeometryObservation,
    zones: Option<PreparedSolidZoneEdits>,
}

/// Immutable complete observation; native preparation cannot substitute unbound net/layer context.
#[derive(Clone, Debug)]
pub struct GeometryObservation {
    live: LiveGeometryEvidence,
    generated: Option<crate::teardrop_refill::TeardropRefillObservation>,
    context: GeometryContext,
    instance: InstanceToken,
    filename: String,
}
impl GeometryObservation {
    pub fn live(&self) -> &LiveGeometryEvidence {
        &self.live
    }
    pub fn generated(&self) -> Option<&crate::teardrop_refill::TeardropRefillObservation> {
        self.generated.as_ref()
    }
    pub fn context(&self) -> &GeometryContext {
        &self.context
    }
    /// Revalidate the editor, document and complete composite revision immediately before mutation.
    pub fn verify_current<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<(), String> {
        if filename != self.filename || client.instance_token() != Some(&self.instance) {
            return Err("geometry editor or document binding changed".into());
        }
        let current = observe_context_inner(
            client,
            filename,
            &self.context.board_id,
            self.generated.is_some(),
        )?;
        if current.context.revision != self.context.revision || current.instance != self.instance {
            return Err("geometry composite observation changed before mutation".into());
        }
        Ok(())
    }
    pub fn prepare(&self, request: &GeometryRequest) -> Result<PreparedGeometryChange, String> {
        PreparedGeometryChange::new(request, self)
    }
}

pub fn observe_context<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board_id: &StableId,
) -> Result<GeometryObservation, String> {
    observe_context_inner(client, filename, board_id, false)
}

/// Observe complete generated regions alongside the authored view used by typed geometry.
pub fn observe_generated_context<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board_id: &StableId,
) -> Result<GeometryObservation, String> {
    observe_context_inner(client, filename, board_id, true)
}

fn observe_context_inner<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board_id: &StableId,
    generated: bool,
) -> Result<GeometryObservation, String> {
    let capture = |client: &mut KiCadClient<T>| -> Result<_, String> {
        if generated {
            let complete =
                crate::teardrop_refill::TeardropRefillObservation::capture(client, filename)?;
            Ok((complete.authored_geometry()?, Some(complete)))
        } else {
            Ok((observe_live_geometry(client, filename)?, None))
        }
    };
    let (before, complete_before) = capture(client)?;
    let token = client
        .instance_token()
        .cloned()
        .ok_or("missing bound instance")?;
    let check_token = |c: &KiCadClient<T>| -> Result<(), String> {
        if c.instance_token() != Some(&token) {
            return Err("geometry net/layer observation changed instance".into());
        }
        Ok(())
    };
    let nets = client.get_board_nets(filename).map_err(|e| e.to_string())?;
    check_token(client)?;
    let stackup = crate::board_stackup_observation(
        board_id.clone(),
        client
            .get_board_stackup(filename)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    check_token(client)?;
    let (after, complete_after) = capture(client)?;
    let repeated_nets = client.get_board_nets(filename).map_err(|e| e.to_string())?;
    check_token(client)?;
    let repeated_stackup = crate::board_stackup_observation(
        board_id.clone(),
        client
            .get_board_stackup(filename)
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    check_token(client)?;
    if nets != repeated_nets || stackup != repeated_stackup {
        return Err("geometry net/layer observation is unstable".into());
    }
    if before.native != after.native || before.text.raw_text() != after.text.raw_text() {
        return Err("geometry changed during net/layer observation".into());
    }
    if let (Some(a), Some(b)) = (&complete_before, &complete_after)
        && (a.native() != b.native() || a.text().raw_text() != b.text().raw_text())
    {
        return Err("complete generated geometry changed during observation".into());
    }
    let nets = nets
        .into_iter()
        .filter(|n| !n.name.is_empty())
        .map(|n| ZoneNetIdentity {
            net_id: kicad_model::board_net_id(board_id, &n.name),
            name: n.name,
        })
        .collect();
    let mut zones = Vec::new();
    let full_native = complete_before
        .as_ref()
        .map(|o| o.native())
        .unwrap_or(&before.native);
    for (id, zone) in full_native.zones() {
        let mut payload = zone.clone();
        payload.filled = false;
        payload.filled_polygons.clear();
        let editable = SolidZonePayload::new(payload).is_ok();
        let net = match &zone.settings {
            Some(board_types::zone::Settings::CopperSettings(c)) => c
                .net
                .as_ref()
                .filter(|n| !n.name.is_empty())
                .map(|n| ZoneNetIdentity {
                    net_id: kicad_model::board_net_id(board_id, &n.name),
                    name: n.name.clone(),
                }),
            _ => None,
        };
        zones.push(ObservedZoneTarget {
            source_uuid: StableId::new(id).map_err(|e| e.to_string())?,
            lock_state: if !before.text.zones().contains_key(id) {
                LockState::Unknown
            } else if before.text.zones()[id].locked {
                LockState::Locked
            } else {
                LockState::Unlocked
            },
            editable_solid: editable,
            refillable: zone.type_.enum_value() != Ok(board_types::ZoneType::ZT_RULE_AREA),
            net,
        });
    }
    let outline_targets = crate::outline::observed_outline_targets(&before)?;
    use sha2::{Digest, Sha256};
    let revision = RevisionDigest::new(hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            board_id,
            token.as_str(),
            filename,
            outline_revision(&before)?,
            complete_before
                .as_ref()
                .map(|o| {
                    Ok::<_, String>((
                        o.native().evidence_bytes().map_err(|e| e.to_string())?,
                        o.text().raw_text(),
                    ))
                })
                .transpose()?,
            &nets,
            &stackup,
        ))
        .map_err(|e| e.to_string())?,
    )))
    .map_err(|e| e.to_string())?;
    let context = GeometryContext {
        board_id: board_id.clone(),
        revision,
        nets,
        enabled_layers: kicad_model::enabled_copper_layers(&stackup).map_err(|e| e.to_string())?,
        zones,
        outline_targets,
    };
    Ok(GeometryObservation {
        live: before,
        generated: complete_before,
        context,
        instance: token,
        filename: filename.to_owned(),
    })
}

impl PreparedGeometryChange {
    pub(crate) fn before(&self) -> &LiveGeometryEvidence {
        &self.observation.live
    }
    pub fn compiled(&self) -> &CompiledGeometryChange {
        &self.compiled
    }
    pub fn zones(&self) -> Option<&PreparedSolidZoneEdits> {
        self.zones.as_ref()
    }

    fn new(request: &GeometryRequest, observation: &GeometryObservation) -> Result<Self, String> {
        let context = &observation.context;
        let observed = &observation.live;
        let compiled = compile_geometry_change(request, context).map_err(|e| e.to_string())?;
        let mut edits = SolidZoneEdits::default();
        for change in &request.zones {
            match change {
                ZoneEdit::Create { definition, .. } => edits
                    .creates
                    .push(crate::geometry_native::payload(definition).map_err(|e| e.to_string())?),
                ZoneEdit::Update { source_uuid, patch } => {
                    let patch = crate::geometry_native::patch(patch).map_err(|e| e.to_string())?;
                    let source = observed
                        .native
                        .zones()
                        .get(source_uuid.as_str())
                        .ok_or("missing native zone target")?;
                    let resolved =
                        SolidZonePayload::patched(source, &patch).map_err(|e| e.to_string())?;
                    crate::geometry_native::require_saved_area(&resolved)
                        .map_err(|e| e.to_string())?;
                    edits.updates.push((source_uuid.to_string(), patch));
                }
                ZoneEdit::Delete { source_uuid } => edits.delete_ids.push(source_uuid.to_string()),
            }
        }
        let zones = if request.zones.is_empty() {
            None
        } else {
            Some(PreparedSolidZoneEdits::new(
                observed,
                &outline_revision(observed)?,
                edits,
            )?)
        };
        Ok(Self {
            compiled,
            observation: observation.clone(),
            zones,
        })
    }
}

/// Receipts are bound to one immutable request and constructed only by its native executor.
#[derive(Clone, Debug)]
pub struct GeometryMutationReceipts {
    request: GeometryRequest,
    zones: crate::zone_edits::SolidZoneMutationReceipts,
    outlines: Vec<kicad_ipc::CreatedOutlineReceipt>,
    deleted_outlines: Option<kicad_ipc::DeleteItemsReceipt>,
}
impl GeometryMutationReceipts {
    pub fn zones(&self) -> &crate::zone_edits::SolidZoneMutationReceipts {
        &self.zones
    }
    pub fn outlines(&self) -> &[kicad_ipc::CreatedOutlineReceipt] {
        &self.outlines
    }
}

impl PreparedGeometryChange {
    /// Apply definitions inside a caller-owned commit; errors require cancellation and unchanged-state verification.
    /// Successful receipts do not establish topology, refill, electrical correctness or source approval.
    pub fn apply_in_transaction<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<GeometryMutationReceipts, String> {
        self.observation.verify_current(client, filename)?;
        let zones = match &self.zones {
            Some(zones) => {
                if let Some(complete) = self.observation.generated() {
                    zones.apply_with_observation(
                        client,
                        filename,
                        self.before(),
                        complete.native(),
                    )?
                } else {
                    zones.apply_in_transaction(client, filename)?
                }
            }
            None => crate::zone_edits::SolidZoneMutationReceipts {
                created: vec![],
                updated: vec![],
                deleted: None,
            },
        };
        let mut outlines = vec![];
        let mut deleted_outlines = None;
        if let Some(outline) = self.compiled.outline() {
            let ids = outline
                .removed_ids()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                let current = client
                    .capture_board_geometry(filename)
                    .map_err(|e| e.to_string())?;
                deleted_outlines = Some(
                    client
                        .delete_outline_items(filename, &current, &ids)
                        .map_err(|e| e.to_string())?,
                );
            }
            let primitives = crate::outline::native_outline_primitives(outline);
            if !primitives.is_empty() {
                outlines = client
                    .create_outline_primitives(filename, &primitives)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(GeometryMutationReceipts {
            request: self.compiled.request().clone(),
            zones,
            outlines,
            deleted_outlines,
        })
    }

    pub fn generated_before(&self) -> Option<&crate::teardrop_refill::TeardropRefillObservation> {
        self.observation.generated()
    }

    pub fn verify_generated_definitions(
        &self,
        after: &crate::teardrop_refill::TeardropRefillObservation,
        receipts: &GeometryMutationReceipts,
    ) -> Result<(), String> {
        self.verify_complete_generated_definitions(after, receipts, false)
    }

    /// Caller must establish the same-editor saved reload; only complete generated UUID correspondence may differ.
    pub fn verify_generated_definitions_after_reload(
        &self,
        after: &crate::teardrop_refill::TeardropRefillObservation,
        receipts: &GeometryMutationReceipts,
    ) -> Result<(), String> {
        self.verify_complete_generated_definitions(after, receipts, true)
    }

    fn verify_complete_generated_definitions(
        &self,
        after: &crate::teardrop_refill::TeardropRefillObservation,
        receipts: &GeometryMutationReceipts,
        reloaded: bool,
    ) -> Result<(), String> {
        let before = self
            .generated_before()
            .ok_or("missing complete generated preparation")?;
        if !before.same_binding(after) {
            return Err(
                "generated definition observations use different editors or documents".into(),
            );
        }
        if !reloaded && before.generated_correspondence() != after.generated_correspondence() {
            return Err(
                "generated native identities or content changed during definition mutation".into(),
            );
        }
        before
            .generated_correspondence()
            .equivalent_to(&after.generated_correspondence())?;
        self.verify_authored_definitions(&after.authored_geometry()?, receipts)
    }

    /// Verify complete native and unedited text definitions before refill, including after same-process reload.
    pub fn verify_definitions(
        &self,
        after: &LiveGeometryEvidence,
        receipts: &GeometryMutationReceipts,
    ) -> Result<(), String> {
        if self
            .generated_before()
            .is_some_and(|g| g.generated_count() > 0)
        {
            return Err(
                "complete generated definitions require generated preservation verification".into(),
            );
        }
        self.verify_authored_definitions(after, receipts)
    }

    fn verify_authored_definitions(
        &self,
        after: &LiveGeometryEvidence,
        receipts: &GeometryMutationReceipts,
    ) -> Result<(), String> {
        use kicad_ipc::DeleteItemsReceipt;
        use std::collections::BTreeSet;
        if &receipts.request != self.compiled.request() {
            return Err("geometry receipts belong to a different request".into());
        }
        let before = &self.observation.live;
        let removed = self
            .compiled
            .outline()
            .map(|o| o.removed_ids())
            .unwrap_or(&[])
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        let expected_count = self.compiled.outline().map_or(0, |o| o.primitives().len());
        if receipts.outlines.len() != expected_count {
            return Err("outline receipt count differs from compiled intent".into());
        }
        match (&receipts.deleted_outlines, removed.is_empty()) {
            (None, true) => {}
            (
                Some(
                    DeleteItemsReceipt::ExactItemStatuses { ids }
                    | DeleteItemsReceipt::NeedsSavedVerification { ids },
                ),
                false,
            ) if ids.len() == removed.len()
                && ids.iter().cloned().collect::<BTreeSet<_>>() == removed => {}
            _ => return Err("outline deletion receipt differs from compiled intent".into()),
        }
        let mut expected = before.native.shapes().clone();
        for id in &removed {
            if expected.remove(id).is_none() {
                return Err("compiled outline removal target missing".into());
            }
        }
        let mut exclusions = removed;
        for (index, receipt) in receipts.outlines.iter().enumerate() {
            if receipt.request_index != index
                || receipt.native().id.as_ref().map(|id| id.value.as_str())
                    != Some(receipt.source_uuid.as_str())
                || before.native.shapes().contains_key(&receipt.source_uuid)
                || before.native.zones().contains_key(&receipt.source_uuid)
                || !exclusions.insert(receipt.source_uuid.clone())
                || expected
                    .insert(receipt.source_uuid.clone(), receipt.native().clone())
                    .is_some()
            {
                return Err("outline receipt identity/index collides or differs".into());
            }
        }
        let mut actual = after.native.shapes().clone();
        for shape in expected.values_mut().chain(actual.values_mut()) {
            shape.parent = None.into();
        }
        if expected != actual {
            return Err("complete geometry graphics differ from native receipts".into());
        }
        let unchanged;
        let zones = match &self.zones {
            Some(zones) => zones,
            None => {
                unchanged = PreparedSolidZoneEdits::unchanged(before);
                &unchanged
            }
        };
        zones.verify_geometry_definitions(after, &receipts.zones, &exclusions)
    }
}
