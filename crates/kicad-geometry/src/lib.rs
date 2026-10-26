//! Exact integer envelopes for bounded board segments and circular arcs.
use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{ToPrimitive, Zero};
use std::fmt::{Display, Formatter};

pub const MAX_COORDINATE_NM: i64 = 2_000_000_000;
pub const MAX_OUTLINE_SPAN_NM: i64 = 1_000_000_000;
pub type Point = (i64, i64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryError {
    CoordinateBounds,
    Degenerate,
    EnvelopeBounds,
    SpanBounds,
}
impl GeometryError {
    pub fn message(self) -> &'static str {
        match self {
            Self::CoordinateBounds => "outline control coordinates exceed bounds",
            Self::Degenerate => "outline primitive is degenerate",
            Self::EnvelopeBounds => "complete outline curve exceeds coordinate bounds",
            Self::SpanBounds => "complete outline span exceeds bound",
        }
    }
}
impl Display for GeometryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for GeometryError {}

/// Complete primitive envelope, rounded outward to integer nanometres.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bounds {
    values: [i64; 4],
}
impl Bounds {
    pub fn as_array(self) -> [i64; 4] {
        self.values
    }
    pub fn union(self, other: Self) -> Self {
        let [a, b, c, d] = self.values;
        let [e, f, g, h] = other.values;
        Self {
            values: [a.min(e), b.min(f), c.max(g), d.max(h)],
        }
    }
    pub fn check_outline_span(self) -> Result<(), GeometryError> {
        let [a, b, c, d] = self.values;
        if c - a > MAX_OUTLINE_SPAN_NM || d - b > MAX_OUTLINE_SPAN_NM {
            Err(GeometryError::SpanBounds)
        } else {
            Ok(())
        }
    }
}
fn controls(points: &[Point]) -> Result<Bounds, GeometryError> {
    if points.iter().any(|&(x, y)| {
        x.unsigned_abs() > MAX_COORDINATE_NM as u64 || y.unsigned_abs() > MAX_COORDINATE_NM as u64
    }) {
        return Err(GeometryError::CoordinateBounds);
    }
    let mut values = [points[0].0, points[0].1, points[0].0, points[0].1];
    for &(x, y) in &points[1..] {
        values[0] = values[0].min(x);
        values[1] = values[1].min(y);
        values[2] = values[2].max(x);
        values[3] = values[3].max(y);
    }
    Ok(Bounds { values })
}
pub fn segment_bounds(start: Point, end: Point) -> Result<Bounds, GeometryError> {
    let result = controls(&[start, end])?;
    if start == end {
        return Err(GeometryError::Degenerate);
    }
    Ok(result)
}

