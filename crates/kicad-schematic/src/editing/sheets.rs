//! Complete existing-sheet projections with explicit hierarchical pin contacts.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetRectangle {
    pub position: Point,
    pub width: Coordinate,
    pub height: Coordinate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetPin {
    pub name: String,
    pub shape: ElectricalShape,
    pub position: Point,
    pub rotation: QuarterTurn,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SheetPinEdit {
    Create { key: String, pin: SheetPin },
    Update { uuid: String, pin: SheetPin },
    Delete { uuid: String },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SheetUpdate {
    pub name: Option<String>,
    pub rectangle: Option<SheetRectangle>,
    pub pins: Vec<SheetPinEdit>,
}

fn segment(name: &str) -> Result<(), String> {
    text(name, false)?;
    if name.trim().is_empty()
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
    {
        return Err("sheet name must be a nonempty single-line path segment".into());
    }
    Ok(())
}

fn pin_text(name: &str) -> Result<(), String> {
    text(name, false)?;
    if name.trim().is_empty()
        || name == "~"
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '[' | ']' | '{' | '}'))
    {
        return Err("empty, multiline, variable or bus sheet pins are unsupported".into());
    }
    Ok(())
}

impl SheetRectangle {
    fn validate(&self) -> Result<(), String> {
        if self.width.nm() <= 0 || self.height.nm() <= 0 {
            return Err("sheet dimensions must be positive".into());
        }
        Point::from_nm(
            self.position.x.nm() + self.width.nm(),
            self.position.y.nm() + self.height.nm(),
        )?;
        Ok(())
    }
    fn contains_pin(&self, pin: &SheetPin) -> bool {
        let (x, y) = (pin.position.x.nm(), pin.position.y.nm());
        let (left, top) = (self.position.x.nm(), self.position.y.nm());
        let (right, bottom) = (left + self.width.nm(), top + self.height.nm());
        match pin.rotation {
            QuarterTurn::Zero => x == right && y > top && y < bottom,
            QuarterTurn::Half => x == left && y > top && y < bottom,
            QuarterTurn::Counterclockwise90 => y == top && x > left && x < right,
            QuarterTurn::Counterclockwise270 => y == bottom && x > left && x < right,
        }
    }
}

fn rectangle(items: &[Node]) -> Result<SheetRectangle, String> {
    let position = values(one(items, "at")?)?;
    let size = values(one(items, "size")?)?;
    if position.len() != 3 || size.len() != 3 {
        return Err("unsupported sheet rectangle".into());
    }
    let rectangle = SheetRectangle {
        position: Point::from_at(position)?,
        width: Coordinate::parse_mm(atom(size, 1)?)?,
        height: Coordinate::parse_mm(atom(size, 2)?)?,
    };
    rectangle.validate()?;
    Ok(rectangle)
}

fn pin(items: &[Node]) -> Result<SheetPin, String> {
    let contact = values(one(items, "at")?)?;
    if contact.len() != 4 {
        return Err("unsupported sheet pin contact".into());
    }
    Ok(SheetPin {
        name: atom(items, 1)?.into(),
        shape: match atom(items, 2)? {
            "input" => ElectricalShape::Input,
            "output" => ElectricalShape::Output,
            "bidirectional" => ElectricalShape::Bidirectional,
            "tri_state" => ElectricalShape::TriState,
            "passive" => ElectricalShape::Passive,
            _ => return Err("unsupported sheet pin electrical type".into()),
        },
        position: Point::from_at(contact)?,
        rotation: match atom(contact, 3)? {
            "0" => QuarterTurn::Zero,
            "90" => QuarterTurn::Counterclockwise90,
            "180" => QuarterTurn::Half,
            "270" => QuarterTurn::Counterclockwise270,
            _ => return Err("unsupported sheet pin rotation".into()),
        },
    })
}

