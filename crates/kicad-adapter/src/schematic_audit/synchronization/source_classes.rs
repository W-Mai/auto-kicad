//! Bound read-only source declarations and complete final native class membership.
use super::*;
use kicad_schematic::cst::parse_one;
use kiutils_sexpr::{Atom, Node};

fn list(node: &Node) -> Option<&[Node]> {
    match node {
        Node::List { items, .. } => Some(items),
        _ => None,
    }
}
fn atom(node: Option<&Node>) -> Option<&str> {
    match node {
        Some(Node::Atom {
            atom: Atom::Symbol(value) | Atom::Quoted(value),
            ..
        }) => Some(value),
        _ => None,
    }
}

fn declaration_names(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    let doc = parse_one(text).map_err(|e| e.to_string())?;
    let root = doc
        .nodes
        .first()
        .and_then(list)
        .ok_or("missing captured schematic root")?;
    if atom(root.first()) != Some("kicad_sch") {
        return Err("invalid captured declaration source".into());
    }
    let mut names = BTreeSet::new();
    for label in root.iter().filter_map(list).filter(|items| {
        matches!(
            atom(items.first()),
            Some(
                "directive_label"
                    | "netclass_flag"
                    | "label"
                    | "global_label"
                    | "hierarchical_label"
            )
        )
    }) {
        for property in label.iter().filter_map(list).filter(|items| {
            atom(items.first()) == Some("property") && atom(items.get(1)) == Some("Netclass")
        }) {
            let value = atom(property.get(2)).ok_or("missing source Netclass value")?;
            if value.len() > 4096 || value.contains('\0') {
                return Err("source class declaration exceeds name bound".into());
            }
            // Dynamic expressions have no textual evaluation authority. Only their complete
            // resolved native class, when independently configured, can establish membership.
            if !value.is_empty() && !value.contains("${") {
                names.insert(value.to_owned());
            }
            if names.len() > 512 {
                return Err("source class declaration inventory exceeds bound".into());
            }
        }
    }
    Ok(names)
}

fn captured_declarations(audit: &SchematicAudit) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for file in &audit.inner.parsed.files {
        let path = audit
            .inner
            .policy
            .project
            .join(&file.artifact.relative_path);
        let bytes = audit
            .inner
            .source
            .files
            .get(&path)
            .and_then(|input| input.bytes.as_deref())
            .ok_or("missing captured declaration source")?;
        names.extend(declaration_names(bytes)?);
        if names.len() > 512 {
            return Err("captured class declaration inventory exceeds bound".into());
        }
    }
    Ok(names)
}

fn resolve(expression: &str, known: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    if expression.is_empty() {
        return Ok(BTreeSet::new());
    }
    let parts = expression
        .split(',')
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let composite = expression.contains(',') && parts.iter().all(|part| known.contains(part));
    if known.contains(expression) {
        if composite {
            return Err(format!(
                "ambiguous native source class expression: {expression:?}"
            ));
        }
        Ok(BTreeSet::from([expression.into()]))
    } else if composite {
        Ok(parts)
    } else {
        Err(format!(
            "native source class has no complete configured/declaration identity: {expression:?}"
        ))
    }
}

/// New original scope derives declarations independently of requested actions or journal roots.
pub(super) fn references(
    audit: &SchematicAudit,
    project: &[u8],
) -> Result<BTreeSet<String>, String> {
    let declarations = captured_declarations(audit)?;
    let mut known = project::configured_class_names(project)?;
    known.extend(declarations.iter().cloned());
    let mut references = declarations;
    for expression in audit
        .netlist()
        .nets
        .iter()
        .filter_map(|net| net.class.as_deref())
    {
        references.extend(resolve(expression, &known)?);
    }
    Ok(references)
}

pub(super) fn requirements(
    source: &SynchronizationSource,
    project: &[u8],
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let eligible = source
        .components()
        .iter()
        .filter(|component| component.eligible_for_board())
        .flat_map(|component| component.pins.values().map(|pin| pin.name.as_str()))
        .collect::<BTreeSet<_>>();
    let nets = source
        .audit
        .netlist()
        .nets
        .iter()
        .filter(|net| eligible.contains(net.name.as_str()))
        .filter(|net| {
            net.class
                .as_deref()
                .is_some_and(|class| !class.is_empty() && class != "Default")
        })
        .collect::<Vec<_>>();
    if nets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let configured = project::configured_class_names(project)?;
    let mut known = configured.clone();
    known.extend(captured_declarations(&source.audit)?);
    let mut requirements = BTreeMap::new();
    for net in nets {
        let mut names = resolve(net.class.as_deref().ok_or("missing source class")?, &known)?;
        names.remove("Default");
        for class in &names {
            if !configured.contains(class) {
                return Err(format!(
                    "source netclass {class:?} on net {:?} requires an explicit project definition",
                    net.name
                ));
            }
        }
        if !names.is_empty() {
            requirements.insert(net.name.clone(), names);
        }
    }
    Ok(requirements)
}

