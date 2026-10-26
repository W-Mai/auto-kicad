//! Compile revision-bound outline intent without treating primitives as a verified board boundary.
use crate::{EngineError, MAX_BOARD_POSITION_NM};
use kicad_model::{LockState, OutlineEdge, OutlineEdit, OutlinePoint, RevisionDigest, StableId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_OUTLINE_LOOPS: usize = 32;
pub const MAX_OUTLINE_PRIMITIVES: usize = 512;
pub const MAX_OUTLINE_REMOVALS: usize = 100;
pub const MAX_OUTLINE_SPAN_NM: i64 = kicad_geometry::MAX_OUTLINE_SPAN_NM;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum OutlineSourceGeometry {
    Segment,
    Arc,
    Circle,
    Rectangle,
    Polygon,
    Unsupported,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedOutlineTarget {
    pub source_uuid: StableId,
    pub lock_state: LockState,
    pub geometry: OutlineSourceGeometry,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledOutlinePrimitive {
    pub target_id: StableId,
    pub edge: OutlineEdge,
    pub width_nm: i64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledOutlineEdit {
    removed_ids: Vec<StableId>,
    primitives: Vec<CompiledOutlinePrimitive>,
}
impl CompiledOutlineEdit {
    pub fn removed_ids(&self) -> &[StableId] {
        &self.removed_ids
    }
    pub fn primitives(&self) -> &[CompiledOutlinePrimitive] {
        &self.primitives
    }
    /// Existing boundaries, arc intersections, hole topology and copper need native preview.
    pub fn requires_native_topology_check(&self) -> bool {
        true
    }
}
fn invalid(reason: &'static str) -> EngineError {
    EngineError::InvalidPlan(reason)
}
fn id(id: &StableId) -> Result<(), EngineError> {
    if id.as_str().trim().is_empty() || id.as_str().len() > 128 || id.as_str().contains('\0') {
        return Err(invalid("outline identity must be nonempty and bounded"));
    }
    Ok(())
}
fn cross(a: OutlinePoint, b: OutlinePoint, c: OutlinePoint) -> i128 {
    (i128::from(b.x_nm) - i128::from(a.x_nm)) * (i128::from(c.y_nm) - i128::from(a.y_nm))
        - (i128::from(b.y_nm) - i128::from(a.y_nm)) * (i128::from(c.x_nm) - i128::from(a.x_nm))
}
fn on(a: OutlinePoint, b: OutlinePoint, p: OutlinePoint) -> bool {
    cross(a, b, p) == 0
        && (a.x_nm.min(b.x_nm)..=a.x_nm.max(b.x_nm)).contains(&p.x_nm)
        && (a.y_nm.min(b.y_nm)..=a.y_nm.max(b.y_nm)).contains(&p.y_nm)
}
fn intersects(a: OutlinePoint, b: OutlinePoint, c: OutlinePoint, d: OutlinePoint) -> bool {
    on(a, b, c)
        || on(a, b, d)
        || on(c, d, a)
        || on(c, d, b)
        || (cross(a, b, c).signum() * cross(a, b, d).signum() < 0
            && cross(c, d, a).signum() * cross(c, d, b).signum() < 0)
}
fn shared_endpoint_only(
    a: OutlinePoint,
    b: OutlinePoint,
    c: OutlinePoint,
    d: OutlinePoint,
) -> bool {
    let shared = [a, b]
        .into_iter()
        .filter(|p| *p == c || *p == d)
        .collect::<Vec<_>>();
    if shared.len() != 1 {
        return false;
    }
    let p = shared[0];
    let left = if a == p { b } else { a };
    let right = if c == p { d } else { c };
    !on(p, left, right) && !on(p, right, left)
}

pub fn compile_outline_edit(
    intent: &OutlineEdit,
    requested_revision: &RevisionDigest,
    observed_revision: &RevisionDigest,
    existing: &[ObservedOutlineTarget],
) -> Result<CompiledOutlineEdit, EngineError> {
    if requested_revision != observed_revision {
        return Err(invalid("outline observation revision changed"));
    }
    if intent.loops.len() > MAX_OUTLINE_LOOPS
        || intent.remove_ids.len() > MAX_OUTLINE_REMOVALS
        || (intent.loops.is_empty() && intent.remove_ids.is_empty())
        || existing.len() > 4096
    {
        return Err(invalid("outline request counts are empty or exceed bounds"));
    }
    let mut sources = BTreeMap::new();
    for source in existing {
        id(&source.source_uuid)?;
        if source.geometry == OutlineSourceGeometry::Unsupported
            || sources.insert(&source.source_uuid, source).is_some()
        {
            return Err(invalid(
                "outline source contains unsupported geometry or duplicate identities",
            ));
        }
    }
    let mut removed = BTreeSet::new();
    for uuid in &intent.remove_ids {
        id(uuid)?;
        if !removed.insert(uuid) {
            return Err(invalid("outline removal identities must be unique"));
        }
        let source = sources
            .get(uuid)
            .ok_or(invalid("outline removal target is missing"))?;
        if source.lock_state != LockState::Unlocked {
            return Err(invalid("outline removal target is locked or unknown"));
        }
    }
    let mut loop_ids = BTreeSet::new();
    let mut primitives = Vec::new();
    let mut segments = Vec::new();
    let mut bounds: Option<kicad_geometry::Bounds> = None;
    for (loop_index, boundary) in intent.loops.iter().enumerate() {
        id(&boundary.loop_id)?;
        if !loop_ids.insert(&boundary.loop_id)
            || boundary.edges.len() < 2
            || boundary.edges.len() > MAX_OUTLINE_PRIMITIVES
            || boundary.width_nm <= 0
            || boundary.width_nm > MAX_BOARD_POSITION_NM
        {
            return Err(invalid(
                "outline loop identity, edge count or width is invalid",
            ));
        }
        let mut area = 0i128;
        let mut has_arc = false;
        for (index, edge) in boundary.edges.iter().enumerate() {
            if primitives.len() >= MAX_OUTLINE_PRIMITIVES {
                return Err(invalid("outline primitive count exceeds bound"));
            }
            let points = edge.control_points();
            let point = |p: OutlinePoint| (p.x_nm, p.y_nm);
            let envelope = match *edge {
                OutlineEdge::Segment { start, end } => {
                    kicad_geometry::segment_bounds(point(start), point(end))
                }
                OutlineEdge::Arc { start, mid, end } => {
                    kicad_geometry::arc_bounds(point(start), point(mid), point(end))
                }
            }
            .map_err(|error| invalid(error.message()))?;
            bounds = Some(bounds.map_or(envelope, |all| all.union(envelope)));
            if points.iter().collect::<BTreeSet<_>>().len() != points.len() {
                return Err(invalid("outline edge repeats a control point"));
            }
            if edge.end() != boundary.edges[(index + 1) % boundary.edges.len()].start() {
                return Err(invalid("outline loop endpoints must close exactly"));
            }
            match *edge {
                OutlineEdge::Arc { start, mid, end } => {
                    has_arc = true;
                    if cross(start, mid, end) == 0 {
                        return Err(invalid("outline arc points are collinear"));
                    }
                }
                OutlineEdge::Segment { start, end } => {
                    segments.push((loop_index, index, start, end))
                }
            }
            let a = edge.start();
            let b = edge.end();
            area +=
                i128::from(a.x_nm) * i128::from(b.y_nm) - i128::from(b.x_nm) * i128::from(a.y_nm);
            let target_id = StableId::new(format!("{}:edge:{index}", boundary.loop_id))
                .map_err(|_| invalid("invalid derived outline identity"))?;
            primitives.push(CompiledOutlinePrimitive {
                target_id,
                edge: edge.clone(),
                width_nm: boundary.width_nm,
            });
        }
        if !has_arc && area == 0 {
            return Err(invalid("straight outline loop has zero area"));
        }
    }
    if let Some(bounds) = bounds {
        bounds
            .check_outline_span()
            .map_err(|error| invalid(error.message()))?;
    }
    for (i, &(left_loop, left_index, a, b)) in segments.iter().enumerate() {
        for &(right_loop, right_index, c, d) in &segments[i + 1..] {
            if intersects(a, b, c, d) {
                let count = intent.loops[left_loop].edges.len();
                let adjacent = left_loop == right_loop
                    && ((left_index + 1) % count == right_index
                        || (right_index + 1) % count == left_index);
                if !adjacent || !shared_endpoint_only(a, b, c, d) {
                    return Err(invalid(
                        "outline straight edges intersect, overlap or touch outside an adjacent endpoint",
                    ));
                }
            }
        }
    }
    Ok(CompiledOutlineEdit {
        removed_ids: intent.remove_ids.clone(),
        primitives,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_model::OutlineLoop;
    fn revision() -> RevisionDigest {
        RevisionDigest::new("1".repeat(64)).unwrap()
    }
    fn polygon(points: &[(i64, i64)]) -> OutlineEdit {
        let p = |&(x_nm, y_nm)| OutlinePoint { x_nm, y_nm };
        OutlineEdit {
            remove_ids: vec![],
            loops: vec![OutlineLoop {
                loop_id: StableId::new("boundary").unwrap(),
                width_nm: 50_000,
                edges: points
                    .iter()
                    .zip(points.iter().cycle().skip(1))
                    .map(|(a, b)| OutlineEdge::Segment {
                        start: p(a),
                        end: p(b),
                    })
                    .collect(),
            }],
        }
    }
    fn compile(intent: &OutlineEdit) -> Result<CompiledOutlineEdit, EngineError> {
        compile_outline_edit(intent, &revision(), &revision(), &[])
    }
    fn arc_loop(start: (i64, i64), mid: (i64, i64), end: (i64, i64)) -> OutlineEdit {
        let p = |(x_nm, y_nm)| OutlinePoint { x_nm, y_nm };
        OutlineEdit {
            remove_ids: vec![],
            loops: vec![OutlineLoop {
                loop_id: StableId::new("arc").unwrap(),
                width_nm: 50_000,
                edges: vec![
                    OutlineEdge::Arc {
                        start: p(start),
                        mid: p(mid),
                        end: p(end),
                    },
                    OutlineEdge::Segment {
                        start: p(end),
                        end: p(start),
                    },
                ],
            }],
        }
    }
    #[test]
    fn compiler_enforces_full_sweeps_instead_of_only_control_points() {
        let valid = arc_loop((0, 0), (10_000_000, 9_000_000), (1_000_000, 0));
        assert!(compile(&valid).is_ok());
        assert!(compile(&arc_loop((0, 0), (100_000_000, 9_000_000), (1_000_000, 0))).is_err());
        assert!(compile(&arc_loop((0, 0), (1_000_000_000, 1), (1, 0))).is_err());
        let mut combined = arc_loop((0, 0), (100_000_000, 20_000_000), (1_000_000, 0));
        let mut second = arc_loop(
            (500_000_000, 0),
            (600_000_000, 20_000_000),
            (501_000_000, 0),
        )
        .loops
        .remove(0);
        second.loop_id = StableId::new("second").unwrap();
        combined.loops.push(second);
        assert!(compile(&combined).is_err());
    }
    #[test]
    fn compiler_requires_exact_closed_simple_straight_boundaries() {
        let square = polygon(&[(0, 0), (10, 0), (10, 10), (0, 10)]);
        assert_eq!(compile(&square).unwrap().primitives().len(), 4);
        for points in [
            vec![(0, 0), (10, 10), (10, 0), (0, 10)],
            vec![(0, 0), (10, 0), (5, 0), (5, 5)],
            vec![(0, 0), (0, 0), (5, 5)],
            vec![(0, 0), (10, 0), (20, 0)],
        ] {
            assert!(compile(&polygon(&points)).is_err());
        }
        let mut open = square.clone();
        open.loops[0].edges.pop();
        assert!(compile(&open).is_err());
        let mut touch = square.clone();
        let mut other = polygon(&[(10, 0), (20, 0), (20, 10), (10, 10)])
            .loops
            .remove(0);
        other.loop_id = StableId::new("other").unwrap();
        touch.loops.push(other);
        assert!(compile(&touch).is_err());
        let roundtrip: OutlineEdit =
            serde_json::from_str(&serde_json::to_string(&square).unwrap()).unwrap();
        assert_eq!(square, roundtrip);
        assert!(
            serde_json::from_str::<OutlineEdit>(r#"{"loops":[],"remove_ids":[],"unknown":true}"#)
                .is_err()
        );
    }
    #[test]
    fn compilation_binds_revision_and_exact_supported_unlocked_removals() {
        let uuid = StableId::new("native").unwrap();
        let intent = OutlineEdit {
            loops: vec![],
            remove_ids: vec![uuid.clone()],
        };
        let mut source = ObservedOutlineTarget {
            source_uuid: uuid,
            lock_state: LockState::Unlocked,
            geometry: OutlineSourceGeometry::Circle,
        };
        assert!(
            compile_outline_edit(&intent, &revision(), &revision(), &[source.clone()])
                .unwrap()
                .requires_native_topology_check()
        );
        assert!(
            compile_outline_edit(
                &intent,
                &revision(),
                &RevisionDigest::new("2".repeat(64)).unwrap(),
                &[source.clone()]
            )
            .is_err()
        );
        assert!(compile(&intent).is_err());
        for state in [LockState::Locked, LockState::Unknown] {
            source.lock_state = state;
            assert!(
                compile_outline_edit(&intent, &revision(), &revision(), &[source.clone()]).is_err()
            );
        }
        source.lock_state = LockState::Unlocked;
        source.geometry = OutlineSourceGeometry::Unsupported;
        assert!(compile_outline_edit(&intent, &revision(), &revision(), &[source]).is_err());
    }
    #[test]
    fn compiler_bounds_total_edges_and_retains_curves_for_native_verification() {
        let points = (0..128)
            .map(|i| (i, 0))
            .chain((0..128).map(|i| (128, i)))
            .chain((0..128).map(|i| (128 - i, 128)))
            .chain((0..128).map(|i| (0, 128 - i)))
            .collect::<Vec<_>>();
        assert_eq!(compile(&polygon(&points)).unwrap().primitives().len(), 512);
        let mut too_many = polygon(&points);
        too_many.loops.push(
            polygon(&[(1000, 0), (1001, 0), (1001, 1), (1000, 1)])
                .loops
                .remove(0),
        );
        too_many.loops[1].loop_id = StableId::new("other").unwrap();
        assert!(compile(&too_many).is_err());
        let a = OutlinePoint { x_nm: 0, y_nm: 0 };
        let b = OutlinePoint { x_nm: 0, y_nm: 20 };
        let circle = OutlineEdit {
            remove_ids: vec![],
            loops: vec![OutlineLoop {
                loop_id: StableId::new("circle").unwrap(),
                width_nm: 1,
                edges: vec![
                    OutlineEdge::Arc {
                        start: a,
                        mid: OutlinePoint { x_nm: 10, y_nm: 10 },
                        end: b,
                    },
                    OutlineEdge::Arc {
                        start: b,
                        mid: OutlinePoint {
                            x_nm: -10,
                            y_nm: 10,
                        },
                        end: a,
                    },
                ],
            }],
        };
        let compiled = compile(&circle).unwrap();
        assert_eq!(compiled.primitives()[0].edge, circle.loops[0].edges[0]);
        assert!(compiled.requires_native_topology_check());
        assert!(compile(&polygon(&[(i64::MIN, 0), (0, 0), (0, 1)])).is_err());
    }
}
