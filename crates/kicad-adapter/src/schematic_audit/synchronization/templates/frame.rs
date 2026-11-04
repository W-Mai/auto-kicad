//! Full native template placement correspondence, including text and graphical children.
use super::*;

fn number(value: &str, scale: usize) -> Result<i64, String> {
    let value = decimal(value);
    let (negative, rest) = value
        .strip_prefix('-')
        .map_or((false, value.as_str()), |value| (true, value));
    let (integer, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    if integer.is_empty()
        || !integer.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > scale
    {
        return Err("invalid exact native template coordinate/angle".into());
    }
    let multiplier = 10_i128.pow(scale as u32);
    let integer = integer
        .parse::<i128>()
        .map_err(|_| "template number overflow")?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i128>()
            .map_err(|_| "template fraction overflow")?
    };
    let value = integer
        .checked_mul(multiplier)
        .and_then(|value| {
            value.checked_add(
                fraction
                    * 10_i128.pow(
                        (scale
                            - rest
                                .split_once('.')
                                .map_or(0, |(_, fraction)| fraction.len()))
                            as u32,
                    ),
            )
        })
        .ok_or("template number overflow")?;
    i64::try_from(if negative { -value } else { value })
        .map_err(|_| "template number exceeds native bounds".into())
}
fn text(value: &Projection) -> Result<&str, String> {
    if let Projection::Atom(_, value) = value {
        Ok(value)
    } else {
        Err("expected template scalar".into())
    }
}
fn tag(value: &Projection) -> Option<&str> {
    if let Projection::List(values) = value {
        values.first().and_then(|value| text(value).ok())
    } else {
        None
    }
}
fn scalar(value: &str) -> Projection {
    Projection::Atom(false, value.into())
}
fn negate(value: &str) -> String {
    let value = decimal(value);
    if value == "0" {
        value
    } else if let Some(value) = value.strip_prefix('-') {
        value.into()
    } else {
        format!("-{value}")
    }
}
fn minimum_rotation(values: &[Projection]) -> usize {
    let n = values.len();
    if n < 2 {
        return 0;
    }
    let (mut a, mut b, mut matched) = (0, 1, 0);
    while a < n && b < n && matched < n {
        match values[(a + matched) % n].cmp(&values[(b + matched) % n]) {
            std::cmp::Ordering::Equal => matched += 1,
            std::cmp::Ordering::Greater => {
                a += matched + 1;
                if a == b {
                    a += 1;
                }
                matched = 0;
            }
            std::cmp::Ordering::Less => {
                b += matched + 1;
                if a == b {
                    b += 1;
                }
                matched = 0;
            }
        }
    }
    a.min(b) % n
}
fn polygon(values: &mut Vec<Projection>) {
    if values.len() > 1 && values.first() == values.last() {
        values.pop();
    }
    if values.is_empty() {
        return;
    }
    let start = minimum_rotation(values);
    values.rotate_left(start);
    let mut reversed = values.iter().rev().cloned().collect::<Vec<_>>();
    let start = minimum_rotation(&reversed);
    reversed.rotate_left(start);
    if reversed < *values {
        *values = reversed;
    }
}

fn mm(value: i64) -> String {
    let magnitude = value.unsigned_abs();
    let fractional = format!("{:06}", magnitude % 1_000_000);
    let fractional = fractional.trim_end_matches('0');
    format!(
        "{}{}{}{}",
        if value < 0 { "-" } else { "" },
        magnitude / 1_000_000,
        if fractional.is_empty() { "" } else { "." },
        fractional
    )
}

