//! Canonical local footprint sources with bounded table inclusion and provenance.

use crate::library_table::{
    LibraryFormat, LibraryTableEntry, LibraryTableEvidence, MAX_TABLE_ENTRIES,
    read_footprint_library_table, validate_library_component,
};
use kicad_model::FootprintLibraryIdentifier;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_FOOTPRINT_SOURCE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_TABLE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TABLE_FILES: usize = 64;
const MAX_INCLUSION_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum LibraryScope {
    Project,
    Global,
    LegacyConfigured,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum LegacyMappingMode {
    ExplicitOverride,
    MissingNicknameFallback,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LegacyLibraryMapping {
    pub identifier: FootprintLibraryIdentifier,
    pub root: PathBuf,
    pub mode: LegacyMappingMode,
}

/// Server-owned inputs. MCP callers must never construct policy from request paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LibraryCatalogPolicy {
    project_root: PathBuf,
    global_table: PathBuf,
    official_root: Option<PathBuf>,
    extra_roots: Vec<PathBuf>,
    variables: BTreeMap<String, String>,
    legacy_mappings: Vec<LegacyLibraryMapping>,
}

impl LibraryCatalogPolicy {
    pub fn new(
        project: &Path,
        global_table: &Path,
        official_root: Option<&Path>,
        extra_roots: &[PathBuf],
        mut variables: BTreeMap<String, String>,
        mut legacy_mappings: Vec<LegacyLibraryMapping>,
    ) -> Result<Self, String> {
        let project_root = canonical_directory(project)?;
        if !global_table.is_absolute() || variables.contains_key("KIPRJMOD") {
            return Err("global table must be absolute; KIPRJMOD is server-derived".into());
        }
        for (key, value) in &variables {
            if !valid_variable(key) || !Path::new(value).is_absolute() || unsafe_path_text(value) {
                return Err("invalid operator library path variable".into());
            }
        }
        variables.insert("KIPRJMOD".into(), path_text(&project_root)?.into());
        let official_root = official_root.map(canonical_directory).transpose()?;
        let mut extra_roots = extra_roots
            .iter()
            .map(|p| canonical_directory(p))
            .collect::<Result<Vec<_>, _>>()?;
        extra_roots.sort();
        extra_roots.dedup();
        legacy_mappings.sort_by(|a, b| {
            (&a.identifier.nickname, &a.identifier.entry_name)
                .cmp(&(&b.identifier.nickname, &b.identifier.entry_name))
        });
        let mut legacy_ids = BTreeSet::new();
        for mapping in &legacy_mappings {
            validate_identifier(&mapping.identifier)?;
            if !mapping.root.is_absolute()
                || !legacy_ids
                    .insert((&mapping.identifier.nickname, &mapping.identifier.entry_name))
            {
                return Err("invalid or duplicate legacy library mapping".into());
            }
        }
        Ok(Self {
            project_root,
            global_table: global_table.to_path_buf(),
            official_root,
            extra_roots,
            variables,
            legacy_mappings,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FootprintResolutionEvidence {
    pub policy_version: u32,
    pub identifier: FootprintLibraryIdentifier,
    pub source_project: PathBuf,
    pub scope: LibraryScope,
    pub method: String,
    pub table_chain: Vec<LibraryTableEvidence>,
    pub canonical_library_root: PathBuf,
    pub canonical_source_path: PathBuf,
    pub source_size: u64,
    pub source_digest: String,
    pub catalog_digest: String,
    pub resolution_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CatalogEntry {
    row: LibraryTableEntry,
    scope: LibraryScope,
    chain: Vec<LibraryTableEvidence>,
}

#[derive(Clone, Debug)]
pub struct FootprintCatalog {
    policy: LibraryCatalogPolicy,
    entries: BTreeMap<String, CatalogEntry>,
    table_files: Vec<LibraryTableEvidence>,
    project_source_roots: Vec<PathBuf>,
    digest: String,
}

impl FootprintCatalog {
    pub fn load(policy: &LibraryCatalogPolicy) -> Result<Self, String> {
        let mut loader = TableLoader::new(policy);
        let global = loader.load_scope(&policy.global_table, LibraryScope::Global)?;
        let mut project_source_roots = vec![policy.project_root.clone()];
        project_source_roots.extend(policy.official_root.iter().cloned());
        project_source_roots.extend(policy.extra_roots.iter().cloned());
        // Only usable operator-owned native rows grant a project access to their roots.
        for entry in global.values() {
            if entry.row.format == LibraryFormat::KiCad
                && !entry.row.disabled
                && entry.row.options.is_empty()
                && let Ok(root) =
                    expand_uri(&entry.row.uri, &policy.variables).and_then(|p| canonical_pretty(&p))
            {
                project_source_roots.push(root);
            }
        }
        project_source_roots.sort();
        project_source_roots.dedup();
        let project = loader.load_scope(
            &policy.project_root.join("fp-lib-table"),
            LibraryScope::Project,
        )?;
        let mut entries = global;
        entries.extend(project);
        let table_files = loader.evidence;
        let digest = hash_json(&(1u32, policy, &table_files, &entries, &project_source_roots))?;
        Ok(Self {
            policy: policy.clone(),
            entries,
            table_files,
            project_source_roots,
            digest,
        })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn table_evidence(&self) -> &[LibraryTableEvidence] {
        &self.table_files
    }
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Resolve exact source bytes; callers must still verify native geometry and binding.
    pub fn resolve(
        &self,
        identifier: &FootprintLibraryIdentifier,
    ) -> Result<FootprintResolutionEvidence, String> {
        validate_identifier(identifier)?;
        self.ensure_current()?;
        let mapping = self.policy.legacy_mappings.iter().find(|m| {
            m.identifier == *identifier
                && (m.mode == LegacyMappingMode::ExplicitOverride
                    || !self.entries.contains_key(&identifier.nickname))
        });
        let (root, scope, chain, method) = if let Some(mapping) = mapping {
            (
                canonical_pretty(&mapping.root)?,
                LibraryScope::LegacyConfigured,
                Vec::new(),
                match mapping.mode {
                    LegacyMappingMode::ExplicitOverride => "explicit_server_mapping",
                    LegacyMappingMode::MissingNicknameFallback => "missing_nickname_fallback",
                },
            )
        } else {
            let entry = self
                .entries
                .get(&identifier.nickname)
                .ok_or("library nickname is unavailable")?;
            if entry.row.disabled {
                return Err("selected library or parent table is disabled".into());
            }
            if entry.row.format != LibraryFormat::KiCad {
                return Err("selected library format is unsupported".into());
            }
            if !entry.row.options.is_empty() {
                return Err("selected library plugin options are unsupported".into());
            }
            let root = canonical_pretty(&expand_uri(&entry.row.uri, &self.policy.variables)?)?;
            if entry.scope == LibraryScope::Project && !within(&root, &self.project_source_roots) {
                return Err("project library escapes approved canonical roots".into());
            }
            (root, entry.scope, entry.chain.clone(), "library_table")
        };
        let path = root.join(format!("{}.kicad_mod", identifier.entry_name));
        let link = fs::symlink_metadata(&path)
            .map_err(|e| format!("footprint source unavailable: {e}"))?;
        if !link.is_file() || link.file_type().is_symlink() {
            return Err("footprint source must be a regular non-symlink file".into());
        }
        let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        if !canonical.starts_with(&root)
            || canonical.extension().and_then(|s| s.to_str()) != Some("kicad_mod")
        {
            return Err("footprint source escaped its library root".into());
        }
        let (size, digest) = read_source_digest(&canonical)?;
        if fs::canonicalize(&path).map_err(|e| e.to_string())? != canonical
            || fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("footprint source path changed during resolution".into());
        }
        self.ensure_current()?;
        let mut evidence = FootprintResolutionEvidence {
            policy_version: 1,
            identifier: identifier.clone(),
            source_project: self.policy.project_root.clone(),
            scope,
            method: method.into(),
            table_chain: chain,
            canonical_library_root: root,
            canonical_source_path: canonical,
            source_size: size,
            source_digest: digest,
            catalog_digest: self.digest.clone(),
            resolution_digest: String::new(),
        };
        evidence.resolution_digest = hash_json(&evidence)?;
        Ok(evidence)
    }

    fn ensure_current(&self) -> Result<(), String> {
        if Self::load(&self.policy)?.digest != self.digest {
            return Err("library catalog changed; acquire fresh source evidence".into());
        }
        Ok(())
    }
}

struct TableLoader<'a> {
    policy: &'a LibraryCatalogPolicy,
    evidence: Vec<LibraryTableEvidence>,
    bytes: u64,
    rows: usize,
}

impl<'a> TableLoader<'a> {
    fn new(policy: &'a LibraryCatalogPolicy) -> Self {
        Self {
            policy,
            evidence: Vec::new(),
            bytes: 0,
            rows: 0,
        }
    }

    fn load_scope(
        &mut self,
        path: &Path,
        scope: LibraryScope,
    ) -> Result<BTreeMap<String, CatalogEntry>, String> {
        let mut entries = BTreeMap::new();
        self.visit(path, scope, &[], false, false, &mut entries)?;
        Ok(entries)
    }

    fn visit(
        &mut self,
        path: &Path,
        scope: LibraryScope,
        chain: &[LibraryTableEvidence],
        disabled: bool,
        hidden: bool,
        entries: &mut BTreeMap<String, CatalogEntry>,
    ) -> Result<(), String> {
        if chain.len() >= MAX_INCLUSION_DEPTH || self.evidence.len() >= MAX_TABLE_FILES {
            return Err("library table inclusion limit exceeded".into());
        }
        let canonical = match fs::canonicalize(path) {
            Ok(p) => Some(p),
            Err(error) if chain.is_empty() && error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("included library table is unavailable: {error}")),
        };
        if let Some(canonical) = &canonical {
            if scope == LibraryScope::Project
                && !canonical.starts_with(&self.policy.project_root)
                && !within(canonical, &self.policy.extra_roots)
            {
                return Err("project table escapes approved canonical roots".into());
            }
            if chain
                .iter()
                .any(|e| e.canonical_path.as_ref() == Some(canonical))
            {
                return Err("library table inclusion cycle".into());
            }
        }
        let file = read_footprint_library_table(path)?;
        if file.evidence.canonical_path != canonical {
            return Err("library table identity changed before reading".into());
        }
        self.bytes += file.evidence.size;
        if self.bytes > MAX_TOTAL_TABLE_BYTES {
            return Err("aggregate library table byte limit exceeded".into());
        }
        self.evidence.push(file.evidence.clone());
        let Some(table) = file.table else {
            return Ok(());
        };
        let mut chain = chain.to_vec();
        chain.push(file.evidence);
        for mut row in table.entries {
            self.rows += 1;
            if self.rows > MAX_TABLE_ENTRIES {
                return Err("aggregate library entry limit exceeded".into());
            }
            row.disabled |= disabled;
            row.hidden |= hidden;
            if row.format == LibraryFormat::Table {
                if !row.options.is_empty() {
                    return Err("nested library table options are unsupported".into());
                }
                // Read disabled tables only to reserve their names; never load their footprints.
                let next = expand_uri(&row.uri, &self.policy.variables)?;
                self.visit(&next, scope, &chain, row.disabled, row.hidden, entries)?;
            } else {
                let name = row.nickname.clone();
                if entries
                    .insert(
                        name,
                        CatalogEntry {
                            row,
                            scope,
                            chain: chain.clone(),
                        },
                    )
                    .is_some()
                {
                    return Err("ambiguous nickname across included library tables".into());
                }
            }
        }
        Ok(())
    }
}

fn valid_variable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}

fn unsafe_path_text(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() || matches!(c, '$' | '`'))
        || text.starts_with('~')
        || text.contains("://")
}

fn expand_uri(uri: &str, variables: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let mut output = String::new();
    let mut rest = uri;
    while let Some(index) = rest.find('$') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        let variable = rest
            .strip_prefix("${")
            .ok_or("only ${NAME} path variables are supported")?;
        let end = variable.find('}').ok_or("unterminated path variable")?;
        let name = &variable[..end];
        if !valid_variable(name) {
            return Err("invalid library path variable name".into());
        }
        output.push_str(variables.get(name).ok_or("unknown library path variable")?);
        rest = &variable[end + 1..];
        if output.len() > 16384 {
            return Err("expanded library path is too long".into());
        }
    }
    output.push_str(rest);
    if output.len() > 16384 || unsafe_path_text(&output) || !Path::new(&output).is_absolute() {
        return Err("library URI must expand to an absolute local path".into());
    }
    Ok(PathBuf::from(output))
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("library policy directories must be absolute".into());
    }
    let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !canonical.is_dir() {
        return Err("library policy path is not a directory".into());
    }
    Ok(canonical)
}

