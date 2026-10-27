//! Source-bound identity allocation for additive copper generators.
use crate::{
    NativePcbBatch, geometry_additions::batch_intent, live_geometry::LiveBoardText,
    teardrop_refill::TeardropRefillObservation,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Immutable allocation; never decoded from caller-controlled request data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdditiveCreationIdentities {
    seed_digest: String,
    batch_digest: String,
    tracks: Vec<String>,
    vias: Vec<String>,
}
fn hash(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}
fn physical_digest(batch: &NativePcbBatch) -> Result<String, String> {
    Ok(hash(
        &serde_json::to_vec(&batch_intent(batch)).map_err(|e| e.to_string())?,
    ))
}
fn uuid(seed: &str, family: &str, index: usize) -> String {
    let mut bytes = Sha256::digest(format!(
        "auto-kicad-additive-uuid-v1:{seed}:{family}:{index}"
    ));
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(&bytes[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}
impl AdditiveCreationIdentities {
    /// Bind original source identity/revision and complete physical batch before shadow creation.
    pub fn derive(source_scope: &[u8], batch: &NativePcbBatch) -> Result<Self, String> {
        let batch_digest = physical_digest(batch)?;
        let seed_digest = hash(
            &serde_json::to_vec(&("auto-kicad-additive-ids-v1", source_scope, &batch_digest))
                .map_err(|e| e.to_string())?,
        );
        Ok(Self {
            tracks: (0..batch.tracks.len())
                .map(|i| uuid(&seed_digest, "track", i))
                .collect(),
            vias: (0..batch.vias.len())
                .map(|i| uuid(&seed_digest, "via", i))
                .collect(),
            seed_digest,
            batch_digest,
        })
    }
    pub(crate) fn track(&self, index: usize) -> &str {
        &self.tracks[index]
    }
    pub(crate) fn via(&self, index: usize) -> &str {
        &self.vias[index]
    }
    pub fn validate_batch(&self, batch: &NativePcbBatch) -> Result<(), String> {
        if self.batch_digest != physical_digest(batch)?
            || self.tracks.len() != batch.tracks.len()
            || self.vias.len() != batch.vias.len()
            || self
                .tracks
                .iter()
                .enumerate()
                .any(|(i, id)| *id != uuid(&self.seed_digest, "track", i))
            || self
                .vias
                .iter()
                .enumerate()
                .any(|(i, id)| *id != uuid(&self.seed_digest, "via", i))
        {
            return Err("additive creation allocation differs from physical batch".into());
        }
        let all = self
            .tracks
            .iter()
            .chain(&self.vias)
            .collect::<BTreeSet<_>>();
        if all.len() != self.tracks.len() + self.vias.len() {
            return Err("additive creation identities repeat".into());
        }
        Ok(())
    }
    pub(crate) fn validate_namespace(
        &self,
        before: &TeardropRefillObservation,
    ) -> Result<(), String> {
        self.validate_available(before.text().raw_text(), before.generated_ids())
    }
    fn validate_available<'a>(
        &self,
        text: &str,
        derived: impl Iterator<Item = &'a str>,
    ) -> Result<(), String> {
        let existing = LiveBoardText::identity_namespace(text, derived)?;
        if self
            .tracks
            .iter()
            .chain(&self.vias)
            .any(|id| existing.contains_key(id))
        {
            return Err("planned additive identity already exists in complete namespace".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_ipc::{CopperLayer, CreateThroughVia, CreateTrack};
    fn batch() -> NativePcbBatch {
        let track = CreateTrack {
            layer: CopperLayer::FRONT,
            start_x_nm: 0,
            start_y_nm: 0,
            end_x_nm: 1_000_000,
            end_y_nm: 0,
            width_nm: 250_000,
            net: None,
        };
        NativePcbBatch {
            graphics: vec![],
            tracks: vec![track.clone(), track],
            vias: vec![CreateThroughVia {
                copper_layers: vec![CopperLayer::FRONT, CopperLayer::BACK],
                position_x_nm: 1_000_000,
                position_y_nm: 0,
                pad_diameter_nm: 600_000,
                drill_diameter_nm: 300_000,
                net: None,
            }],
        }
    }
    #[test]
    fn allocation_binds_source_physical_intent_family_and_order() {
        let batch = batch();
        let ids = AdditiveCreationIdentities::derive(b"source-revision", &batch).unwrap();
        assert_eq!(
            ids,
            AdditiveCreationIdentities::derive(b"source-revision", &batch).unwrap()
        );
        assert_ne!(
            ids,
            AdditiveCreationIdentities::derive(b"other-revision", &batch).unwrap()
        );
        ids.validate_batch(&batch).unwrap();
        assert_ne!(ids.track(0), ids.track(1));
        assert_ne!(ids.track(0), ids.via(0));
        for mode in 0..5 {
            let mut changed = ids.clone();
            match mode {
                0 => changed.tracks.swap(0, 1),
                1 => {
                    changed.tracks.pop();
                }
                2 => changed.vias[0] = ids.track(0).into(),
                3 => changed.seed_digest.push('0'),
                _ => changed.tracks[0] = "malformed".into(),
            }
            assert!(changed.validate_batch(&batch).is_err());
        }
        let mut changed = batch.clone();
        changed.tracks[0].width_nm += 1;
        assert!(ids.validate_batch(&changed).is_err());
        assert_ne!(
            ids,
            AdditiveCreationIdentities::derive(b"source-revision", &changed).unwrap()
        );
        for source in [
            format!("(kicad_pcb (uuid board) (segment (uuid {})))", ids.track(0)),
            format!(
                "(kicad_pcb (uuid board) (footprint (uuid fp) (pad (uuid {}))))",
                ids.via(0)
            ),
        ] {
            assert!(ids.validate_available(&source, std::iter::empty()).is_err());
        }
        assert!(
            ids.validate_available("(kicad_pcb (uuid board))", [ids.track(0)].into_iter())
                .is_err()
        );
        assert!(
            ids.validate_available(
                "(kicad_pcb (uuid repeated) (pad (uuid repeated)))",
                std::iter::empty()
            )
            .is_err()
        );
        ids.validate_available("(kicad_pcb (uuid board))", ["derived"].into_iter())
            .unwrap();
    }
}
