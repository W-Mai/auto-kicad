//! Expand source-bound inheritance into a standalone native schematic-cache definition.
use super::*;

struct Field {
    tag: String,
    property: Option<String>,
    value: Option<String>,
    raw: String,
}
struct Definition {
    name: String,
    fields: Vec<Field>,
}
fn fields(raw: &str) -> Result<Definition, String> {
    let document = parse_one(raw).map_err(|error| error.to_string())?;
    if document.nodes.len() != 1 {
        return Err("one inherited symbol definition is required".into());
    }
    let items = list_items(&document.nodes[0]).ok_or("invalid inherited definition")?;
    let name = atom_text(items.get(1))
        .ok_or("missing inherited symbol name")?
        .to_owned();
    let mut fields = Vec::new();
    let mut properties = BTreeSet::new();
    let mut singleton = BTreeSet::new();
    for node in &items[2..] {
        let children = list_items(node).ok_or("invalid inherited symbol field")?;
        let tag = atom_text(children.first())
            .ok_or("missing inherited field tag")?
            .to_owned();
        if !matches!(
            tag.as_str(),
            "power"
                | "body_styles"
                | "pin_names"
                | "pin_numbers"
                | "exclude_from_sim"
                | "in_bom"
                | "on_board"
                | "in_pos_files"
                | "duplicate_pin_numbers_are_jumpers"
                | "jumper_pin_groups"
                | "property"
                | "extends"
                | "symbol"
                | "arc"
                | "bezier"
                | "circle"
                | "pin"
                | "polyline"
                | "rectangle"
                | "text"
                | "text_box"
                | "embedded_fonts"
                | "embedded_files"
        ) {
            return Err(format!("unsupported inherited symbol field: {tag}"));
        }
        let property = if tag == "property" {
            let node = children.get(1).ok_or("missing inherited property name")?;
            let span = node_span(node);
            if raw[span.start..span.end].contains('\\') {
                return Err("escaped inherited property identifiers are ambiguous".into());
            }
            let name = atom_text(Some(node))
                .ok_or("invalid inherited property name")?
                .to_owned();
            if !properties.insert(name.clone()) {
                return Err("duplicate inherited symbol property".into());
            }
            Some(name)
        } else {
            if !matches!(
                tag.as_str(),
                "symbol"
                    | "arc"
                    | "bezier"
                    | "circle"
                    | "pin"
                    | "polyline"
                    | "rectangle"
                    | "text"
                    | "text_box"
            ) && !singleton.insert(tag.clone())
            {
                return Err("duplicate inherited symbol field".into());
            }
            None
        };
        let value =
            atom_text(children.get(if property.is_some() { 2 } else { 1 })).map(str::to_owned);
        if (tag == "embedded_files" && children.len() > 1)
            || (tag == "embedded_fonts" && value.as_deref() != Some("no"))
        {
            return Err(
                "inherited embedded files and fonts require native resource support".into(),
            );
        }
        let span = node_span(node);
        fields.push(Field {
            tag,
            property,
            value,
            raw: raw[span.start..span.end].to_owned(),
        });
    }
    Ok(Definition { name, fields })
}

fn replace_name(raw: &str, name: &str) -> Result<String, String> {
    let document = parse_one(raw).map_err(|error| error.to_string())?;
    let items = list_items(&document.nodes[0]).ok_or("invalid inherited body")?;
    let span = node_span(items.get(1).ok_or("missing inherited body name")?);
    let mut result = raw.to_owned();
    let quoted = format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""));
    result.replace_range(span.start..span.end, &quoted);
    Ok(result)
}

