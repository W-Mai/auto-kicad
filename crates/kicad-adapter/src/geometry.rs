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
    zones: Option<PreparedSolidZoneEdits>,
}

/// Immutable complete observation; native preparation cannot substitute unbound net/layer context.
#[derive(Clone, Debug)]
pub struct GeometryObservation {
    live: LiveGeometryEvidence,
    context: GeometryContext,
    instance: InstanceToken,
    filename: String,
}
impl GeometryObservation {
    pub fn live(&self) -> &LiveGeometryEvidence {
        &self.live
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
        let current = observe_context(client, filename, &self.context.board_id)?;
        if current.context.revision != self.context.revision || current.instance != self.instance {
            return Err("geometry composite observation changed before mutation".into());
        }
        Ok(())
    }
    pub fn prepare(&self, request: &GeometryRequest) -> Result<PreparedGeometryChange, String> {
        PreparedGeometryChange::new(request, &self.context, &self.live)
    }
}

pub fn observe_context<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board_id: &StableId,
) -> Result<GeometryObservation, String> {
    let before = observe_live_geometry(client, filename)?;
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
    let after = observe_live_geometry(client, filename)?;
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
    let nets = nets
        .into_iter()
        .filter(|n| !n.name.is_empty())
        .map(|n| ZoneNetIdentity {
            net_id: kicad_model::board_net_id(board_id, &n.name),
            name: n.name,
        })
        .collect();
    let mut zones = Vec::new();
    for (id, zone) in before.native.zones() {
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
            lock_state: if before.text.zones()[id].locked {
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
        context,
        instance: token,
        filename: filename.to_owned(),
    })
}

impl PreparedGeometryChange {
    pub fn compiled(&self) -> &CompiledGeometryChange {
        &self.compiled
    }
    pub fn zones(&self) -> Option<&PreparedSolidZoneEdits> {
        self.zones.as_ref()
    }

    fn new(
        request: &GeometryRequest,
        context: &GeometryContext,
        observed: &LiveGeometryEvidence,
    ) -> Result<Self, String> {
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
        Ok(Self { compiled, zones })
    }
}
