//! Complete observed-component ownership for explicit member edits.
use super::*;
use crate::{RawSymbol, identities::SymbolUnitIdentity};

type Source<'a> = (&'a str, &'a str);

struct Groups<'a> {
    members: Vec<Vec<&'a SymbolUnitIdentity>>,
    by_source: BTreeMap<&'a str, BTreeMap<&'a str, Vec<usize>>>,
    raw: BTreeMap<Source<'a>, &'a RawSymbol>,
}
impl<'a> Groups<'a> {
    fn new(project: &'a ParsedSchematicProject, identities: &'a SchematicIdentityIndex) -> Self {
        let mut groups = BTreeMap::<(&str, &str), Vec<_>>::new();
        for unit in identities.units() {
            groups
                .entry((&unit.sheet_path, &unit.reference))
                .or_default()
                .push(unit);
        }
        let members = groups.into_values().collect::<Vec<_>>();
        let mut by_source = BTreeMap::<&str, BTreeMap<&str, Vec<usize>>>::new();
        for (index, group) in members.iter().enumerate() {
            for unit in group {
                by_source
                    .entry(&unit.file)
                    .or_default()
                    .entry(&unit.source_uuid)
                    .or_default()
                    .push(index);
            }
        }
        let raw = project
            .files
            .iter()
            .flat_map(|file| {
                file.symbols.iter().map(|symbol| {
                    (
                        (
                            file.artifact.relative_path.as_str(),
                            symbol.source_uuid.as_str(),
                        ),
                        symbol,
                    )
                })
            })
            .collect();
        Self {
            members,
            by_source,
            raw,
        }
    }

    fn indices(&self, file: &str, uuid: &str) -> Result<&[usize], String> {
        self.by_source
            .get(file)
            .and_then(|units| units.get(uuid))
            .map(Vec::as_slice)
            .ok_or_else(|| "component target is not observed".into())
    }

    fn source(&self, uuid: &str) -> Result<Source<'a>, String> {
        let matches = self
            .by_source
            .iter()
            .flat_map(|(&file, units)| {
                units
                    .keys()
                    .filter(move |id| **id == uuid)
                    .map(move |&id| (file, id))
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err("annotation anchor UUID is missing or ambiguous across files".into());
        }
        Ok(matches[0])
    }
}

#[derive(Default)]
struct Update<'a> {
    value: Option<&'a str>,
    footprint: Option<&'a str>,
    in_bom: Option<bool>,
    on_board: Option<bool>,
    dnp: Option<bool>,
}
#[derive(Clone, Copy, Default)]
struct Touched {
    reference: bool,
    value: bool,
    footprint: bool,
    in_bom: bool,
    on_board: bool,
    dnp: bool,
}
impl Touched {
    fn any(self) -> bool {
        self.reference || self.value || self.footprint || self.in_bom || self.on_board || self.dnp
    }
}

