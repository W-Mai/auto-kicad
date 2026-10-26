//! Bind outline compilation to complete native/live observations and translate exact primitives.
use crate::live_geometry::LiveGeometryEvidence;
use kicad_engine::outline::{
    CompiledOutlineEdit, ObservedOutlineTarget, OutlineSourceGeometry, compile_outline_edit,
};
use kicad_ipc::{OutlineGeometry, OutlinePrimitive, base_types, board_types};
use kicad_model::{LockState, OutlineEdge, OutlineEdit, RevisionDigest, StableId};
use sha2::{Digest, Sha256};

pub fn outline_revision(observed: &LiveGeometryEvidence) -> Result<RevisionDigest, String> {
    let native = observed
        .native
        .evidence_bytes()
        .map_err(|e| e.to_string())?;
    let text = observed.text.raw_text().as_bytes();
    let mut hash = Sha256::new();
    for bytes in [native.as_slice(), text] {
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    RevisionDigest::new(hex::encode(hash.finalize())).map_err(|e| e.to_string())
}

pub fn compile_outline_change(
    intent: &OutlineEdit,
    expected_revision: &RevisionDigest,
    observed: &LiveGeometryEvidence,
) -> Result<CompiledOutlineEdit, String> {
    let mut targets = Vec::new();
    for (uuid, shape) in observed.native.shapes() {
        if shape.layer.enum_value() != Ok(board_types::BoardLayer::BL_Edge_Cuts) {
            continue;
        }
        let lock_state = match shape.locked.enum_value() {
            Ok(base_types::LockedState::LS_UNLOCKED) => LockState::Unlocked,
            Ok(base_types::LockedState::LS_LOCKED) => LockState::Locked,
            _ => LockState::Unknown,
        };
        let geometry = match shape.shape.as_ref().and_then(|s| s.geometry.as_ref()) {
            Some(base_types::graphic_shape::Geometry::Segment(_)) => OutlineSourceGeometry::Segment,
            Some(base_types::graphic_shape::Geometry::Arc(_)) => OutlineSourceGeometry::Arc,
            Some(base_types::graphic_shape::Geometry::Circle(_)) => OutlineSourceGeometry::Circle,
            Some(base_types::graphic_shape::Geometry::Rectangle(_)) => {
                OutlineSourceGeometry::Rectangle
            }
            Some(base_types::graphic_shape::Geometry::Polygon(_)) => OutlineSourceGeometry::Polygon,
            _ => OutlineSourceGeometry::Unsupported,
        };
        targets.push(ObservedOutlineTarget {
            source_uuid: StableId::new(uuid).map_err(|e| e.to_string())?,
            lock_state,
            geometry,
        });
    }
    compile_outline_edit(
        intent,
        expected_revision,
        &outline_revision(observed)?,
        &targets,
    )
    .map_err(|e| e.to_string())
}

pub fn native_outline_primitives(compiled: &CompiledOutlineEdit) -> Vec<OutlinePrimitive> {
    let point = |p: kicad_model::OutlinePoint| (p.x_nm, p.y_nm);
    compiled
        .primitives()
        .iter()
        .map(|primitive| OutlinePrimitive {
            geometry: match primitive.edge {
                OutlineEdge::Segment { start, end } => OutlineGeometry::Segment {
                    start: point(start),
                    end: point(end),
                },
                OutlineEdge::Arc { start, mid, end } => OutlineGeometry::Arc {
                    start: point(start),
                    mid: point(mid),
                    end: point(end),
                },
            },
            width_nm: primitive.width_nm,
        })
        .collect()
}
