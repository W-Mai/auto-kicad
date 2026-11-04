//! Exact observed assignment/pattern selections. These records confer no mutation authority.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassAssignment {
    pub classes: Vec<String>,
    pub raw_json: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassPattern {
    pub index: u32,
    pub class: String,
    pub pattern: String,
    pub raw_json: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectNetclassSelections {
    pub schema_version: u32,
    pub project_sha256: String,
    pub net_settings_version: Option<u32>,
    pub assignments: BTreeMap<String, ProjectNetclassAssignment>,
    pub patterns: Vec<ProjectNetclassPattern>,
}

pub fn inspect_project_netclass_selections(
    bytes: &[u8],
) -> Result<ProjectNetclassSelections, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let root = Object::new(json::parse(text)?)?;
    let mut observed = ProjectNetclassSelections {
        schema_version: 1,
        project_sha256: hex::encode(Sha256::digest(bytes)),
        net_settings_version: None,
        assignments: BTreeMap::new(),
        patterns: Vec::new(),
    };
    let Some(settings) = root.get("net_settings") else {
        return Ok(observed);
    };
    let settings = Object::new(settings)?;
    if let Some(meta) = settings.get("meta") {
        observed.net_settings_version = Some(
            serde_json::from_str(required(&Object::new(meta)?, "version")?.get())
                .map_err(|e| e.to_string())?,
        );
    }
    if let Some(assignments) = settings.get("netclass_assignments") {
        let assignments = Object::new(assignments)?;
        if assignments.entries().count() > 4096 {
            return Err("project assignment selection bound exceeded".into());
        }
        for (net, value) in assignments.entries() {
            name(net)?;
            observed.assignments.insert(
                net.into(),
                ProjectNetclassAssignment {
                    classes: assignment(value)?,
                    raw_json: value.get().into(),
                },
            );
        }
    }
    if let Some(patterns) = settings.get("netclass_patterns") {
        let patterns = Array::new(patterns)?;
        if patterns.len() > 4096 {
            return Err("project pattern selection bound exceeded".into());
        }
        for (index, value) in patterns.values().enumerate() {
            let row = Object::new(value)?;
            observed.patterns.push(ProjectNetclassPattern {
                index: index as u32,
                class: string(required(&row, "netclass")?)?,
                pattern: string(required(&row, "pattern")?)?,
                raw_json: value.get().into(),
            });
        }
    }
    Ok(observed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selections_preserve_empty_legacy_and_ordered_unknown_tokens_without_scope_authority() {
        let raw=br#"{"net_settings":{"meta":{"version":5},"netclass_assignments":{"N":"Power","EMPTY":[],"A":["Power","Default"]},"netclass_patterns":[{"netclass":"Power","pattern":"N*","unknown":123456789012345678901234567890},{"netclass":"Default","pattern":"*"}]}}"#;
        let observed = inspect_project_netclass_selections(raw).unwrap();
        assert_eq!(observed.assignments["N"].raw_json, r#""Power""#);
        assert_eq!(observed.assignments["EMPTY"].classes, Vec::<String>::new());
        assert_eq!(observed.assignments["EMPTY"].raw_json, "[]");
        assert_eq!(observed.assignments["A"].classes, vec!["Power", "Default"]);
        assert_eq!(
            observed
                .patterns
                .iter()
                .map(|p| p.index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            observed.patterns[0]
                .raw_json
                .contains("123456789012345678901234567890")
        );
        assert!(
            inspect_project_netclass_selections(
                br#"{"net_settings":{"netclass_assignments":{"N":[],"N":[]}}}"#
            )
            .is_err()
        );
        assert!(
            inspect_project_netclass_selections(
                br#"{"net_settings":{"netclass_patterns":[{"netclass":"Power"}]}}"#
            )
            .is_err()
        );
        let absent = inspect_project_netclass_selections(b"{}").unwrap();
        assert!(absent.assignments.is_empty() && absent.patterns.is_empty());
        assert_eq!(absent.net_settings_version, None);
    }
}