pub(super) fn allocate<'a>(
    revision: &str,
    edits: &'a [SchematicEdit],
    seen: &mut BTreeSet<String>,
    keys: &mut BTreeSet<&'a String>,
) -> Result<BTreeMap<(usize, usize), String>, String> {
    let mut allocations = BTreeMap::new();
    let mut targets = BTreeSet::new();
    let mut count = edits.len();
    for (ordinal, edit) in edits.iter().enumerate() {
        let EditAction::UpdateSheet { uuid, update } = &edit.action else {
            continue;
        };
        if !targets.insert((&edit.file, uuid)) {
            return Err("one complete update per sheet placement is required".into());
        }
        count = count
            .checked_add(update.pins.len())
            .ok_or("sheet edit bound exceeded")?;
        if count > 128 {
            return Err("outer and nested schematic edits exceed 128 operations".into());
        }
        for (nested, edit) in update.pins.iter().enumerate() {
            if let SheetPinEdit::Create { key, .. } = edit {
                text(key, false)?;
                if !keys.insert(key) {
                    return Err("duplicate creation key".into());
                }
                let id =
                    generated_uuid_seed(&format!("{revision}:sheet-pin:{ordinal}:{nested}:{key}"));
                if !seen.insert(id.clone()) {
                    return Err("allocated sheet pin UUID collision".into());
                }
                allocations.insert((ordinal, nested), id);
            }
        }
    }
    Ok(allocations)
}

pub(super) fn validate_names(
    root: &[Node],
    file: &str,
    edits: &[SchematicEdit],
) -> Result<(), String> {
    let mut final_names = BTreeSet::new();
    let mut touched = false;
    let mut remaining = 0;
    for node in children(root, "sheet") {
        let items = values(node)?;
        let uuid = atom(values(one(items, "uuid")?)?, 1)?;
        if edits.iter().any(|edit| edit.file == file && matches!(&edit.action, EditAction::RemoveSheet { uuid: removed } if removed == uuid)) {
            continue;
        }
        remaining += 1;
        let requested = edits.iter().find_map(|edit| match &edit.action {
            EditAction::UpdateSheet { uuid: id, update } if edit.file == file && id == uuid => {
                touched = true;
                update.name.as_deref()
            }
            _ => None,
        });
        let properties = children(items, "property")
            .filter(|p| list_items(p).is_some_and(|v| atom_text(v.get(1)) == Some("Sheetname")))
            .collect::<Vec<_>>();
        if properties.len() != 1 {
            return Err("sheet requires exactly one Sheetname property".into());
        }
        let name = requested.unwrap_or(atom(values(properties[0])?, 2)?);
        final_names.insert(name);
    }
    if touched && final_names.len() != remaining {
        return Err("resulting sibling sheet names must be unique".into());
    }
    Ok(())
}

