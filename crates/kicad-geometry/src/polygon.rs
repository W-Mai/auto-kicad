//! Bounded exact straight polygon topology without snapping or winding normalization.
use crate::{Bounds, GeometryError, Point, controls};

pub const MAX_POLYGON_VERTICES: usize = 512;
pub const MAX_POLYGON_HOLES: usize = 16;

fn cross(a: Point, b: Point, c: Point) -> i128 {
    (i128::from(b.0) - i128::from(a.0)) * (i128::from(c.1) - i128::from(a.1))
        - (i128::from(b.1) - i128::from(a.1)) * (i128::from(c.0) - i128::from(a.0))
}
fn on(a: Point, b: Point, p: Point) -> bool {
    cross(a, b, p) == 0
        && (a.0.min(b.0)..=a.0.max(b.0)).contains(&p.0)
        && (a.1.min(b.1)..=a.1.max(b.1)).contains(&p.1)
}
fn intersects(a: Point, b: Point, c: Point, d: Point) -> bool {
    on(a, b, c)
        || on(a, b, d)
        || on(c, d, a)
        || on(c, d, b)
        || (cross(a, b, c).signum() * cross(a, b, d).signum() < 0
            && cross(c, d, a).signum() * cross(c, d, b).signum() < 0)
}
fn edges(ring: &[Point]) -> impl Iterator<Item = (Point, Point)> + '_ {
    ring.iter()
        .copied()
        .zip(ring.iter().copied().cycle().skip(1))
}
// The caller has already excluded boundary contact. Half-open crossings avoid double-counting vertices.
fn inside(ring: &[Point], p: Point) -> bool {
    let mut winding = 0;
    for (a, b) in edges(ring) {
        if a.1 <= p.1 && b.1 > p.1 && cross(a, b, p) > 0 {
            winding += 1;
        }
        if a.1 > p.1 && b.1 <= p.1 && cross(a, b, p) < 0 {
            winding -= 1;
        }
    }
    winding != 0
}

