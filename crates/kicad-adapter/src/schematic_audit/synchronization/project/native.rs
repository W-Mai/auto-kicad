//! Complete native configured/effective netclass data; observations confer no approval.
use super::{NetclassDimension, json};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassIdentity {
    pub name: String,
    pub priority: i32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassDimension {
    pub defined: bool,
    pub value_nm: i64,
    pub parent: Option<ClassIdentity>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClassText {
    pub defined: bool,
    pub value: String,
    pub parent: Option<ClassIdentity>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredClass {
    pub name: String,
    pub priority: i32,
    pub description: String,
    pub tuning_profile: ClassText,
    pub fields: BTreeMap<String, ClassDimension>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectiveClass {
    pub name: String,
    pub priority: i32,
    pub description: String,
    pub tuning_profile: ClassText,
    pub constituents: Vec<ClassIdentity>,
    pub fields: BTreeMap<String, ClassDimension>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassSnapshot {
    schema_version: u32,
    native_version: String,
    board_sha256: String,
    project_sha256: String,
    native_project_sha256: String,
    native_project_json: String,
    configured: BTreeMap<String, ConfiguredClass>,
    effective: BTreeMap<String, EffectiveClass>,
}

fn valid(text: &str) -> bool {
    text.len() <= 65536 && !text.contains('\0')
}
fn identity(value: &ClassIdentity) -> bool {
    !value.name.is_empty() && valid(&value.name)
}
fn validate_fields(
    name: &str,
    description: &str,
    text: &ClassText,
    fields: &BTreeMap<String, ClassDimension>,
) -> Result<(), String> {
    let keys = [
        NetclassDimension::Clearance,
        NetclassDimension::TrackWidth,
        NetclassDimension::ViaDiameter,
        NetclassDimension::ViaDrill,
        NetclassDimension::MicroviaDiameter,
        NetclassDimension::MicroviaDrill,
        NetclassDimension::DiffPairWidth,
        NetclassDimension::DiffPairGap,
        NetclassDimension::DiffPairViaGap,
    ]
    .into_iter()
    .map(NetclassDimension::native_key)
    .collect::<BTreeSet<_>>();
    if name.is_empty()
        || !valid(name)
        || !valid(description)
        || !valid(&text.value)
        || text.parent.as_ref().is_some_and(|v| !identity(v))
        || fields.keys().map(String::as_str).collect::<BTreeSet<_>>() != keys
        || fields.values().any(|v| {
            !(-1..=2_000_000_000).contains(&v.value_nm)
                || (v.defined && v.value_nm < 0)
                || v.parent.as_ref().is_some_and(|p| !identity(p))
        })
    {
        return Err("incomplete native project class definition".into());
    }
    Ok(())
}
fn assignments(settings: &json::Object<'_>) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let empty: &RawValue = serde_json::from_str("{}").map_err(|e| e.to_string())?;
    let object = json::Object::new(settings.get("netclass_assignments").unwrap_or(empty))?;
    let mut result = BTreeMap::new();
    if object.entries().count() > 4096 {
        return Err("native project assignment count exceeds bound".into());
    }
    for (net, raw) in object.entries() {
        if net.is_empty() || !valid(net) {
            return Err("invalid native assigned net".into());
        }
        let classes = super::assignment(raw)?.into_iter().collect::<BTreeSet<_>>();
        // Empty label assignments preserve native pattern matching and are omitted semantically.
        if !classes.is_empty() {
            result.insert(net.into(), classes);
        }
    }
    Ok(result)
}
fn patterns(settings: &json::Object<'_>) -> Result<Vec<(String, String)>, String> {
    let empty: &RawValue = serde_json::from_str("[]").map_err(|e| e.to_string())?;
    let array = json::Array::new(settings.get("netclass_patterns").unwrap_or(empty))?;
    if array.len() > 4096 {
        return Err("native pattern count exceeds bound".into());
    }
    array
        .values()
        .map(|raw| {
            let row = json::Object::new(raw)?;
            Ok((
                super::string(super::required(&row, "netclass")?)?,
                super::string(super::required(&row, "pattern")?)?,
            ))
        })
        .collect()
}
fn classes(settings: &json::Object<'_>) -> Result<BTreeSet<String>, String> {
    let empty: &RawValue = serde_json::from_str("[]").map_err(|e| e.to_string())?;
    let array = json::Array::new(settings.get("classes").unwrap_or(empty))?;
    if array.len() > 512 {
        return Err("native configured class count exceeds bound".into());
    }
    let mut result = BTreeSet::new();
    for raw in array.values() {
        let row = json::Object::new(raw)?;
        if !result.insert(super::string(super::required(&row, "name")?)?) {
            return Err("duplicate native configured class".into());
        }
    }
    result.insert("Default".into());
    Ok(result)
}

fn validate_parent(
    value: &Option<ClassIdentity>,
    name: &str,
    priority: i32,
    configured: &BTreeMap<String, ConfiguredClass>,
) -> Result<(), String> {
    if let Some(parent) = value {
        let expected = configured
            .get(&parent.name)
            .map(|class| class.priority)
            .or_else(|| (parent.name == name).then_some(priority));
        if expected != Some(parent.priority) {
            return Err(
                "native class parameter provenance differs from configured identities".into(),
            );
        }
    }
    Ok(())
}

fn validate_source_definitions(
    settings: &json::Object<'_>,
    configured: &BTreeMap<String, ConfiguredClass>,
) -> Result<(), String> {
    let empty: &RawValue = serde_json::from_str("[]").map_err(|e| e.to_string())?;
    let definitions = json::Array::new(settings.get("classes").unwrap_or(empty))?;
    for raw in definitions.values() {
        let object = json::Object::new(raw)?;
        let name = super::string(super::required(&object, "name")?)?;
        let native = configured.get(&name).ok_or("missing native source class")?;
        if let Some(value) = object.get("priority") {
            let priority: i32 = serde_json::from_str(value.get()).map_err(|e| e.to_string())?;
            if priority >= 0 && priority != native.priority {
                return Err("native class priority differs from its explicit source".into());
            }
        }
        for field in [
            NetclassDimension::Clearance,
            NetclassDimension::TrackWidth,
            NetclassDimension::ViaDiameter,
            NetclassDimension::ViaDrill,
            NetclassDimension::MicroviaDiameter,
            NetclassDimension::MicroviaDrill,
            NetclassDimension::DiffPairWidth,
            NetclassDimension::DiffPairGap,
            NetclassDimension::DiffPairViaGap,
        ] {
            let value = &native.fields[field.native_key()];
            match object.get(field.json_key()).filter(|v| v.get() != "-1") {
                Some(raw) => {
                    let expected = crate::routing_rules::mm_to_nm(raw)?;
                    if !value.defined || value.value_nm != expected {
                        return Err("native class dimension differs from its exact source".into());
                    }
                }
                None if name != "Default" && (value.defined || value.value_nm != -1) => {
                    return Err("native class fabricated an unspecified dimension".into());
                }
                // Default retains independently observed native factory values for omissions.
                None => {}
            }
        }
    }
    Ok(())
}

impl ProjectNetclassSnapshot {
    pub fn configured(&self) -> &BTreeMap<String, ConfiguredClass> {
        &self.configured
    }
    pub fn effective(&self) -> &BTreeMap<String, EffectiveClass> {
        &self.effective
    }
    pub fn native_project_json(&self) -> &str {
        &self.native_project_json
    }
    pub fn board_sha256(&self) -> &str {
        &self.board_sha256
    }
    pub fn project_sha256(&self) -> &str {
        &self.project_sha256
    }
    pub fn digest(&self) -> Result<String, String> {
        serde_json::to_vec(self)
            .map(|bytes| hex::encode(Sha256::digest(&bytes)))
            .map_err(|e| e.to_string())
    }
    pub(crate) fn validate(
        &self,
        board_sha256: &str,
        project: &[u8],
        effective: &serde_json::Value,
    ) -> Result<(), String> {
        if self.schema_version != 1
            || self.native_version != "10.0.7"
            || self.board_sha256 != board_sha256
            || self.project_sha256 != hex::encode(Sha256::digest(project))
            || self.native_project_json.len() > 32 * 1024 * 1024
            || self.native_project_sha256
                != hex::encode(Sha256::digest(self.native_project_json.as_bytes()))
            || self.configured.is_empty()
            || self.configured.len() > 512
            || self.effective.len() > 100_000
            || serde_json::to_value(&self.effective).map_err(|e| e.to_string())? != *effective
        {
            return Err("native project class input/report binding differs".into());
        }
        for (key, class) in &self.configured {
            if key != &class.name {
                return Err("native configured class key differs".into());
            }
            validate_fields(
                &class.name,
                &class.description,
                &class.tuning_profile,
                &class.fields,
            )?;
            validate_parent(
                &class.tuning_profile.parent,
                &class.name,
                class.priority,
                &self.configured,
            )?;
            for value in class.fields.values() {
                validate_parent(&value.parent, &class.name, class.priority, &self.configured)?;
            }
        }
        for (net, class) in &self.effective {
            if net.is_empty()
                || !valid(net)
                || class.constituents.is_empty()
                || class.constituents.len() > 512
                || class
                    .constituents
                    .iter()
                    .any(|v| !identity(v) || !self.configured.contains_key(&v.name))
                || class
                    .constituents
                    .iter()
                    .map(|v| &v.name)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != class.constituents.len()
            {
                return Err("native effective class membership differs".into());
            }
            validate_fields(
                &class.name,
                &class.description,
                &class.tuning_profile,
                &class.fields,
            )?;
            validate_parent(
                &class.tuning_profile.parent,
                &class.name,
                class.priority,
                &self.configured,
            )?;
            for value in class.fields.values() {
                validate_parent(&value.parent, &class.name, class.priority, &self.configured)?;
            }
        }
        let raw = json::parse(std::str::from_utf8(project).map_err(|e| e.to_string())?)?;
        let original = json::Object::new(raw)?;
        let empty: &RawValue = serde_json::from_str("{}").map_err(|e| e.to_string())?;
        let before = json::Object::new(original.get("net_settings").unwrap_or(empty))?;
        let normalized = json::Object::new(json::parse(&self.native_project_json)?)?;
        let after = json::Object::new(super::required(&normalized, "net_settings")?)?;
        validate_source_definitions(&before, &self.configured)?;
        for class in self.effective.values() {
            if class.constituents.iter().any(|member| {
                self.configured
                    .get(&member.name)
                    .is_none_or(|value| value.priority != member.priority)
            }) {
                return Err("native constituent priority differs from configured class".into());
            }
        }
        let names = self.configured.keys().cloned().collect::<BTreeSet<_>>();
        if classes(&before)? != names
            || classes(&after)? != names
            || assignments(&before)? != assignments(&after)?
            || patterns(&before)? != patterns(&after)?
        {
            return Err("native serialized assignment/pattern/configured inventory differs".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(raw: &str) -> ProjectNetclassSnapshot {
        let fields = [
            NetclassDimension::Clearance,
            NetclassDimension::TrackWidth,
            NetclassDimension::ViaDiameter,
            NetclassDimension::ViaDrill,
            NetclassDimension::MicroviaDiameter,
            NetclassDimension::MicroviaDrill,
            NetclassDimension::DiffPairWidth,
            NetclassDimension::DiffPairGap,
            NetclassDimension::DiffPairViaGap,
        ]
        .into_iter()
        .map(|key| {
            (
                key.native_key().into(),
                ClassDimension {
                    defined: false,
                    value_nm: -1,
                    parent: None,
                },
            )
        })
        .collect();
        let class = ConfiguredClass {
            name: "Default".into(),
            priority: i32::MAX,
            description: String::new(),
            tuning_profile: ClassText {
                defined: false,
                value: String::new(),
                parent: None,
            },
            fields,
        };
        ProjectNetclassSnapshot {
            schema_version: 1,
            native_version: "10.0.7".into(),
            board_sha256: "board".into(),
            project_sha256: hex::encode(Sha256::digest(raw.as_bytes())),
            native_project_sha256: hex::encode(Sha256::digest(raw.as_bytes())),
            native_project_json: raw.into(),
            configured: BTreeMap::from([("Default".into(), class)]),
            effective: BTreeMap::new(),
        }
    }

    #[test]
    fn complete_native_inventory_keeps_membership_and_raw_binding_mandatory() {
        let raw = "{\"net_settings\":{\"classes\":[{\"name\":\"Default\"}]}}";
        let snapshot = fixture(raw);
        snapshot
            .validate("board", raw.as_bytes(), &serde_json::json!({}))
            .unwrap();
        let mut missing = snapshot.clone();
        missing
            .configured
            .get_mut("Default")
            .unwrap()
            .fields
            .remove("ViaDrill");
        assert!(
            missing
                .validate("board", raw.as_bytes(), &serde_json::json!({}))
                .is_err()
        );
        let mut altered = snapshot.clone();
        altered.native_project_json.push(' ');
        assert!(
            altered
                .validate("board", raw.as_bytes(), &serde_json::json!({}))
                .is_err()
        );
        let mut foreign = snapshot.clone();
        foreign.project_sha256 = "other".into();
        assert!(
            foreign
                .validate("board", raw.as_bytes(), &serde_json::json!({}))
                .is_err()
        );
    }
    #[test]
    fn unused_class_values_and_inherited_provenance_must_match_the_exact_source() {
        let raw = "{\"net_settings\":{\"classes\":[{\"name\":\"Default\"},{\"name\":\"Power\",\"priority\":0,\"clearance\":0.420001}]}}";
        let mut snapshot = fixture(raw);
        let mut power = snapshot.configured["Default"].clone();
        power.name = "Power".into();
        power.priority = 0;
        power.fields.insert(
            "Clearance".into(),
            ClassDimension {
                defined: true,
                value_nm: 420001,
                parent: Some(ClassIdentity {
                    name: "Power".into(),
                    priority: 0,
                }),
            },
        );
        snapshot.configured.insert("Power".into(), power);
        snapshot
            .validate("board", raw.as_bytes(), &serde_json::json!({}))
            .unwrap();
        let mut wrong_value = snapshot.clone();
        wrong_value
            .configured
            .get_mut("Power")
            .unwrap()
            .fields
            .get_mut("Clearance")
            .unwrap()
            .value_nm += 1;
        assert!(
            wrong_value
                .validate("board", raw.as_bytes(), &serde_json::json!({}))
                .is_err()
        );
        let mut wrong_parent = snapshot.clone();
        wrong_parent
            .configured
            .get_mut("Power")
            .unwrap()
            .fields
            .get_mut("Clearance")
            .unwrap()
            .parent
            .as_mut()
            .unwrap()
            .priority += 1;
        assert!(
            wrong_parent
                .validate("board", raw.as_bytes(), &serde_json::json!({}))
                .is_err()
        );
        let power = &snapshot.configured["Power"];
        snapshot.effective.insert(
            "N".into(),
            EffectiveClass {
                name: "Power,Default".into(),
                priority: -1,
                description: String::new(),
                tuning_profile: power.tuning_profile.clone(),
                fields: power.fields.clone(),
                constituents: vec![
                    ClassIdentity {
                        name: "Power".into(),
                        priority: 0,
                    },
                    ClassIdentity {
                        name: "Default".into(),
                        priority: i32::MAX,
                    },
                ],
            },
        );
        let effective = serde_json::to_value(&snapshot.effective).unwrap();
        snapshot
            .validate("board", raw.as_bytes(), &effective)
            .unwrap();
        snapshot.effective.get_mut("N").unwrap().constituents[0].priority += 1;
        let effective = serde_json::to_value(&snapshot.effective).unwrap();
        assert!(
            snapshot
                .validate("board", raw.as_bytes(), &effective)
                .is_err()
        );
    }
}
