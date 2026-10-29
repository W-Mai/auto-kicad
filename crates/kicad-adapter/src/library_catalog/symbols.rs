//! Immutable effective symbol tables and approved symbol-definition observations.
//!
//! Catalog resolution is source-bound: returned definitions carry the catalog and
//! source digests that were checked while reading the approved native library file.
use super::*;
use kiutils_sexpr::{Atom, Node, Span, parse_one};

const MAX_SYMBOL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_USED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SYMBOL_DEFINITIONS: usize = 4096;
const MAX_SYMBOL_NESTING: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum SymbolLibraryState {
    MissingNickname,
    Disabled,
    MissingFile,
    Present,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SymbolLibraryFile {
    nickname: String,
    state: SymbolLibraryState,
    path: Option<PathBuf>,
    size: u64,
    digest: Option<String>,
}
impl SymbolLibraryFile {
    pub fn nickname(&self) -> &str {
        &self.nickname
    }
    pub fn state(&self) -> &SymbolLibraryState {
        &self.state
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn digest(&self) -> Option<&str> {
        self.digest.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct SymbolCatalog {
    policy: LibraryCatalogPolicy,
    entries: BTreeMap<String, CatalogEntry>,
    tables: Vec<LibraryTableEvidence>,
    approved_files: BTreeSet<PathBuf>,
    digest: String,
}

/// Private construction prevents imported hashes from becoming trusted observations.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SymbolLibraryContext {
    catalog_digest: String,
    files: Vec<SymbolLibraryFile>,
    digest: String,
}
impl SymbolLibraryContext {
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn files(&self) -> &[SymbolLibraryFile] {
        &self.files
    }
}

/// A complete embedded symbol definition selected from an approved native library.
///
/// `definition` is copied losslessly from the source span, including nested units and
/// inheritance fields. The digests must be checked again before a write is published.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SymbolDefinitionEvidence {
    pub identifier: String,
    pub catalog_digest: String,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub definition: String,
    pub inheritance: Vec<String>,
}

impl SymbolDefinitionEvidence {
    pub fn catalog_digest(&self) -> &str {
        &self.catalog_digest
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn definition(&self) -> &str {
        &self.definition
    }
}

impl SymbolCatalog {
    pub fn load(policy: &LibraryCatalogPolicy) -> Result<Self, String> {
        if !policy.legacy_mappings.is_empty() {
            return Err("footprint fallback mappings cannot configure symbol libraries".into());
        }
        let mut loader = TableLoader::new(policy, TableKind::Symbol)?;
        let global = loader.load_scope(&policy.global_table, LibraryScope::Global)?;
        let mut approved_files = BTreeSet::new();
        for entry in global.values() {
            if entry.row.format == LibraryFormat::KiCad
                && !entry.row.disabled
                && entry.row.options.is_empty()
                && let Ok(path) =
                    expand_uri(&entry.row.uri, &policy.variables).and_then(|p| symbol_path(&p))
            {
                approved_files.insert(path);
            }
        }
        let project = loader.load_scope(
            &policy.project_root.join("sym-lib-table"),
            LibraryScope::Project,
        )?;
        let mut entries = global;
        entries.extend(project);
        let tables = loader.evidence;
        let digest = hash_json(&(
            "symbol-library-tables-v1",
            policy,
            &tables,
            &entries,
            &approved_files,
            &loader.overrides,
        ))?;
        Ok(Self {
            policy: policy.clone(),
            entries,
            tables,
            approved_files,
            digest,
        })
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub(crate) fn effective_rows(&self) -> Result<Vec<LibraryTableEntry>, String> {
        self.ensure_current()?;
        Ok(self.entries.values().map(|e| e.row.clone()).collect())
    }
    pub fn table_evidence(&self) -> &[LibraryTableEvidence] {
        &self.tables
    }

    /// Resolve one `nickname:entry` from the effective native symbol catalog.
    ///
    /// Only native KiCad rows with empty plugin options are accepted. The complete
    /// top-level `(symbol ...)` span is retained, and every direct `extends` target
    /// is required to exist in the same source file with cycle-free inheritance.
    pub fn resolve_definition(&self, identifier: &str) -> Result<SymbolDefinitionEvidence, String> {
        self.ensure_current()?;
        let (nickname, entry_name) = split_symbol_identifier(identifier)?;
        let entry = self
            .entries
            .get(nickname)
            .ok_or("symbol library nickname is unavailable")?;
        if entry.row.disabled {
            return Err("selected symbol library is disabled".into());
        }
        if entry.row.format != LibraryFormat::KiCad || !entry.row.options.is_empty() {
            return Err("selected symbol library is not a native KiCad file".into());
        }
        let requested = expand_uri(&entry.row.uri, &self.policy.variables)?;
        let canonical = symbol_path(&requested)?;
        let mut roots = vec![self.policy.project_root.clone()];
        roots.extend(self.policy.official_root.iter().cloned());
        roots.extend(self.policy.extra_roots.iter().cloned());
        if entry.scope == LibraryScope::Project
            && !within(&canonical, &roots)
            && !self.approved_files.contains(&canonical)
        {
            return Err(
                "project symbol library escapes approved canonical roots or exact global files"
                    .into(),
            );
        }
        let metadata = fs::symlink_metadata(&requested)
            .map_err(|e| format!("symbol source unavailable: {e}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("symbol source must be a regular non-symlink file".into());
        }
        let (size, source_digest) = read_bounded_source_digest(&canonical, MAX_SYMBOL_BYTES)?;
        if size > MAX_SYMBOL_BYTES
            || symbol_path(&requested)? != canonical
            || fs::symlink_metadata(&requested)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("symbol source path changed during reading".into());
        }
        let bytes = fs::read(&canonical).map_err(|e| e.to_string())?;
        if bytes.len() as u64 != size || hex::encode(sha2::Sha256::digest(&bytes)) != source_digest
        {
            return Err("symbol source changed during reading".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "symbol source is not UTF-8")?;
        validate_raw_nesting(text.as_bytes())?;
        let document =
            parse_one(text).map_err(|e| format!("invalid symbol library syntax: {e}"))?;
        validate_symbol_depth(&document.nodes, 0)?;
        let root = document
            .nodes
            .first()
            .ok_or("missing symbol library root")?;
        let root_items = list_items(root).ok_or("symbol library root is not a list")?;
        if atom_text(root_items.first()) != Some("kicad_symbol_lib") {
            return Err("expected kicad_symbol_lib root".into());
        }
        let mut definitions = BTreeMap::<String, (Span, Vec<String>)>::new();
        for node in &root_items[1..] {
            let Some(items) = list_items(node) else {
                continue;
            };
            match atom_text(items.first()) {
                Some("version" | "generator" | "generator_version") => continue,
                Some("symbol") => {}
                _ => return Err("unsupported symbol library top-level field".into()),
            }
            let name = atom_text(items.get(1)).ok_or("symbol definition is missing its name")?;
            validate_library_component(name)?;
            if definitions.contains_key(name) {
                return Err("duplicate symbol definition".into());
            }
            let inheritance = items[2..]
                .iter()
                .filter_map(|child| {
                    let fields = list_items(child)?;
                    (atom_text(fields.first()) == Some("extends"))
                        .then(|| atom_text(fields.get(1)).map(str::to_owned))
                        .flatten()
                })
                .collect::<Vec<_>>();
            if inheritance.len() > 1 {
                return Err("symbol definition has duplicate extends fields".into());
            }
            definitions.insert(name.to_owned(), (node_span(node), inheritance));
            if definitions.len() > MAX_SYMBOL_DEFINITIONS {
                return Err("symbol library definition count exceeds bound".into());
            }
        }
        let (span, inheritance) = definitions
            .get(entry_name)
            .cloned()
            .ok_or("symbol entry is missing from approved library")?;
        let mut state = BTreeMap::new();
        for name in definitions.keys() {
            check_inheritance(name, &definitions, &mut state)?;
        }
        let definition = text
            .get(span.start..span.end)
            .ok_or("invalid symbol definition span")?
            .to_owned();
        self.ensure_current()?;
        let (final_size, final_digest) = read_bounded_source_digest(&canonical, MAX_SYMBOL_BYTES)?;
        if final_size != size || final_digest != source_digest {
            return Err("symbol source changed during resolution".into());
        }
        Ok(SymbolDefinitionEvidence {
            identifier: identifier.to_owned(),
            catalog_digest: self.digest.clone(),
            source_path: canonical,
            source_digest,
            definition,
            inheritance,
        })
    }

    fn ensure_current(&self) -> Result<(), String> {
        if Self::load(&self.policy)?.digest != self.digest {
            return Err("symbol library tables changed; acquire fresh evidence".into());
        }
        Ok(())
    }

    fn observe(&self, nickname: &str) -> Result<SymbolLibraryFile, String> {
        validate_library_component(nickname)?;
        let mut result = SymbolLibraryFile {
            nickname: nickname.into(),
            state: SymbolLibraryState::MissingNickname,
            path: None,
            size: 0,
            digest: None,
        };
        let Some(entry) = self.entries.get(nickname) else {
            return Ok(result);
        };
        if entry.row.disabled {
            result.state = SymbolLibraryState::Disabled;
            return Ok(result);
        }
        if entry.row.format != LibraryFormat::KiCad || !entry.row.options.is_empty() {
            return Err(
                "used symbol libraries require native KiCad files without plugin options".into(),
            );
        }
        let requested = expand_uri(&entry.row.uri, &self.policy.variables)?;
        let canonical = symbol_path(&requested)?;
        let mut roots = vec![self.policy.project_root.clone()];
        roots.extend(self.policy.official_root.iter().cloned());
        roots.extend(self.policy.extra_roots.iter().cloned());
        if entry.scope == LibraryScope::Project
            && !within(&canonical, &roots)
            && !self.approved_files.contains(&canonical)
        {
            return Err(
                "project symbol library escapes approved canonical roots or exact global files"
                    .into(),
            );
        }
        result.path = Some(canonical.clone());
        match fs::symlink_metadata(&requested) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err("symbol source must be a regular non-symlink file".into());
                }
                let (size, digest) = read_bounded_source_digest(&canonical, MAX_SYMBOL_BYTES)?;
                if symbol_path(&requested)? != canonical
                    || fs::symlink_metadata(&requested)
                        .map_err(|e| e.to_string())?
                        .file_type()
                        .is_symlink()
                {
                    return Err("symbol source path changed during reading".into());
                }
                result.state = SymbolLibraryState::Present;
                result.size = size;
                result.digest = Some(digest);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                result.state = SymbolLibraryState::MissingFile
            }
            Err(e) => return Err(format!("cannot read symbol source: {e}")),
        }
        Ok(result)
    }

    pub fn capture(&self, nicknames: &[String]) -> Result<SymbolLibraryContext, String> {
        if nicknames.len() > MAX_TABLE_ENTRIES {
            return Err("used symbol library count exceeds bound".into());
        }
        self.ensure_current()?;
        let names = nicknames.iter().collect::<BTreeSet<_>>();
        let mut files = Vec::new();
        let mut total = 0;
        for nickname in names {
            let file = self.observe(nickname)?;
            total += file.size;
            if total > MAX_USED_BYTES {
                return Err("used symbol library bytes exceed bound".into());
            }
            files.push(file);
        }
        self.ensure_current()?;
        // Re-read complete used files so mixed observations cannot pass a visible drift check.
        for file in &files {
            if self.observe(&file.nickname)? != *file {
                return Err("symbol source changed during capture".into());
            }
        }
        self.ensure_current()?;
        let digest = hash_json(&("symbol-library-context-v1", &self.digest, &files))?;
        Ok(SymbolLibraryContext {
            catalog_digest: self.digest.clone(),
            files,
            digest,
        })
    }

    pub fn verify(&self, context: &SymbolLibraryContext) -> Result<(), String> {
        let names = context
            .files
            .iter()
            .map(|f| f.nickname.clone())
            .collect::<Vec<_>>();
        if self.capture(&names)? != *context {
            return Err("used symbol library context changed".into());
        }
        Ok(())
    }
}

fn split_symbol_identifier(identifier: &str) -> Result<(&str, &str), String> {
    let mut parts = identifier.split(':');
    let nickname = parts
        .next()
        .ok_or("symbol identifier is missing nickname")?;
    let entry = parts.next().ok_or("symbol identifier is missing entry")?;
    if parts.next().is_some() {
        return Err("symbol identifier must contain exactly one colon".into());
    }
    validate_library_component(nickname)?;
    validate_library_component(entry)?;
    Ok((nickname, entry))
}

fn list_items(node: &Node) -> Option<&[Node]> {
    match node {
        Node::List { items, .. } => Some(items),
        Node::Atom { .. } => None,
    }
}

fn atom_text(node: Option<&Node>) -> Option<&str> {
    match node {
        Some(Node::Atom {
            atom: Atom::Symbol(value) | Atom::Quoted(value),
            ..
        }) => Some(value),
        _ => None,
    }
}

fn node_span(node: &Node) -> Span {
    match node {
        Node::List { span, .. } | Node::Atom { span, .. } => *span,
    }
}

fn validate_symbol_depth(nodes: &[Node], depth: usize) -> Result<(), String> {
    if depth > MAX_SYMBOL_NESTING {
        return Err("symbol library nesting exceeds bound".into());
    }
    for node in nodes {
        if let Some(items) = list_items(node) {
            validate_symbol_depth(items, depth + 1)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_raw_nesting(bytes: &[u8]) -> Result<(), String> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'(' => {
                depth = depth.checked_add(1).ok_or("symbol nesting overflow")?;
                if depth > MAX_SYMBOL_NESTING {
                    return Err("symbol library nesting exceeds bound".into());
                }
            }
            b')' => depth = depth.checked_sub(1).ok_or("unbalanced symbol library")?,
            _ => {}
        }
    }
    if quoted || escaped || depth != 0 {
        return Err("unterminated or unbalanced symbol library".into());
    }
    Ok(())
}

fn check_inheritance(
    name: &str,
    definitions: &BTreeMap<String, (Span, Vec<String>)>,
    state: &mut BTreeMap<String, u8>,
) -> Result<(), String> {
    match state.get(name).copied() {
        Some(2) => return Ok(()),
        Some(1) => return Err("symbol inheritance cycle".into()),
        _ => {}
    }
    state.insert(name.to_owned(), 1);
    if let Some(parent) = definitions
        .get(name)
        .and_then(|(_, parents)| parents.first())
    {
        if !definitions.contains_key(parent) {
            return Err(format!("symbol inheritance target `{parent}` is missing"));
        }
        check_inheritance(parent, definitions, state)?;
    }
    state.insert(name.to_owned(), 2);
    Ok(())
}

fn symbol_path(path: &Path) -> Result<PathBuf, String> {
    if path.extension().and_then(|s| s.to_str()) != Some("kicad_sym") {
        return Err("native symbol library must be a .kicad_sym file".into());
    }
    let canonical = native_drc::canonical_with_missing(path)?;
    if canonical.extension().and_then(|s| s.to_str()) != Some("kicad_sym") {
        return Err("symbol library target has unsupported extension".into());
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(name: &str, path: &Path) -> String {
        format!(
            "(lib (name {name}) (type KiCad) (uri {}))",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        )
    }
    fn table(path: &Path, body: &str) {
        fs::write(path, format!("(sym_lib_table (version 7) {body})")).unwrap();
    }
    fn policy(root: &Path) -> LibraryCatalogPolicy {
        LibraryCatalogPolicy::new(
            &root.join("project"),
            &root.join("global"),
            None,
            &[],
            BTreeMap::new(),
            vec![],
        )
        .unwrap()
    }
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Fixture, PathBuf) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "auto-kicad-symbol-context-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let r = fs::canonicalize(&path).unwrap();
        let d = Fixture(r.clone());
        fs::create_dir(r.join("project")).unwrap();
        (d, r)
    }
    #[test]
    fn complete_tables_and_used_content_or_absence_revoke_context() {
        let (_d, r) = fixture();
        let file = r.join("project/Used.kicad_sym");
        fs::write(&file, "(kicad_symbol_lib)").unwrap();
        table(
            &r.join("project/sym-lib-table"),
            &(row("Used", &file) + &row("Missing", &r.join("project/Missing.kicad_sym"))),
        );
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        let names = vec!["Used".into(), "Missing".into(), "Absent".into()];
        let context = catalog.capture(&names).unwrap();
        assert_eq!(context.files[0].state, SymbolLibraryState::MissingNickname);
        assert_eq!(context.files[1].state, SymbolLibraryState::MissingFile);
        assert_eq!(context.files[2].state, SymbolLibraryState::Present);
        catalog.verify(&context).unwrap();
        fs::write(&file, "(kicad_symbol_lib )").unwrap();
        assert!(catalog.verify(&context).is_err());
        fs::write(&file, "(kicad_symbol_lib)").unwrap();
        catalog.verify(&context).unwrap();
        fs::write(r.join("project/Missing.kicad_sym"), "(kicad_symbol_lib)").unwrap();
        assert!(catalog.verify(&context).is_err());
        fs::remove_file(r.join("project/Missing.kicad_sym")).unwrap();
        table(&r.join("global"), "");
        assert!(catalog.verify(&context).is_err());
    }

    #[test]
    fn resolve_definition_binds_complete_source_and_inheritance() {
        let (_d, r) = fixture();
        let file = r.join("project/Used.kicad_sym");
        fs::write(
            &file,
            r#"(kicad_symbol_lib (version 20231120) (generator kicad_symbol_editor)
  (symbol "Base"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1 1))))
    (symbol "Base_1_1" (pin passive line (at 0 0 0) (name "") (number "1"))))
  (symbol "Derived" (extends "Base")
    (symbol "Derived_1_1" (pin passive line (at 0 0 0) (name "") (number "1"))))
)"#,
        )
        .unwrap();
        table(&r.join("project/sym-lib-table"), &row("Used", &file));
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        let evidence = catalog.resolve_definition("Used:Derived").unwrap();
        assert_eq!(evidence.identifier, "Used:Derived");
        assert_eq!(evidence.inheritance, vec!["Base"]);
        assert!(evidence.definition.contains("(extends \"Base\")"));
        assert_eq!(evidence.source_path, fs::canonicalize(file).unwrap());
        assert_eq!(evidence.source_digest.len(), 64);
        assert_eq!(evidence.catalog_digest, catalog.digest());
    }

    #[test]
    fn resolve_definition_rejects_missing_and_cyclic_inheritance() {
        let (_d, r) = fixture();
        let file = r.join("project/Used.kicad_sym");
        table(&r.join("project/sym-lib-table"), &row("Used", &file));
        fs::write(
            &file,
            r#"(kicad_symbol_lib (symbol "A" (extends "B")) (symbol "B" (extends "A")))"#,
        )
        .unwrap();
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        assert!(catalog.resolve_definition("Used:A").is_err());
        fs::write(
            &file,
            r#"(kicad_symbol_lib (symbol "A" (extends "Missing")))"#,
        )
        .unwrap();
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        assert!(catalog.resolve_definition("Used:A").is_err());
        assert!(catalog.resolve_definition("Used").is_err());
        assert!(catalog.resolve_definition("Used:A:extra").is_err());
    }
    #[test]
    fn nested_symbols_override_global_without_granting_sibling_files() {
        let (_d, r) = fixture();
        let external = r.join("Trusted.kicad_sym");
        fs::write(&external, "(kicad_symbol_lib)").unwrap();
        let nested = r.join("nested");
        table(&nested, &row("Trusted", &external));
        table(
            &r.join("global"),
            &format!(
                "(lib (name Nested) (type Table) (uri {}))",
                serde_json::to_string(nested.to_str().unwrap()).unwrap()
            ),
        );
        table(
            &r.join("project/sym-lib-table"),
            &(row("Alias", &external) + &row("Escape", &r.join("Other.kicad_sym"))),
        );
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        assert_eq!(catalog.table_evidence().len(), 3);
        assert!(catalog.capture(&["Alias".into()]).is_ok());
        assert!(catalog.capture(&["Escape".into()]).is_err());
        table(
            &r.join("project/sym-lib-table"),
            "(lib (name Trusted) (type KiCad) (uri /missing.kicad_sym) (disabled))",
        );
        let catalog = SymbolCatalog::load(&policy(&r)).unwrap();
        assert_eq!(
            catalog.capture(&["Trusted".into()]).unwrap().files[0].state,
            SymbolLibraryState::Disabled
        );
    }
    #[cfg(unix)]
    #[test]
    fn unsupported_tables_plugins_links_and_source_bounds_reject() {
        let (_d, r) = fixture();
        fs::write(r.join("global"), "(fp_lib_table)").unwrap();
        assert!(SymbolCatalog::load(&policy(&r)).is_err());
        table(&r.join("global"), "");
        let file = r.join("project/Test.kicad_sym");
        table(&r.join("project/sym-lib-table"), &row("Test", &file));
        std::os::unix::fs::symlink(r.join("missing.kicad_sym"), &file).unwrap();
        assert!(
            SymbolCatalog::load(&policy(&r))
                .unwrap()
                .capture(&["Test".into()])
                .is_err()
        );
        fs::remove_file(&file).unwrap();
        File::create(&file)
            .unwrap()
            .set_len(MAX_SYMBOL_BYTES + 1)
            .unwrap();
        assert!(
            SymbolCatalog::load(&policy(&r))
                .unwrap()
                .capture(&["Test".into()])
                .is_err()
        );
        table(
            &r.join("project/sym-lib-table"),
            "(lib (name Test) (type Legacy) (uri /x.lib))",
        );
        assert!(
            SymbolCatalog::load(&policy(&r))
                .unwrap()
                .capture(&["Test".into()])
                .is_err()
        );
    }
}