pub(super) fn edit(
    root: &[Node],
    id: &str,
    update: &SheetUpdate,
    ordinal: usize,
    allocations: &BTreeMap<(usize, usize), String>,
    patches: &mut Patches<'_>,
) -> Result<(String, ObjectKind), String> {
    let node = object(root, id, ObjectKind::Sheet)?;
    let items = values(node)?;
    let old_rectangle = rectangle(items)?;
    let new_rectangle = update.rectangle.as_ref().unwrap_or(&old_rectangle);
    new_rectangle.validate()?;
    if update.name.is_none() && update.rectangle.is_none() && update.pins.is_empty() {
        return Err("empty sheet update".into());
    }
    if let Some(name) = &update.name {
        segment(name)?;
        let field = children(items, "property")
            .find(|p| list_items(p).is_some_and(|v| atom_text(v.get(1)) == Some("Sheetname")))
            .ok_or("missing Sheetname property")?;
        patches.node(&values(field)?[2], quote(name))?;
    }
    if update.rectangle.is_some() {
        patches.node(one(items, "at")?, at(new_rectangle.position, None))?;
        patches.node(
            one(items, "size")?,
            format!(
                "(size {} {})",
                new_rectangle.width.mm(),
                new_rectangle.height.mm()
            ),
        )?;
        let dx = new_rectangle.position.x.nm() - old_rectangle.position.x.nm();
        let dy = new_rectangle.position.y.nm() - old_rectangle.position.y.nm();
        if dx != 0 || dy != 0 {
            for field in children(items, "property") {
                let contact = one(values(field)?, "at")?;
                let coordinates = values(contact)?;
                if coordinates.len() != 4 {
                    return Err("unsupported sheet property position".into());
                }
                let point = Point::from_at(coordinates)?;
                let point = Point::from_nm(point.x.nm() + dx, point.y.nm() + dy)?;
                // Preserve the source rotation token and every property effect.
                patches.node(&coordinates[1], point.x.mm())?;
                patches.node(&coordinates[2], point.y.mm())?;
            }
        }
    }
    let mut nodes = BTreeMap::new();
    let mut projected = BTreeMap::new();
    for node in children(items, "pin") {
        let items = values(node)?;
        let uuid = atom(values(one(items, "uuid")?)?, 1)?.to_owned();
        nodes.insert(uuid.clone(), node);
        projected.insert(uuid, pin(items)?);
    }
    let mut touched = BTreeSet::new();
    for (nested, edit) in update.pins.iter().enumerate() {
        match edit {
            SheetPinEdit::Create { pin, .. } => {
                let uuid = &allocations[&(ordinal, nested)];
                projected.insert(uuid.clone(), pin.clone());
                patches.insert(
                    node_span(node).end - 1,
                    format!(
                        "\n(pin {} {} {} (uuid {}) (effects (font (size 1.27 1.27))))\n",
                        quote(&pin.name),
                        pin.shape.atom(),
                        at(pin.position, Some(pin.rotation)),
                        quote(uuid),
                    ),
                )?;
            }
            SheetPinEdit::Update { uuid, pin } => {
                if !touched.insert(uuid) {
                    return Err("repeated sheet pin edit".into());
                }
                let node = nodes
                    .get(uuid)
                    .ok_or("sheet pin UUID does not belong to target placement")?;
                let items = values(node)?;
                patches.node(&items[1], quote(&pin.name))?;
                patches.node(&items[2], pin.shape.atom().into())?;
                patches.node(one(items, "at")?, at(pin.position, Some(pin.rotation)))?;
                projected.insert(uuid.clone(), pin.clone());
            }
            SheetPinEdit::Delete { uuid } => {
                if !touched.insert(uuid) {
                    return Err("repeated sheet pin edit".into());
                }
                let node = nodes
                    .get(uuid)
                    .ok_or("sheet pin UUID does not belong to target placement")?;
                patches.node(node, String::new())?;
                projected.remove(uuid);
            }
        }
    }
    let mut names = BTreeSet::new();
    let mut contacts = BTreeSet::new();
    for pin in projected.values() {
        pin_text(&pin.name)?;
        if !names.insert(&pin.name) || !contacts.insert(pin.position) {
            return Err("resulting sheet pin names and contacts must be unique".into());
        }
        if !new_rectangle.contains_pin(pin) {
            return Err(
                "sheet pin must lie on a non-corner edge with its matching direction".into(),
            );
        }
    }
    Ok((id.to_owned(), ObjectKind::Sheet))
}

