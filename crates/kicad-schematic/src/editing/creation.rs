//! Typed component membership and insertion from immutable observed source symbols.
use super::*;
use crate::RawSymbol;
use kiutils_sexpr::CstDocument;

#[derive(Eq, PartialEq)]
struct Signature<'a> {
    file: &'a str,
    library: &'a str,
    catalog: &'a str,
    source: &'a str,
    definition: &'a str,
    value: &'a str,
    footprint: &'a str,
    flags: (bool, bool, bool),
    instances: [u8; 32],
}
fn raw_symbol<'a>(
    project: &'a ParsedSchematicProject,
    file: &str,
    uuid: &str,
) -> Result<&'a RawSymbol, String> {
    project
        .files
        .iter()
        .find(|item| item.artifact.relative_path == file)
        .and_then(|item| {
            item.symbols
                .iter()
                .find(|symbol| symbol.source_uuid == uuid)
        })
        .ok_or_else(|| "unit source symbol is not observed".into())
}
fn flags(raw: &RawSymbol) -> (bool, bool, bool) {
    (
        raw.in_bom.unwrap_or(true),
        raw.on_board.unwrap_or(true),
        raw.dnp.unwrap_or(false),
    )
}
fn compatible(left: &RawSymbol, right: &RawSymbol) -> bool {
    left.lib_id == right.lib_id
        && left.lib_name.as_deref().unwrap_or(&left.lib_id)
            == right.lib_name.as_deref().unwrap_or(&right.lib_id)
        && left.value == right.value
        && left.footprint.as_deref().unwrap_or("") == right.footprint.as_deref().unwrap_or("")
        && flags(left) == flags(right)
}

