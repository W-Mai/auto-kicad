//! Bounded, read-only decoding of native footprint library tables.

use kiutils_sexpr::{Atom, Node, parse_one};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

pub const MAX_TABLE_BYTES: usize = 1024 * 1024;
pub const MAX_TABLE_ENTRIES: usize = 4096;
const MAX_TABLE_NESTING: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum LibraryFormat {
    KiCad,
    Table,
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LibraryTableEntry {
    pub nickname: String,
    pub format: LibraryFormat,
    pub uri: String,
    pub options: String,
    pub description: String,
    pub disabled: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FootprintLibraryTable {
    pub version: Option<u32>,
    pub entries: Vec<LibraryTableEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LibraryTableEvidence {
    pub requested_path: PathBuf,
    pub canonical_path: Option<PathBuf>,
    pub present: bool,
    pub size: u64,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LibraryTableFile {
    pub evidence: LibraryTableEvidence,
    pub table: Option<FootprintLibraryTable>,
}

pub(crate) fn render_footprint_library_rows(rows: &[LibraryTableEntry]) -> Result<String, String> {
    let quote = |value: &str| serde_json::to_string(value).map_err(|e| e.to_string());
    let mut table = String::from("(fp_lib_table (version 7)\n");
    for row in rows {
        let format = match &row.format {
            LibraryFormat::KiCad => "KiCad",
            LibraryFormat::Table => "Table",
            LibraryFormat::Unsupported(v) => v,
        };
        table.push_str(&format!(
            "  (lib (name {}) (type {}) (uri {}) (options {}) (descr {}){}{})\n",
            quote(&row.nickname)?,
            quote(format)?,
            quote(&row.uri)?,
            quote(&row.options)?,
            quote(&row.description)?,
            if row.disabled { " (disabled)" } else { "" },
            if row.hidden { " (hidden)" } else { "" }
        ));
        if table.len() >= MAX_TABLE_BYTES {
            return Err("relocated library table exceeds byte limit".into());
        }
    }
    table.push_str(")\n");
    if parse_footprint_library_table(table.as_bytes())?.entries != rows {
        return Err("library table serialization changed rows".into());
    }
    Ok(table)
}

/// Validate a single library identity component before constructing a filename.
pub fn validate_library_component(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 512
        || matches!(value, "." | "..")
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err("library identity must be a nonempty filename component".into());
    }
    Ok(())
}

/// Read one table without expanding paths or granting access to its entries.
pub fn read_footprint_library_table(path: &Path) -> Result<LibraryTableFile, String> {
    if !path.is_absolute() {
        return Err("library table path must be absolute".into());
    }
    let canonical = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // A dangling link is not an absent configuration file.
            if fs::symlink_metadata(path).is_ok() {
                return Err("library table link target is missing".into());
            }
            return Ok(LibraryTableFile {
                evidence: LibraryTableEvidence {
                    requested_path: path.to_path_buf(),
                    canonical_path: None,
                    present: false,
                    size: 0,
                    digest: None,
                },
                table: None,
            });
        }
        Err(error) => return Err(format!("cannot resolve library table: {error}")),
    };
    let metadata = fs::metadata(&canonical).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_TABLE_BYTES as u64 {
        return Err("library table is not a bounded regular file".into());
    }
    let file = File::open(&canonical).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.len() > MAX_TABLE_BYTES as u64 {
        return Err("opened library table is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_TABLE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let table = parse_footprint_library_table(&bytes)?;
    if bytes.len() as u64 != opened.len()
        || fs::canonicalize(path).map_err(|e| e.to_string())? != canonical
    {
        return Err("library table changed during reading".into());
    }
    Ok(LibraryTableFile {
        evidence: LibraryTableEvidence {
            requested_path: path.to_path_buf(),
            canonical_path: Some(canonical),
            present: true,
            size: bytes.len() as u64,
            digest: Some(hex::encode(Sha256::digest(&bytes))),
        },
        table: Some(table),
    })
}

/// Decode table syntax; URI expansion, trust and inclusion policy are separate.
pub fn parse_footprint_library_table(bytes: &[u8]) -> Result<FootprintLibraryTable, String> {
    if bytes.len() > MAX_TABLE_BYTES {
        return Err("library table exceeds byte limit".into());
    }
    bound_nesting(bytes)?;
    let text = std::str::from_utf8(bytes).map_err(|_| "library table is not UTF-8")?;
    let cst = parse_one(text).map_err(|e| format!("invalid library table syntax: {e}"))?;
    let root = list(cst.nodes.first().ok_or("missing library table root")?)?;
    if atom(root.first().ok_or("missing library table tag")?)? != "fp_lib_table" {
        return Err("expected fp_lib_table root".into());
    }
    let mut version = None;
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    for node in &root[1..] {
        let fields = list(node)?;
        match atom(fields.first().ok_or("empty table form")?)? {
            "version" => {
                if version.is_some() || scalar(fields)? != "7" {
                    return Err("duplicate or unsupported library table version".into());
                }
                version = Some(7);
            }
            "lib" => {
                if entries.len() >= MAX_TABLE_ENTRIES {
                    return Err("library table exceeds entry limit".into());
                }
                let entry = parse_entry(&fields[1..])?;
                if !names.insert(entry.nickname.clone()) {
                    return Err("duplicate library nickname in table".into());
                }
                entries.push(entry);
            }
            _ => return Err("unsupported library table field".into()),
        }
    }
    Ok(FootprintLibraryTable { version, entries })
}

fn list(node: &Node) -> Result<&[Node], String> {
    match node {
        Node::List { items, .. } => Ok(items),
        _ => Err("expected library table list".into()),
    }
}

fn atom(node: &Node) -> Result<&str, String> {
    match node {
        Node::Atom {
            atom: Atom::Quoted(value) | Atom::Symbol(value),
            ..
        } => Ok(value),
        _ => Err("expected library table atom".into()),
    }
}

fn scalar(fields: &[Node]) -> Result<&str, String> {
    if fields.len() != 2 {
        return Err("library table value must be scalar".into());
    }
    atom(&fields[1])
}

fn parse_entry(fields: &[Node]) -> Result<LibraryTableEntry, String> {
    let mut seen = BTreeSet::new();
    let mut nickname = None;
    let mut format = None;
    let mut uri = None;
    let mut options = String::new();
    let mut description = String::new();
    let mut disabled = false;
    let mut hidden = false;
    for field in fields {
        let items = list(field)?;
        let tag = atom(items.first().ok_or("empty library field")?)?;
        if !seen.insert(tag) {
            return Err("duplicate field in library entry".into());
        }
        match tag {
            "name" => {
                let name = scalar(items)?;
                validate_library_component(name)?;
                nickname = Some(name.to_owned());
            }
            "type" => {
                let value = scalar(items)?;
                if value.is_empty() {
                    return Err("empty library format".into());
                }
                format = Some(match value {
                    "KiCad" => LibraryFormat::KiCad,
                    "Table" => LibraryFormat::Table,
                    _ => LibraryFormat::Unsupported(value.to_owned()),
                });
            }
            "uri" => {
                let value = scalar(items)?;
                if value.is_empty() || value.chars().any(char::is_control) {
                    return Err("library URI is empty or contains controls".into());
                }
                uri = Some(value.to_owned());
            }
            "options" => options = scalar(items)?.to_owned(),
            "descr" => description = scalar(items)?.to_owned(),
            "disabled" | "hidden" if items.len() == 1 => {
                if tag == "disabled" {
                    disabled = true;
                } else {
                    hidden = true;
                }
            }
            _ => return Err("unsupported library entry field".into()),
        }
    }
    Ok(LibraryTableEntry {
        nickname: nickname.ok_or("missing library nickname")?,
        format: format.ok_or("missing library format")?,
        uri: uri.ok_or("missing library URI")?,
        options,
        description,
        disabled,
        hidden,
    })
}

// Check depth before allocating the recursive CST. Quoted text does not change depth.
fn bound_nesting(bytes: &[u8]) -> Result<(), String> {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for &byte in bytes {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted {
            if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'(' => {
                    depth += 1;
                    if depth > MAX_TABLE_NESTING {
                        return Err("library table exceeds nesting limit".into());
                    }
                }
                b')' => depth = depth.checked_sub(1).ok_or("unbalanced table parentheses")?,
                _ => {}
            }
        }
    }
    if quoted || depth != 0 {
        return Err("unterminated library table form".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<FootprintLibraryTable, String> {
        parse_footprint_library_table(text.as_bytes())
    }

    #[test]
    fn retains_aliases_flags_formats_and_literal_uris_without_resolving() {
        let table = parse(
            r#"(fp_lib_table (version 7)
          (lib (name "Alias") (type "KiCad") (uri "${KIPRJMOD}/Other.pretty")
            (descr "quoted \"name\" (测试)") (hidden))
          (lib (name "Nested") (type "Table") (uri "/outside/fp-lib-table") (disabled))
          (lib (name "Foreign") (type "Eagle") (uri "https://example.invalid/lib")))"#,
        )
        .unwrap();
        assert_eq!(table.version, Some(7));
        assert_eq!(table.entries[0].uri, "${KIPRJMOD}/Other.pretty");
        assert_eq!(table.entries[0].description, "quoted \"name\" (测试)");
        assert!(table.entries[0].hidden);
        assert!(table.entries[1].disabled);
        assert_eq!(table.entries[1].format, LibraryFormat::Table);
        assert_eq!(
            table.entries[2].format,
            LibraryFormat::Unsupported("Eagle".into())
        );
        assert_eq!(parse("(fp_lib_table)").unwrap().version, None);
    }

    #[test]
    fn rejects_ambiguous_or_unmodelled_structure() {
        for input in [
            "(fp_lib_table (version 7) (version 7))",
            "(fp_lib_table (version 8))",
            "(fp_lib_table (unknown 1))",
            "(fp_lib_table (lib (name A) (type KiCad)))",
            "(fp_lib_table (lib (name A) (name B) (type KiCad) (uri /x)))",
            "(fp_lib_table (lib (name A) (type KiCad) (uri /x) (disabled false)))",
            "(fp_lib_table (lib (name A) (type KiCad) (uri /x)) (lib (name A) (type Table) (uri /y)))",
            "(fp_lib_table (lib (name A) (type KiCad) (uri /x extra)))",
            "(fp_lib_table) (fp_lib_table)",
            "(sym_lib_table)",
        ] {
            assert!(parse(input).is_err(), "accepted {input}");
        }
        for value in ["", ".", "..", "../part", "a/b", "a\\b", "a:b", "a\0b"] {
            assert!(validate_library_component(value).is_err());
        }
    }

    #[test]
    fn enforces_resource_bounds_before_recursive_parsing() {
        assert!(parse_footprint_library_table(&vec![b' '; MAX_TABLE_BYTES + 1]).is_err());
        assert!(parse(&format!("{}{}", "(".repeat(33), ")".repeat(33))).is_err());
        assert!(parse_footprint_library_table(&[255]).is_err());
        let entries = (0..=MAX_TABLE_ENTRIES)
            .map(|i| format!("(lib (name L{i}) (type KiCad) (uri /x))"))
            .collect::<String>();
        assert!(parse(&format!("(fp_lib_table {entries})")).is_err());
        assert!(read_footprint_library_table(Path::new("fp-lib-table")).is_err());
    }
}