fn canonical_pretty(path: &Path) -> Result<PathBuf, String> {
    let root = canonical_directory(path)?;
    if root.extension().and_then(|s| s.to_str()) != Some("pretty") {
        return Err("native library root must be a .pretty directory".into());
    }
    Ok(root)
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}
fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "library path is not UTF-8".into())
}
fn validate_identifier(id: &FootprintLibraryIdentifier) -> Result<(), String> {
    validate_library_component(&id.nickname)?;
    validate_library_component(&id.entry_name)
}
fn hash_json(value: &impl Serialize) -> Result<String, String> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )))
}

fn read_source_digest(path: &Path) -> Result<(u64, String), String> {
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_FOOTPRINT_SOURCE_BYTES {
        return Err("footprint source exceeds regular-file size bounds".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.len() == 0 || opened.len() > MAX_FOOTPRINT_SOURCE_BYTES {
        return Err("opened footprint source exceeds bounds".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FOOTPRINT_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != opened.len() || bytes.len() as u64 > MAX_FOOTPRINT_SOURCE_BYTES {
        return Err("footprint source changed or exceeded read bounds".into());
    }
    Ok((bytes.len() as u64, hex::encode(Sha256::digest(bytes))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "auto-kicad-catalog-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let fixture = Self(fs::canonicalize(path).unwrap());
            fs::create_dir(fixture.0.join("project")).unwrap();
            fs::create_dir(fixture.0.join("official")).unwrap();
            fixture
        }
        fn library(&self, path: &str) -> PathBuf {
            let root = self.0.join(path);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("Part.kicad_mod"), "(footprint Part)").unwrap();
            root
        }
        fn table(&self, path: &str, rows: &str) {
            fs::write(
                self.0.join(path),
                format!("(fp_lib_table (version 7) {rows})"),
            )
            .unwrap();
        }
        fn policy(&self) -> LibraryCatalogPolicy {
            LibraryCatalogPolicy::new(
                &self.0.join("project"),
                &self.0.join("global"),
                Some(&self.0.join("official")),
                &[],
                BTreeMap::from([(
                    "KICAD10_FOOTPRINT_DIR".into(),
                    self.0.join("official").to_str().unwrap().into(),
                )]),
                vec![],
            )
            .unwrap()
        }
        fn catalog(&self) -> FootprintCatalog {
            FootprintCatalog::load(&self.policy()).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn id(name: &str) -> FootprintLibraryIdentifier {
        FootprintLibraryIdentifier {
            nickname: name.into(),
            entry_name: "Part".into(),
        }
    }
    fn row(name: &str, path: &Path) -> String {
        format!(
            "(lib (name {name}) (type KiCad) (uri {}))",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        )
    }
    fn table_row(name: &str, path: &Path, flags: &str) -> String {
        format!(
            "(lib (name {name}) (type Table) (uri {}) {flags})",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        )
    }

    #[test]
    fn binds_project_precedence_aliases_table_presence_and_source_changes() {
        let f = Fixture::new();
        let global = f.library("official/Different.pretty");
        let project = f.library("project/BoardParts.pretty");
        f.table("global", &row("Alias", &global));
        let before = f.catalog();
        let source = before.resolve(&id("Alias")).unwrap();
        assert_eq!(source.scope, LibraryScope::Global);
        assert_eq!(source.canonical_library_root, global);
        assert!(before.table_evidence().iter().any(|e| !e.present));
        f.table("project/fp-lib-table", &row("Alias", &project));
        assert!(
            before
                .resolve(&id("Alias"))
                .unwrap_err()
                .contains("catalog changed")
        );
        let after = f.catalog();
        let selected = after.resolve(&id("Alias")).unwrap();
        assert_eq!(selected.scope, LibraryScope::Project);
        assert_eq!(selected.source_digest, source.source_digest);
        assert_ne!(selected.resolution_digest, source.resolution_digest);
        fs::write(project.join("Part.kicad_mod"), "(footprint Changed)").unwrap();
        assert_ne!(
            selected.source_digest,
            after.resolve(&id("Alias")).unwrap().source_digest
        );
        assert_eq!(selected.table_chain.len(), 1);
    }

    #[test]
    fn nested_tables_preserve_disabled_names_and_reject_cycles_and_ambiguity() {
        let f = Fixture::new();
        let root = f.library("official/Native.pretty");
        f.table("nested", &row("Alias", &root));
        f.table(
            "global",
            &table_row("Bundle", &f.0.join("nested"), "(disabled)"),
        );
        assert!(
            f.catalog()
                .resolve(&id("Alias"))
                .unwrap_err()
                .contains("disabled")
        );
        f.table("global", &table_row("Bundle", &f.0.join("nested"), ""));
        assert_eq!(
            f.catalog().resolve(&id("Alias")).unwrap().table_chain.len(),
            2
        );
        f.table(
            "global",
            &(table_row("Bundle", &f.0.join("nested"), "") + &row("Alias", &root)),
        );
        assert!(
            FootprintCatalog::load(&f.policy())
                .unwrap_err()
                .contains("ambiguous")
        );
        f.table("nested", &table_row("Cycle", &f.0.join("global"), ""));
        assert!(
            FootprintCatalog::load(&f.policy())
                .unwrap_err()
                .contains("cycle")
        );
    }

    #[test]
    fn confines_project_sources_and_nested_tables_before_reading() {
        let f = Fixture::new();
        let outside = f.library("outside.pretty");
        f.table("project/fp-lib-table", &row("Outside", &outside));
        assert!(
            f.catalog()
                .resolve(&id("Outside"))
                .unwrap_err()
                .contains("escapes")
        );
        f.table("global", &row("Operator", &outside));
        assert_eq!(
            f.catalog()
                .resolve(&id("Outside"))
                .unwrap()
                .canonical_library_root,
            outside
        );
        f.table("nested", &row("Outside", &outside));
        f.table(
            "project/fp-lib-table",
            &table_row("OutsideTable", &f.0.join("nested"), ""),
        );
        assert!(
            FootprintCatalog::load(&f.policy())
                .unwrap_err()
                .contains("project table escapes")
        );
        let policy = LibraryCatalogPolicy::new(
            &f.0.join("project"),
            &f.0.join("global"),
            None,
            std::slice::from_ref(&f.0),
            BTreeMap::new(),
            vec![],
        )
        .unwrap();
        assert!(
            FootprintCatalog::load(&policy)
                .unwrap()
                .resolve(&id("Outside"))
                .is_ok()
        );
    }

    #[test]
    fn resolves_variables_without_shell_or_environment_fallback() {
        let f = Fixture::new();
        let root = f.library("project/Parts.pretty");
        f.table(
            "project/fp-lib-table",
            "(lib (name Alias) (type KiCad) (uri \"${KIPRJMOD}/Parts.pretty\"))",
        );
        assert_eq!(
            f.catalog()
                .resolve(&id("Alias"))
                .unwrap()
                .canonical_library_root,
            root
        );
        for uri in [
            "relative.pretty",
            "~/parts",
            "https://host/lib",
            "${UNKNOWN}/x",
            "$HOME/x",
            "$(command)",
            "/tmp/`command`",
            "${KIPRJMOD",
        ] {
            assert!(
                expand_uri(uri, &f.policy().variables).is_err(),
                "accepted {uri}"
            );
        }
        assert!(
            LibraryCatalogPolicy::new(
                &f.0.join("project"),
                &f.0.join("global"),
                None,
                &[],
                BTreeMap::from([("KIPRJMOD".into(), "/tmp".into())]),
                vec![]
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_fallback_cannot_bypass_a_selected_unusable_library() {
        let f = Fixture::new();
        let root = f.library("legacy.pretty");
        let mut policy = f.policy();
        policy.legacy_mappings.push(LegacyLibraryMapping {
            identifier: id("Alias"),
            root: root.clone(),
            mode: LegacyMappingMode::MissingNicknameFallback,
        });
        assert_eq!(
            FootprintCatalog::load(&policy)
                .unwrap()
                .resolve(&id("Alias"))
                .unwrap()
                .method,
            "missing_nickname_fallback"
        );
        f.table(
            "global",
            "(lib (name Alias) (type KiCad) (uri /missing.pretty) (disabled))",
        );
        assert!(
            FootprintCatalog::load(&policy)
                .unwrap()
                .resolve(&id("Alias"))
                .is_err()
        );
        policy.legacy_mappings[0].mode = LegacyMappingMode::ExplicitOverride;
        assert_eq!(
            FootprintCatalog::load(&policy)
                .unwrap()
                .resolve(&id("Alias"))
                .unwrap()
                .method,
            "explicit_server_mapping"
        );
    }

    #[test]
    fn bounds_graph_and_source_resources_without_partial_catalogs() {
        let f = Fixture::new();
        let root = f.library("official/Native.pretty");
        f.table("global", &row("Alias", &root));
        let file = File::options()
            .write(true)
            .open(root.join("Part.kicad_mod"))
            .unwrap();
        file.set_len(MAX_FOOTPRINT_SOURCE_BYTES + 1).unwrap();
        assert!(
            f.catalog()
                .resolve(&id("Alias"))
                .unwrap_err()
                .contains("bounds")
        );
        f.table("global", &table_row("D0", &f.0.join("nested0"), ""));
        for n in 0..MAX_INCLUSION_DEPTH {
            f.table(
                &format!("nested{n}"),
                &table_row(
                    &format!("D{}", n + 1),
                    &f.0.join(format!("nested{}", n + 1)),
                    "",
                ),
            );
        }
        assert!(
            FootprintCatalog::load(&f.policy())
                .unwrap_err()
                .contains("inclusion limit")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_symlinks_and_directory_aliases_outside_policy() {
        let f = Fixture::new();
        let root = f.library("project/Native.pretty");
        let outside = f.library("outside.pretty");
        f.table("project/fp-lib-table", &row("Alias", &root));
        fs::remove_file(root.join("Part.kicad_mod")).unwrap();
        std::os::unix::fs::symlink(outside.join("Part.kicad_mod"), root.join("Part.kicad_mod"))
            .unwrap();
        assert!(
            f.catalog()
                .resolve(&id("Alias"))
                .unwrap_err()
                .contains("non-symlink")
        );
        let linked = f.0.join("project/Link.pretty");
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        f.table("project/fp-lib-table", &row("Alias", &linked));
        assert!(
            f.catalog()
                .resolve(&id("Alias"))
                .unwrap_err()
                .contains("escapes")
        );
        fs::remove_file(linked).unwrap();
    }
}