/// Validate one exterior and disjoint interior holes; returned bounds do not certify board clearance.
pub fn polygon_bounds(outline: &[Point], holes: &[Vec<Point>]) -> Result<Bounds, GeometryError> {
    if holes.len() > MAX_POLYGON_HOLES
        || outline.len() < 3
        || holes.iter().any(|h| h.len() < 3)
        || outline.len() > MAX_POLYGON_VERTICES
        || holes
            .iter()
            .try_fold(outline.len(), |n, h| n.checked_add(h.len()))
            .is_none_or(|n| n > MAX_POLYGON_VERTICES)
    {
        return Err(GeometryError::PolygonCounts);
    }
    let rings = std::iter::once(outline)
        .chain(holes.iter().map(Vec::as_slice))
        .collect::<Vec<_>>();
    let mut bounds = controls(outline)?;
    for ring in &rings {
        bounds = bounds.union(controls(ring)?);
    }
    bounds.check_outline_span()?;
    for ring in &rings {
        let mut area = 0i128;
        for (i, (a, b)) in edges(ring).enumerate() {
            if a == b {
                return Err(GeometryError::Degenerate);
            }
            area += i128::from(a.0) * i128::from(b.1) - i128::from(b.0) * i128::from(a.1);
            for (j, (c, d)) in edges(ring).enumerate().skip(i + 1) {
                if !intersects(a, b, c, d) {
                    continue;
                }
                if j == i + 1 {
                    if on(a, b, d) || on(c, d, a) {
                        return Err(GeometryError::PolygonTopology);
                    }
                } else if i == 0 && j + 1 == ring.len() {
                    if on(a, b, c) || on(c, d, b) {
                        return Err(GeometryError::PolygonTopology);
                    }
                } else {
                    return Err(GeometryError::PolygonTopology);
                }
            }
        }
        if area == 0 {
            return Err(GeometryError::Degenerate);
        }
    }
    for (i, left) in rings.iter().enumerate() {
        for right in &rings[i + 1..] {
            if edges(left).any(|(a, b)| edges(right).any(|(c, d)| intersects(a, b, c, d))) {
                return Err(GeometryError::PolygonTopology);
            }
        }
    }
    for (i, hole) in holes.iter().enumerate() {
        if !inside(outline, hole[0]) {
            return Err(GeometryError::PolygonTopology);
        }
        for other in &holes[i + 1..] {
            if inside(hole, other[0]) || inside(other, hole[0]) {
                return Err(GeometryError::PolygonTopology);
            }
        }
    }
    Ok(bounds)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(a: i64, b: i64, c: i64, d: i64) -> Vec<Point> {
        vec![(a, b), (c, b), (c, d), (a, d)]
    }
    #[test]
    fn winding_start_and_forward_subdivision_do_not_change_topology() {
        let outer = vec![
            (0, 0),
            (20, 0),
            (20, 20),
            (12, 20),
            (12, 8),
            (8, 8),
            (8, 20),
            (0, 20),
        ];
        let hole = rect(2, 2, 6, 6);
        for reverse in [false, true] {
            for offset in 0..outer.len() {
                let mut o = outer.clone();
                o.rotate_left(offset);
                if reverse {
                    o.reverse();
                }
                let mut h = hole.clone();
                if !reverse {
                    h.reverse();
                }
                assert_eq!(polygon_bounds(&o, &[h]).unwrap().as_array(), [0, 0, 20, 20]);
            }
        }
        let mut subdivided = rect(0, 0, 20, 20);
        subdivided.insert(1, (10, 0));
        assert!(polygon_bounds(&subdivided, &[hole]).is_ok());
    }
    #[test]
    fn exact_topology_refuses_touches_crossings_and_invalid_holes() {
        let outer = rect(0, 0, 20, 20);
        for points in [
            vec![(0, 0), (20, 20), (0, 20), (20, 0)],
            vec![(0, 0), (20, 0), (10, 0), (10, 20)],
            vec![(0, 0), (20, 0), (20, 20), (0, 0)],
            vec![(0, 0), (10, 0), (20, 0)],
        ] {
            assert!(polygon_bounds(&points, &[]).is_err());
        }
        for holes in [
            vec![rect(-1, 2, 2, 4)],
            vec![rect(0, 2, 2, 4)],
            vec![rect(21, 21, 25, 25)],
            vec![rect(2, 2, 18, 18), rect(4, 4, 6, 6)],
            vec![rect(2, 2, 10, 10), rect(10, 2, 15, 8)],
            vec![rect(2, 2, 10, 10), rect(8, 8, 15, 15)],
        ] {
            assert!(polygon_bounds(&outer, &holes).is_err());
        }
        let concave = vec![
            (0, 0),
            (20, 0),
            (20, 20),
            (12, 20),
            (12, 8),
            (8, 8),
            (8, 20),
            (0, 20),
        ];
        assert!(polygon_bounds(&concave, &[rect(9, 10, 11, 12)]).is_err());
        assert!(polygon_bounds(&outer, &[rect(1, 1, 2, 2), rect(3, 1, 4, 2)]).is_ok());
    }
    #[test]
    fn resource_and_arithmetic_limits_apply_before_predicates() {
        let maximum = (0..128)
            .map(|i| (i, 0))
            .chain((0..128).map(|i| (128, i)))
            .chain((0..128).map(|i| (128 - i, 128)))
            .chain((0..128).map(|i| (0, 128 - i)))
            .collect::<Vec<_>>();
        assert!(polygon_bounds(&maximum, &[]).is_ok());
        let mut excess = maximum;
        excess.insert(1, (1, 1));
        assert!(polygon_bounds(&excess, &[]).is_err());
        assert!(polygon_bounds(&rect(0, 0, 100, 100), &vec![rect(1, 1, 2, 2); 17]).is_err());
        assert!(polygon_bounds(&rect(i64::MIN, 0, 1, 1), &[]).is_err());
        assert!(
            polygon_bounds(
                &rect(
                    -2_000_000_000,
                    -2_000_000_000,
                    -1_000_000_000,
                    -1_000_000_000
                ),
                &[]
            )
            .is_ok()
        );
        assert!(polygon_bounds(&rect(-2_000_000_000, 0, 2_000_000_000, 1), &[]).is_err());
    }
}
