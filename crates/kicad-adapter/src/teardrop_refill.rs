//! Complete generated copper and mask verification across native refill, saved reload and CLI.
use crate::live_geometry::{
    FilledCopperEvidence, TeardropIdentity, TeardropTextPartition, capture_geometry_text,
};
use kicad_checks::refill::ClosedRefillResult;
use kicad_ipc::{InstanceToken, KiCadClient, KiCadTransport, NativeBoardGeometry, board_types};
use protobuf::Message;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Immutable repeated native/text observation of one exact editor and document.
#[derive(Clone, Debug)]
pub struct TeardropRefillObservation {
    instance: InstanceToken,
    filename: String,
    native: NativeBoardGeometry,
    text: TeardropTextPartition,
    identities: Vec<TeardropIdentity>,
}
impl TeardropRefillObservation {
    pub fn capture<T: KiCadTransport>(
        client: &mut KiCadClient<T>,
        filename: &str,
    ) -> Result<Self, String> {
        let instance = client
            .instance_token()
            .cloned()
            .ok_or("unbound refill editor")?;
        let (native, raw) = capture_geometry_text(client, filename)?;
        let text = TeardropTextPartition::parse(&raw)?;
        let identities = text.bind(&native)?;
        Ok(Self {
            instance,
            filename: filename.into(),
            native,
            text,
            identities,
        })
    }
    pub fn generated_correspondence(&self) -> crate::live_geometry::GeneratedZoneCorrespondence {
        crate::live_geometry::GeneratedZoneCorrespondence::from_bound(self.identities.clone())
    }
    /// Compile identities into a private DRC copy; caller must bind actual saved bytes and project context.
    pub fn drc_snapshot(
        &self,
        saved: &str,
    ) -> Result<crate::live_geometry::GeneratedDrcSnapshot, String> {
        crate::live_geometry::GeneratedDrcSnapshot::bind(saved, &self.text, &self.native)
    }
    pub fn text(&self) -> &TeardropTextPartition {
        &self.text
    }
    pub fn native(&self) -> &NativeBoardGeometry {
        &self.native
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn ordinary_native(native: &NativeBoardGeometry, definition_only: bool) -> Result<String, String> {
    let mut records = BTreeMap::new();
    for (id, zone) in native.zones() {
        if zone.type_.enum_value() == Ok(board_types::ZoneType::ZT_TEARDROP) {
            continue;
        }
        let mut value = zone.clone();
        value.parent = None.into();
        if definition_only {
            value.filled = false;
            value.filled_polygons.clear();
        }
        records.insert(
            ("zone", id),
            value.write_to_bytes().map_err(|e| e.to_string())?,
        );
    }
    for (id, shape) in native.shapes() {
        let mut value = shape.clone();
        value.parent = None.into();
        records.insert(
            ("shape", id),
            value.write_to_bytes().map_err(|e| e.to_string())?,
        );
    }
    Ok(digest(
        &serde_json::to_vec(&records.into_iter().collect::<Vec<_>>()).map_err(|e| e.to_string())?,
    ))
}

#[derive(Clone, Debug, Serialize)]
pub struct GeneratedReloadIdentity {
    correspondence_key: String,
    before_uuid: String,
    after_uuid: String,
    invisible_to_solid_border: bool,
}

// Projection is directional and restricted to a generated zone with separately proven text.
fn compare_generated_reload(
    before: &board_types::Zone,
    after: &board_types::Zone,
    default_generated_hatch: bool,
) -> Result<bool, String> {
    if before.type_.enum_value() != Ok(board_types::ZoneType::ZT_TEARDROP)
        || after.type_.enum_value() != Ok(board_types::ZoneType::ZT_TEARDROP)
    {
        return Err("reload projection requires generated zones".into());
    }
    let mut a = before.clone();
    let mut b = after.clone();
    a.id = None.into();
    a.parent = None.into();
    b.id = None.into();
    b.parent = None.into();
    let mut transition = false;
    if a.border != b.border {
        let left = a.border.as_mut().ok_or("generated border missing")?;
        let right = b.border.as_ref().ok_or("reloaded border missing")?;
        if !default_generated_hatch
            || left.style.enum_value() != Ok(board_types::ZoneBorderStyle::ZBS_INVISIBLE)
            || right.style.enum_value() != Ok(board_types::ZoneBorderStyle::ZBS_SOLID)
            || left.pitch.as_ref().map(|p| p.value_nm) != Some(100_000)
            || right.pitch.as_ref().map(|p| p.value_nm) != Some(100_000)
        {
            return Err("unclassified generated border transition".into());
        }
        left.style = right.style;
        transition = true;
    }
    // Retains fill polygons, every unknown field and all border/pitch submessages.
    if a != b {
        return Err("complete native generated reload content differs".into());
    }
    Ok(transition)
}

/// Content verification only; electrical checks, saved-source binding and approval remain separate.
#[derive(Clone, Debug, Serialize)]
pub struct VerifiedTeardropRefill {
    authored_definition_digest: String,
    authored_native_definition_digest: String,
    ordinary_copper: FilledCopperEvidence,
    generated_content: BTreeMap<String, String>,
    reload_identities: Vec<GeneratedReloadIdentity>,
    saved_input_sha256: String,
    independent_output_sha256: String,
}
impl VerifiedTeardropRefill {
    /// Caller binds the CLI input to the saved board and proves dispatch completion independently.
    pub fn verify(
        before: &TeardropRefillObservation,
        after: &TeardropRefillObservation,
        reloaded: &TeardropRefillObservation,
        independent: &ClosedRefillResult,
    ) -> Result<Self, String> {
        for observed in [before, reloaded] {
            if observed.instance != after.instance || observed.filename != after.filename {
                return Err("refill observations use different editors or documents".into());
            }
        }
        let authored_definition_digest = before.text.authored().refill_definition_digest()?;
        let authored_native_definition_digest = ordinary_native(&before.native, true)?;
        if authored_definition_digest != after.text.authored().refill_definition_digest()?
            || authored_native_definition_digest != ordinary_native(&after.native, true)?
        {
            return Err("refill changed authored definitions or generating settings".into());
        }
        if after.text.authored().refill_definition_digest()?
            != reloaded.text.authored().refill_definition_digest()?
            || ordinary_native(&after.native, false)? != ordinary_native(&reloaded.native, false)?
        {
            return Err("reload changed complete authored content".into());
        }
        let saved = TeardropTextPartition::parse(independent.input_board())?;
        let closed = TeardropTextPartition::parse(independent.output_board())?;
        if saved.authored().closed_refill_definitions()?
            != closed.authored().closed_refill_definitions()?
        {
            return Err("independent refill changed authored saved definitions".into());
        }
        let ordinary_copper = after.text.authored().filled_copper()?;
        let generated_content = after.text.generated_content()?;
        for text in [&saved, &closed, &reloaded.text] {
            if ordinary_copper != text.authored().filled_copper()?
                || generated_content != text.generated_content()?
            {
                return Err("complete generated copper/mask or ordinary copper differs".into());
            }
        }
        let mut reload_identities = Vec::new();
        for (a, b) in after.identities.iter().zip(&reloaded.identities) {
            if a.correspondence_key() != b.correspondence_key() {
                return Err("generated reload correspondence differs".into());
            }
            let transition = compare_generated_reload(
                &after.native.zones()[a.native_uuid()],
                &reloaded.native.zones()[b.native_uuid()],
                after
                    .text
                    .has_default_generated_hatch(a.correspondence_key())?
                    && reloaded
                        .text
                        .has_default_generated_hatch(b.correspondence_key())?,
            )?;
            reload_identities.push(GeneratedReloadIdentity {
                correspondence_key: a.correspondence_key().into(),
                before_uuid: a.native_uuid().into(),
                after_uuid: b.native_uuid().into(),
                invisible_to_solid_border: transition,
            });
        }
        if reload_identities.len() != generated_content.len()
            || reloaded.identities.len() != generated_content.len()
        {
            return Err("generated reload count differs".into());
        }
        Ok(Self {
            authored_definition_digest,
            authored_native_definition_digest,
            ordinary_copper,
            generated_content,
            reload_identities,
            saved_input_sha256: independent.input_sha256().into(),
            independent_output_sha256: independent.output_sha256().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reload_projection_is_directional_and_retains_every_other_field() {
        let mut a = board_types::Zone {
            type_: board_types::ZoneType::ZT_TEARDROP.into(),
            border: protobuf::MessageField::some(board_types::ZoneBorderSettings {
                style: board_types::ZoneBorderStyle::ZBS_INVISIBLE.into(),
                pitch: protobuf::MessageField::some(kicad_ipc::base_types::Distance {
                    value_nm: 100_000,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut b = a.clone();
        b.border.as_mut().unwrap().style = board_types::ZoneBorderStyle::ZBS_SOLID.into();
        assert_eq!(compare_generated_reload(&a, &b, true), Ok(true));
        assert!(compare_generated_reload(&b, &a, true).is_err());
        assert!(compare_generated_reload(&a, &b, false).is_err());
        for mode in 0..6 {
            let mut changed = b.clone();
            match mode {
                0 => changed
                    .special_fields
                    .mut_unknown_fields()
                    .add_varint(999, 7),
                1 => changed
                    .border
                    .as_mut()
                    .unwrap()
                    .special_fields
                    .mut_unknown_fields()
                    .add_varint(999, 7),
                2 => {
                    changed
                        .border
                        .as_mut()
                        .unwrap()
                        .pitch
                        .as_mut()
                        .unwrap()
                        .value_nm = 1
                }
                3 => changed.filled = true,
                4 => changed.priority = 1,
                _ => changed.type_ = board_types::ZoneType::ZT_COPPER.into(),
            }
            assert!(compare_generated_reload(&a, &changed, true).is_err());
        }
        a.type_ = board_types::ZoneType::ZT_COPPER.into();
        b.type_ = a.type_;
        assert!(compare_generated_reload(&a, &b, true).is_err());
    }
}
