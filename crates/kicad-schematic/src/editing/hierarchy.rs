//! Explicit child placement and instance annotation projections.
use super::*;
use crate::{ParsedSchematic, identities::SheetInstance};
use kiutils_sexpr::CstDocument;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SheetChild {
    CreateEmpty {
        file: String,
    },
    AttachExisting {
        file: String,
        source_revision: String,
    },
}
impl SheetChild {
    pub fn file(&self) -> &str {
        match self {
            Self::CreateEmpty { file } | Self::AttachExisting { file, .. } => file,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewSheetPin {
    pub key: String,
    pub pin: SheetPin,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetPage {
    pub parent_instance: String,
    pub relative_path: String,
    pub page: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetReference {
    pub parent_instance: String,
    pub relative_path: String,
    pub symbol_uuid: String,
    pub reference: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetCreation {
    pub name: String,
    pub rectangle: SheetRectangle,
    pub child: SheetChild,
    pub pins: Vec<NewSheetPin>,
    pub pages: Vec<SheetPage>,
    pub references: Vec<SheetReference>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SheetRetarget {
    pub child: SheetChild,
    pub pages: Vec<SheetPage>,
    pub references: Vec<SheetReference>,
}

pub fn validate_child_file(file: &str) -> Result<(), String> {
    if file.is_empty()
        || file.len() > 4096
        || !file.ends_with(".kicad_sch")
        || file.chars().any(|c| c.is_control() || c == '\\')
        || file
            .split('/')
            .any(|p| p.is_empty() || matches!(p, "." | ".."))
        || Path::new(file)
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err("child file must be a normalized project-relative .kicad_sch path".into());
    }
    Ok(())
}
/// Read configured page labels from exact captured records; XML ordinals are separate.
pub fn configured_sheet_pages(
    project: &ParsedSchematicProject,
    sources: &BTreeMap<String, &[u8]>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let graph = crate::identities::source_instances(project).map_err(|e| e.to_string())?;
    let current = Path::new(&project.root_relative_path)
        .file_stem()
        .and_then(|p| p.to_str())
        .ok_or("invalid project name")?;
    let mut documents = BTreeMap::new();
    for file in &project.files {
        let bytes = *sources
            .get(&file.artifact.relative_path)
            .ok_or("missing page source")?;
        if parse_schematic(&file.artifact.relative_path, bytes)
            .map_err(|e| e.to_string())?
            .artifact
            != file.artifact
        {
            return Err("page source differs from captured artifact".into());
        }
        documents.insert(
            file.artifact.relative_path.as_str(),
            parse_one(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
    }
    let mut pages = BTreeMap::new();
    for instance in &graph {
        let page = if instance.file == project.root_relative_path
            && instance.path.matches('/').count() == 1
        {
            let root = values(&documents[instance.file.as_str()].nodes[0])?;
            let mut found = Vec::new();
            if let Some(records) = optional(root, "sheet_instances")? {
                for row in children(values(records)?, "path") {
                    if atom(values(row)?, 1)? == "/" {
                        found.push(row);
                    }
                }
            }
            if found.len() > 1 {
                return Err("ambiguous root page records".into());
            }
            if let Some(row) = found.first() {
                optional(values(row)?, "page")?
                    .map(|field| atom(values(field)?, 1).map(str::to_owned))
                    .transpose()?
            } else {
                None
            }
        } else {
            let (parent_path, id) = instance
                .path
                .rsplit_once('/')
                .ok_or("invalid page instance path")?;
            let parent = graph
                .iter()
                .find(|s| s.path == parent_path)
                .ok_or("missing page parent")?;
            let file = project
                .files
                .iter()
                .find(|f| f.artifact.relative_path == parent.file)
                .ok_or("missing parent file")?;
            let placement = file
                .sheets
                .iter()
                .find(|s| s.source_uuid == id)
                .ok_or("missing page placement")?;
            let rows = placement
                .instance_paths
                .iter()
                .filter(|row| {
                    row.project == current && row.path.trim_end_matches('/') == parent_path
                })
                .collect::<Vec<_>>();
            if rows.len() > 1 {
                return Err("ambiguous configured page rows".into());
            }
            rows.first().and_then(|row| row.page.clone())
        };
        pages.insert(instance.path.clone(), page);
    }
    Ok(pages)
}

fn page_text(page: &str) -> Result<(), String> {
    if page.is_empty()
        || page.len() > 32
        || !page.bytes().all(|b| b.is_ascii_alphanumeric())
        || (page.bytes().all(|b| b.is_ascii_digit())
            && page
                .parse::<u32>()
                .ok()
                .is_none_or(|n| n == 0 || n.to_string() != page))
    {
        return Err(
            "page labels require bounded canonical positive numbers or alphanumeric text".into(),
        );
    }
    Ok(())
}

fn relative_file(parent: &str, child: &str) -> Result<String, String> {
    validate_child_file(parent)?;
    validate_child_file(child)?;
    let parent = parent.split('/').collect::<Vec<_>>();
    let child = child.split('/').collect::<Vec<_>>();
    let shared = parent[..parent.len() - 1]
        .iter()
        .zip(&child)
        .take_while(|(a, b)| a == b)
        .count();
    Ok(std::iter::repeat_n("..", parent.len() - 1 - shared)
        .chain(child[shared..].iter().copied())
        .collect::<Vec<_>>()
        .join("/"))
}
fn local_path(path: &str, root_uuid: &str) -> Result<String, String> {
    let prefix = format!("/{root_uuid}");
    let suffix = path
        .strip_prefix(&prefix)
        .ok_or("invalid captured child path")?;
    Ok(if suffix.is_empty() {
        "/".into()
    } else {
        suffix.into()
    })
}
fn actual_path(parent: &str, placement: &str, local: &str) -> String {
    format!(
        "{parent}/{placement}{}",
        if local == "/" { "" } else { local }
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Row {
    Symbol { reference: String, unit: u32 },
    Page(String),
}
struct BoundRow {
    row: Row,
    replace: bool,
}
struct Rows {
    kind: ObjectKind,
    paths: BTreeMap<String, BoundRow>,
}
pub(super) struct Placement {
    pub uuid: String,
    pub rendered: Option<String>,
    pub child: String,
}
#[derive(Default)]
pub(super) struct Projection {
    pub placements: BTreeMap<usize, Placement>,
    pub created: BTreeMap<String, Vec<u8>>,
    rows: BTreeMap<(String, String), Rows>,
    pages: BTreeMap<String, String>,
    root_pages: BTreeMap<String, String>,
    pub new_paths: BTreeSet<String>,
}
impl Projection {
    pub fn pages(&self) -> &BTreeMap<String, String> {
        &self.pages
    }
    fn bind(
        &mut self,
        file: &str,
        id: &str,
        kind: ObjectKind,
        path: String,
        row: Row,
        replace: bool,
    ) -> Result<(), String> {
        let target = self
            .rows
            .entry((file.into(), id.into()))
            .or_insert_with(|| Rows {
                kind,
                paths: BTreeMap::new(),
            });
        if target.kind != kind
            || target
                .paths
                .insert(path, BoundRow { row, replace })
                .is_some()
        {
            return Err("repeated hierarchy annotation target".into());
        }
        Ok(())
    }
    pub fn apply(
        &self,
        file: &str,
        root_node: &Node,
        project: &str,
        patches: &mut Patches<'_>,
    ) -> Result<Vec<ObjectChange>, String> {
        let root = values(root_node)?;
        let mut changes = Vec::new();
        if let Some(page) = self.root_pages.get(file) {
            let records = optional(root, "sheet_instances")?;
            let paths = records
                .map(values)
                .transpose()?
                .map(|items| {
                    children(items, "path")
                        .filter(|n| values(n).is_ok_and(|v| atom_text(v.get(1)) == Some("/")))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if paths.len() > 1 {
                return Err("ambiguous root page rows".into());
            }
            if let Some(path) = paths.first() {
                if let Some(field) = optional(values(path)?, "page")? {
                    patches.node(field, format!("(page {})", quote(page)))?;
                } else {
                    patches.insert(node_span(path).end - 1, format!(" (page {})", quote(page)))?;
                }
            } else if let Some(records) = records {
                patches.insert(
                    node_span(records).end - 1,
                    format!("\n(path \"/\" (page {}))\n", quote(page)),
                )?;
            } else {
                let offset = node_span(root_node).end - 1;
                patches.insert(
                    offset,
                    format!("\n(sheet_instances (path \"/\" (page {})))\n", quote(page)),
                )?;
            }
        }
        for ((target_file, id), rows) in &self.rows {
            if target_file != file {
                continue;
            }
            let node = object(root, id, rows.kind)?;
            let fields = values(node)?;
            let instances = optional(fields, "instances")?;
            let projects = instances
                .map(values)
                .transpose()?
                .map(|items| {
                    children(items, "project")
                        .filter(|n| values(n).is_ok_and(|v| atom_text(v.get(1)) == Some(project)))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if projects.len() > 1 {
                return Err("ambiguous current-project annotations".into());
            }
            let mut added = String::new();
            for (path, binding) in &rows.paths {
                let existing = projects
                    .first()
                    .map(|node| values(node))
                    .transpose()?
                    .map(|items| {
                        children(items, "path")
                            .filter(|n| {
                                values(n).is_ok_and(|v| {
                                    atom_text(v.get(1))
                                        .is_some_and(|p| p.trim_end_matches('/') == path)
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if existing.len() > 1 {
                    return Err("ambiguous hierarchy instance row".into());
                }
                if let Some(existing) = existing.first() {
                    let old = values(existing)?;
                    match &binding.row {
                        Row::Symbol { reference, unit } => {
                            let reference_node = one(old, "reference")?;
                            let unit_node = one(old, "unit")?;
                            if atom(values(unit_node)?, 1)? != unit.to_string() {
                                return Err("captured unit row differs".into());
                            }
                            if atom(values(reference_node)?, 1)? != reference {
                                if !binding.replace {
                                    return Err("inactive hierarchy reference collision".into());
                                }
                                patches.node(
                                    reference_node,
                                    format!("(reference {})", quote(reference)),
                                )?;
                            }
                        }
                        Row::Page(page) => {
                            if let Some(page_node) = optional(old, "page")? {
                                if atom(values(page_node)?, 1)? != page {
                                    if !binding.replace {
                                        return Err("inactive hierarchy page collision".into());
                                    }
                                    patches.node(page_node, format!("(page {})", quote(page)))?;
                                }
                            } else if binding.replace {
                                patches.insert(
                                    node_span(existing).end - 1,
                                    format!(" (page {})", quote(page)),
                                )?;
                            } else {
                                return Err("inactive incomplete hierarchy page row".into());
                            }
                        }
                    }
                } else {
                    added.push_str(&render_row(path, &binding.row));
                }
            }
            if !added.is_empty() {
                if let Some(project) = projects.first() {
                    patches.insert(node_span(project).end - 1, added)?;
                } else if let Some(instances) = instances {
                    patches.insert(
                        node_span(instances).end - 1,
                        format!("\n(project {} {added})\n", quote(project)),
                    )?;
                } else {
                    patches.insert(
                        node_span(node).end - 1,
                        format!("\n(instances (project {} {added}))\n", quote(project)),
                    )?;
                }
            }
            changes.push(ObjectChange {
                file: file.into(),
                uuid: id.clone(),
                kind: rows.kind,
                instance_paths: rows.paths.keys().cloned().collect(),
                action: EditAction::BindHierarchyRows {
                    uuid: id.clone(),
                    kind: rows.kind,
                },
            });
        }
        Ok(changes)
    }
    pub fn validate_final(
        &self,
        project: &ParsedSchematicProject,
        sources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), String> {
        if self.placements.is_empty() && self.pages.is_empty() {
            return Ok(());
        }
        let map = project
            .files
            .iter()
            .map(|f| (f.artifact.relative_path.as_str(), f))
            .collect();
        let graph = crate::identities::expand_source_graph(&project.root_relative_path, &map)
            .map_err(|e| e.to_string())?;
        let current = Path::new(&project.root_relative_path)
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or("invalid project name")?;
        let page_sources = sources
            .iter()
            .map(|(file, bytes)| (file.clone(), bytes.as_slice()))
            .collect();
        let configured = configured_sheet_pages(project, &page_sources)?;
        let mut owners = BTreeMap::<String, String>::new();
        for (path, page) in &configured {
            let page = page
                .as_ref()
                .ok_or("final page records are incomplete; set missing page records explicitly")?;
            if let Some(old) = owners.insert(page.clone(), path.clone())
                && (self.pages.contains_key(path) || self.pages.contains_key(&old))
            {
                return Err("configured page label collides".into());
            }
        }
        for (path, expected) in &self.pages {
            if configured.get(path).and_then(Option::as_ref) != Some(expected) {
                return Err("configured page differs from explicit assignment".into());
            }
        }
        for placement in self.placements.values().filter(|p| p.rendered.is_some()) {
            let matching = project
                .files
                .iter()
                .flat_map(|f| &f.sheets)
                .find(|s| s.source_uuid == placement.uuid)
                .ok_or("missing created placement")?;
            let host = project
                .files
                .iter()
                .find(|f| f.sheets.iter().any(|s| s.source_uuid == placement.uuid))
                .ok_or("missing created parent")?;
            if host
                .sheets
                .iter()
                .filter(|s| s.name == matching.name)
                .count()
                != 1
            {
                return Err("new sibling sheet name is not unique".into());
            }
        }
        let mut dnp = BTreeMap::new();
        for file in &project.files {
            let document = parse_one(
                std::str::from_utf8(&sources[&file.artifact.relative_path])
                    .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            for node in children(values(&document.nodes[0])?, "symbol") {
                let items = values(node)?;
                let id = atom(values(one(items, "uuid")?)?, 1)?;
                let value = optional(items, "dnp")?
                    .map(values)
                    .transpose()?
                    .map(|v| atom(v, 1))
                    .transpose()?
                    .unwrap_or("no");
                if !matches!(value, "yes" | "no") {
                    return Err("unsupported child DNP flag".into());
                }
                dnp.insert(
                    (file.artifact.relative_path.as_str(), id.to_owned()),
                    value == "yes",
                );
            }
        }
        let mut refs = BTreeMap::<String, BTreeSet<String>>::new();
        let mut new_groups = BTreeMap::<(String, String), Vec<(&crate::RawSymbol, bool)>>::new();
        let mut units = 0;
        for sheet in &graph {
            let file = project
                .files
                .iter()
                .find(|f| f.artifact.relative_path == sheet.file)
                .ok_or("missing final sheet")?;
            for symbol in &file.symbols {
                units += 1;
                if units > 100_000 {
                    return Err("expanded source-unit bound exceeded".into());
                }
                let rows = symbol
                    .instance_paths
                    .iter()
                    .filter(|r| r.project == current && r.path.trim_end_matches('/') == sheet.path)
                    .collect::<Vec<_>>();
                if rows.len() != 1 || rows[0].unit != Some(symbol.unit) {
                    return Err("final hierarchy annotations are incomplete".into());
                }
                let reference = rows[0]
                    .reference
                    .as_ref()
                    .ok_or("missing final reference")?;
                refs.entry(reference.clone())
                    .or_default()
                    .insert(sheet.path.clone());
                if self.new_paths.contains(&sheet.path) {
                    validate_reference(reference)?;
                    new_groups
                        .entry((sheet.path.clone(), reference.clone()))
                        .or_default()
                        .push((
                            symbol,
                            dnp[&(
                                file.artifact.relative_path.as_str(),
                                symbol.source_uuid.clone(),
                            )],
                        ));
                }
            }
        }
        for ((_, reference), members) in new_groups {
            if refs[&reference].len() != 1 {
                return Err("new child reference collides across instances".into());
            }
            let (first, first_dnp) = members[0];
            let mut slots = BTreeSet::new();
            for (member, member_dnp) in members {
                if member.unit == 0
                    || !slots.insert(member.unit)
                    || member.lib_id != first.lib_id
                    || member.lib_name != first.lib_name
                    || member.value != first.value
                    || member.footprint != first.footprint
                    || member.on_board.unwrap_or(true) != first.on_board.unwrap_or(true)
                    || member.in_bom.unwrap_or(true) != first.in_bom.unwrap_or(true)
                    || member_dnp != first_dnp
                {
                    return Err("new child component units are incompatible".into());
                }
            }
        }
        Ok(())
    }
}
fn render_row(path: &str, row: &Row) -> String {
    match row {
        Row::Symbol { reference, unit } => format!(
            "\n(path {} (reference {}) (unit {unit}))\n",
            quote(path),
            quote(reference)
        ),
        Row::Page(page) => format!("\n(path {} (page {}))\n", quote(path), quote(page)),
    }
}

pub(super) fn prepare<'a>(
    project: &ParsedSchematicProject,
    identities: &SchematicIdentityIndex,
    inputs: &[ParsedSchematic],
    documents: &BTreeMap<String, CstDocument>,
    edits: &'a [SchematicEdit],
    seen: &mut BTreeSet<String>,
    keys: &mut BTreeSet<&'a String>,
) -> Result<Projection, String> {
    let mut projection = Projection::default();
    let files = inputs
        .iter()
        .map(|f| (f.artifact.relative_path.as_str(), f))
        .collect::<BTreeMap<_, _>>();
    let original_units = identities
        .units()
        .iter()
        .map(|u| {
            (
                u.file.as_str(),
                u.sheet_path.as_str(),
                u.source_uuid.as_str(),
            )
        })
        .collect::<BTreeSet<_>>();
    let original_parents = identities
        .sheets()
        .iter()
        .map(|s| (s.file.as_str(), s.path.as_str()))
        .collect::<BTreeSet<_>>();
    let mut targets = BTreeSet::new();
    let mut operations = edits.len()
        + edits
            .iter()
            .map(|e| {
                if let EditAction::UpdateSheet { update, .. } = &e.action {
                    update.pins.len()
                } else {
                    0
                }
            })
            .sum::<usize>();
    for (ordinal, edit) in edits.iter().enumerate() {
        if let EditAction::SetSheetPage { uuid, pages } = &edit.action {
            if !matches!(edit.scope, EditScope::FileAllInstances) {
                return Err("page records require file_all_instances".into());
            }
            let parents = identities
                .sheets()
                .iter()
                .filter(|s| s.file == edit.file)
                .collect::<Vec<_>>();
            let mut requested = BTreeMap::new();
            for page in pages {
                page_text(&page.page)?;
                if page.relative_path != "/"
                    || requested
                        .insert(page.parent_instance.as_str(), &page.page)
                        .is_some()
                {
                    return Err("invalid or duplicate existing page assignment".into());
                }
            }
            if let Some(id) = uuid {
                object(
                    values(&documents[&edit.file].nodes[0])?,
                    id,
                    ObjectKind::Sheet,
                )?;
                if !targets.insert((&edit.file, id)) {
                    return Err("repeated placement target".into());
                }
                for parent in parents {
                    let page = requested
                        .remove(parent.path.as_str())
                        .ok_or("page maps must cover every parent instance")?;
                    if projection
                        .pages
                        .insert(format!("{}/{id}", parent.path), page.clone())
                        .is_some()
                    {
                        return Err("repeated configured page".into());
                    }
                    projection.bind(
                        &edit.file,
                        id,
                        ObjectKind::Sheet,
                        parent.path.clone(),
                        Row::Page(page.clone()),
                        true,
                    )?;
                }
            } else {
                if edit.file != project.root_relative_path || parents.len() != 1 {
                    return Err("root page target must be the observed root file".into());
                }
                let page = requested
                    .remove(parents[0].path.as_str())
                    .ok_or("missing root page assignment")?;
                if projection
                    .root_pages
                    .insert(edit.file.clone(), page.clone())
                    .is_some()
                    || projection
                        .pages
                        .insert(parents[0].path.clone(), page.clone())
                        .is_some()
                {
                    return Err("repeated root page write".into());
                }
            }
            if !requested.is_empty() {
                return Err("page assignment targets an unobserved parent".into());
            }
            continue;
        }
        let (child, pages, references, creation, existing) = match &edit.action {
            EditAction::CreateSheet { key, sheet } => {
                text(key, false)?;
                if !keys.insert(key) {
                    return Err("duplicate creation key".into());
                }
                operations += sheet.pins.len();
                (
                    &sheet.child,
                    &sheet.pages,
                    &sheet.references,
                    Some(sheet),
                    None,
                )
            }
            EditAction::RetargetSheet { uuid, target } => (
                &target.child,
                &target.pages,
                &target.references,
                None,
                Some(uuid),
            ),
            _ => continue,
        };
        if operations > 128 || references.len() > 100_000 || pages.len() > 4096 {
            return Err("hierarchy operation bound exceeded".into());
        }
        if !matches!(edit.scope, EditScope::FileAllInstances) {
            return Err("child placement edits require file_all_instances".into());
        }
        let host = files
            .get(edit.file.as_str())
            .ok_or("unobserved placement parent")?;
        let parents = identities
            .sheets()
            .iter()
            .filter(|s| s.file == edit.file)
            .map(|s| s.path.clone())
            .collect::<Vec<_>>();
        if parents.is_empty() {
            return Err("placement parent is not observed".into());
        }
        let (placement, child_relative, tree) = {
            let id = if let Some(id) = existing {
                let node = object(
                    values(&documents[&edit.file].nodes[0])?,
                    id,
                    ObjectKind::Sheet,
                )?;
                let old = values(node)?;
                let old_file = children(old, "property")
                    .find(|n| values(n).is_ok_and(|p| atom_text(p.get(1)) == Some("Sheetfile")))
                    .ok_or("missing Sheetfile")?;
                if atom(values(old_file)?, 2)? == relative_file(&edit.file, child.file())? {
                    return Err("retarget requires a different child file".into());
                }
                if !targets.insert((&edit.file, id)) {
                    return Err("repeated placement target".into());
                }
                id.clone()
            } else {
                let id = generated_uuid_seed(&format!(
                    "{}:sheet-placement:{ordinal}",
                    project.revision.digest.as_str()
                ));
                if !seen.insert(id.clone()) {
                    return Err("allocated placement UUID collision".into());
                }
                id
            };
            validate_child_file(child.file())?;
            let relative = relative_file(&edit.file, child.file())?;
            let tree = match child {
                SheetChild::CreateEmpty { file } => {
                    if files.contains_key(file.as_str()) || projection.created.contains_key(file) {
                        return Err(
                            "empty child destination already exists or has another creator".into(),
                        );
                    }
                    let file_uuid = generated_uuid_seed(&format!(
                        "{}:child-file:{ordinal}:{file}",
                        project.revision.digest.as_str()
                    ));
                    if !seen.insert(file_uuid.clone()) {
                        return Err("allocated file UUID collision".into());
                    }
                    let bytes = format!("(kicad_sch (version {}) (generator eeschema) (generator_version \"10.0\") (uuid {}) (paper A4) (lib_symbols))\n", host.format_version, quote(&file_uuid)).into_bytes();
                    projection.created.insert(file.clone(), bytes);
                    vec![SheetInstance {
                        file: file.clone(),
                        path: format!("/{file_uuid}"),
                    }]
                }
                SheetChild::AttachExisting {
                    file,
                    source_revision,
                } => {
                    if source_revision.is_empty() {
                        return Err("attachment needs an inspected source revision".into());
                    }
                    let graph = crate::identities::expand_source_graph(file, &files)
                        .map_err(|e| e.to_string())?;
                    let selected = graph
                        .iter()
                        .map(|s| s.file.as_str())
                        .collect::<BTreeSet<_>>();
                    let revision = SchematicArtifactRevision::new(
                        selected
                            .iter()
                            .map(|path| files[path].artifact.clone())
                            .collect(),
                        &project.revision.kicad_cli_version,
                        project.revision.extractor_schema_version,
                        false,
                    )
                    .map_err(|e| e.to_string())?;
                    if format!("sha256:{}", revision.digest.as_str()) != *source_revision {
                        return Err("captured child revision differs from attachment".into());
                    }
                    graph
                }
            };
            (id, relative, tree)
        };
        if parents
            .len()
            .checked_mul(tree.len())
            .is_none_or(|n| n > 4096)
        {
            return Err("new instance bound exceeded".into());
        }
        let root_uuid = match child {
            SheetChild::CreateEmpty { file } => {
                parse_schematic(file, &projection.created[file])
                    .map_err(|e| e.to_string())?
                    .file_uuid
            }
            SheetChild::AttachExisting { file, .. } => files[file.as_str()].file_uuid.clone(),
        };
        let mut declared_pages = BTreeMap::new();
        for row in pages {
            page_text(&row.page)?;
            if declared_pages
                .insert(
                    (row.parent_instance.clone(), row.relative_path.clone()),
                    &row.page,
                )
                .is_some()
            {
                return Err("invalid or repeated page assignment".into());
            }
        }
        let mut declared_refs = BTreeMap::new();
        for row in references {
            validate_reference(&row.reference)?;
            if declared_refs
                .insert(
                    (
                        row.parent_instance.clone(),
                        row.relative_path.clone(),
                        row.symbol_uuid.clone(),
                    ),
                    &row.reference,
                )
                .is_some()
            {
                return Err("repeated child reference assignment".into());
            }
        }
        let mut top_pages = BTreeMap::new();
        for parent in &parents {
            for instance in &tree {
                let local = local_path(&instance.path, &root_uuid)?;
                let actual = actual_path(parent, &placement, &local);
                let page = declared_pages
                    .remove(&(parent.clone(), local.clone()))
                    .ok_or("page assignments must cover every new sheet instance")?;
                if projection
                    .pages
                    .insert(actual.clone(), page.clone())
                    .is_some()
                {
                    return Err("overlapping new sheet instances".into());
                }
                projection.new_paths.insert(actual.clone());
                if local == "/" {
                    top_pages.insert(parent.clone(), page.clone());
                } else {
                    let (parent_local, node_uuid) =
                        local.rsplit_once('/').ok_or("invalid local sheet path")?;
                    let parent_local = if parent_local.is_empty() {
                        "/"
                    } else {
                        parent_local
                    };
                    let source_parent = tree
                        .iter()
                        .find(|s| local_path(&s.path, &root_uuid).is_ok_and(|p| p == parent_local))
                        .ok_or("missing nested parent")?;
                    let parent_actual = actual_path(parent, &placement, parent_local);
                    let replace = existing.is_some()
                        && original_parents
                            .contains(&(source_parent.file.as_str(), parent_actual.as_str()));
                    projection.bind(
                        &source_parent.file,
                        node_uuid,
                        ObjectKind::Sheet,
                        parent_actual,
                        Row::Page(page.clone()),
                        replace,
                    )?;
                }
                if let Some(file) = files.get(instance.file.as_str()) {
                    for symbol in &file.symbols {
                        let reference = declared_refs
                            .remove(&(parent.clone(), local.clone(), symbol.source_uuid.clone()))
                            .ok_or("reference assignments must cover every new source unit")?;
                        let replace = existing.is_some()
                            && original_units.contains(&(
                                instance.file.as_str(),
                                actual.as_str(),
                                symbol.source_uuid.as_str(),
                            ));
                        projection.bind(
                            &instance.file,
                            &symbol.source_uuid,
                            ObjectKind::Symbol,
                            actual.clone(),
                            Row::Symbol {
                                reference: reference.clone(),
                                unit: symbol.unit,
                            },
                            replace,
                        )?;
                    }
                }
            }
        }
        if !declared_pages.is_empty() || !declared_refs.is_empty() {
            return Err("child assignments contain unobserved paths or source units".into());
        }
        let rendered = if let Some(sheet) = creation {
            sheets::segment(&sheet.name)?;
            sheet.rectangle.validate()?;
            let mut names = BTreeSet::new();
            let mut positions = BTreeSet::new();
            let mut rendered_pins = String::new();
            for (nested, new) in sheet.pins.iter().enumerate() {
                text(&new.key, false)?;
                if !keys.insert(&new.key) {
                    return Err("duplicate creation key".into());
                }
                sheets::pin_text(&new.pin.name)?;
                if !names.insert(&new.pin.name)
                    || !positions.insert(new.pin.position)
                    || !sheet.rectangle.contains_pin(&new.pin)
                {
                    return Err("invalid new sheet pin projection".into());
                }
                let id = generated_uuid_seed(&format!(
                    "{}:new-sheet-pin:{ordinal}:{nested}:{}",
                    project.revision.digest.as_str(),
                    new.key
                ));
                if !seen.insert(id.clone()) {
                    return Err("allocated sheet pin UUID collision".into());
                }
                rendered_pins.push_str(&format!(
                    "\n(pin {} {} {} (uuid {}) (effects (font (size 1.27 1.27))))\n",
                    quote(&new.pin.name),
                    new.pin.shape.atom(),
                    at(new.pin.position, Some(new.pin.rotation)),
                    quote(&id)
                ));
            }
            let position = sheet.rectangle.position;
            let name_position = Point::from_nm(position.x.nm(), position.y.nm() - 1_270_000)?;
            let file_position = Point::from_nm(
                position.x.nm(),
                position.y.nm() + sheet.rectangle.height.nm() + 1_270_000,
            )?;
            let rows = top_pages
                .iter()
                .map(|(path, page)| render_row(path, &Row::Page(page.clone())))
                .collect::<String>();
            let current = Path::new(&project.root_relative_path)
                .file_stem()
                .and_then(|v| v.to_str())
                .ok_or("invalid project name")?;
            Some(format!(
                "\n(sheet {} (size {} {}) (uuid {}) (stroke (width 0.1524) (type solid)) (fill (color 0 0 0 0))\n(property \"Sheetname\" {} {} (effects (font (size 1.27 1.27)) (justify left bottom)))\n(property \"Sheetfile\" {} {} (effects (font (size 1.27 1.27)) (justify left top)))\n{rendered_pins}\n(instances (project {} {rows})))\n",
                at(position, None),
                sheet.rectangle.width.mm(),
                sheet.rectangle.height.mm(),
                quote(&placement),
                quote(&sheet.name),
                at(name_position, Some(QuarterTurn::Zero)),
                quote(&child_relative),
                at(file_position, Some(QuarterTurn::Zero)),
                quote(current)
            ))
        } else {
            for (parent, page) in top_pages {
                projection.bind(
                    &edit.file,
                    &placement,
                    ObjectKind::Sheet,
                    parent,
                    Row::Page(page),
                    true,
                )?;
            }
            None
        };
        projection.placements.insert(
            ordinal,
            Placement {
                uuid: placement,
                rendered,
                child: child_relative,
            },
        );
    }
    Ok(projection)
}
