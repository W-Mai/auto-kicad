//! Exact schematic coordinates and placed-symbol contact transforms.
use crate::{atom_text, list_items, list_tag};
use kiutils_sexpr::Node;

/// One schematic internal unit is 100 nm; construction never rounds coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Coordinate(i32);

impl Coordinate {
    pub fn from_nm(nm: i64) -> Result<Self, String> {
        if nm % 100 != 0 {
            return Err("schematic coordinate is off the 100 nm native grid".into());
        }
        i32::try_from(nm / 100)
            .map(Self)
            .map_err(|_| "schematic coordinate exceeds native range".into())
    }

    pub fn nm(self) -> i64 {
        i64::from(self.0) * 100
    }

    pub fn parse_mm(value: &str) -> Result<Self, String> {
        if value.is_empty() || value.len() > 64 || !value.is_ascii() {
            return Err("invalid schematic coordinate atom".into());
        }
        let (mantissa, exponent) = match value.split_once(['e', 'E']) {
            Some((m, e)) => (
                m,
                e.parse::<i32>()
                    .map_err(|_| "invalid coordinate exponent")?,
            ),
            None => (value, 0),
        };
        if !(-32..=32).contains(&exponent) {
            return Err("coordinate exponent exceeds bound".into());
        }
        let negative = mantissa.starts_with('-');
        let mantissa = mantissa.strip_prefix(['-', '+']).unwrap_or(mantissa);
        let mut magnitude = 0_i128;
        let mut fractional = None;
        let mut digits = 0;
        for byte in mantissa.bytes() {
            if byte == b'.' && fractional.is_none() {
                fractional = Some(0);
            } else if byte.is_ascii_digit() {
                magnitude = magnitude
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(i128::from(byte - b'0')))
                    .ok_or("coordinate magnitude exceeds bound")?;
                digits += 1;
                if let Some(n) = &mut fractional {
                    *n += 1;
                }
            } else {
                return Err("invalid schematic coordinate decimal".into());
            }
        }
        if digits == 0 {
            return Err("coordinate has no digits".into());
        }
        let scale: i32 = 4 + exponent - fractional.unwrap_or(0);
        let units = if magnitude == 0 {
            0
        } else if scale >= 0 {
            magnitude
                .checked_mul(
                    10_i128
                        .checked_pow(scale as u32)
                        .ok_or("coordinate scale overflow")?,
                )
                .ok_or("coordinate magnitude overflow")?
        } else {
            let divisor = 10_i128
                .checked_pow(scale.unsigned_abs())
                .ok_or("coordinate precision exceeds bound")?;
            if magnitude % divisor != 0 {
                return Err("schematic coordinate is off the 100 nm native grid".into());
            }
            magnitude / divisor
        };
        i32::try_from(if negative { -units } else { units })
            .map(Self)
            .map_err(|_| "schematic coordinate exceeds native range".into())
    }

    pub fn mm(self) -> String {
        let value = i64::from(self.0);
        let magnitude = value.abs();
        let sign = if value < 0 { "-" } else { "" };
        let fraction = format!("{:04}", magnitude % 10_000);
        let fraction = fraction.trim_end_matches('0');
        if fraction.is_empty() {
            format!("{sign}{}", magnitude / 10_000)
        } else {
            format!("{sign}{}.{fraction}", magnitude / 10_000)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Point {
    pub x: Coordinate,
    pub y: Coordinate,
}
impl Point {
    pub fn from_nm(x: i64, y: i64) -> Result<Self, String> {
        Ok(Self {
            x: Coordinate::from_nm(x)?,
            y: Coordinate::from_nm(y)?,
        })
    }

    pub(crate) fn from_at(items: &[Node]) -> Result<Self, String> {
        let coordinate =
            |i| Coordinate::parse_mm(atom_text(items.get(i)).ok_or("missing contact coordinate")?);
        Ok(Self {
            x: coordinate(1)?,
            y: coordinate(2)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuarterTurn {
    Zero,
    Counterclockwise90,
    Half,
    Counterclockwise270,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mirror {
    None,
    X,
    Y,
}

/// Library Y points upwards; sheet Y points downwards. Mirror applies after rotation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolTransform {
    pub origin: Point,
    pub rotation: QuarterTurn,
    pub mirror: Mirror,
}
impl SymbolTransform {
    pub fn contact(self, library: Point) -> Result<Point, String> {
        let (x, y) = (library.x.nm(), -library.y.nm());
        let (mut x, mut y) = match self.rotation {
            QuarterTurn::Zero => (x, y),
            QuarterTurn::Counterclockwise90 => (y, -x),
            QuarterTurn::Half => (-x, -y),
            QuarterTurn::Counterclockwise270 => (-y, x),
        };
        match self.mirror {
            Mirror::None => {}
            Mirror::X => y = -y,
            Mirror::Y => x = -x,
        }
        Point::from_nm(self.origin.x.nm() + x, self.origin.y.nm() + y)
    }

    pub(crate) fn from_cst(items: &[Node]) -> Result<Self, String> {
        let mut result = None;
        let mut mirrored = false;
        for child in items.iter().filter_map(list_items) {
            match list_tag(child) {
                Some("at") => {
                    if result.is_some() || child.len() != 4 {
                        return Err("ambiguous symbol position/rotation".into());
                    }
                    let angle = Coordinate::parse_mm(
                        atom_text(child.get(3)).ok_or("missing symbol rotation")?,
                    )?
                    .nm();
                    let rotation = match angle {
                        0 => QuarterTurn::Zero,
                        90_000_000 => QuarterTurn::Counterclockwise90,
                        180_000_000 => QuarterTurn::Half,
                        270_000_000 => QuarterTurn::Counterclockwise270,
                        _ => return Err("unsupported symbol rotation".into()),
                    };
                    result = Some(Self {
                        origin: Point::from_at(child)?,
                        rotation,
                        mirror: Mirror::None,
                    });
                }
                Some("mirror") => {
                    if mirrored || child.len() != 2 {
                        return Err("ambiguous symbol mirror".into());
                    }
                    let transform = result.as_mut().ok_or("mirror precedes symbol transform")?;
                    transform.mirror = match atom_text(child.get(1)) {
                        Some("x") => Mirror::X,
                        Some("y") => Mirror::Y,
                        _ => return Err("unsupported symbol mirror".into()),
                    };
                    mirrored = true;
                }
                _ => {}
            }
        }
        result.ok_or("missing symbol contact transform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_exact_bounded_and_round_trip_without_floats() {
        for iu in [i32::MIN, -1_000_001, -1, 0, 1, 9999, 10000, i32::MAX] {
            let coordinate = Coordinate::from_nm(i64::from(iu) * 100).unwrap();
            assert_eq!(Coordinate::parse_mm(&coordinate.mm()).unwrap(), coordinate);
        }
        for value in [".0001", "+0.00010", "1e-4", "100E-6"] {
            assert_eq!(Coordinate::parse_mm(value).unwrap().nm(), 100);
        }
        for invalid in [
            "NaN",
            "inf",
            "1e9999",
            "1e-5",
            "0.00001",
            "214748.3648",
            "-214748.3649",
            "1..0",
            ".",
            "1e",
            "1e--1",
            "1 0",
        ] {
            assert!(Coordinate::parse_mm(invalid).is_err(), "{invalid}");
        }
        assert!(Coordinate::from_nm(101).is_err());
    }

    #[test]
    fn transforms_preserve_grid_and_reject_overflow() {
        let origin = Point::from_nm(10_000_000, 20_000_000).unwrap();
        let pin = Point::from_nm(2_000_000, 3_000_000).unwrap();
        for rotation in [
            QuarterTurn::Zero,
            QuarterTurn::Counterclockwise90,
            QuarterTurn::Half,
            QuarterTurn::Counterclockwise270,
        ] {
            for mirror in [Mirror::None, Mirror::X, Mirror::Y] {
                let p = SymbolTransform {
                    origin,
                    rotation,
                    mirror,
                }
                .contact(pin)
                .unwrap();
                let dx = p.x.nm() - origin.x.nm();
                let dy = p.y.nm() - origin.y.nm();
                assert_eq!(dx * dx + dy * dy, 13_000_000_000_000);
            }
        }
        let max = Point::from_nm(i64::from(i32::MAX) * 100, 0).unwrap();
        assert!(
            SymbolTransform {
                origin: max,
                rotation: QuarterTurn::Zero,
                mirror: Mirror::None
            }
            .contact(pin)
            .is_err()
        );
    }

    #[test]
    fn ambiguous_or_unsupported_cst_transforms_reject() {
        for fields in [
            "(at 1 2 45)",
            "(at 1 2 0) (at 1 2 0)",
            "(mirror x) (at 1 2 0)",
            "(at 1 2 0) (mirror x) (mirror y)",
            "(at 1 2 0) (mirror z)",
            "(at 1 2)",
        ] {
            let doc = kiutils_sexpr::parse_one(&format!("(symbol {fields})")).unwrap();
            assert!(SymbolTransform::from_cst(list_items(&doc.nodes[0]).unwrap()).is_err());
        }
    }
}
