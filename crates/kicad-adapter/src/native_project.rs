//! Exact project-variable binding for private native snapshots.
use serde_json::value::RawValue;
use std::collections::BTreeMap;

/// Bind the original project directory while retaining every other raw JSON value.
pub fn project_with_root(project: &[u8], root: &str) -> Result<Vec<u8>, String> {
    if project.len() > 32 * 1024 * 1024 || !std::path::Path::new(root).is_absolute() {
        return Err("private project context exceeds bounds or has a relative root".into());
    }
    kicad_checks::validate_json_structure(project).map_err(|e| e.to_string())?;
    let mut top: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(project).map_err(|e| e.to_string())?;
    let mut vars: BTreeMap<String, Box<RawValue>> = match top.remove("text_variables") {
        Some(raw) => serde_json::from_str(raw.get()).map_err(|e| e.to_string())?,
        None => BTreeMap::new(),
    };
    for value in vars.values() {
        serde_json::from_str::<String>(value.get()).map_err(|e| e.to_string())?;
    }
    vars.insert(
        "KIPRJMOD".into(),
        RawValue::from_string(serde_json::to_string(root).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    );
    top.insert(
        "text_variables".into(),
        RawValue::from_string(serde_json::to_string(&vars).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    );
    serde_json::to_vec(&top).map_err(|e| e.to_string())
}
