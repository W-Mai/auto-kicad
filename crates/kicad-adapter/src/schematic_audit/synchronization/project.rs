//! Exact selected project netclass candidates. Compilation grants no file-write authority.
mod json;
pub mod native;
mod selections;
use json::{Array, Object};
pub use selections::{
    ProjectNetclassAssignment, ProjectNetclassPattern, ProjectNetclassSelections,
    inspect_project_netclass_selections,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(
    Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NetclassDimension {
    Clearance,
    TrackWidth,
    ViaDiameter,
    ViaDrill,
    MicroviaDiameter,
    MicroviaDrill,
    DiffPairWidth,
    DiffPairGap,
    DiffPairViaGap,
}
impl NetclassDimension {
    pub fn json_key(self) -> &'static str {
        match self {
            Self::Clearance => "clearance",
            Self::TrackWidth => "track_width",
            Self::ViaDiameter => "via_diameter",
            Self::ViaDrill => "via_drill",
            Self::MicroviaDiameter => "microvia_diameter",
            Self::MicroviaDrill => "microvia_drill",
            Self::DiffPairWidth => "diff_pair_width",
            Self::DiffPairGap => "diff_pair_gap",
            Self::DiffPairViaGap => "diff_pair_via_gap",
        }
    }
    pub fn native_key(self) -> &'static str {
        match self {
            Self::Clearance => "Clearance",
            Self::TrackWidth => "TrackWidth",
            Self::ViaDiameter => "ViaDiameter",
            Self::ViaDrill => "ViaDrill",
            Self::MicroviaDiameter => "uViaDiameter",
            Self::MicroviaDrill => "uViaDrill",
            Self::DiffPairWidth => "DiffPairWidth",
            Self::DiffPairGap => "DiffPairGap",
            Self::DiffPairViaGap => "DiffPairViaGap",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetclassDimensionValue {
    Set { nm: i64 },
    Inherit,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NetclassDimensionEdit {
    pub field: NetclassDimension,
    pub change: NetclassDimensionValue,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectNetclassChange {
    Assign {
        net: String,
        classes: Vec<String>,
    },
    RemoveAssignment {
        net: String,
    },
    AddPattern {
        position: u32,
        class: String,
        pattern: String,
    },
    UpdatePattern {
        index: u32,
        class: Option<String>,
        pattern: Option<String>,
    },
    RemovePattern {
        index: u32,
    },
    Define {
        name: String,
        priority: Option<i32>,
        dimensions: Vec<NetclassDimensionEdit>,
    },
    RemoveClass {
        name: String,
    },
}

/// A server-derived semantic selection path, never an arbitrary writable JSON path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassDifference {
    pub path: Vec<String>,
    pub before: Option<String>,
    pub after: Option<String>,
}

pub struct ProjectNetclassCandidate {
    bytes: Vec<u8>,
    before_sha256: String,
    after_sha256: String,
    differences: Vec<ProjectNetclassDifference>,
}
impl ProjectNetclassCandidate {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn before_sha256(&self) -> &str {
        &self.before_sha256
    }
    pub fn after_sha256(&self) -> &str {
        &self.after_sha256
    }
    pub fn differences(&self) -> &[ProjectNetclassDifference] {
        &self.differences
    }
    pub fn has_changes(&self) -> bool {
        self.before_sha256 != self.after_sha256
    }
}

fn name(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > 4096 || text.contains('\0') {
        return Err("invalid project netclass name, net or pattern".into());
    }
    Ok(())
}
fn encoded<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| e.to_string())
}
fn string(raw: &RawValue) -> Result<String, String> {
    let value: String = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
    name(&value)?;
    Ok(value)
}
fn required<'a>(object: &Object<'a>, key: &str) -> Result<&'a RawValue, String> {
    object
        .get(key)
        .ok_or_else(|| format!("missing project netclass field: {key}"))
}
fn dimension(nm: i64) -> Result<String, String> {
    if !(0..=2_000_000_000).contains(&nm) {
        return Err("project dimension exceeds exact coordinate bound".into());
    }
    let whole = nm / 1_000_000;
    let fraction = nm % 1_000_000;
    if fraction == 0 {
        return Ok(whole.to_string());
    }
    Ok(format!(
        "{whole}.{}",
        format!("{fraction:06}").trim_end_matches('0')
    ))
}
fn difference(
    rows: &mut Vec<ProjectNetclassDifference>,
    path: &[&str],
    before: Option<&str>,
    after: Option<&str>,
) {
    if before != after {
        rows.push(ProjectNetclassDifference {
            path: path.iter().map(|v| (*v).into()).collect(),
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        });
    }
}
fn set_once(
    map: &mut BTreeMap<String, Option<String>>,
    key: &str,
    value: Option<String>,
) -> Result<(), String> {
    if map.insert(key.into(), value).is_some() {
        return Err("repeated project netclass property selection".into());
    }
    Ok(())
}
fn assignment(raw: &RawValue) -> Result<Vec<String>, String> {
    let values: Vec<String> = if raw.get().starts_with('"') {
        vec![string(raw)?]
    } else {
        serde_json::from_str(raw.get()).map_err(|e| e.to_string())?
    };
    if values.len() > 512 {
        return Err("project assignment class count exceeds bound".into());
    }
    for value in &values {
        name(value)?;
    }
    Ok(values)
}

