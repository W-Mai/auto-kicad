//! Complete effective table and used-file evidence for private native DRC.
use super::*;
use kicad_ipc::{KiCadClient, KiCadTransport};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct NativeDrcSource {
    identifier: FootprintLibraryIdentifier,
    state: String,
    path: Option<PathBuf>,
    size: u64,
    digest: Option<String>,
}

/// Immutable source observations; source/preview equivalence retains every used file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeDrcLibraries {
    catalog_digest: String,
    instance: String,
    filename: String,
    project_variable: String,
    table_digest: String,
    sources: Vec<NativeDrcSource>,
    #[serde(skip)]
    table: String,
}

impl NativeDrcLibraries {
    pub fn table(&self) -> &str {
        &self.table
    }
    pub fn project_variable(&self) -> &str {
        &self.project_variable
    }
    fn permits_file(&self, id: &FootprintLibraryIdentifier, path: &Path) -> bool {
        self.sources
            .iter()
            .any(|s| &s.identifier == id && s.path.as_deref() == Some(path))
    }
    pub fn equivalent_to(&self, other: &Self) -> bool {
        self.table_digest == other.table_digest && self.sources == other.sources
    }
}

impl FootprintCatalog {
    /// Bind native-expanded library paths and exact source files without placement fallbacks.
    pub fn native_drc_libraries<T: KiCadTransport>(
        &self,
        client: &mut KiCadClient<T>,
        filename: &str,
        identifiers: &[FootprintLibraryIdentifier],
        reference: Option<&NativeDrcLibraries>,
    ) -> Result<NativeDrcLibraries, String> {
        self.ensure_current()?;
        if identifiers.len() > MAX_TABLE_ENTRIES {
            return Err("DRC source count exceeds bounds".into());
        }
        let instance = client
            .instance_token()
            .ok_or("DRC libraries require a bound editor")?
            .as_str()
            .to_owned();
        let mut ids = BTreeMap::new();
        for id in identifiers {
            if !id.nickname.is_empty() {
                validate_identifier(id)?;
            } else if id.entry_name.len() > 512 {
                return Err("unlinked footprint name exceeds bounds".into());
            }
            ids.insert((id.nickname.clone(), id.entry_name.clone()), id.clone());
        }
        let mut queries = vec!["$(KIPRJMOD)".to_owned()];
        let mut query_indices = BTreeMap::new();
        for id in ids.values() {
            if let Some(entry) = self.entries.get(&id.nickname)
                && !entry.row.disabled
                && !query_indices.contains_key(&id.nickname)
            {
                if entry.row.format != LibraryFormat::KiCad || !entry.row.options.is_empty() {
                    return Err(
                        "used DRC libraries require native KiCad files without plugin options"
                            .into(),
                    );
                }
                query_indices.insert(id.nickname.clone(), queries.len());
                queries.push(entry.row.uri.clone());
            }
        }
        let expand = |client: &mut KiCadClient<T>| -> Result<Vec<String>, String> {
            let mut values = Vec::new();
            for chunk in queries.chunks(64) {
                values.extend(
                    client
                        .expand_environment_paths(filename, chunk)
                        .map_err(|e| e.to_string())?,
                );
            }
            Ok(values)
        };
        let expanded = expand(client)?;
        let project_variable = expanded
            .first()
            .ok_or("missing native project path")?
            .clone();
        absolute_native_path(&project_variable)?;
        let mut sources = Vec::new();
        let mut total = 0;
        for id in ids.into_values() {
            let mut source = NativeDrcSource {
                identifier: id.clone(),
                state: "unlinked".into(),
                path: None,
                size: 0,
                digest: None,
            };
            if !id.nickname.is_empty() {
                match self.entries.get(&id.nickname) {
                    None => source.state = "missing_nickname".into(),
                    Some(entry) if entry.row.disabled => source.state = "disabled".into(),
                    Some(entry) => {
                        let uri = &expanded[query_indices[&id.nickname]];
                        let raw_root = absolute_native_path(uri)?;
                        if raw_root.extension().and_then(|s| s.to_str()) != Some("pretty") {
                            return Err(
                                "native DRC library must be a local .pretty directory".into()
                            );
                        }
                        let root = canonical_with_missing(&raw_root)?;
                        let path = root.join(format!("{}.kicad_mod", id.entry_name));
                        if entry.scope == LibraryScope::Project
                            && !within(&root, &self.project_source_roots)
                            && !reference.is_some_and(|r| r.permits_file(&id, &path))
                        {
                            return Err(
                                "native DRC library escaped approved canonical roots".into()
                            );
                        }
                        source.path = Some(path.clone());
                        match fs::symlink_metadata(&path) {
                            Ok(meta) => {
                                if !meta.is_file() || meta.file_type().is_symlink() {
                                    return Err(
                                        "DRC library source must be a regular non-symlink file"
                                            .into(),
                                    );
                                }
                                let (size, digest) = read_source_digest(&path)?;
                                total += size;
                                if total > 128 * 1024 * 1024 {
                                    return Err("DRC library source bytes exceed bounds".into());
                                }
                                if fs::canonicalize(&path).map_err(|e| e.to_string())? != path {
                                    return Err("DRC source path changed during reading".into());
                                }
                                source.size = size;
                                source.digest = Some(digest);
                                source.state = "present".into();
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                source.state = "missing_file".into()
                            }
                            Err(e) => return Err(format!("cannot read DRC library source: {e}")),
                        }
                    }
                }
            }
            sources.push(source);
        }
        let mut rows = Vec::new();
        for entry in self.entries.values() {
            let mut row = entry.row.clone();
            if let Some(index) = query_indices.get(&row.nickname) {
                row.uri = expanded[*index].clone();
            } else {
                row.uri = row
                    .uri
                    .replace("${KIPRJMOD}", &project_variable)
                    .replace("$(KIPRJMOD)", &project_variable);
            }
            rows.push(row);
        }
        let table = crate::library_table::render_footprint_library_rows(&rows)?;
        if expand(client)? != expanded {
            return Err("native DRC library environment changed".into());
        }
        self.ensure_current()?;
        let result = NativeDrcLibraries {
            catalog_digest: self.digest.clone(),
            instance,
            filename: filename.into(),
            project_variable,
            table_digest: hash_json(&table)?,
            sources,
            table,
        };
        if reference.is_some_and(|r| !r.equivalent_to(&result)) {
            return Err("native DRC library context differs from verified source".into());
        }
        Ok(result)
    }
}

