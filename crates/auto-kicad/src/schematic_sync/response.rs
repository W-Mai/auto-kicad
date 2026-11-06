//! Exact retained proof artifacts for responses whose native detail exceeds the transport bound.
use super::*;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
};

const MAX_EVIDENCE: u64 = 64 * 1024 * 1024;

pub(super) struct ResponseEvidence {
    path: PathBuf,
    bytes: u64,
    sha256: String,
    _owner: runtime::owned_temp::TempDir,
}
impl ResponseEvidence {
    pub(super) fn verify(&self) -> Result<(), String> {
        if fs::canonicalize(&self.path).map_err(|e| e.to_string())? != self.path {
            return Err("synchronization response evidence path changed".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&self.path).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() != self.bytes || metadata.len() > MAX_EVIDENCE {
            return Err("synchronization response evidence size or type changed".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_EVIDENCE + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 != self.bytes || crate::digest_bytes(&bytes).as_str() != self.sha256 {
            return Err("synchronization response evidence bytes changed".into());
        }
        Ok(())
    }
}

pub(super) fn compact_native_evidence(
    data: &mut Value,
    evidence: &Value,
    expected_digest: &str,
) -> Result<Option<ResponseEvidence>, String> {
    if bounded(data).is_ok() {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(evidence).map_err(|e| e.to_string())?;
    let sha256 = crate::digest_bytes(&bytes).as_str().to_owned();
    if bytes.len() as u64 > MAX_EVIDENCE || sha256 != expected_digest {
        return Err("oversized synchronization proof exceeds its exact digest/byte bounds".into());
    }
    let owner = runtime::owned_temp::Builder::new()
        .prefix("auto-kicad-sync-response-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
    let path = root.join("native-preview.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    fs::File::open(&root)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    let retained = ResponseEvidence {
        path,
        bytes: bytes.len() as u64,
        sha256,
        _owner: owner,
    };
    retained.verify()?;
    let summary = evidence
        .as_object()
        .ok_or("native proof must be an object")?
        .iter()
        .filter(|(_, value)| !value.is_array() && !value.is_object())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();
    let mut summary = Value::Object(summary);
    summary["evidence_externalized"] = json!(true);
    summary["complete_evidence"] =
        json!({"path":retained.path,"bytes":retained.bytes,"sha256":retained.sha256});
    data["native_preview"] = summary;
    bounded(data)?;
    Ok(Some(retained))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_differences_stay_complete_and_externalized_evidence_keeps_exact_authority() {
        let small = json!({"candidate_board_sha256":"board","native_before":{"unknown":42}});
        let mut data = json!({"differences":[{"exact":"selected"}],"native_preview":small});
        let original = data.clone();
        assert!(
            compact_native_evidence(&mut data, &small, "unused")
                .unwrap()
                .is_none()
        );
        assert_eq!(data, original);
        let large = json!({"candidate_board_sha256":"board","native_before":{"unknown":"x".repeat(4*1024*1024)}});
        let encoded = serde_json::to_vec(&large).unwrap();
        data["native_preview"] = large.clone();
        let proof =
            compact_native_evidence(&mut data, &large, crate::digest_bytes(&encoded).as_str())
                .unwrap()
                .unwrap();
        assert_eq!(data["differences"], original["differences"]);
        assert_eq!(data["native_preview"]["candidate_board_sha256"], "board");
        assert_eq!(fs::read(&proof.path).unwrap(), encoded);
        bounded(&data).unwrap();
        proof.verify().unwrap();
        let path = proof.path.clone();
        let registry = Registry::default();
        let plan = Arc::new(Plan {
            source_run: "response-contract".into(),
            project: "/unused".into(),
            hash: "bound".into(),
            created: Instant::now(),
            phase: Mutex::new(Phase::Busy),
            response_evidence: Mutex::new(Some(proof)),
        });
        registry
            .state
            .lock()
            .unwrap()
            .plans
            .insert("proof".into(), plan.clone());
        fs::write(&path, b"later user bytes").unwrap();
        assert!(registry.get("proof", "bound").is_err());
        assert!(matches!(*plan.phase.lock().unwrap(), Phase::Terminal(_)));
        assert!(plan.response_evidence.lock().unwrap().is_none());
        assert!(!path.exists());
    }
}