fn validate_references(
    settings: &Object<'_>,
    source_classes: &BTreeSet<String>,
) -> Result<(), String> {
    let empty_array: &RawValue = serde_json::from_str("[]").map_err(|e| e.to_string())?;
    let empty_object: &RawValue = serde_json::from_str("{}").map_err(|e| e.to_string())?;
    let classes = Array::new(settings.get("classes").unwrap_or(empty_array))?;
    let mut names = BTreeSet::new();
    if classes.len() > 512 {
        return Err("configured project class count exceeds bound".into());
    }
    for definition in classes.values() {
        let definition = Object::new(definition)?;
        let class = string(required(&definition, "name")?)?;
        if !names.insert(class) {
            return Err("duplicate project netclass name".into());
        }
    }
    // The native backend supplies its implicit Default class when it is not serialized.
    names.insert("Default".into());
    if !source_classes.is_subset(&names) {
        return Err("removed or missing schematic source netclass definition".into());
    }
    let assignments = Object::new(settings.get("netclass_assignments").unwrap_or(empty_object))?;
    for (net, value) in assignments.entries() {
        name(net)?;
        for class in assignment(value)? {
            if !names.contains(&class) {
                return Err("assignment references missing project netclass".into());
            }
        }
    }
    let patterns = Array::new(settings.get("netclass_patterns").unwrap_or(empty_array))?;
    if patterns.len() > 4096 {
        return Err("project netclass pattern count exceeds bound".into());
    }
    for pattern in patterns.values() {
        let object = Object::new(pattern)?;
        if !names.contains(&string(required(&object, "netclass")?)?) {
            return Err("pattern references missing project netclass".into());
        }
        string(required(&object, "pattern")?)?;
    }
    Ok(())
}