/// Bound the exact sweep from start through mid to end without changing any control point.
pub fn arc_bounds(start: Point, mid: Point, end: Point) -> Result<Bounds, GeometryError> {
    let mut result = controls(&[start, mid, end])?;
    let (ax, ay) = (BigInt::from(start.0), BigInt::from(start.1));
    let (ux, uy) = (BigInt::from(mid.0) - &ax, BigInt::from(mid.1) - &ay);
    let (vx, vy) = (BigInt::from(end.0) - &ax, BigInt::from(end.1) - &ay);
    let cross = &ux * &vy - &uy * &vx;
    if cross.is_zero() {
        return Err(GeometryError::Degenerate);
    }
    let mut den: BigInt = &cross * 2;
    let unorm = &ux * &ux + &uy * &uy;
    let vnorm = &vx * &vx + &vy * &vy;
    let mut cx = &ax * &den + &unorm * &vy - &vnorm * &uy;
    let mut cy = &ay * &den + &ux * &vnorm - &vx * &unorm;
    if den.sign() == Sign::Minus {
        den = -den;
        cx = -cx;
        cy = -cy;
    }
    let mut a = (&ax * &den - &cx, &ay * &den - &cy);
    let mut b = (
        BigInt::from(end.0) * &den - &cx,
        BigInt::from(end.1) * &den - &cy,
    );
    let radius2 = &a.0 * &a.0 + &a.1 * &a.1;
    let floor_radius = radius2.sqrt();
    let ceil_radius =
        &floor_radius + BigInt::from(u8::from(&floor_radius * &floor_radius != radius2));
    if cross.sign() == Sign::Minus {
        std::mem::swap(&mut a, &mut b);
    }
    let sweep = &a.0 * &b.1 - &a.1 * &b.0;
    for (qx, qy) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
        let first: BigInt = &a.0 * qy - &a.1 * qx;
        let last: BigInt = qx * &b.1 - qy * &b.0;
        let first = first.sign() != Sign::Minus;
        let last = last.sign() != Sign::Minus;
        let inside = if sweep.sign() == Sign::Minus {
            first || last
        } else {
            first && last
        };
        if !inside {
            continue;
        }
        for (axis, center, q) in [(0, &cx, qx), (1, &cy, qy)] {
            let one: BigInt = center + q * &floor_radius;
            let two: BigInt = center + q * &ceil_radius;
            let (lo, hi) = if one <= two { (one, two) } else { (two, one) };
            let low = lo
                .div_floor(&den)
                .to_i64()
                .ok_or(GeometryError::EnvelopeBounds)?;
            let high = hi
                .div_ceil(&den)
                .to_i64()
                .ok_or(GeometryError::EnvelopeBounds)?;
            if low < -MAX_COORDINATE_NM || high > MAX_COORDINATE_NM {
                return Err(GeometryError::EnvelopeBounds);
            }
            result.values[axis] = result.values[axis].min(low);
            result.values[axis + 2] = result.values[axis + 2].max(high);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelopes_preserve_reversal_translation_and_exact_cardinal_extrema() {
        let ring = [
            (5, 0),
            (4, 3),
            (3, 4),
            (0, 5),
            (-3, 4),
            (-4, 3),
            (-5, 0),
            (-4, -3),
            (-3, -4),
            (0, -5),
            (3, -4),
            (4, -3),
        ];
        for (scale, cx, cy) in [
            (1, 0, 0),
            (100_000_000, -500_000_000, 400_000_000),
            (7, 30, -100),
        ] {
            let points = ring.map(|(x, y)| (x * scale + cx, y * scale + cy));
            for a in 0..12 {
                for b in 0..12 {
                    for c in 0..12 {
                        if a == b || a == c || b == c {
                            continue;
                        }
                        let forward = (b + 12 - a) % 12 < (c + 12 - a) % 12;
                        let (first, last) = if forward { (a, c) } else { (c, a) };
                        let selected = (first..=first + (last + 12 - first) % 12)
                            .map(|i| points[i % 12])
                            .collect::<Vec<_>>();
                        let expected = controls(&selected).unwrap();
                        let actual = arc_bounds(points[a], points[b], points[c]).unwrap();
                        assert_eq!(actual, expected);
                        assert_eq!(actual, arc_bounds(points[c], points[b], points[a]).unwrap());
                    }
                }
            }
        }
    }
    #[test]
    fn limits_apply_to_complete_curves_and_combined_envelopes() {
        let major = arc_bounds((0, 0), (100_000_000, 9_000_000), (1_000_000, 0)).unwrap();
        assert_eq!(major.check_outline_span(), Err(GeometryError::SpanBounds));
        assert!(
            arc_bounds((0, 0), (10_000_000, 9_000_000), (1_000_000, 0))
                .unwrap()
                .check_outline_span()
                .is_ok()
        );
        let half = arc_bounds((0, 0), (500_000_000, 500_000_000), (1_000_000_000, 0)).unwrap();
        assert!(half.check_outline_span().is_ok());
        let outside = segment_bounds((-1, 0), (0, 0)).unwrap();
        assert_eq!(
            half.union(outside).check_outline_span(),
            Err(GeometryError::SpanBounds)
        );
        assert!(arc_bounds((i64::MIN, 0), (0, 1), (1, 0)).is_err());
        assert!(arc_bounds((0, 0), (1, 1), (2, 2)).is_err());
        assert!(arc_bounds((0, 0), (1_000_000_000, 1), (1, 0)).is_err());
        assert!(arc_bounds((2_000_000_000, 0), (1_999_999_999, 1), (2_000_000_000, 2)).is_ok());
        assert!(arc_bounds((1_999_999_999, 0), (2_000_000_000, 1), (1_999_999_999, 1)).is_err());
    }
}