/// Pinned native footprint graphics, text and pads store integer board coordinates. Their serializer
/// subtracts the footprint origin and rounds the inverse rotation back to local coordinates.
/// This is an exact expected conversion, never a numeric tolerance.
fn serialized_coordinate(
    point: [i64; 2],
    placement: &SynchronizationPlacement,
) -> Result<[i64; 2], String> {
    let radians = (f64::from(placement.orientation_mdeg) / 1000.0).to_radians();
    let (sin, cos) = radians.sin_cos();
    let [x, y] = point.map(|value| value as f64);
    let world = [
        (x * cos + y * sin).round() as i64,
        (-x * sin + y * cos).round() as i64,
    ];
    if world
        .iter()
        .zip(placement.position_nm)
        .any(|(value, origin)| {
            value
                .checked_add(origin)
                .is_none_or(|value| value.unsigned_abs() > 2_000_000_000)
        })
    {
        return Err("native template graphical coordinate exceeds board bounds".into());
    }
    Ok([
        ((world[0] as f64) * cos - (world[1] as f64) * sin).round() as i64,
        ((world[0] as f64) * sin + (world[1] as f64) * cos).round() as i64,
    ])
}

fn project_frame(
    node: &Node,
    root: bool,
    owner: &str,
    in_model: bool,
    placement: &SynchronizationPlacement,
    before: bool,
) -> Result<Projection, String> {
    let Node::List { items: values, .. } = node else {
        return project(node, false, &BTreeSet::new());
    };
    let name = atom(values.first().ok_or("empty native template form")?)?;
    let in_model = in_model || name == "model";
    let owner = if [
        "property",
        "fp_text",
        "fp_text_box",
        "pad",
        "fp_line",
        "fp_arc",
        "fp_circle",
        "fp_rect",
        "fp_poly",
        "fp_curve",
        "gr_line",
        "gr_arc",
        "gr_circle",
        "gr_rect",
        "gr_poly",
        "gr_curve",
    ]
    .contains(&name)
    {
        name
    } else {
        owner
    };
    let back = before && placement.side == SynchronizationSide::Back;
    let mut atoms = Vec::new();
    let mut lists = Vec::new();
    for (index, child) in values.iter().enumerate() {
        if let Node::List {
            items: children, ..
        } = child
        {
            let child_tag = atom(children.first().ok_or("empty native template child")?)?;
            if root && ["at", "layer"].contains(&child_tag) {
                continue;
            }
            lists.push(project_frame(
                child, false, owner, in_model, placement, before,
            )?);
        } else {
            let mut value = project(child, false, &BTreeSet::new())?;
            if back && ["layer", "layers"].contains(&name) && index > 0 {
                value = Projection::Atom(
                    matches!(
                        child,
                        Node::Atom {
                            atom: Atom::Quoted(_),
                            ..
                        }
                    ),
                    layer_flip(atom(child)?),
                );
            }
            if back
                && !in_model
                && index == 2
                && [
                    "at",
                    "start",
                    "end",
                    "mid",
                    "center",
                    "xy",
                    "offset",
                    "rect_delta",
                ]
                .contains(&name)
            {
                value = scalar(&negate(atom(child)?));
            }
            atoms.push(value);
        }
    }
    if before
        && !in_model
        && atoms.len() >= 3
        && ["at", "start", "end", "mid", "center", "xy"].contains(&name)
        && [
            "property",
            "fp_text",
            "fp_text_box",
            "fp_line",
            "fp_rect",
            "fp_arc",
            "fp_circle",
            "fp_curve",
            "fp_poly",
            "pad",
        ]
        .contains(&owner)
    {
        let point = serialized_coordinate(
            [number(text(&atoms[1])?, 6)?, number(text(&atoms[2])?, 6)?],
            placement,
        )?;
        atoms[1] = scalar(&mm(point[0]));
        atoms[2] = scalar(&mm(point[1]));
    }
    if !in_model && name == "at" && ["property", "fp_text", "fp_text_box", "pad"].contains(&owner) {
        if !(atoms.len() == 3 || atoms.len() == 4) {
            return Err("unexpected native template pose grammar".into());
        }
        let old = if atoms.len() == 4 {
            number(text(&atoms[3])?, 6)?
        } else {
            0
        };
        let next = if before {
            let text_back = back && owner != "pad";
            (if back { -old } else { old })
                + i64::from(placement.orientation_mdeg) * 1000
                + if text_back { 180_000_000 } else { 0 }
        } else {
            old
        };
        atoms.truncate(3);
        atoms.push(scalar(&next.rem_euclid(360_000_000).to_string()));
    }
    if name == "layers" && atoms.len() > 1 {
        atoms[1..].sort();
    }
    if name == "justify" {
        if back {
            if let Some(index) = atoms
                .iter()
                .position(|value| text(value).is_ok_and(|value| value == "mirror"))
            {
                atoms.remove(index);
            } else {
                atoms.push(scalar("mirror"));
            }
        }
        if atoms.len() > 1 {
            atoms[1..].sort();
        }
    }
    if name == "effects" && back && !lists.iter().any(|child| tag(child) == Some("justify")) {
        lists.push(Projection::List(vec![scalar("justify"), scalar("mirror")]));
    }
    lists.retain(|child| !matches!(child,Projection::List(values) if values.len()==1 && tag(child)==Some("justify")));
    let converted_rectangle =
        before && name == "fp_rect" && !placement.orientation_mdeg.is_multiple_of(90000);
    if converted_rectangle {
        // Native EDA_SHAPE rotation converts a non-rounded rectangle into a closed polygon.
        // Derive all four vertices from the original local rectangle before integer rotation.
        let start = fields(node, "start")?;
        let end = fields(node, "end")?;
        if start.len() != 1 || end.len() != 1 {
            return Err("ambiguous native rectangle definition".into());
        }
        for radius in fields(node, "radius")? {
            let radius = items(radius)?;
            if radius.len() != 2 || number(atom(&radius[1])?, 6)? != 0 {
                return Err(
                    "rounded rectangle conversion requires complete arc qualification".into(),
                );
            }
        }
        let coords = |node: &Node| -> Result<[i64; 2], String> {
            let values = items(node)?;
            if values.len() != 3 {
                return Err("invalid native rectangle coordinate".into());
            }
            Ok([number(atom(&values[1])?, 6)?, number(atom(&values[2])?, 6)?])
        };
        let a = coords(start[0])?;
        let b = coords(end[0])?;
        let mut points = Vec::new();
        for mut point in [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]] {
            if placement.side == SynchronizationSide::Back {
                point[1] = -point[1];
            }
            let point = serialized_coordinate(point, placement)?;
            points.push(Projection::List(vec![
                scalar("xy"),
                scalar(&mm(point[0])),
                scalar(&mm(point[1])),
            ]));
        }
        polygon(&mut points);
        let mut pts = vec![scalar("pts")];
        pts.extend(points);
        lists.retain(|child| ![Some("start"), Some("end"), Some("radius")].contains(&tag(child)));
        lists.push(Projection::List(pts));
        atoms[0] = scalar("fp_poly");
    }
    if !converted_rectangle && ["fp_rect", "gr_rect"].contains(&name) {
        let start = lists
            .iter()
            .position(|node| tag(node) == Some("start"))
            .ok_or("rectangle start absent")?;
        let end = lists
            .iter()
            .position(|node| tag(node) == Some("end"))
            .ok_or("rectangle end absent")?;
        let coords = |node: &Projection| -> Result<[i64; 2], String> {
            let Projection::List(values) = node else {
                return Err("invalid rectangle coordinate".into());
            };
            if values.len() != 3 {
                return Err("invalid rectangle coordinate".into());
            }
            Ok([number(text(&values[1])?, 6)?, number(text(&values[2])?, 6)?])
        };
        let a = coords(&lists[start])?;
        let b = coords(&lists[end])?;
        lists[start] = Projection::List(vec![
            scalar("start"),
            scalar(&a[0].min(b[0]).to_string()),
            scalar(&a[1].min(b[1]).to_string()),
        ]);
        lists[end] = Projection::List(vec![
            scalar("end"),
            scalar(&a[0].max(b[0]).to_string()),
            scalar(&a[1].max(b[1]).to_string()),
        ]);
    }
    if name == "pts"
        && ["fp_poly", "gr_poly"].contains(&owner)
        && lists.iter().all(|node| tag(node) == Some("xy"))
    {
        polygon(&mut lists);
    }
    if !["pts", "primitives", "pins", "members"].contains(&name) {
        lists.sort();
    }
    atoms.extend(lists);
    Ok(Projection::List(atoms))
}

