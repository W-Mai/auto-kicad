//! Original child/source absence capture for native schematic previews.
use super::*;
use kicad_schematic::editing::{SheetChild, validate_child_file};

#[derive(Default)]
pub(super) struct ChildInputs {
    pub inputs: Inputs,
    pub schematics: BTreeSet<String>,
    directories: BTreeMap<PathBuf, DirectoryIdentity>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
struct DirectoryIdentity {
    canonical: PathBuf,
    device: u64,
    inode: u64,
}
impl DirectoryIdentity {
    fn read(path: &Path) -> Result<Self, String> {
        let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if canonical != path || !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("child parent must be an existing canonical directory".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                canonical,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            Err("native child authoring requires Unix directory identity".into())
        }
    }
}
fn lock(path: &Path) -> Result<PathBuf, String> {
    Ok(path.with_file_name(format!(
        "~{}.lck",
        path.file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid child name")?
    )))
}
fn child_path(audit: &SchematicAudit, relative: &str) -> Result<PathBuf, String> {
    validate_child_file(relative)?;
    Ok(audit.inner.policy.project.join(relative))
}
impl ChildInputs {
    fn directory(&mut self, path: &Path) -> Result<(), String> {
        let identity = DirectoryIdentity::read(path)?;
        if self
            .directories
            .get(path)
            .is_some_and(|old| old != &identity)
        {
            return Err("child parent identity changed".into());
        }
        self.directories.insert(path.into(), identity);
        Ok(())
    }
    fn track<'a>(
        &'a mut self,
        audit: &'a SchematicAudit,
        path: &Path,
    ) -> Result<&'a InputFile, String> {
        if let Some(input) = audit.inner.source.files.get(path) {
            Ok(input)
        } else {
            self.inputs.track(path)
        }
    }
    fn destination(&mut self, audit: &SchematicAudit, relative: &str) -> Result<(), String> {
        let path = child_path(audit, relative)?;
        self.directory(path.parent().ok_or("missing child parent")?)?;
        if self.track(audit, &path)?.bytes.is_some() {
            return Err("new child destination is not absent".into());
        }
        if self.track(audit, &lock(&path)?)?.bytes.is_some() {
            return Err("child editor lock present".into());
        }
        Ok(())
    }
    fn existing(
        &mut self,
        audit: &SchematicAudit,
        relative: &str,
    ) -> Result<ParsedSchematicProject, String> {
        let path = child_path(audit, relative)?;
        if fs::canonicalize(&path).map_err(|e| e.to_string())? != path {
            return Err("attached root must be canonical".into());
        }
        let project = load_schematic_project(
            &audit.inner.policy.project,
            &path,
            &audit.inner.parsed.revision.kicad_cli_version,
            audit.inner.parsed.revision.extractor_schema_version,
            Limits::default(),
        )
        .map_err(|e| e.to_string())?;
        if project.editor_lock_present {
            return Err("attached child editor lock present".into());
        }
        kicad_schematic::identities::source_instances(&project).map_err(|e| e.to_string())?;
        for file in &project.files {
            let path = audit
                .inner
                .policy
                .project
                .join(&file.artifact.relative_path);
            self.directory(path.parent().ok_or("missing attached parent")?)?;
            let input = self.track(audit, &path)?;
            if input.canonical.as_ref() != Some(&path)
                || input.digest.as_deref() != Some(file.artifact.content_digest.as_str())
                || input.size != file.artifact.size
            {
                return Err("child changed during capture".into());
            }
            if self.track(audit, &lock(&path)?)?.bytes.is_some() {
                return Err("attached child lock present".into());
            }
            if !audit.inner.source.files.contains_key(&path) {
                self.schematics.insert(file.artifact.relative_path.clone());
            }
        }
        self.budget(audit)?;
        Ok(project)
    }
    fn budget(&self, audit: &SchematicAudit) -> Result<(), String> {
        if self.inputs.total + audit.inner.source.total > MAX_TOTAL
            || self.inputs.files.len() + audit.inner.source.files.len() > 8192
            || self.schematics.len() + audit.inner.parsed.files.len() > 128
        {
            return Err("combined child/source capture bound exceeded".into());
        }
        Ok(())
    }
    pub fn capture(audit: &SchematicAudit, edits: &[SchematicEdit]) -> Result<Self, String> {
        let mut result = Self::default();
        let mut destinations = BTreeSet::new();
        for edit in edits {
            let child = match &edit.action {
                EditAction::CreateSheet { sheet, .. } => &sheet.child,
                EditAction::RetargetSheet { target, .. } => &target.child,
                _ => continue,
            };
            match child {
                SheetChild::CreateEmpty { file } => {
                    if !destinations.insert(file) {
                        return Err("repeated child creation destination".into());
                    }
                    result.destination(audit, file)?;
                }
                SheetChild::AttachExisting {
                    file,
                    source_revision,
                } => {
                    let captured = result.existing(audit, file)?;
                    if format!("sha256:{}", captured.revision.digest.as_str()) != *source_revision {
                        return Err("attached child differs from inspected source revision".into());
                    }
                }
            }
        }
        result.budget(audit)?;
        result.verify()?;
        Ok(result)
    }
    pub fn verify(&self) -> Result<(), String> {
        self.inputs.verify()?;
        self.verify_directories()
    }
    pub fn verify_directories(&self) -> Result<(), String> {
        for (path, expected) in &self.directories {
            if &DirectoryIdentity::read(path)? != expected {
                return Err("captured child parent identity changed".into());
            }
        }
        Ok(())
    }
    pub fn evidence(&self) -> Value {
        serde_json::json!({"inputs":self.inputs,"directories":self.directories,"schematics":self.schematics})
    }
    pub fn capture_resources(
        &mut self,
        baseline: &SchematicAudit,
        observed: &SchematicAudit,
        candidate_project: &Path,
    ) -> Result<(), String> {
        for (path, expected) in &observed.inner.source.files {
            if path.starts_with(candidate_project)
                || path.starts_with(observed.private_root())
                || baseline.inner.source.files.contains_key(path)
            {
                continue;
            }
            let actual = self.inputs.track(path)?;
            if actual.canonical != expected.canonical
                || actual.digest != expected.digest
                || actual.size != expected.size
            {
                return Err("new child resource changed during capture".into());
            }
        }
        self.budget(baseline)?;
        self.verify()
    }
}

