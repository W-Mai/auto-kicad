//! Durable exact native refill inputs/outputs without expanding legacy journal encodings.
use super::*;
use serde_json::value::RawValue;
const NAMES: [&str; 3] = [
    "region-input.kicad_pcb",
    "region-native-0.kicad_pcb",
    "region-native-1.kicad_pcb",
];

pub(in crate::recovery) fn region_payload_bytes(payloads: &[(&str, &[u8])]) -> io::Result<usize> {
    if payloads.is_empty() {
        return Ok(0);
    }
    if payloads.len() != 3
        || payloads.iter().zip(NAMES).any(|((name, bytes), expected)| {
            *name != expected || bytes.is_empty() || bytes.len() > 128 * 1024 * 1024
        })
    {
        return Err(invalid(
            "native region archive exceeds its exact file/byte bounds",
        ));
    }
    payloads.iter().try_fold(0usize, |total, (_, bytes)| {
        total
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("native region archive size overflow"))
    })
}
fn hashes(preview: &[u8], candidate: &kicad_model::RevisionDigest) -> io::Result<[String; 3]> {
    let fields: std::collections::BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(preview)?;
    let refill: Value = serde_json::from_str(
        fields
            .get("region_refill")
            .ok_or_else(|| invalid("region archive lacks matched refill evidence"))?
            .get(),
    )?;
    let content = &refill["content"];
    if content["candidate_sha256"].as_str() != Some(candidate.as_str())
        || refill["native_executions"].as_array().map(Vec::len) != Some(2)
    {
        return Err(invalid(
            "region archive does not bind the exact publication candidate",
        ));
    }
    let strings = [
        &content["input_sha256"],
        &content["native_sha256"],
        &content["independent_sha256"],
    ];
    let result = strings
        .into_iter()
        .map(|value| {
            value
                .as_str()
                .filter(|text| text.len() == 64 && text.bytes().all(|c| c.is_ascii_hexdigit()))
                .map(str::to_owned)
                .ok_or_else(|| invalid("invalid matched native region digest"))
        })
        .collect::<io::Result<Vec<_>>>()?;
    for index in 0..2 {
        if refill["native_executions"][index]["input_sha256"].as_str() != Some(result[0].as_str())
            || refill["native_executions"][index]["output_sha256"].as_str()
                != Some(result[index + 1].as_str())
        {
            return Err(invalid("native region execution digests disagree"));
        }
    }
    result
        .try_into()
        .map_err(|_| invalid("incomplete native region digest tuple"))
}

pub(in crate::recovery) fn archive_region_preview(
    directory: &Path,
    preview: &[u8],
    payloads: &[(&str, &[u8])],
    candidate: &kicad_model::RevisionDigest,
) -> io::Result<Vec<u8>> {
    if payloads.is_empty() {
        return Ok(preview.to_vec());
    }
    region_payload_bytes(payloads)?;
    let expected = hashes(preview, candidate)?;
    let mut artifacts = Vec::new();
    for ((name, bytes), expected) in payloads.iter().zip(expected) {
        if crate::digest_bytes(bytes).as_str() != expected {
            return Err(invalid(
                "native region archive bytes differ from matched proof",
            ));
        }
        artifacts.push(artifact(directory, name, bytes)?);
    }
    // Keep every original raw JSON number/token; the added array contains only bounded artifact metadata.
    let mut fields: std::collections::BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(preview)?;
    if fields
        .insert(
            "region_native_artifacts".into(),
            RawValue::from_string(serde_json::to_string(&artifacts)?)?,
        )
        .is_some()
    {
        return Err(invalid("duplicate native region archive"));
    }
    Ok(serde_json::to_vec(&fields)?)
}

pub(in crate::recovery) fn validate_region_preview(
    directory: &Path,
    preview: &[u8],
    required: bool,
    candidate: &kicad_model::RevisionDigest,
) -> io::Result<()> {
    let fields: std::collections::BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(preview)?;
    let archive = fields.get("region_native_artifacts");
    if !required {
        if archive.is_some() {
            return Err(invalid(
                "legacy synchronization cannot borrow native region authority",
            ));
        }
        return Ok(());
    }
    let artifacts: Vec<SynchronizationArtifact> = serde_json::from_str(
        archive
            .ok_or_else(|| invalid("region recovery has no durable native archive"))?
            .get(),
    )?;
    if artifacts.len() != 3 {
        return Err(invalid("native region archive is incomplete"));
    }
    let expected = hashes(preview, candidate)?;
    for ((artifact, name), expected) in artifacts.iter().zip(NAMES).zip(expected) {
        if artifact.bytes > 128 * 1024 * 1024 || artifact.sha256.as_str() != expected {
            return Err(invalid(
                "native region artifact disagrees with matched evidence",
            ));
        }
        read_artifact(directory, artifact, name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_region_archives_are_required_and_legacy_preview_tokens_remain_unchanged() {
        let owner = runtime::owned_temp::Builder::new()
            .prefix("auto-kicad-region-contract-")
            .tempdir()
            .unwrap();
        let canonical = fs::canonicalize(owner.path()).unwrap();
        let root = canonical.as_path();
        let payloads = vec![
            (NAMES[0], b"input".as_slice()),
            (NAMES[1], b"native".as_slice()),
            (NAMES[2], b"independent".as_slice()),
        ];
        let digests = payloads
            .iter()
            .map(|(_, bytes)| crate::digest_bytes(bytes))
            .collect::<Vec<_>>();
        let candidate = crate::digest_bytes(b"candidate");
        let preview=serde_json::to_vec(&serde_json::json!({"region_refill":{
            "content":{"candidate_sha256":candidate,"input_sha256":digests[0],"native_sha256":digests[1],"independent_sha256":digests[2]},
            "native_executions":[{"input_sha256":digests[0],"output_sha256":digests[1]},{"input_sha256":digests[0],"output_sha256":digests[2]}]}})).unwrap();
        let legacy = br#"{ "precise": 1.00000000000000007 }"#;
        assert_eq!(
            archive_region_preview(root, legacy, &[], &candidate).unwrap(),
            legacy
        );
        assert!(validate_region_preview(root, &preview, true, &candidate).is_err());
        let archived = archive_region_preview(root, &preview, &payloads, &candidate).unwrap();
        validate_region_preview(root, &archived, true, &candidate).unwrap();
        assert!(validate_region_preview(root, &archived, false, &candidate).is_err());
        assert!(
            validate_region_preview(root, &archived, true, &crate::digest_bytes(b"other")).is_err()
        );
        fs::write(root.join(NAMES[1]), b"later bytes").unwrap();
        assert!(validate_region_preview(root, &archived, true, &candidate).is_err());
    }
}