pub(super) fn expand(
    text: &str,
    definitions: &BTreeMap<String, (Span, Vec<String>)>,
    selected: &str,
) -> Result<(String, Vec<String>), String> {
    let mut chain = Vec::new();
    let mut name = selected;
    let mut seen = BTreeSet::new();
    loop {
        if chain.len() >= 32 || !seen.insert(name) {
            return Err("symbol inheritance cycle or depth exceeds bound".into());
        }
        let (span, parents) = definitions.get(name).ok_or("missing inherited symbol")?;
        chain.push(
            text.get(span.start..span.end)
                .ok_or("invalid inherited symbol span")?,
        );
        if parents.is_empty() {
            break;
        }
        if parents.len() != 1 {
            return Err("ambiguous symbol inheritance".into());
        }
        name = &parents[0];
    }
    if chain.len() == 1 {
        return Ok((chain[0].to_owned(), Vec::new()));
    }
    chain.reverse();
    let ancestors = chain[..chain.len() - 1]
        .iter()
        .map(|raw| (*raw).to_owned())
        .collect();
    let parsed = chain
        .iter()
        .map(|raw| fields(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let root = &parsed[0];
    let leaf = parsed.last().ok_or("missing inherited leaf")?;
    let mut properties = BTreeMap::<String, String>::new();
    for definition in &parsed {
        for field in &definition.fields {
            if let Some(property) = &field.property {
                let inherited_if_empty = matches!(
                    property.as_str(),
                    "Reference"
                        | "Value"
                        | "Footprint"
                        | "Datasheet"
                        | "Description"
                        | "ki_keywords"
                        | "ki_fp_filters"
                );
                let value = field
                    .value
                    .as_deref()
                    .ok_or("missing inherited property value")?;
                if !inherited_if_empty || !value.is_empty() || !properties.contains_key(property) {
                    properties.insert(property.clone(), field.raw.clone());
                }
            }
        }
    }
    let parent = &parsed[parsed.len() - 2];
    let mut flags = BTreeMap::new();
    for (tag, default) in [
        ("exclude_from_sim", "no"),
        ("in_bom", "yes"),
        ("on_board", "yes"),
        ("in_pos_files", "yes"),
    ] {
        let value = parent
            .fields
            .iter()
            .find(|field| field.tag == tag)
            .map(|field| {
                field
                    .value
                    .as_deref()
                    .ok_or("missing inherited exclusion flag value")
            })
            .transpose()?
            .unwrap_or(default);
        if !matches!(value, "yes" | "no") {
            return Err("invalid inherited exclusion flag".into());
        }
        flags.insert(tag.to_owned(), format!("({tag} {value})"));
    }
    let mut result = format!("(symbol \"{}\"\n", leaf.name);
    for field in &root.fields {
        let raw = if let Some(property) = &field.property {
            properties
                .remove(property)
                .ok_or("missing merged inherited property")?
        } else if let Some(value) = flags.remove(&field.tag) {
            value
        } else if field.tag == "symbol" {
            let document = parse_one(&field.raw).map_err(|error| error.to_string())?;
            let items = list_items(&document.nodes[0]).ok_or("invalid inherited unit")?;
            let body = atom_text(items.get(1)).ok_or("missing inherited unit name")?;
            let suffix = body
                .strip_prefix(&format!("{}_", root.name))
                .ok_or("invalid inherited unit name prefix")?;
            replace_name(&field.raw, &format!("{}_{}", leaf.name, suffix))?
        } else {
            field.raw.clone()
        };
        result.push_str(&raw);
        result.push('\n');
    }
    for raw in properties.values().chain(flags.values()) {
        result.push_str(raw);
        result.push('\n');
    }
    result.push(')');
    if result.len() > MAX_SYMBOL_BYTES as usize {
        return Err("expanded symbol exceeds byte bound".into());
    }
    Ok((result, ancestors))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expand_library(text: &str, selected: &str) -> Result<(String, Vec<String>), String> {
        validate_raw_nesting(text.as_bytes())?;
        let document = parse_one(text).map_err(|error| error.to_string())?;
        let mut definitions = BTreeMap::new();
        for node in list_items(&document.nodes[0]).unwrap().iter().skip(1) {
            let items = list_items(node).unwrap();
            let name = atom_text(items.get(1)).unwrap().to_owned();
            let parents = items
                .iter()
                .filter_map(list_items)
                .filter(|items| atom_text(items.first()) == Some("extends"))
                .map(|items| atom_text(items.get(1)).unwrap().to_owned())
                .collect();
            definitions.insert(name, (node_span(node), parents));
        }
        expand(text, &definitions, selected)
    }
    #[test]
    fn native_inheritance_keeps_root_geometry_and_applies_fields_and_immediate_parent_flags() {
        let text = r#"(lib
            (symbol "Base" (in_bom no) (on_board no)
                (property "Value" "base") (property "Footprint" "P:Base") (property "Custom" "base")
                (symbol "Base_1_1" (pin passive line (number "1"))))
            (symbol "Middle" (extends "Base") (property "Value" "middle") (property "Custom" "middle"))
            (symbol "Leaf" (extends "Middle") (in_bom no) (property "Value" "") (property "Footprint" "")
                (property "Custom" "") (property "Added" "中文\\text")
                (symbol "Leaf_1_1" (pin output line (number "99")))))"#;
        let (expanded, ancestors) = expand_library(text, "Leaf").unwrap();
        assert_eq!(ancestors.len(), 2);
        assert!(ancestors[0].contains("\"Base\""));
        assert!(expanded.contains("(property \"Value\" \"middle\")"));
        assert!(expanded.contains("(property \"Footprint\" \"P:Base\")"));
        assert!(expanded.contains("(property \"Custom\" \"\")"));
        assert!(expanded.contains("(symbol \"Leaf_1_1\" (pin passive"));
        assert!(!expanded.contains("extends") && !expanded.contains("\"99\""));
        assert!(expanded.contains("(in_bom yes)") && expanded.contains("(on_board yes)"));
        assert_eq!(expand_library(text, "Base").unwrap().0, ancestors[0]);
    }
    #[test]
    fn inheritance_rejects_cycles_resources_and_ambiguous_properties() {
        for text in [
            r#"(lib (symbol "Leaf" (extends "Leaf")))"#,
            r#"(lib (symbol "Leaf" (extends "Missing")))"#,
            r#"(lib (symbol "Base") (symbol "Leaf" (extends "Base") (property "Value" "a") (property "Value" "b")))"#,
            r#"(lib (symbol "Base") (symbol "Leaf" (extends "Base") (property "\x56alue" "a")))"#,
            r#"(lib (symbol "Base" (embedded_fonts yes)) (symbol "Leaf" (extends "Base")))"#,
            r#"(lib (symbol "Base") (symbol "Leaf" (extends "Base") (embedded_files (file "a"))))"#,
            r#"(lib (symbol "Base") (symbol "Leaf" (extends "Base") (future_semantics yes)))"#,
            r#"(lib (symbol "Base" (in_bom)) (symbol "Leaf" (extends "Base")))"#,
        ] {
            assert!(expand_library(text, "Leaf").is_err(), "{text}");
        }
        let text = format!(
            "(lib (symbol \"Base\") {})",
            (0..33)
                .map(|index| format!(
                    "(symbol \"D{index}\" (extends \"{}\"))",
                    if index == 0 {
                        "Base".into()
                    } else {
                        format!("D{}", index - 1)
                    }
                ))
                .collect::<String>()
        );
        assert!(expand_library(&text, "D32").is_err());
    }
}
