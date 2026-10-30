//! Instance-aware identities for source symbols and native netlist components.
//!
//! This index is a model, not write authorization or a complete library/ERC proof.
use crate::{NetlistDocument, ParsedSchematicProject, RawSymbol, normalize_netlist_instance_path};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};
use thiserror::Error;

const MAX_INSTANCES: usize = 4096;
const MAX_UNITS: usize = 100_000;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SheetInstance {
    pub file: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct SymbolUnitIdentity {
    pub file: String,
    pub sheet_path: String,
    pub source_uuid: String,
    pub reference: String,
    pub unit: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentIdentity {
    pub reference: String,
    pub sheet_path: String,
    pub units: Vec<SymbolUnitIdentity>,
}

/// Complete physical sheet expansion and native component/unit correspondence.
/// Power and non-board source units remain in `units`, even if absent from the netlist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicIdentityIndex {
    sheets: Vec<SheetInstance>,
    units: Vec<SymbolUnitIdentity>,
    components: Vec<ComponentIdentity>,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum IdentityError {
    #[error("invalid schematic identity graph: {0}")]
    Invalid(&'static str),
    #[error("schematic identity mismatch for {reference}: {reason}")]
    Component {
        reference: String,
        reason: &'static str,
    },
}

fn invalid(message: &'static str) -> IdentityError {
    IdentityError::Invalid(message)
}
pub(crate) fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

fn child_path(parent: &str, child: &str) -> Result<String, IdentityError> {
    if child.contains('\\') {
        return Err(invalid("nonportable sheet path"));
    }
    let joined = Path::new(parent)
        .parent()
        .ok_or(invalid("missing file parent"))?
        .join(child);
    let mut parts = Vec::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str().ok_or(invalid("non-UTF-8 path"))?),
            Component::CurDir => {}
            Component::ParentDir if !parts.is_empty() => {
                parts.pop();
            }
            _ => return Err(invalid("sheet path escapes project")),
        }
    }
    let result = parts.join("/");
    if !result.ends_with(".kicad_sch") {
        return Err(invalid("invalid child schematic"));
    }
    Ok(result)
}

/// Expand the final root-reachable graph without granting native or filesystem authority.
pub(crate) fn expand_source_graph(
    root_path: &str,
    files: &BTreeMap<&str, &crate::ParsedSchematic>,
) -> Result<Vec<SheetInstance>, IdentityError> {
    let root = files.get(root_path).ok_or(invalid("missing root"))?;
    if files.is_empty() || files.len() > 128 {
        return Err(invalid("physical file bound"));
    }
    let mut sheets = Vec::new();
    let mut expanded = BTreeSet::new();
    let mut stack = vec![(
        root_path.to_owned(),
        format!("/{}", root.file_uuid),
        Vec::<String>::new(),
    )];
    while let Some((file_path, instance_path, mut ancestors)) = stack.pop() {
        if sheets.len() >= MAX_INSTANCES || ancestors.len() > 32 {
            return Err(invalid("instance/depth bound"));
        }
        if ancestors.contains(&file_path) || !expanded.insert(instance_path.clone()) {
            return Err(invalid("cycle or repeated instance"));
        }
        let file = files
            .get(file_path.as_str())
            .ok_or(invalid("unobserved child file"))?;
        ancestors.push(file_path.clone());
        sheets.push(SheetInstance {
            file: file_path.clone(),
            path: instance_path.clone(),
        });
        for child in &file.sheets {
            stack.push((
                child_path(&file_path, &child.file)?,
                format!("{instance_path}/{}", child.source_uuid),
                ancestors.clone(),
            ));
        }
    }
    sheets.sort();
    Ok(sheets)
}

impl SchematicIdentityIndex {
    pub fn sheets(&self) -> &[SheetInstance] {
        &self.sheets
    }
    pub fn units(&self) -> &[SymbolUnitIdentity] {
        &self.units
    }
    pub fn components(&self) -> &[ComponentIdentity] {
        &self.components
    }