/// Only exact native undefined-class diagnostics owned by selected new source definitions resolve.
pub(super) fn definition_erc(
    before: &SchematicAudit,
    after: &SchematicAudit,
    original_project: &[u8],
    actions: &[project::ProjectNetclassChange],
) -> Result<kicad_checks::erc::ErcDelta, String> {
    let delta = before
        .erc()
        .compare(after.erc())
        .map_err(|e| e.to_string())?;
    if delta.metadata_changed || delta.sheet_headers_changed || !delta.introduced.is_empty() {
        return Err(
            "project netclass changes cannot change ERC metadata, sheets or introduce diagnostics"
                .into(),
        );
    }
    if delta.resolved.is_empty() {
        return Ok(delta);
    }
    let configured = project::configured_class_names(original_project)?;
    let newly_defined = actions
        .iter()
        .filter_map(|action| match action {
            project::ProjectNetclassChange::Define { name, .. } if !configured.contains(name) => {
                Some(name.as_str())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut owners = BTreeSet::new();
    for file in &before.inner.parsed.files {
        let path = before
            .inner
            .policy
            .project
            .join(&file.artifact.relative_path);
        let bytes = before
            .inner
            .source
            .files
            .get(&path)
            .and_then(|input| input.bytes.as_deref())
            .ok_or("missing captured ERC declaration source")?;
        let doc = parse_one(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let root = doc
            .nodes
            .first()
            .and_then(list)
            .ok_or("missing declaration source root")?;
        for label in root.iter().filter_map(list).filter(|items| {
            matches!(
                atom(items.first()),
                Some(
                    "directive_label"
                        | "netclass_flag"
                        | "label"
                        | "global_label"
                        | "hierarchical_label"
                )
            )
        }) {
            if !label.iter().filter_map(list).any(|items| {
                atom(items.first()) == Some("property")
                    && atom(items.get(1)) == Some("Netclass")
                    && atom(items.get(2)).is_some_and(|name| newly_defined.contains(name))
            }) {
                continue;
            }
            let ids = label
                .iter()
                .filter_map(list)
                .filter(|items| atom(items.first()) == Some("uuid"))
                .map(|items| atom(items.get(1)).ok_or("missing declaring label UUID"))
                .collect::<Result<Vec<_>, _>>()?;
            if ids.len() != 1 || !super::board::uuid(ids[0]) {
                return Err(
                    "resolved source diagnostic lacks one exact declaring label UUID".into(),
                );
            }
            for sheet in before
                .identities()
                .sheets()
                .iter()
                .filter(|sheet| sheet.file == file.artifact.relative_path)
            {
                owners.insert((sheet.path.clone(), ids[0].to_owned()));
            }
        }
    }
    for diagnostic in &delta.resolved {
        let value: Value =
            serde_json::from_str(&diagnostic.diagnostic_json).map_err(|e| e.to_string())?;
        if value.get("type").and_then(Value::as_str) != Some("undefined_netclass") {
            return Err("project definition cannot resolve an unrelated ERC diagnostic".into());
        }
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .ok_or("missing source diagnostic items")?;
        if items.is_empty()
            || items.iter().any(|item| {
                item.get("uuid").and_then(Value::as_str).is_none_or(|uuid| {
                    !owners.contains(&(
                        diagnostic.sheet_uuid_path.trim_end_matches('/').to_owned(),
                        uuid.to_owned(),
                    ))
                })
            })
        {
            return Err(
                "resolved ERC diagnostic is not owned by a newly defined source declaration".into(),
            );
        }
    }
    Ok(delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_expressions_require_complete_unambiguous_bound_names() {
        let mut names = BTreeSet::from(["Power".into(), "Secondary".into(), "Default".into()]);
        assert_eq!(
            resolve("Power,Secondary", &names).unwrap(),
            BTreeSet::from(["Power".into(), "Secondary".into()])
        );
        assert!(resolve("Unknown,Default", &names).is_err());
        names.insert("Power,Secondary".into());
        assert!(resolve("Power,Secondary", &names).is_err());
        names.insert("Unknown".into());
        assert_eq!(
            resolve("Unknown,Default", &names).unwrap(),
            BTreeSet::from(["Unknown".into(), "Default".into()])
        );
    }
    #[test]
    fn declaration_scope_is_exact_read_only_and_never_promotes_similar_fields() {
        let raw = br#"(kicad_sch (directive_label "" (property "Netclass" "Power")) (netclass_flag "" (property "Netclass" "Unknown")) (label "N" (property "NetClass" "Ignored")) (symbol (property "Netclass" "NotALabel")) (global_label "G" (property "Netclass" "${DYNAMIC}")))"#;
        assert_eq!(
            declaration_names(raw).unwrap(),
            BTreeSet::from(["Power".into(), "Unknown".into()])
        );
        assert!(declaration_names(b"(other)").is_err());
    }
}
