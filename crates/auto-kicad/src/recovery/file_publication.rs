//! Domain-neutral durable file IO; callers retain journal, approval and native authority.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationFile {
    pub original: PathBuf,
    pub before: SourceExistence,
    pub candidate_sha256: kicad_model::RevisionDigest,
    pub staging: PathBuf,
}

fn present(file: &PublicationFile) -> SourceExistence {
    SourceExistence::Present {
        sha256: file.candidate_sha256.clone(),
    }
}

fn require_paths(project: &Path, file: &PublicationFile) -> io::Result<()> {
    if file.original == file.staging
        || file.original.parent() != file.staging.parent()
        || file.before == present(file)
    {
        return Err(invalid(
            "publication requires distinct sibling paths and changed bytes",
        ));
    }
    if !project.is_dir() || fs::canonicalize(project)? != project {
        return Err(invalid("publication project is not canonical"));
    }
    Ok(())
}

pub(super) fn stage(
    project: &Path,
    file: &PublicationFile,
    bytes: &[u8],
    limit: u64,
) -> io::Result<()> {
    validate_staging(project, file, bytes, limit)?;
    write_new(&file.staging, bytes)?;
    sync_directory(file.staging.parent().expect("staging parent"))
}

fn validate_staging(
    project: &Path,
    file: &PublicationFile,
    bytes: &[u8],
    limit: u64,
) -> io::Result<()> {
    require_paths(project, file)?;
    if bytes.len() as u64 > limit
        || crate::digest_bytes(bytes) != file.candidate_sha256
        || confined_existence_with_limit(project, &file.original, limit)? != file.before
        || confined_existence_with_limit(project, &file.staging, limit)? != SourceExistence::Absent
    {
        return Err(invalid(
            "publication candidate or source/staging state changed",
        ));
    }
    Ok(())
}

pub(super) fn stage_checkpoints(
    project: &Path,
    file: &PublicationFile,
    bytes: &[u8],
    limit: u64,
    mut checkpoint: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    validate_staging(project, file, bytes, limit)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut staged = options.open(&file.staging)?;
    checkpoint("stage_opened")?;
    let split = bytes.len() / 2;
    staged.write_all(&bytes[..split])?;
    checkpoint("stage_partial")?;
    staged.write_all(&bytes[split..])?;
    staged.sync_all()?;
    checkpoint("stage_file_synchronized")?;
    sync_directory(file.staging.parent().expect("staging parent"))
}

pub(super) fn publish(
    project: &Path,
    file: &PublicationFile,
    limit: u64,
    mut checkpoint: impl FnMut(&str) -> io::Result<()>,
) -> io::Result<()> {
    require_paths(project, file)?;
    if confined_existence_with_limit(project, &file.original, limit)? != file.before
        || confined_existence_with_limit(project, &file.staging, limit)? != present(file)
    {
        return Err(invalid("publication source or staged candidate changed"));
    }
    match file.before {
        SourceExistence::Present { .. } => fs::rename(&file.staging, &file.original)?,
        SourceExistence::Absent => {
            fs::hard_link(&file.staging, &file.original)?;
            checkpoint("linked")?;
            fs::remove_file(&file.staging)?;
        }
    }
    checkpoint("published")?;
    sync_directory(file.original.parent().expect("source parent"))?;
    checkpoint("synchronized")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    #[test]
    fn domain_neutral_file_record_preserves_legacy_schematic_journal_bytes() {
        let before = crate::digest_bytes(b"before");
        let candidate = crate::digest_bytes(b"candidate");
        let before_hex = hex::encode(sha2::Sha256::digest(b"before"));
        let candidate_hex = hex::encode(sha2::Sha256::digest(b"candidate"));
        assert_eq!(serde_json::to_value(before).unwrap(), before_hex);
        let raw = format!(
            r#"{{"schema_version":3,"attempt_id":"attempt","project_dir":"/project","plan_hash":"plan","approval_hash":"approval","domain":"schematic","state":"in_flight","evidence":{{}},"outcome":null,"backups":[],"post_attempt":null,"publication":{{"files":[{{"original":"/project/root.kicad_sch","before":{{"kind":"present","sha256":"{before_hex}"}},"candidate_sha256":"{candidate_hex}","staging":"/project/.attempt-0.auto-kicad-schematic.tmp"}}]}}}}"#
        );
        let record: ExecutionRecord = serde_json::from_str(&raw).unwrap();
        assert_eq!(serde_json::to_string(&record).unwrap(), raw);
        let generic: PublicationFile = serde_json::from_value(
            serde_json::to_value(&record.publication.as_ref().unwrap().files[0]).unwrap(),
        )
        .unwrap();
        assert_eq!(generic.candidate_sha256, candidate);
    }
}