pub(super) fn validate(
    project: &ParsedSchematicProject,
    identities: &SchematicIdentityIndex,
    sources: &BTreeMap<String, &[u8]>,
    edits: &[SchematicEdit],
) -> Result<(), String> {
    let groups = Groups::new(project, identities);
    let mut references = BTreeMap::<(usize, &str, &str), &str>::new();
    let mut updates = BTreeMap::<Source<'_>, Update<'_>>::new();
    let mut touched = vec![Touched::default(); groups.members.len()];
    let mut membership_edits = BTreeSet::<usize>::new();
    for edit in edits {
        match &edit.action {
            EditAction::SetReference {
                uuid,
                reference,
                instance_path,
            } => {
                validate_reference(reference)?;
                let indices = groups
                    .indices(&edit.file, uuid)?
                    .iter()
                    .copied()
                    .filter(|index| {
                        instance_path
                            .as_deref()
                            .is_none_or(|path| groups.members[*index][0].sheet_path == path)
                    })
                    .collect::<Vec<_>>();
                if indices.is_empty() {
                    return Err("reference target instance path is not observed".into());
                }
                for index in indices {
                    references.insert((index, &edit.file, uuid), reference);
                    touched[index].reference = true;
                }
            }
            EditAction::SetProperty {
                uuid,
                property: SymbolProperty::Value,
                value,
            } => {
                for &index in groups.indices(&edit.file, uuid)? {
                    touched[index].value = true;
                }
                updates.entry((&edit.file, uuid)).or_default().value = Some(value);
            }
            EditAction::AssignFootprint {
                uuid, footprint, ..
            } => {
                for &index in groups.indices(&edit.file, uuid)? {
                    touched[index].footprint = true;
                }
                updates.entry((&edit.file, uuid)).or_default().footprint = Some(footprint);
            }
            EditAction::SetFlags { uuid, flags } => {
                for &index in groups.indices(&edit.file, uuid)? {
                    touched[index].in_bom |= flags.in_bom.is_some();
                    touched[index].on_board |= flags.on_board.is_some();
                    touched[index].dnp |= flags.dnp.is_some();
                }
                let update = updates.entry((&edit.file, uuid)).or_default();
                if flags.in_bom.is_some() {
                    update.in_bom = flags.in_bom;
                }
                if flags.on_board.is_some() {
                    update.on_board = flags.on_board;
                }
                if flags.dnp.is_some() {
                    update.dnp = flags.dnp;
                }
            }
            EditAction::AddSymbolUnit { uuid, .. }
            | EditAction::Delete {
                uuid,
                kind: ObjectKind::Symbol,
            } => {
                membership_edits.extend(groups.indices(&edit.file, uuid)?.iter().copied());
            }
            _ => {}
        }
    }
    let mut owners = BTreeMap::<&str, usize>::new();
    for (index, members) in groups.members.iter().enumerate() {
        let selected = touched[index];
        if selected.any() && membership_edits.contains(&index) {
            return Err(
                "component shared-field edits cannot combine with unit insertion/deletion".into(),
            );
        }
        let state = |unit: &SymbolUnitIdentity| {
            let key = (unit.file.as_str(), unit.source_uuid.as_str());
            let raw = groups.raw[&key];
            let update = updates.get(&key);
            (
                references
                    .get(&(index, key.0, key.1))
                    .copied()
                    .unwrap_or(&members[0].reference),
                update.and_then(|u| u.value).unwrap_or(&raw.value),
                update
                    .and_then(|u| u.footprint)
                    .unwrap_or(raw.footprint.as_deref().unwrap_or("")),
                update
                    .and_then(|u| u.in_bom)
                    .unwrap_or(raw.in_bom.unwrap_or(true)),
                update
                    .and_then(|u| u.on_board)
                    .unwrap_or(raw.on_board.unwrap_or(true)),
                update
                    .and_then(|u| u.dnp)
                    .unwrap_or(raw.dnp.unwrap_or(false)),
            )
        };
        let first = state(members[0]);
        for unit in members.iter().skip(1) {
            let other = state(unit);
            if first.0 != other.0
                || selected.value && first.1 != other.1
                || selected.footprint && first.2 != other.2
                || selected.in_bom && first.3 != other.3
                || selected.on_board && first.4 != other.4
                || selected.dnp && first.5 != other.5
            {
                return Err(
                    "shared component edits must cover every unit with consistent resulting fields"
                        .into(),
                );
            }
        }
        if let Some(&owner) = owners.get(first.0) {
            // Untouched legacy private references do not acquire new merge authority.
            if first.0 != members[0].reference || first.0 != groups.members[owner][0].reference {
                return Err(format!(
                    "reference `{}` would merge distinct observed components",
                    first.0
                ));
            }
        } else {
            owners.insert(first.0, index);
        }
    }
    if !edits
        .iter()
        .any(|edit| matches!(edit.action, EditAction::AssignFootprint { .. }))
    {
        return Ok(());
    }
    let active = crate::pins::active_source_pin_numbers(project, sources)?;
    for edit in edits {
        let EditAction::AssignFootprint {
            uuid,
            footprint,
            evidence,
        } = &edit.action
        else {
            continue;
        };
        validate_footprint_identifier(footprint)?;
        if !validate_digest(&evidence.catalog_digest) || !validate_digest(&evidence.source_digest) {
            return Err("footprint assignment evidence digest is invalid".into());
        }
        let pads = evidence.pad_numbers.iter().collect::<BTreeSet<_>>();
        if pads.len() > 512
            || pads.len() != evidence.pad_numbers.len()
            || pads.iter().any(|pad| {
                pad.is_empty()
                    || pad.len() > 32
                    || !pad.is_ascii()
                    || pad
                        .chars()
                        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
            })
        {
            return Err("footprint assignment pad evidence is invalid or oversized".into());
        }
        let mut expected = None;
        for &index in groups.indices(&edit.file, uuid)? {
            let mut pins = BTreeSet::new();
            for unit in &groups.members[index] {
                pins.extend(
                    active
                        .get(&(unit.file.clone(), unit.source_uuid.clone()))
                        .ok_or("missing active component pin source")?
                        .iter()
                        .cloned(),
                );
            }
            if expected.as_ref().is_some_and(|first| first != &pins) {
                return Err(
                    "physical target instances have differing component pin universes".into(),
                );
            }
            expected = Some(pins);
        }
        let expected = expected.ok_or("no observed footprint target component")?;
        if evidence.pin_pad_map.iter().any(|(pin, pad)| pin != pad) {
            return Err("native footprint assignment preserves literal pin/pad numbers; pin remapping is not represented by a Footprint field".into());
        }
        let mapped_pads = evidence.pin_pad_map.values().collect::<BTreeSet<_>>();
        if evidence
            .pin_pad_map
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != expected
            || mapped_pads != pads
            || mapped_pads.len() != evidence.pin_pad_map.len()
        {
            return Err(
                "footprint assignment pin-pad evidence is not a complete component bijection"
                    .into(),
            );
        }
    }
    Ok(())
}