#[cfg(test)]
mod tests {
    use super::*;
    const SHEET: &str = "11111111-1111-4111-8111-111111111111";
    const PIN: &str = "22222222-2222-4222-8222-222222222222";
    fn source() -> String {
        format!(
            r#"(kicad_sch (sheet (at 10 10) (size 20 10) (uuid "{SHEET}")
            (property "Sheetname" "Child" (at 10 9 90) (effects (font (size 1.27 1.27))))
            (property "Sheetfile" "child.kicad_sch" (at 10 21 0) (effects (font (size 1.27 1.27))))
            (property "Custom" "keep" (at 11 12 270) (unknown (nested "exact bytes")))
            (pin "OUT" output (at 30 15 0) (uuid "{PIN}") (effects (font (size 2 2))) (unknown pin-data))
            (instances (project "test" (path "/root" (page "2")))) (unknown sheet-data)))"#
        )
    }
    fn point(x: i64, y: i64) -> Point {
        Point::from_nm(x * 1_000_000, y * 1_000_000).unwrap()
    }
    fn output(x: i64, y: i64) -> SheetPin {
        SheetPin {
            name: "OUT".into(),
            shape: ElectricalShape::Output,
            position: point(x, y),
            rotation: QuarterTurn::Zero,
        }
    }
    fn apply(source: &str, update: &SheetUpdate) -> Result<String, String> {
        let document = parse_one(source).unwrap();
        let mut patches = Patches {
            text: source,
            replacements: vec![],
            claims: BTreeSet::new(),
        };
        edit(
            values(&document.nodes[0]).unwrap(),
            SHEET,
            update,
            0,
            &BTreeMap::new(),
            &mut patches,
        )?;
        let (bytes, spans) = patches.finish()?;
        // Every replacement is bound to its exact immutable input span.
        for span in &spans {
            assert_eq!(&source[span.start..span.end], span.before);
        }
        Ok(String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn complete_projection_requires_explicit_pin_movement_and_preserves_other_fields() {
        let source = source();
        let mut update = SheetUpdate {
            name: Some("Renamed".into()),
            rectangle: Some(SheetRectangle {
                position: point(11, 12),
                width: Coordinate::parse_mm("30").unwrap(),
                height: Coordinate::parse_mm("20").unwrap(),
            }),
            pins: vec![],
        };
        assert!(apply(&source, &update).is_err());
        update.pins.push(SheetPinEdit::Update {
            uuid: PIN.into(),
            pin: output(41, 17),
        });
        let changed = apply(&source, &update).unwrap();
        for protected in [
            "(unknown sheet-data)",
            "(unknown pin-data)",
            "(effects (font (size 2 2)))",
            "(instances (project \"test\" (path \"/root\" (page \"2\"))))",
            "(unknown (nested \"exact bytes\"))",
        ] {
            assert!(changed.contains(protected));
        }
        assert!(changed.contains("(at 11 11 90)"));
        assert!(changed.contains("(at 12 14 270)"));
        assert!(changed.contains(&format!("(uuid \"{PIN}\")")));
        update.pins.push(SheetPinEdit::Delete { uuid: PIN.into() });
        assert!(apply(&source, &update).is_err());
        update.pins.pop();
        if let SheetPinEdit::Update { pin, .. } = &mut update.pins[0] {
            pin.rotation = QuarterTurn::Half;
        }
        assert!(apply(&source, &update).is_err());
    }

    #[test]
    fn nested_allocation_shares_global_keys_collision_checks_and_total_operation_bound() {
        let action = SchematicEdit {
            file: "root.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::UpdateSheet {
                uuid: SHEET.into(),
                update: SheetUpdate {
                    pins: vec![SheetPinEdit::Create {
                        key: "new-pin".into(),
                        pin: output(30, 16),
                    }],
                    ..Default::default()
                },
            },
        };
        let edits = vec![action.clone()];
        let allocated = allocate(
            "revision",
            &edits,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )
        .unwrap();
        let mut seen = allocated.values().cloned().collect();
        assert!(allocate("revision", &edits, &mut seen, &mut BTreeSet::new()).is_err());
        let key = String::from("new-pin");
        assert!(
            allocate(
                "revision",
                &edits,
                &mut BTreeSet::new(),
                &mut BTreeSet::from([&key])
            )
            .is_err()
        );
        assert!(
            allocate(
                "revision",
                &[action.clone(), action.clone()],
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            )
            .is_err()
        );
        let mut oversized = action;
        if let EditAction::UpdateSheet { update, .. } = &mut oversized.action {
            update.pins = (0..128)
                .map(|i| SheetPinEdit::Create {
                    key: format!("pin-{i}"),
                    pin: output(30, 16),
                })
                .collect();
        }
        assert!(
            allocate(
                "revision",
                &[oversized],
                &mut BTreeSet::new(),
                &mut BTreeSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn sibling_names_are_validated_after_the_complete_explicit_projection() {
        let original = source();
        let second = r#"(sheet (uuid "33333333-3333-4333-8333-333333333333") (property "Sheetname" "Other"))"#;
        let source = format!("{} {})", &original[..original.len() - 1], second);
        let document = parse_one(&source).unwrap();
        let change = |uuid: &str, name: &str| SchematicEdit {
            file: "root.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::UpdateSheet {
                uuid: uuid.into(),
                update: SheetUpdate {
                    name: Some(name.into()),
                    ..Default::default()
                },
            },
        };
        let edits = vec![change(SHEET, "Other")];
        assert!(
            validate_names(
                values(&document.nodes[0]).unwrap(),
                "root.kicad_sch",
                &edits
            )
            .is_err()
        );
        let edits = vec![
            change(SHEET, "Other"),
            change("33333333-3333-4333-8333-333333333333", "Child"),
        ];
        validate_names(
            values(&document.nodes[0]).unwrap(),
            "root.kicad_sch",
            &edits,
        )
        .unwrap();
        assert!(segment("bad/name").is_err());
        assert!(pin_text("BUS[0..7]").is_err());
    }
}