fn difference(left: &Projection, right: &Projection, path: &str) -> String {
    match (left, right) {
        (Projection::List(left), Projection::List(right)) => {
            if left.len() != right.len() {
                return format!("{path}: lengths {} vs {}", left.len(), right.len());
            }
            for (index, (a, b)) in left.iter().zip(right).enumerate() {
                if a != b {
                    return difference(
                        a,
                        b,
                        &format!("{path}/{index}:{}", tag(a).unwrap_or("scalar")),
                    );
                }
            }
            path.into()
        }
        _ => format!("{path}: {left:?} vs {right:?}"),
    }
}

pub(super) fn verify(
    before: &SavedSynchronizationBoard,
    after: &SavedSynchronizationBoard,
    id: &str,
    placement: &SynchronizationPlacement,
) -> Result<(), String> {
    let before_node = before.node(id)?;
    let after_node = after.node(id)?;
    for (node, expected_side, expected_position, expected_angle) in [
        (before_node, "F.Cu", [0, 0], 0_i64),
        (
            after_node,
            if placement.side == SynchronizationSide::Front {
                "F.Cu"
            } else {
                "B.Cu"
            },
            placement.position_nm,
            i64::from(placement.orientation_mdeg) * 1000,
        ),
    ] {
        let layer = fields(node, "layer")?;
        let at = fields(node, "at")?;
        if layer.len() != 1 || at.len() != 1 {
            return Err("native template root pose is ambiguous".into());
        }
        let layer = items(layer[0])?;
        let at = items(at[0])?;
        if layer.len() != 2
            || atom(&layer[1])? != expected_side
            || !(at.len() == 3 || at.len() == 4)
            || [number(atom(&at[1])?, 6)?, number(atom(&at[2])?, 6)?] != expected_position
            || (if at.len() == 4 {
                number(atom(&at[3])?, 6)?
            } else {
                0
            })
            .rem_euclid(360_000_000)
                != expected_angle
        {
            return Err(
                "complete native template root pose differs from the explicit placement".into(),
            );
        }
    }
    let expected = project_frame(before_node, true, "footprint", false, placement, true)?;
    let actual = project_frame(after_node, true, "footprint", false, placement, false)?;
    if expected != actual {
        return Err(format!(
            "native template placement changed unrelated complete child content or geometry: {}",
            difference(&expected, &actual, "footprint")
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibrated_serialization_preserves_every_other_field_and_rejects_one_nm_corruption() {
        let id = "11111111-1111-4111-8111-111111111111";
        let child = "22222222-2222-4222-8222-222222222222";
        let before = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{id}") (layer "F.Cu") (at 0 0)
            (property "Reference" "REF**" (at 0 0 0) (layer "F.Fab") (effects (font (size 1 1)))) (property "Value" "Part")
            (future "retained") (fp_line (uuid "{child}") (start -3.1 -2.13) (end -2.13 3.1) (layer "F.SilkS") (stroke (width 0.12) (type solid)))))"#
        );
        let after = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{id}") (layer "B.Cu") (at 30 25 45)
            (property "Reference" "REF**" (at 0 0 225) (layer "B.Fab") (effects (font (size 1 1)) (justify mirror))) (property "Value" "Part")
            (future "retained") (fp_line (uuid "{child}") (start -3.1 2.129999) (end -2.129999 -3.1) (layer "B.SilkS") (stroke (width 0.12) (type solid)))))"#
        );
        let before = SavedSynchronizationBoard::parse(&before).unwrap();
        let placement = SynchronizationPlacement {
            position_nm: [30_000_000, 25_000_000],
            orientation_mdeg: 45000,
            side: SynchronizationSide::Back,
        };
        verify(
            &before,
            &SavedSynchronizationBoard::parse(&after).unwrap(),
            id,
            &placement,
        )
        .unwrap();
        for corrupted in [
            after.replace("-2.129999", "-2.129998"),
            after.replace("retained", "changed"),
            after.replace("(size 1 1)", "(size 2 1)"),
            after.replace("(at 30 25 45)", "(at 31 25 45)"),
        ] {
            assert!(
                verify(
                    &before,
                    &SavedSynchronizationBoard::parse(&corrupted).unwrap(),
                    id,
                    &placement
                )
                .is_err()
            );
        }
    }

    #[test]
    fn rectangle_conversion_requires_all_four_exact_corners_and_retains_every_other_field() {
        let id = "11111111-1111-4111-8111-111111111111";
        let child = "22222222-2222-4222-8222-222222222222";
        let before = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{id}") (layer "F.Cu") (at 0 0)
            (property "Reference" "REF**") (property "Value" "Part")
            (fp_rect (uuid "{child}") (start -0.8 -0.4125) (end 0.8 0.4125) (layer "F.Fab") (fill no) (stroke (width 0.1) (type solid)))))"#
        );
        let after = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (uuid "{id}") (layer "B.Cu") (at 30 25 45)
            (property "Reference" "REF**") (property "Value" "Part")
            (fp_poly (uuid "{child}") (pts (xy 0.8 0.4125) (xy -0.8 0.4125) (xy -0.8 -0.4125) (xy 0.8 -0.4125)) (layer "B.Fab") (fill no) (stroke (width 0.1) (type solid)))))"#
        );
        let before = SavedSynchronizationBoard::parse(&before).unwrap();
        let placement = SynchronizationPlacement {
            position_nm: [30_000_000, 25_000_000],
            orientation_mdeg: 45000,
            side: SynchronizationSide::Back,
        };
        verify(
            &before,
            &SavedSynchronizationBoard::parse(&after).unwrap(),
            id,
            &placement,
        )
        .unwrap();
        for corrupted in [
            after.replace("(xy 0.8 -0.4125)", ""),
            after.replace("(xy 0.8 -0.4125)", "(xy 0.800001 -0.4125)"),
            after.replace("(width 0.1)", "(width 0.2)"),
            after.replace("(fill no)", "(fill yes)"),
        ] {
            assert!(
                verify(
                    &before,
                    &SavedSynchronizationBoard::parse(&corrupted).unwrap(),
                    id,
                    &placement
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cyclic_polygon_and_exact_decimal_operations_preserve_complete_order_and_values() {
        let placement = SynchronizationPlacement {
            position_nm: [30_000_000, 25_000_000],
            orientation_mdeg: 45000,
            side: SynchronizationSide::Back,
        };
        assert_eq!(
            serialized_coordinate([-2_130_000, -3_100_000], &placement).unwrap(),
            [-2_129_999, -3_100_000]
        );
        assert_eq!(
            serialized_coordinate([-2_129_999, -3_100_000], &placement).unwrap(),
            [-2_129_999, -3_100_000]
        );
        assert_ne!(
            serialized_coordinate([-2_130_100, -3_100_000], &placement).unwrap(),
            [-2_129_999, -3_100_000]
        );
        assert_eq!(number("-0.000001", 6).unwrap(), -1);
        assert!(number("0.0000001", 6).is_err());
        assert!(number(".", 6).is_err());
        let points = vec![scalar("a"), scalar("b"), scalar("c"), scalar("a")];
        let mut a = points.clone();
        polygon(&mut a);
        let mut b = points.iter().rev().cloned().collect();
        polygon(&mut b);
        assert_eq!(a, b);
        let mut changed = vec![scalar("a"), scalar("b"), scalar("d")];
        polygon(&mut changed);
        assert_ne!(a, changed);
    }
}
