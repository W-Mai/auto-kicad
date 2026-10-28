//! Used footprint-file observations for closed schematic validation.
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SchematicFootprintState {
    MissingNickname,
    Disabled,
    MissingFile,
    Present,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchematicFootprintFile {
    pub(crate) identifier: FootprintLibraryIdentifier,
    pub(crate) state: SchematicFootprintState,
    pub(crate) path: Option<PathBuf>,
    pub(crate) size: u64,
    pub(crate) digest: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchematicFootprintContext {
    catalog_digest: String,
    files: Vec<SchematicFootprintFile>,
}
impl SchematicFootprintContext {
    pub fn files(&self) -> &[SchematicFootprintFile] {
        &self.files
    }
}
impl FootprintCatalog {
    /// Observe exact native table lookup without placement fallback mappings.
    pub fn schematic_files(
        &self,
        ids: &[FootprintLibraryIdentifier],
    ) -> Result<SchematicFootprintContext, String> {
        if !self.policy.legacy_mappings.is_empty() || ids.len() > MAX_TABLE_ENTRIES {
            return Err(
                "schematic footprint context rejects fallback mappings or excessive identifiers"
                    .into(),
            );
        }
        self.ensure_current()?;
        let mut unique = BTreeMap::new();
        for id in ids {
            validate_identifier(id)?;
            unique.insert((&id.nickname, &id.entry_name), id);
        }
        let mut files = Vec::new();
        let mut total = 0;
        for id in unique.values() {
            let mut file = SchematicFootprintFile {
                identifier: (*id).clone(),
                state: SchematicFootprintState::MissingNickname,
                path: None,
                size: 0,
                digest: None,
            };
            if let Some(entry) = self.entries.get(&id.nickname) {
                if entry.row.disabled {
                    file.state = SchematicFootprintState::Disabled;
                } else {
                    if entry.row.format != LibraryFormat::KiCad || !entry.row.options.is_empty() {
                        return Err("used schematic footprints require native KiCad libraries without plugin options".into());
                    }
                    let requested = expand_uri(&entry.row.uri, &self.policy.variables)?;
                    let root = native_drc::canonical_with_missing(&requested)?;
                    if root.extension().and_then(|s| s.to_str()) != Some("pretty") {
                        return Err("footprint library must be a .pretty directory".into());
                    }
                    if entry.scope == LibraryScope::Project
                        && !within(&root, &self.project_source_roots)
                    {
                        return Err("project footprint library escapes approved roots".into());
                    }
                    let path = root.join(format!("{}.kicad_mod", id.entry_name));
                    file.path = Some(path.clone());
                    match fs::symlink_metadata(&path) {
                        Ok(meta) => {
                            if !meta.is_file() || meta.file_type().is_symlink() {
                                return Err(
                                    "footprint source must be a regular non-symlink file".into()
                                );
                            }
                            let (size, digest) = read_source_digest(&path)?;
                            total += size;
                            if total > 128 * 1024 * 1024 {
                                return Err("used footprint source bytes exceed bound".into());
                            }
                            if fs::canonicalize(&path).map_err(|e| e.to_string())? != path {
                                return Err("footprint source path changed".into());
                            }
                            file.state = SchematicFootprintState::Present;
                            file.size = size;
                            file.digest = Some(digest);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            file.state = SchematicFootprintState::MissingFile
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
            }
            files.push(file);
        }
        self.ensure_current()?;
        Ok(SchematicFootprintContext {
            catalog_digest: self.digest.clone(),
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_lookup_preserves_missing_disabled_and_file_states() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        fs::write(root.join("global"), "(fp_lib_table (version 7))").unwrap();
        fs::write(root.join("fp-lib-table"), r#"(fp_lib_table (version 7)
            (lib (name "Enabled") (type "KiCad") (uri "${KIPRJMOD}/used.pretty") (options "") (descr ""))
            (lib (name "Disabled") (type "KiCad") (uri "${KIPRJMOD}/used.pretty") (options "") (descr "") (disabled)))"#).unwrap();
        let policy = LibraryCatalogPolicy::new(
            &root,
            &root.join("global"),
            None,
            &[],
            BTreeMap::new(),
            vec![],
        )
        .unwrap();
        let catalog = FootprintCatalog::load(&policy).unwrap();
        let ids = ["Absent", "Disabled", "Enabled"].map(|nickname| FootprintLibraryIdentifier {
            nickname: nickname.into(),
            entry_name: "Part".into(),
        });
        let missing = catalog.schematic_files(&ids).unwrap();
        assert_eq!(
            missing.files.iter().map(|f| f.state).collect::<Vec<_>>(),
            vec![
                SchematicFootprintState::MissingNickname,
                SchematicFootprintState::Disabled,
                SchematicFootprintState::MissingFile
            ]
        );
        fs::create_dir(root.join("used.pretty")).unwrap();
        fs::write(root.join("used.pretty/Part.kicad_mod"), "(footprint Part)").unwrap();
        let present = catalog.schematic_files(&ids).unwrap();
        assert_ne!(present, missing);
        assert_eq!(present.files[2].state, SchematicFootprintState::Present);
        fs::write(
            root.join("used.pretty/Part.kicad_mod"),
            "(footprint Part)\n",
        )
        .unwrap();
        assert_ne!(catalog.schematic_files(&ids).unwrap(), present);
        fs::write(root.join("fp-lib-table"), "(fp_lib_table (version 7))").unwrap();
        assert!(catalog.schematic_files(&ids).is_err());
    }
}
