//! Journal arm compatibility and ordered-state architecture contracts.
use super::*;

fn artifact_stub() -> SynchronizationArtifact {
    SynchronizationArtifact {
        path: PathBuf::from("/unused/artifact"),
        sha256: crate::digest_bytes(b"candidate"),
        bytes: 9,
    }
}
fn record(files: Vec<PublicationFile>) -> ExecutionRecord {
    ExecutionRecord {
        schema_version: 5,
        attempt_id: "attempt".into(),
        project_dir: PathBuf::from("/project"),
        plan_hash: "plan".into(),
        approval_hash: "approval".into(),
        domain: DOMAIN.into(),
        state: "in_flight".into(),
        evidence: Value::Null,
        outcome: None,
        backups: vec![],
        post_attempt: None,
        created_sources: vec![],
        publication: None,
        sync_publication: None,
        sync_project_publication: Some(SynchronizationProjectPublicationIntent {
            candidates: files.iter().map(|_| artifact_stub()).collect(),
            files,
            original_board: artifact_stub(),
            baseline: artifact_stub(),
            preview: artifact_stub(),
            expectation: artifact_stub(),
            matched_digest: "0".repeat(64),
        }),
    }
}

#[test]
fn legacy_record_encoding_omits_the_new_arm_and_overlapping_intents_reject() {
    let hash = "0".repeat(64);
    let artifact = format!(r#"{{"path":"/unused/artifact","sha256":"{hash}","bytes":0}}"#);
    let raw = format!(
        r#"{{"schema_version":4,"attempt_id":"attempt","project_dir":"/project","plan_hash":"plan","approval_hash":"approval","domain":"pcb_sync","state":"in_flight","evidence":null,"outcome":null,"backups":[],"post_attempt":null,"sync_publication":{{"file":{{"original":"/project/main.kicad_pcb","before":{{"kind":"present","sha256":"{hash}"}},"candidate_sha256":"{hash}","staging":"/project/.attempt-0.auto-kicad-sync.tmp"}},"candidate":{artifact},"baseline":{artifact},"preview":{artifact},"expectation":{artifact},"matched_digest":"{hash}"}}}}"#
    );
    let mut legacy: ExecutionRecord = serde_json::from_str(&raw).unwrap();
    assert_eq!(serde_json::to_string(&legacy).unwrap(), raw);
    assert!(valid_execution_schema(&legacy));
    let current = record(vec![]);
    legacy.sync_project_publication = current.sync_project_publication;
    assert!(!valid_execution_schema(&legacy));
    assert!(sync_intent_digest(&legacy).is_err());
    legacy.schema_version = 5;
    assert!(!valid_execution_schema(&legacy));
    legacy.sync_publication = None;
    assert!(valid_execution_schema(&legacy));
    assert!(validate_intent(Path::new("/project"), &legacy).is_err());
    legacy.domain = "schematic".into();
    assert!(!valid_execution_schema(&legacy));
}

#[test]
fn recovery_accepts_only_ordered_states_for_one_or_two_files() {
    let directory = std::env::temp_dir().join(format!(
        "auto-kicad-project-prefix-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    let directory = fs::canonicalize(directory).unwrap();
    struct Owned(PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let _owner = Owned(directory.clone());
    let project = directory.join("main.kicad_pro");
    let board = directory.join("main.kicad_pcb");
    let files = [&project, &board]
        .iter()
        .map(|path| PublicationFile {
            original: path.to_path_buf(),
            before: SourceExistence::Present {
                sha256: crate::digest_bytes(b"before"),
            },
            candidate_sha256: crate::digest_bytes(b"candidate"),
            staging: path.with_extension("stage"),
        })
        .collect::<Vec<_>>();
    let mut execution = record(files);
    execution.project_dir = directory.clone();
    for path in [&project, &board] {
        fs::write(path, b"before").unwrap();
    }
    assert_eq!(current_prefix(&execution).unwrap(), 0);
    fs::write(&board, b"candidate").unwrap();
    assert!(current_prefix(&execution).is_err());
    fs::write(&project, b"candidate").unwrap();
    assert_eq!(current_prefix(&execution).unwrap(), 2);
    fs::write(&board, b"before").unwrap();
    assert_eq!(current_prefix(&execution).unwrap(), 1);
    fs::write(&project, b"later user bytes").unwrap();
    assert!(current_prefix(&execution).is_err());
    execution
        .sync_project_publication
        .as_mut()
        .unwrap()
        .files
        .pop();
    fs::write(&project, b"before").unwrap();
    assert_eq!(current_prefix(&execution).unwrap(), 0);
    fs::write(&project, b"candidate").unwrap();
    assert_eq!(current_prefix(&execution).unwrap(), 1);
}

#[test]
fn legacy_backup_display_encoding_is_decoded_at_the_typed_boundary() {
    let digest = crate::digest_bytes(b"original bytes");
    let mut backup = Backup {
        original: PathBuf::from("/project/main.kicad_pro"),
        saved: PathBuf::from("/state/source-0.backup"),
        sha256: digest.to_string(),
    };
    assert_ne!(backup.sha256, digest.as_str());
    assert_eq!(backup_digest(&backup).unwrap(), digest);
    backup.sha256 = digest.as_str().into();
    assert!(backup_digest(&backup).is_err());
    backup.sha256 = "sha256:invalid".into();
    assert!(backup_digest(&backup).is_err());
}