/// Compile typed selections against exact captured settings; no IO, approval or native-rule claim.
pub fn compile_project_netclasses(
    bytes: &[u8],
    actions: &[ProjectNetclassChange],
    allowed_nets: &BTreeSet<String>,
    source_classes: &BTreeSet<String>,
) -> Result<ProjectNetclassCandidate, String> {
    if actions.len() > 128 || allowed_nets.len() > 100_000 || source_classes.len() > 512 {
        return Err("project netclass intent exceeds bound".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let raw = json::parse(text)?;
    let mut root = Object::new(raw)?;
    let empty_object: &RawValue = serde_json::from_str("{}").map_err(|e| e.to_string())?;
    let empty_array: &RawValue = serde_json::from_str("[]").map_err(|e| e.to_string())?;
    let settings_raw = root.get("net_settings");
    let mut settings = Object::new(settings_raw.unwrap_or(empty_object))?;
    if let Some(value) = settings.get("meta") {
        let meta = Object::new(value)?;
        if serde_json::from_str::<u32>(required(&meta, "version")?.get())
            .map_err(|e| e.to_string())?
            != 5
        {
            return Err("unqualified project net_settings schema".into());
        }
    } else if settings_raw.is_some() && !actions.is_empty() {
        return Err("unqualified project net_settings metadata".into());
    }
    let mut classes = Array::new(settings.get("classes").unwrap_or(empty_array))?;
    let mut patterns = Array::new(settings.get("netclass_patterns").unwrap_or(empty_array))?;
    let mut assignments =
        Object::new(settings.get("netclass_assignments").unwrap_or(empty_object))?;
    if classes.len() > 512 || patterns.len() > 4096 || assignments.entries().count() > 4096 {
        return Err("project class/assignment/pattern inventory exceeds bound".into());
    }
    let mut definitions = BTreeMap::new();
    for (index, value) in classes.values().enumerate() {
        let object = Object::new(value)?;
        let class = string(required(&object, "name")?)?;
        if definitions.insert(class, (index, value)).is_some() {
            return Err("duplicate project netclass name".into());
        }
    }
    let original_patterns = patterns.values().collect::<Vec<_>>();
    let mut class_changes: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();
    let mut class_removals = BTreeSet::new();
    let mut assignment_changes = BTreeMap::new();
    let mut pattern_changes: BTreeMap<usize, BTreeMap<String, Option<String>>> = BTreeMap::new();
    let mut pattern_removals = BTreeSet::new();
    let mut pattern_additions = Vec::new();
    let mut request_bytes = 0usize;
    for action in actions {
        let input = encoded(action)?;
        request_bytes = request_bytes
            .checked_add(input.len())
            .ok_or("project intent size overflow")?;
        if request_bytes > 4 * 1024 * 1024 {
            return Err("project intent exceeds message bound".into());
        }
        match action {
            ProjectNetclassChange::Assign { net, classes } => {
                name(net)?;
                if !allowed_nets.contains(net) || classes.len() > 512 {
                    return Err("foreign project net or excessive class assignment".into());
                }
                let mut seen = BTreeSet::new();
                for class in classes {
                    name(class)?;
                    if !seen.insert(class) {
                        return Err("duplicate requested assignment class".into());
                    }
                }
                let value = match assignments.get(net) {
                    Some(old) if assignment(old)? == *classes => old.get().to_owned(),
                    _ => encoded(classes)?,
                };
                set_once(&mut assignment_changes, net, Some(value))?;
            }
            ProjectNetclassChange::RemoveAssignment { net } => {
                name(net)?;
                if !allowed_nets.contains(net) || assignments.get(net).is_none() {
                    return Err("foreign or absent project net assignment".into());
                }
                set_once(&mut assignment_changes, net, None)?;
            }
            ProjectNetclassChange::Define {
                name: class,
                priority,
                dimensions,
            } => {
                name(class)?;
                if class_removals.contains(class)
                    || dimensions.len() > 9
                    || (priority.is_none() && dimensions.is_empty())
                {
                    return Err("conflicting or empty project class definition edit".into());
                }
                let existing = definitions
                    .get(class)
                    .map(|(_, value)| Object::new(value))
                    .transpose()?;
                if existing.is_none() && priority.is_none() {
                    return Err("new project class requires explicit priority".into());
                }
                let changes = class_changes.entry(class.clone()).or_default();
                if let Some(priority) = priority {
                    if *priority < 0 || (class == "Default" && *priority != i32::MAX) {
                        return Err("invalid configured netclass priority".into());
                    }
                    let value = if let Some(old) = existing.as_ref().and_then(|o| o.get("priority"))
                    {
                        if serde_json::from_str::<i32>(old.get()).map_err(|e| e.to_string())?
                            == *priority
                        {
                            old.get().to_owned()
                        } else {
                            priority.to_string()
                        }
                    } else {
                        priority.to_string()
                    };
                    set_once(changes, "priority", Some(value))?;
                }
                for edit in dimensions {
                    let key = edit.field.json_key();
                    let old = existing.as_ref().and_then(|object| object.get(key));
                    let old_value = old
                        .filter(|value| value.get() != "-1")
                        .map(crate::routing_rules::mm_to_nm)
                        .transpose()?;
                    let value = match edit.change {
                        NetclassDimensionValue::Set { nm } => {
                            let value = dimension(nm)?;
                            Some(if old_value == Some(nm) {
                                old.ok_or("missing equivalent class dimension")?
                                    .get()
                                    .into()
                            } else {
                                value
                            })
                        }
                        NetclassDimensionValue::Inherit => None,
                    };
                    set_once(changes, key, value)?;
                }
            }
            ProjectNetclassChange::RemoveClass { name: class } => {
                name(class)?;
                if class == "Default"
                    || !definitions.contains_key(class)
                    || class_changes.contains_key(class)
                    || !class_removals.insert(class.clone())
                {
                    return Err("unknown, repeated, edited or Default class removal".into());
                }
            }
            ProjectNetclassChange::AddPattern {
                position,
                class,
                pattern,
            } => {
                name(class)?;
                name(pattern)?;
                if *position as usize > patterns.len() {
                    return Err("project pattern insertion exceeds observed bounds".into());
                }
                pattern_additions.push((
                    *position as usize,
                    encoded(&serde_json::json!({"netclass":class,"pattern":pattern}))?,
                ));
            }
            ProjectNetclassChange::UpdatePattern {
                index,
                class,
                pattern,
            } => {
                let index = *index as usize;
                if index >= patterns.len()
                    || pattern_removals.contains(&index)
                    || (class.is_none() && pattern.is_none())
                {
                    return Err("invalid, removed or empty project pattern update".into());
                }
                let changes = pattern_changes.entry(index).or_default();
                for (key, value) in [("netclass", class), ("pattern", pattern)] {
                    if let Some(value) = value {
                        name(value)?;
                        set_once(changes, key, Some(encoded(value)?))?;
                    }
                }
            }
            ProjectNetclassChange::RemovePattern { index } => {
                let index = *index as usize;
                if index >= patterns.len()
                    || pattern_changes.contains_key(&index)
                    || !pattern_removals.insert(index)
                {
                    return Err("unknown, repeated or edited project pattern removal".into());
                }
            }
        }
    }
    let mut differences = Vec::new();
    for (class, changes) in &class_changes {
        let existing = definitions.get(class);
        let mut object = Object::new(existing.map(|(_, v)| *v).unwrap_or(empty_object))?;
        if existing.is_none() {
            object.set("name", Some(encoded(class)?))?;
        }
        for (key, value) in changes {
            let before = object.get(key);
            if existing.is_some() {
                difference(
                    &mut differences,
                    &["net_settings", "classes", class, key],
                    before.map(RawValue::get),
                    value.as_deref(),
                );
            }
            if before.is_some() || value.is_some() {
                object.set(key, value.clone())?;
            }
        }
        let value = object.render()?;
        if let Some((index, _)) = existing {
            classes.set(*index, Some(value))?;
        } else {
            difference(
                &mut differences,
                &["net_settings", "classes", class],
                None,
                Some(&value),
            );
            classes.insert(classes.len(), value)?;
        }
    }
    for class in &class_removals {
        let (index, value) = definitions[class];
        difference(
            &mut differences,
            &["net_settings", "classes", class],
            Some(value.get()),
            None,
        );
        classes.set(index, None)?;
    }
    for (net, value) in &assignment_changes {
        difference(
            &mut differences,
            &["net_settings", "netclass_assignments", net],
            assignments.get(net).map(RawValue::get),
            value.as_deref(),
        );
        assignments.set(net, value.clone())?;
    }
    for (index, changes) in &pattern_changes {
        let mut object = Object::new(original_patterns[*index])?;
        for (key, value) in changes {
            difference(
                &mut differences,
                &["net_settings", "netclass_patterns", &index.to_string(), key],
                object.get(key).map(RawValue::get),
                value.as_deref(),
            );
            object.set(key, value.clone())?;
        }
        patterns.set(*index, Some(object.render()?))?;
    }
    for index in &pattern_removals {
        difference(
            &mut differences,
            &["net_settings", "netclass_patterns", &index.to_string()],
            Some(original_patterns[*index].get()),
            None,
        );
        patterns.set(*index, None)?;
    }
    for (ordinal, (position, value)) in pattern_additions.iter().enumerate() {
        difference(
            &mut differences,
            &[
                "net_settings",
                "netclass_patterns",
                "insert",
                &position.to_string(),
                &ordinal.to_string(),
            ],
            None,
            Some(value),
        );
        patterns.insert(*position, value.clone())?;
    }
    if !class_changes.is_empty() || !class_removals.is_empty() {
        settings.set("classes", Some(classes.render()))?;
    }
    if !assignment_changes.is_empty() {
        settings.set("netclass_assignments", Some(assignments.render()?))?;
    }
    if !pattern_changes.is_empty() || !pattern_removals.is_empty() || !pattern_additions.is_empty()
    {
        settings.set("netclass_patterns", Some(patterns.render()))?;
    }
    let bytes = if actions.is_empty() {
        bytes.to_vec()
    } else {
        if settings_raw.is_none() {
            settings.set("meta", Some("{\"version\":5}".into()))?;
        }
        let rendered = settings.render()?;
        if settings_raw.is_none() {
            differences.clear();
            difference(&mut differences, &["net_settings"], None, Some(&rendered));
        }
        let validated = Object::new(json::parse(&rendered)?)?;
        validate_references(&validated, source_classes)?;
        if let Some(original) = settings_raw {
            json::replace(text, original, &rendered)?.into_bytes()
        } else {
            root.set("net_settings", Some(rendered))?;
            json::replace(text, raw, &root.render()?)?.into_bytes()
        }
    };
    json::parse(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)?;
    differences.sort_by(|a, b| a.path.cmp(&b.path));
    if differences
        .windows(2)
        .any(|pair| pair[0].path == pair[1].path)
    {
        return Err("duplicate project difference selection".into());
    }
    let before_sha256 = hex::encode(Sha256::digest(text.as_bytes()));
    let after_sha256 = hex::encode(Sha256::digest(&bytes));
    if differences.is_empty() && before_sha256 != after_sha256 {
        return Err("project bytes changed without an exact selected difference".into());
    }
    Ok(ProjectNetclassCandidate {
        bytes,
        before_sha256,
        after_sha256,
        differences,
    })
}

/// Exact selected assignment tokens, including newly created net_settings namespaces.
pub(super) fn assignment_changes(before: &[u8], after: &[u8]) -> Result<BTreeSet<String>, String> {
    fn captured(bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
        let root = Object::new(json::parse(
            std::str::from_utf8(bytes).map_err(|e| e.to_string())?,
        )?)?;
        let empty: &RawValue = serde_json::from_str("{}").map_err(|e| e.to_string())?;
        let settings = Object::new(root.get("net_settings").unwrap_or(empty))?;
        let assignments = Object::new(settings.get("netclass_assignments").unwrap_or(empty))?;
        Ok(assignments
            .entries()
            .map(|(name, value)| (name.to_owned(), value.get().to_owned()))
            .collect())
    }
    let old = captured(before)?;
    let new = captured(after)?;
    Ok(old
        .keys()
        .chain(new.keys())
        .filter(|net| old.get(*net) != new.get(*net))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = " \n{\"future\":{\"n\":1e-0000003},\"net_settings\":{\"meta\":{\"version\":5},\"classes\":[{\"name\":\"Default\",\"priority\":2147483647,\"track_width\":0.200000,\"future\":[-0.00]},{\"name\":\"Power\",\"priority\":0,\"clearance\":4.20000e-1,\"future\":1e-000003}],\"netclass_assignments\":{\"N\":\"Power\"},\"netclass_patterns\":[{\"netclass\":\"Power\",\"pattern\":\"N*\",\"future\":-0.000}]}}\n ";
    fn compile(actions: &[ProjectNetclassChange]) -> Result<ProjectNetclassCandidate, String> {
        compile_project_netclasses(
            SOURCE.as_bytes(),
            actions,
            &BTreeSet::from(["N".into()]),
            &BTreeSet::from(["Default".into()]),
        )
    }
    #[test]
    fn selected_dimensions_cannot_rewrite_unknown_tokens_or_unselected_source_bytes() {
        let candidate = compile(&[ProjectNetclassChange::Define {
            name: "Power".into(),
            priority: None,
            dimensions: vec![NetclassDimensionEdit {
                field: NetclassDimension::Clearance,
                change: NetclassDimensionValue::Set { nm: 420001 },
            }],
        }])
        .unwrap();
        assert_eq!(
            std::str::from_utf8(candidate.bytes()).unwrap(),
            SOURCE.replace("4.20000e-1", "0.420001")
        );
        assert_eq!(candidate.differences().len(), 1);
        let unchanged = compile(&[
            ProjectNetclassChange::Define {
                name: "Power".into(),
                priority: None,
                dimensions: vec![NetclassDimensionEdit {
                    field: NetclassDimension::Clearance,
                    change: NetclassDimensionValue::Set { nm: 420000 },
                }],
            },
            ProjectNetclassChange::Assign {
                net: "N".into(),
                classes: vec!["Power".into()],
            },
        ])
        .unwrap();
        assert!(!unchanged.has_changes());
        assert_eq!(unchanged.bytes(), SOURCE.as_bytes());
    }
    #[test]
    fn exact_references_and_unique_selections_protect_class_ownership() {
        assert!(
            compile(&[ProjectNetclassChange::RemoveClass {
                name: "Power".into()
            }])
            .is_err()
        );
        assert!(
            compile(&[ProjectNetclassChange::RemoveClass {
                name: "Default".into()
            }])
            .is_err()
        );
        assert!(
            compile(&[ProjectNetclassChange::Assign {
                net: "foreign".into(),
                classes: vec![]
            }])
            .is_err()
        );
        assert!(
            compile(&[ProjectNetclassChange::Assign {
                net: "N".into(),
                classes: vec!["missing".into()]
            }])
            .is_err()
        );
        let update = || ProjectNetclassChange::Define {
            name: "Power".into(),
            priority: Some(1),
            dimensions: vec![],
        };
        assert!(compile(&[update(), update()]).is_err());
        let candidate = compile(&[
            ProjectNetclassChange::RemoveAssignment { net: "N".into() },
            ProjectNetclassChange::RemovePattern { index: 0 },
            ProjectNetclassChange::RemoveClass {
                name: "Power".into(),
            },
        ])
        .unwrap();
        assert_eq!(candidate.differences().len(), 3);
        assert!(
            std::str::from_utf8(candidate.bytes())
                .unwrap()
                .contains("\"future\":{\"n\":1e-0000003}")
        );
        assert!(
            std::str::from_utf8(candidate.bytes())
                .unwrap()
                .contains("\"future\":[-0.00]")
        );
    }
}