    pub fn resolve(
        project: &ParsedSchematicProject,
        netlist: &NetlistDocument,
    ) -> Result<Self, IdentityError> {
        project
            .revision
            .validate()
            .map_err(|_| invalid("invalid project revision"))?;
        if project.files.is_empty() || project.files.len() > 128 {
            return Err(invalid("physical file bound"));
        }
        let files: BTreeMap<_, _> = project
            .files
            .iter()
            .map(|f| (f.artifact.relative_path.as_str(), f))
            .collect();
        if files.len() != project.files.len() {
            return Err(invalid("duplicate physical file"));
        }
        let artifacts: BTreeSet<_> = project
            .files
            .iter()
            .map(|f| {
                (
                    &f.artifact.relative_path,
                    f.artifact.content_digest.as_str(),
                    f.artifact.size,
                )
            })
            .collect();
        let expected: BTreeSet<_> = project
            .revision
            .files
            .iter()
            .map(|f| (&f.relative_path, f.content_digest.as_str(), f.size))
            .collect();
        if artifacts != expected {
            return Err(invalid("file set differs from revision"));
        }
        let root = files
            .get(project.root_relative_path.as_str())
            .ok_or(invalid("missing root"))?;
        let project_name = Path::new(&project.root_relative_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or(invalid("invalid project name"))?;
        let mut file_ids = BTreeSet::new();
        for file in files.values() {
            if !uuid(&file.file_uuid) || !file_ids.insert(&file.file_uuid) {
                return Err(invalid("invalid or repeated file UUID"));
            }
            let mut ids = BTreeSet::new();
            for id in file
                .sheets
                .iter()
                .map(|s| &s.source_uuid)
                .chain(file.symbols.iter().map(|s| &s.source_uuid))
            {
                if !uuid(id) || !ids.insert(id) {
                    return Err(invalid("invalid or repeated object UUID"));
                }
            }
        }
        let sheets = expand_source_graph(&project.root_relative_path, &files)?;
        let expanded = sheets
            .iter()
            .map(|sheet| sheet.path.clone())
            .collect::<BTreeSet<_>>();
        if sheets
            .iter()
            .map(|s| s.file.as_str())
            .collect::<BTreeSet<_>>()
            .len()
            != files.len()
        {
            return Err(invalid("unreachable observed file"));
        }
        let native_sheets: Vec<_> = netlist
            .sheets
            .iter()
            .map(|s| {
                normalize_netlist_instance_path(&s.uuid_path, &root.file_uuid)
                    .ok_or(invalid("invalid native sheet path"))
            })
            .collect::<Result<_, _>>()?;
        if native_sheets.iter().collect::<BTreeSet<_>>().len() != native_sheets.len()
            || native_sheets.into_iter().collect::<BTreeSet<_>>() != expanded
        {
            return Err(invalid("native sheet set differs"));
        }
        let mut units = Vec::new();
        let mut sources = BTreeMap::<(String, String), &RawSymbol>::new();
        for sheet in &sheets {
            for symbol in &files[sheet.file.as_str()].symbols {
                if units.len() >= MAX_UNITS {
                    return Err(invalid("expanded unit bound"));
                }
                let matching = symbol
                    .instance_paths
                    .iter()
                    .filter(|i| {
                        i.project == project_name && i.path.trim_end_matches('/') == sheet.path
                    })
                    .collect::<Vec<_>>();
                if matching.len() != 1 {
                    return Err(invalid("missing or duplicate source instance annotation"));
                }
                let instance = matching[0];
                let reference = instance
                    .reference
                    .as_ref()
                    .filter(|r| !r.is_empty())
                    .ok_or(invalid("missing instance reference"))?;
                if instance.unit != Some(symbol.unit) || symbol.unit == 0 {
                    return Err(invalid("unit annotation differs from source"));
                }
                if sources
                    .insert((sheet.path.clone(), symbol.source_uuid.clone()), symbol)
                    .is_some()
                {
                    return Err(invalid("duplicate expanded source"));
                }
                units.push(SymbolUnitIdentity {
                    file: sheet.file.clone(),
                    sheet_path: sheet.path.clone(),
                    source_uuid: symbol.source_uuid.clone(),
                    reference: reference.clone(),
                    unit: symbol.unit,
                });
            }
        }
        units.sort();
        let mut component_units = BTreeMap::<(&str, &str), Vec<&SymbolUnitIdentity>>::new();
        for unit in &units {
            component_units
                .entry((&unit.sheet_path, &unit.reference))
                .or_default()
                .push(unit);
        }
        let mut references = BTreeSet::new();
        let mut used = BTreeSet::new();
        let mut components = Vec::new();
        for component in &netlist.components {
            let fail = |reason| IdentityError::Component {
                reference: component.reference.clone(),
                reason,
            };
            if !references.insert(&component.reference) {
                return Err(fail("duplicate native reference"));
            }
            let path = normalize_netlist_instance_path(&component.sheet_uuid_path, &root.file_uuid)
                .ok_or_else(|| fail("invalid instance path"))?;
            let ids = component
                .symbol_uuid
                .as_deref()
                .ok_or_else(|| fail("missing native unit identities"))?
                .split_whitespace()
                .collect::<Vec<_>>();
            let id_set = ids.iter().copied().collect::<BTreeSet<_>>();
            if ids.is_empty() || ids.len() != id_set.len() || ids.iter().any(|id| !uuid(id)) {
                return Err(fail("invalid or repeated native unit UUID"));
            }
            let matching = component_units
                .get(&(path.as_str(), component.reference.as_str()))
                .ok_or_else(|| fail("missing source component instance"))?;
            if matching
                .iter()
                .map(|u| u.source_uuid.as_str())
                .collect::<BTreeSet<_>>()
                != id_set
            {
                return Err(fail("native/source unit set differs"));
            }
            let mut numbers = BTreeSet::new();
            for unit in matching {
                if !numbers.insert(unit.unit)
                    || !used.insert((unit.sheet_path.clone(), unit.source_uuid.clone()))
                {
                    return Err(fail("unit reused by a component"));
                }
                let raw = sources[&(unit.sheet_path.clone(), unit.source_uuid.clone())];
                if raw.value != component.value
                    || raw.footprint.as_deref().unwrap_or("")
                        != component.footprint.as_deref().unwrap_or("")
                    || raw.lib_name.as_deref().unwrap_or(&raw.lib_id)
                        != if component.library.is_empty() {
                            component.library_part.clone()
                        } else {
                            format!("{}:{}", component.library, component.library_part)
                        }
                {
                    return Err(fail("native/source component properties differ"));
                }
            }
            let matched_units = matching.iter().map(|unit| (*unit).clone()).collect();
            components.push(ComponentIdentity {
                reference: component.reference.clone(),
                sheet_path: path,
                units: matched_units,
            });
        }
        components
            .sort_by(|a, b| (&a.sheet_path, &a.reference).cmp(&(&b.sheet_path, &b.reference)));
        Ok(Self {
            sheets,
            units,
            components,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Limits, NetlistComponent, NetlistSheet, load_schematic_project};

    const ROOT: &str = "11111111-1111-4111-8111-111111111111";
    const CHILD: &str = "22222222-2222-4222-8222-222222222222";
    const FIRST: &str = "33333333-3333-4333-8333-333333333333";
    const SECOND: &str = "44444444-4444-4444-8444-444444444444";
    const UNIT_A: &str = "55555555-5555-4555-8555-555555555555";
    const UNIT_B: &str = "66666666-6666-4666-8666-666666666666";

    fn fixture() -> (tempfile::TempDir, ParsedSchematicProject, NetlistDocument) {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        std::fs::write(root.join("root.kicad_sch"),format!("(kicad_sch (version 20260306) (generator eeschema) (uuid {ROOT}) (sheet (uuid {FIRST}) (property \"Sheetname\" \"One\") (property \"Sheetfile\" \"child.kicad_sch\")) (sheet (uuid {SECOND}) (property \"Sheetname\" \"Two\") (property \"Sheetfile\" \"child.kicad_sch\")))")).unwrap();
        let symbols=[(UNIT_A,1),(UNIT_B,2)].iter().map(|(id,unit)|format!("(symbol (uuid {id}) (lib_id \"Amplifier:OpAmp\") (unit {unit}) (at 10 10 0) (property \"Reference\" \"U1\") (property \"Value\" \"OpAmp\") (instances (project \"root\" (path \"/{ROOT}/{FIRST}\" (reference \"U1\") (unit {unit})) (path \"/{ROOT}/{SECOND}\" (reference \"U2\") (unit {unit})))))")).collect::<String>();
        std::fs::write(
            root.join("child.kicad_sch"),
            format!("(kicad_sch (version 20260306) (generator eeschema) (uuid {CHILD}) {symbols})"),
        )
        .unwrap();
        let project = load_schematic_project(
            &root,
            &root.join("root.kicad_sch"),
            "10.0.7",
            1,
            Limits::default(),
        )
        .unwrap();
        let sheets = [
            ("/".to_string(), "/".to_string()),
            (format!("/{FIRST}/"), "/One/".to_string()),
            (format!("/{SECOND}/"), "/Two/".to_string()),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (uuid_path, name))| NetlistSheet {
            number: (i + 1).to_string(),
            name,
            uuid_path,
        })
        .collect();
        let components = [("U1", FIRST), ("U2", SECOND)]
            .into_iter()
            .map(|(reference, path)| NetlistComponent {
                units: None,
                reference: reference.into(),
                value: "OpAmp".into(),
                footprint: None,
                symbol_uuid: Some(format!("{UNIT_A} {UNIT_B}")),
                sheet_path: "/".into(),
                sheet_uuid_path: format!("/{path}/"),
                library: "Amplifier".into(),
                library_part: "OpAmp".into(),
            })
            .collect();
        (
            directory,
            project,
            NetlistDocument {
                export_version: "E".into(),
                sheets,
                components,
                nets: vec![],
            },
        )
    }

    #[test]
    fn physical_units_expand_by_instance_and_preserve_native_unit_sets() {
        let (_directory, mut project, mut netlist) = fixture();
        let first = SchematicIdentityIndex::resolve(&project, &netlist).unwrap();
        assert_eq!(first.sheets().len(), 3);
        assert_eq!(first.units().len(), 4);
        assert_eq!(first.components().len(), 2);
        assert!(first.components().iter().all(|c| c.units.len() == 2));
        assert_ne!(first.units()[0].sheet_path, first.units()[2].sheet_path);
        // Stored physical Reference is U1 in both cases; CH2 annotation owns U2.
        assert_eq!(first.components()[1].reference, "U2");
        project.files.reverse();
        for file in &mut project.files {
            file.symbols.reverse();
            file.sheets.reverse();
        }
        netlist.sheets.reverse();
        netlist.components.reverse();
        for component in &mut netlist.components {
            component.symbol_uuid = Some(format!("{UNIT_B} {UNIT_A}"));
        }
        assert_eq!(
            first,
            SchematicIdentityIndex::resolve(&project, &netlist).unwrap()
        );
    }

    #[test]
    fn mismatched_unit_instance_and_property_evidence_cannot_resolve() {
        let (_directory, project, netlist) = fixture();
        for case in 0..8 {
            let mut p = project.clone();
            let mut n = netlist.clone();
            match case {
                0 => n.components[0].symbol_uuid = Some(UNIT_A.into()),
                1 => n.components[0].symbol_uuid = Some(format!("{UNIT_A} {UNIT_A}")),
                2 => n.components[0].sheet_uuid_path = format!("/{ROOT}/{FIRST}"),
                3 => n.components[0].value = "changed".into(),
                4 => {
                    n.sheets.pop();
                }
                5 => {
                    let file = p.files.iter_mut().find(|f| !f.symbols.is_empty()).unwrap();
                    let duplicate = file.symbols[0].instance_paths[0].clone();
                    file.symbols[0].instance_paths.push(duplicate);
                }
                6 => {
                    let file = p.files.iter_mut().find(|f| !f.symbols.is_empty()).unwrap();
                    file.symbols[0].instance_paths[0].unit = Some(99);
                }
                7 => {
                    p.files[0].artifact.size += 1;
                }
                _ => unreachable!(),
            }
            assert!(
                SchematicIdentityIndex::resolve(&p, &n).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn hierarchy_cycles_escapes_and_identity_collisions_are_rejected() {
        let (_directory, project, netlist) = fixture();
        for target in [
            "../outside.kicad_sch",
            "/absolute.kicad_sch",
            "root.kicad_sch",
            "missing.kicad_sch",
        ] {
            let mut changed = project.clone();
            changed
                .files
                .iter_mut()
                .find(|f| !f.sheets.is_empty())
                .unwrap()
                .sheets[0]
                .file = target.into();
            assert!(SchematicIdentityIndex::resolve(&changed, &netlist).is_err());
        }
        let mut changed = project.clone();
        let file = changed
            .files
            .iter_mut()
            .find(|f| !f.sheets.is_empty())
            .unwrap();
        file.sheets[1].source_uuid = file.sheets[0].source_uuid.clone();
        assert!(SchematicIdentityIndex::resolve(&changed, &netlist).is_err());
    }
    #[test]
    fn embedded_library_override_is_retained_and_matched_exactly() {
        let (directory, _, mut netlist) = fixture();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let path = root.join("child.kicad_sch");
        let before = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            before.replace("(lib_id", "(lib_name \"PrivateAmp\") (lib_id"),
        )
        .unwrap();
        let project = load_schematic_project(
            &root,
            &root.join("root.kicad_sch"),
            "10.0.7",
            1,
            Limits::default(),
        )
        .unwrap();
        for component in &mut netlist.components {
            component.library.clear();
            component.library_part = "PrivateAmp".into();
        }
        assert_eq!(
            SchematicIdentityIndex::resolve(&project, &netlist)
                .unwrap()
                .components()
                .len(),
            2
        );
        assert!(
            project
                .files
                .iter()
                .flat_map(|f| &f.symbols)
                .all(|s| s.lib_name.as_deref() == Some("PrivateAmp")
                    && s.lib_id == "Amplifier:OpAmp")
        );
        netlist.components[0].library_part = "OtherAmp".into();
        assert!(SchematicIdentityIndex::resolve(&project, &netlist).is_err());
    }

    #[test]
    fn expanded_instance_bound_is_checked_before_symbol_resolution() {
        let (_directory, mut project, netlist) = fixture();
        let file = project
            .files
            .iter_mut()
            .find(|f| !f.sheets.is_empty())
            .unwrap();
        let template = file.sheets[0].clone();
        file.sheets = (0..MAX_INSTANCES)
            .map(|i| {
                let mut sheet = template.clone();
                sheet.source_uuid = format!("{i:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
                sheet
            })
            .collect();
        assert_eq!(
            SchematicIdentityIndex::resolve(&project, &netlist),
            Err(IdentityError::Invalid("instance/depth bound"))
        );
    }

    #[test]
    fn placement_removal_keeps_physical_children_but_verifies_only_reachable_edits() {
        use crate::editing::{EditAction, EditScope, SchematicEdit, SymbolProperty, compile_edits};
        let (owned, _, native) = fixture();
        let root = std::fs::canonicalize(owned.path()).unwrap();
        for name in ["root.kicad_sch", "child.kicad_sch"] {
            let path = root.join(name);
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("(generator eeschema)", "(generator eeschema) (lib_symbols)")
                .replace("(sheet (uuid", "(sheet (at 10 10) (size 20 10) (uuid");
            std::fs::write(path, text).unwrap();
        }
        let project = load_schematic_project(
            &root,
            &root.join("root.kicad_sch"),
            "10.0.7",
            1,
            Limits::default(),
        )
        .unwrap();
        let bytes = project
            .files
            .iter()
            .map(|file| {
                (
                    file.artifact.relative_path.clone(),
                    std::fs::read(root.join(&file.artifact.relative_path)).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let sources = bytes
            .iter()
            .map(|(file, bytes)| (file.clone(), bytes.as_slice()))
            .collect();
        let remove = |uuid: &str| SchematicEdit {
            file: "root.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::RemoveSheet { uuid: uuid.into() },
        };
        let one = compile_edits(&project, &native, &sources, &[remove(FIRST)]).unwrap();
        assert_eq!(one.parsed().files.len(), 2);
        assert_eq!(one.files()["child.kicad_sch"], bytes["child.kicad_sch"]);
        assert_eq!(one.replacements().len(), 1);
        let rename = SchematicEdit {
            file: "root.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::UpdateSheet {
                uuid: SECOND.into(),
                update: crate::editing::SheetUpdate {
                    name: Some("One".into()),
                    ..Default::default()
                },
            },
        };
        let swapped = compile_edits(&project, &native, &sources, &[remove(FIRST), rename]).unwrap();
        let root_after = swapped
            .parsed()
            .files
            .iter()
            .find(|file| file.artifact.relative_path == "root.kicad_sch")
            .unwrap();
        assert_eq!(root_after.sheets.len(), 1);
        assert_eq!(root_after.sheets[0].name, "One");
        let removed = [remove(FIRST), remove(SECOND)];
        let both = compile_edits(&project, &native, &sources, &removed).unwrap();
        assert_eq!(both.parsed().files.len(), 1);
        assert_eq!(both.parsed().revision.files.len(), 1);
        assert_eq!(both.files().len(), 2);
        assert_eq!(both.files()["child.kicad_sch"], bytes["child.kicad_sch"]);
        assert_eq!(both.source_revision(), &project.revision);
        let mut detached = removed.to_vec();
        detached.push(SchematicEdit {
            file: "child.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::SetProperty {
                uuid: UNIT_A.into(),
                property: SymbolProperty::Named("Note".into()),
                value: "detached".into(),
            },
        });
        assert!(
            compile_edits(&project, &native, &sources, &detached)
                .err()
                .unwrap()
                .contains("detached")
        );
        assert!(
            compile_edits(&project, &native, &sources, &[remove(FIRST), remove(FIRST)]).is_err()
        );
        let mut mixed = vec![remove(FIRST)];
        mixed.push(SchematicEdit {
            file: "root.kicad_sch".into(),
            scope: EditScope::FileAllInstances,
            action: EditAction::UpdateSheet {
                uuid: FIRST.into(),
                update: crate::editing::SheetUpdate {
                    name: Some("Changed".into()),
                    ..Default::default()
                },
            },
        });
        assert!(compile_edits(&project, &native, &sources, &mixed).is_err());
    }
}