fn absolute_native_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || unsafe_path_text(value)
        || value.contains('@')
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("native DRC path is not an expanded absolute local path".into());
    }
    Ok(path)
}

pub(super) fn canonical_with_missing(path: &Path) -> Result<PathBuf, String> {
    let mut suffix = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(mut root) => {
                for name in suffix.iter().rev() {
                    root.push(name);
                }
                return Ok(root);
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && fs::symlink_metadata(current).is_err() =>
            {
                suffix.push(
                    current
                        .file_name()
                        .ok_or("missing path component")?
                        .to_owned(),
                );
                current = current.parent().ok_or("missing path ancestor")?;
            }
            Err(e) => return Err(format!("cannot resolve native DRC library root: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_capability_binds_exact_files_and_complete_source_state() {
        let id = FootprintLibraryIdentifier {
            nickname: "Local".into(),
            entry_name: "Part".into(),
        };
        let path = PathBuf::from("/approved/Local.pretty/Part.kicad_mod");
        let source = NativeDrcSource {
            identifier: id.clone(),
            state: "present".into(),
            path: Some(path.clone()),
            size: 17,
            digest: Some("content".into()),
        };
        let reference = NativeDrcLibraries {
            catalog_digest: "catalog".into(),
            instance: "editor".into(),
            filename: "board".into(),
            project_variable: "/approved".into(),
            table_digest: "table".into(),
            sources: vec![source],
            table: "table".into(),
        };
        assert!(reference.permits_file(&id, &path));
        assert!(!reference.permits_file(&id, &path.with_file_name("Other.kicad_mod")));
        assert!(!reference.permits_file(
            &FootprintLibraryIdentifier {
                nickname: "Other".into(),
                entry_name: "Part".into()
            },
            &path
        ));
        for mode in 0..5 {
            let mut changed = reference.clone();
            match mode {
                0 => changed.sources[0].digest = Some("changed".into()),
                1 => changed.sources[0].size += 1,
                2 => changed.sources[0].state = "missing_file".into(),
                3 => changed.sources[0].path = Some(path.with_file_name("Other.kicad_mod")),
                _ => changed.table_digest = "other".into(),
            }
            assert!(!reference.equivalent_to(&changed));
        }
    }
}