pub(super) fn allocate(
    project: &ParsedSchematicProject,
    native: &NetlistDocument,
    requests: &[ReferenceRequest],
) -> Result<Vec<SchematicEdit>, String> {
    if requests.is_empty() || requests.len() > 128 {
        return Err("reference allocation requires 1..128 requests".into());
    }
    let identities = SchematicIdentityIndex::resolve(project, native).map_err(|e| e.to_string())?;
    let groups = Groups::new(project, &identities);
    let mut occupied = identities
        .units()
        .iter()
        .map(|unit| unit.reference.clone())
        .collect::<BTreeSet<_>>();
    let mut assigned = BTreeMap::new();
    let mut edits = Vec::new();
    for request in requests {
        if !uuid(&request.uuid) || request.prefix.is_empty() || !request.prefix.is_ascii() {
            return Err("invalid reference allocation request".into());
        }
        let source = groups.source(&request.uuid)?;
        let indices = groups
            .indices(source.0, source.1)?
            .iter()
            .copied()
            .filter(|index| {
                request
                    .instance_path
                    .as_deref()
                    .is_none_or(|path| groups.members[*index][0].sheet_path == path)
            })
            .collect::<Vec<_>>();
        if indices.is_empty() {
            return Err("reference request instance path is not observed".into());
        }
        for index in indices {
            let signature = (
                &request.prefix,
                request.minimum,
                request.instance_path.as_deref(),
            );
            if let Some(previous) = assigned.get(&index) {
                if previous != &signature {
                    return Err("conflicting allocation requests for one observed component".into());
                }
                continue;
            }
            assigned.insert(index, signature);
            let mut number = request.minimum.max(1);
            let reference = loop {
                let candidate = format!("{}{number}", request.prefix);
                validate_reference(&candidate)?;
                if occupied.insert(candidate.clone()) {
                    break candidate;
                }
                number = number
                    .checked_add(1)
                    .ok_or("reference allocation overflow")?;
            };
            for unit in &groups.members[index] {
                if edits.len() >= 128 {
                    return Err("expanded component annotation exceeds 128 edits".into());
                }
                let instance_path = if request.instance_path.is_some()
                    || groups.indices(&unit.file, &unit.source_uuid)?.len() > 1
                {
                    Some(unit.sheet_path.clone())
                } else {
                    None
                };
                edits.push(SchematicEdit {
                    file: unit.file.clone(),
                    scope: instance_path
                        .as_ref()
                        .map_or(EditScope::FileAllInstances, |path| {
                            EditScope::SingleInstance(path.clone())
                        }),
                    action: EditAction::SetReference {
                        uuid: unit.source_uuid.clone(),
                        reference: reference.clone(),
                        instance_path,
                    },
                });
            }
        }
    }
    Ok(edits)
}