pub(super) fn validate_groups(
    project: &ParsedSchematicProject,
    identities: &SchematicIdentityIndex,
    edits: &[SchematicEdit],
) -> Result<(), String> {
    let mut groups = BTreeMap::<(String, String), (Signature<'_>, BTreeSet<u32>)>::new();
    let mut owners = BTreeMap::<String, String>::new();
    let mut inserted = BTreeMap::<(String, String), BTreeSet<u32>>::new();
    let mut protected = BTreeSet::new();
    let existing_references = identities
        .units()
        .iter()
        .map(|unit| unit.reference.as_str())
        .collect::<BTreeSet<_>>();
    let renamed_references = edits
        .iter()
        .filter_map(|edit| match &edit.action {
            EditAction::SetReference { reference, .. } => Some(reference.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut memberships =
        BTreeMap::<(&str, &str), Vec<&crate::identities::SymbolUnitIdentity>>::new();
    for unit in identities.units() {
        memberships
            .entry((&unit.sheet_path, &unit.reference))
            .or_default()
            .push(unit);
    }
    for edit in edits {
        match &edit.action {
            EditAction::CreateSymbol { symbol, .. } => {
                validate_symbol_creation(symbol)?;
                let mut instances = symbol
                    .instance_paths
                    .iter()
                    .map(|instance| {
                        (
                            instance.path.as_str(),
                            instance.reference.as_deref().unwrap_or(&symbol.reference),
                        )
                    })
                    .collect::<Vec<_>>();
                instances.sort();
                let mut digest = Sha256::new();
                for (path, reference) in &instances {
                    for value in [path, reference] {
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value.as_bytes());
                    }
                }
                let instance_digest: [u8; 32] = digest.finalize().into();
                for (path, reference) in &instances {
                    if existing_references.contains(reference)
                        || renamed_references.contains(reference)
                    {
                        return Err("created reference is occupied; use add_symbol_unit for an existing component".into());
                    }
                    if owners.get(*reference).is_some_and(|owner| owner != path) {
                        return Err(
                            "component references must be distinct across sheet instances".into(),
                        );
                    }
                    owners.insert((*reference).into(), (*path).into());
                    let signature = Signature {
                        file: &edit.file,
                        library: &symbol.evidence.lib_id,
                        catalog: &symbol.evidence.catalog_digest,
                        source: &symbol.evidence.source_digest,
                        definition: &symbol.evidence.definition,
                        value: &symbol.value,
                        footprint: symbol.footprint.as_deref().unwrap_or(""),
                        flags: (
                            symbol.flags.in_bom.unwrap_or(true),
                            symbol.flags.on_board.unwrap_or(true),
                            symbol.flags.dnp.unwrap_or(false),
                        ),
                        instances: instance_digest,
                    };
                    let key = ((*path).into(), (*reference).into());
                    if let Some(entry) = groups.get_mut(&key) {
                        if entry.0 != signature || !entry.1.insert(symbol.unit) {
                            return Err("new component units conflict in definition, properties, flags, annotations or slot".into());
                        }
                    } else {
                        groups.insert(key, (signature, BTreeSet::from([symbol.unit])));
                    }
                }
            }
            EditAction::AddSymbolUnit { uuid, unit, .. } => {
                if *unit == 0 || *unit > 1024 || !matches!(edit.scope, EditScope::FileAllInstances)
                {
                    return Err(
                        "unit insertion requires a valid slot and file_all_instances scope".into(),
                    );
                }
                let anchor = raw_symbol(project, &edit.file, uuid)?;
                let anchors = identities
                    .units()
                    .iter()
                    .filter(|entry| entry.file == edit.file && entry.source_uuid == *uuid)
                    .collect::<Vec<_>>();
                if anchors.is_empty() {
                    return Err("unit source has no observed instances".into());
                }
                for anchor_instance in anchors {
                    let members = &memberships[&(
                        anchor_instance.sheet_path.as_str(),
                        anchor_instance.reference.as_str(),
                    )];
                    let numbers = members
                        .iter()
                        .map(|entry| entry.unit)
                        .collect::<BTreeSet<_>>();
                    if numbers.len() != members.len() || numbers.contains(unit) {
                        return Err("component unit slot is already occupied or ambiguous".into());
                    }
                    for member in members {
                        let raw = raw_symbol(project, &member.file, &member.source_uuid)?;
                        if !compatible(anchor, raw) {
                            return Err(
                                "observed component unit properties or flags conflict".into()
                            );
                        }
                        protected.insert((member.file.clone(), member.source_uuid.clone()));
                    }
                    if !inserted
                        .entry((
                            anchor_instance.sheet_path.clone(),
                            anchor_instance.reference.clone(),
                        ))
                        .or_insert(numbers)
                        .insert(*unit)
                    {
                        return Err("unit insertion repeats a component slot".into());
                    }
                }
            }
            _ => {}
        }
    }
    for edit in edits {
        let target = match &edit.action {
            EditAction::SetReference { uuid, .. }
            | EditAction::SetProperty { uuid, .. }
            | EditAction::SetFlags { uuid, .. }
            | EditAction::TransformSymbol { uuid, .. }
            | EditAction::AssignFootprint { uuid, .. }
            | EditAction::Delete {
                uuid,
                kind: ObjectKind::Symbol,
            } => Some(uuid),
            _ => None,
        };
        if target.is_some_and(|uuid| protected.contains(&(edit.file.clone(), uuid.clone()))) {
            return Err(
                "unit insertion cannot combine edits to its observed component members".into(),
            );
        }
    }
    Ok(())
}

pub(super) fn selected_pins(
    root: &[Node],
    unit: u32,
    body_style: Option<u32>,
) -> Result<Vec<String>, String> {
    let mut pins = BTreeSet::new();
    let mut maximum = 1;
    for body in children(root, "symbol") {
        let name = atom(values(body)?, 1)?;
        let mut parts = name.rsplitn(3, '_');
        let style = parts
            .next()
            .ok_or("missing unit body style")?
            .parse::<u32>()
            .map_err(|_| "invalid unit body style")?;
        let slot = parts
            .next()
            .ok_or("missing unit slot")?
            .parse::<u32>()
            .map_err(|_| "invalid unit slot")?;
        if parts.next().is_none() {
            return Err("invalid unit body name".into());
        }
        maximum = maximum.max(slot);
        if slot != 0 && slot != unit || style != 0 && style != body_style.unwrap_or(1) {
            continue;
        }
        for pin in children(values(body)?, "pin") {
            let number = atom(values(one(values(pin)?, "number")?)?, 1)?.to_owned();
            if number.is_empty() || number.contains(['[', ']', '$']) || !pins.insert(number) {
                return Err("unsupported or repeated unit pin number".into());
            }
        }
    }
    if unit == 0 || unit > maximum || pins.is_empty() {
        return Err("requested unit/body has no supported pin definition".into());
    }
    Ok(pins.into_iter().collect())
}

pub(super) struct Insertion<'a> {
    pub project: &'a ParsedSchematicProject,
    pub identities: &'a SchematicIdentityIndex,
    pub file: &'a crate::ParsedSchematic,
    pub document: &'a CstDocument,
    pub bytes: &'a [u8],
    pub anchor_uuid: &'a str,
    pub unit: u32,
    pub transform: SymbolTransform,
    pub symbol_uuid: &'a str,
    pub ordinal: usize,
}
pub(super) fn insert_unit(
    input: Insertion<'_>,
    seen: &mut BTreeSet<String>,
) -> Result<String, String> {
    let Insertion {
        project,
        identities,
        file,
        document,
        bytes,
        anchor_uuid,
        unit,
        transform,
        symbol_uuid,
        ordinal,
    } = input;
    let root = values(&document.nodes[0])?;
    let anchor = raw_symbol(project, &file.artifact.relative_path, anchor_uuid)?;
    let original = object(root, anchor_uuid, ObjectKind::Symbol)?;
    let library = values(one(root, "lib_symbols")?)?;
    let identifier = anchor.lib_name.as_deref().unwrap_or(&anchor.lib_id);
    let definitions = children(library, "symbol")
        .filter(|node| values(node).is_ok_and(|items| atom_text(items.get(1)) == Some(identifier)))
        .collect::<Vec<_>>();
    if definitions.len() != 1 {
        return Err("unit source cache definition is ambiguous".into());
    }
    let definition = values(definitions[0])?;
    if children(definition, "extends").next().is_some() {
        return Err("unit insertion requires a standalone observed cache definition".into());
    }
    let pins = selected_pins(definition, unit, anchor.body_style)?;
    let span = node_span(original);
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let wrapper = format!("(kicad_sch {})", &text[span.start..span.end]);
    let cloned = parse_one(&wrapper).map_err(|error| error.to_string())?;
    let clone_root = values(&cloned.nodes[0])?;
    let clone_node = object(clone_root, anchor_uuid, ObjectKind::Symbol)?;
    let items = values(clone_node)?;
    let mut patches = Patches {
        text: &wrapper,
        replacements: Vec::new(),
        claims: BTreeSet::new(),
    };
    edit_existing(
        clone_root,
        &EditAction::TransformSymbol {
            uuid: anchor_uuid.into(),
            transform,
        },
        &mut patches,
        &file.format_version,
    )?;
    let mut stack = items
        .iter()
        .filter(|node| {
            !list_items(node)
                .is_some_and(|items| matches!(list_tag(items), Some("pin" | "instances")))
        })
        .collect::<Vec<_>>();
    while let Some(node) = stack.pop() {
        if let Some(items) = list_items(node) {
            if list_tag(items) == Some("uuid") {
                let old = atom(items, 1)?;
                let id = if old == anchor_uuid {
                    symbol_uuid.to_owned()
                } else {
                    let id = generated_uuid_seed(&format!(
                        "schematic-edit-v1:{}:{ordinal}:cloned:{old}",
                        project.revision.digest
                    ));
                    if !seen.insert(id.clone()) {
                        return Err("allocated copied UUID collision".into());
                    }
                    id
                };
                patches.node(node, format!("(uuid {})", quote(&id)))?;
            } else {
                stack.extend(items);
            }
        }
    }
    patches.node(one(items, "unit")?, format!("(unit {unit})"))?;
    for pin in children(items, "pin") {
        patches.node(pin, String::new())?;
    }
    let project_name = Path::new(&project.root_relative_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or("invalid project basename")?;
    let mut instances = format!("(instances (project {}\n", quote(project_name));
    for instance in identities.units().iter().filter(|entry| {
        entry.file == file.artifact.relative_path && entry.source_uuid == anchor_uuid
    }) {
        instances.push_str(&format!(
            "(path {} (reference {}) (unit {unit}))\n",
            quote(&instance.sheet_path),
            quote(&instance.reference)
        ));
    }
    instances.push_str("))");
    patches.node(one(items, "instances")?, instances)?;
    let mut records = String::new();
    for (index, number) in pins.iter().enumerate() {
        let id = generated_uuid_seed(&format!(
            "schematic-edit-v1:{}:{ordinal}:pin:{index}",
            project.revision.digest
        ));
        if !seen.insert(id.clone()) {
            return Err("allocated unit pin UUID collision".into());
        }
        records.push_str(&format!("(pin {} (uuid {}))\n", quote(number), quote(&id)));
    }
    patches.insert(node_span(clone_node).end - 1, records)?;
    let (rendered, _) = patches.finish()?;
    let rendered = std::str::from_utf8(&rendered).map_err(|error| error.to_string())?;
    let parsed = parse_one(rendered).map_err(|error| error.to_string())?;
    let node = object(values(&parsed.nodes[0])?, symbol_uuid, ObjectKind::Symbol)?;
    let span = node_span(node);
    Ok(format!("\n{}\n", &rendered[span.start..span.end]))
}