impl SchematicAudit {
    /// Read a confined detached child graph; this grants no publication authority.
    pub fn inspect_child(&self, file: &str) -> Result<Value, String> {
        self.verify()?;
        let mut captured = ChildInputs::default();
        let child = captured.existing(self, file)?;
        let root = child
            .files
            .iter()
            .find(|f| f.artifact.relative_path == file)
            .ok_or("missing child root")?;
        let prefix = format!("/{}", root.file_uuid);
        let sheets = kicad_schematic::identities::source_instances(&child).map_err(|e| e.to_string())?.iter().map(|sheet| {
            let relative = sheet.path.strip_prefix(&prefix).ok_or("invalid child path")?;
            let source = child.files.iter().find(|f| f.artifact.relative_path == sheet.file).ok_or("missing child source")?;
            let symbols = source.symbols.iter().map(|symbol| serde_json::json!({"uuid":symbol.source_uuid,"reference":symbol.reference,"lib_id":symbol.lib_id,"value":symbol.value,
                "footprint":symbol.footprint,"unit":symbol.unit,"in_bom":symbol.in_bom,"on_board":symbol.on_board})).collect::<Vec<_>>();
            Ok(serde_json::json!({"file":sheet.file,"relative_path":if relative.is_empty(){"/"}else{relative},"symbols":symbols}))
        }).collect::<Result<Vec<_>, String>>()?;
        self.verify()?;
        captured.verify()?;
        Ok(
            serde_json::json!({"file":file,"source_revision":format!("sha256:{}",child.revision.digest.as_str()),"sheets":sheets,"source_context":captured.evidence(),"publication_authorized":false}),
        )
    }
    fn configured_pages(&self) -> Result<BTreeMap<String, Option<String>>, String> {
        let sources = self
            .inner
            .parsed
            .files
            .iter()
            .map(|file| {
                let bytes = self
                    .inner
                    .source
                    .files
                    .get(&self.inner.policy.project.join(&file.artifact.relative_path))
                    .and_then(|input| input.bytes.as_deref())
                    .ok_or("missing page source")?;
                Ok((file.artifact.relative_path.clone(), bytes))
            })
            .collect::<Result<_, String>>()?;
        kicad_schematic::editing::configured_sheet_pages(&self.inner.parsed, &sources)
    }
    pub fn sheet_inventory(&self) -> Result<Vec<Value>, String> {
        let root = self
            .inner
            .parsed
            .files
            .iter()
            .find(|f| f.artifact.relative_path == self.inner.parsed.root_relative_path)
            .ok_or("missing root")?;
        let configured = self.configured_pages()?;
        self.identities().sheets().iter().map(|sheet| {
            let native = self.netlist().sheets.iter().find(|native| kicad_schematic::normalize_netlist_instance_path(&native.uuid_path,&root.file_uuid).as_deref()==Some(sheet.path.as_str())).ok_or("missing native ordinal")?;
            Ok(serde_json::json!({"file":sheet.file,"sheet_path":sheet.path,"page":configured[&sheet.path],"native_ordinal":native.number}))
        }).collect()
    }
}

pub(super) fn verify_candidate_pages(
    candidate: &SchematicCandidate,
    observed: &SchematicAudit,
) -> Result<(), String> {
    let configured = observed.configured_pages()?;
    for (path, expected) in candidate.sheet_pages() {
        if configured.get(path).and_then(Option::as_ref) != Some(expected) {
            return Err("captured source page differs from explicit assignment".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_parents_tolerate_staging_but_reject_replacement_and_redirection() {
        let owned = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(owned.path()).unwrap();
        let parent = root.join("children");
        fs::create_dir(&parent).unwrap();
        let expected = DirectoryIdentity::read(&parent).unwrap();
        fs::write(parent.join("own-staging"), b"staged").unwrap();
        assert_eq!(DirectoryIdentity::read(&parent).unwrap(), expected);
        fs::rename(&parent, root.join("retired")).unwrap();
        fs::create_dir(&parent).unwrap();
        assert_ne!(DirectoryIdentity::read(&parent).unwrap(), expected);
        #[cfg(unix)]
        {
            fs::remove_dir(&parent).unwrap();
            std::os::unix::fs::symlink(root.join("retired"), &parent).unwrap();
            assert!(DirectoryIdentity::read(&parent).is_err());
        }
    }
}
