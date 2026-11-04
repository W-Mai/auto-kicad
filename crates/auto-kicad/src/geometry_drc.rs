//! Private generated DRC inputs bound to saved bytes and native library sources.
use crate::digest_bytes;
use kicad_adapter::{
    LiveBoardObservation,
    library_catalog::NativeDrcLibraries,
    live_geometry::{GeneratedDrcSnapshot, TeardropTextPartition},
    teardrop_refill::TeardropRefillObservation,
};
use kicad_ipc::{KiCadClient, NngTransport};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn libraries(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    reference: Option<&NativeDrcLibraries>,
) -> Result<NativeDrcLibraries, String> {
    let settings = crate::library_settings::capture();
    let registry = crate::library_settings::registry(
        &settings,
        &source.project_dir,
        &source.snapshot.kicad_version,
    )?;
    let ids = source
        .snapshot
        .native_footprints
        .as_ref()
        .ok_or("missing DRC footprint observations")?
        .placements
        .iter()
        .map(|p| p.library_identifier.clone())
        .collect::<Vec<_>>();
    registry
        .native_drc_libraries(client, &source.board_filename, &ids, reference)
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize)]
pub struct PrivateDrcEvidence {
    snapshot: GeneratedDrcSnapshot,
    project_input_sha256: String,
    project_snapshot_sha256: String,
    rules_sha256: Option<String>,
    libraries: NativeDrcLibraries,
    native_settings: Vec<NativeSettingEvidence>,
    native_settings_after: Vec<NativeSettingEvidence>,
    environment_sha256: String,
    connectivity: Option<kicad_checks::connectivity::ClosedConnectivityEvidence>,
}

fn sha(bytes: &[u8]) -> String {
    digest_bytes(bytes).to_string()
}

pub(super) fn read(path: &Path, required: bool) -> Result<Option<Vec<u8>>, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(f) => f,
        Err(e)
            if !required
                && e.kind() == std::io::ErrorKind::NotFound
                && fs::symlink_metadata(path).is_err() =>
        {
            return Ok(None);
        }
        Err(e) => return Err(format!("cannot read private DRC source: {e}")),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 32 * 1024 * 1024 {
        return Err("private DRC source exceeds bounds".into());
    }
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != meta.len() {
        return Err("private DRC source changed during read".into());
    }
    Ok(Some(bytes))
}

struct RawMap(BTreeMap<String, Box<RawValue>>);
impl<'de> Deserialize<'de> for RawMap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = RawMap;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object with unique keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<RawMap, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((key, value)) = m.next_entry::<String, Box<RawValue>>()? {
                    if out.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate project key"));
                    }
                }
                Ok(RawMap(out))
            }
        }
        d.deserialize_map(Visitor)
    }
}

use kicad_adapter::native_project::project_with_root;

fn environment() -> Result<String, String> {
    let values = std::env::vars_os()
        .map(|(k, v)| {
            Ok((
                k.into_string().map_err(|_| "non-UTF8 environment")?,
                v.into_string().map_err(|_| "non-UTF8 environment")?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    Ok(sha(&serde_json::to_vec(&values).map_err(|e| e.to_string())?))
}

#[derive(Clone, Debug, Serialize)]
struct NativeSettingEvidence {
    path: PathBuf,
    raw_sha256: Option<String>,
    validation_sha256: Option<String>,
}

fn settings_digest(bytes: &[u8], common: bool) -> Result<String, String> {
    if !common {
        return Ok(sha(bytes));
    }
    let RawMap(mut top) = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if let Some(raw) = top.remove("system") {
        let RawMap(mut system) = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
        // Native CLI updates this UI working-directory memory on exit. Every DRC input
        // path is explicit and every used library URI has already been expanded natively.
        if let Some(raw) = system.remove("working_dir") {
            serde_json::from_str::<String>(raw.get()).map_err(|e| e.to_string())?;
        }
        top.insert(
            "system".into(),
            RawValue::from_string(serde_json::to_string(&system).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(sha(&serde_json::to_vec(&top).map_err(|e| e.to_string())?))
}

fn native_settings(source: &LiveBoardObservation) -> Result<Vec<NativeSettingEvidence>, String> {
    crate::library_settings::native_settings_paths(&source.snapshot.kicad_version)?
        .into_iter()
        .map(|path| {
            let bytes = read(&path, false)?;
            let validation_sha256 = bytes
                .as_ref()
                .map(|b| {
                    settings_digest(
                        b,
                        path.file_name().is_some_and(|n| n == "kicad_common.json"),
                    )
                })
                .transpose()?;
            Ok(NativeSettingEvidence {
                path,
                raw_sha256: bytes.as_ref().map(|b| sha(b)),
                validation_sha256,
            })
        })
        .collect()
}

fn same_settings(a: &[NativeSettingEvidence], b: &[NativeSettingEvidence]) -> bool {
    a.iter()
        .map(|v| (&v.path, &v.validation_sha256))
        .eq(b.iter().map(|v| (&v.path, &v.validation_sha256)))
}

pub(super) fn run(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    observed: &TeardropRefillObservation,
    expected: &NativeDrcLibraries,
    timeout: Duration,
    connectivity_required: bool,
) -> Result<(kicad_checks::CompleteDrcReport, PrivateDrcEvidence), String> {
    if &libraries(client, source, Some(expected))? != expected {
        return Err("DRC library source or native environment changed".into());
    }
    let board = read(&source.board_path, true)?.ok_or("missing PCB")?;
    let project =
        read(&source.board_path.with_extension("kicad_pro"), true)?.ok_or("missing project")?;
    let rules = read(&source.board_path.with_extension("kicad_dru"), false)?;
    let settings = native_settings(source)?;
    let env = environment()?;
    let saved = std::str::from_utf8(&board).map_err(|e| e.to_string())?;
    let partition = TeardropTextPartition::parse(saved)?;
    let authored = observed.authored_geometry()?;
    partition
        .authored()
        .validate_native_zones(&authored.native)?;
    authored.text.verify_saved_geometry(partition.authored())?;
    let snapshot = observed.drc_snapshot(saved)?;
    let compiled_project = project_with_root(&project, expected.project_variable())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let root = source.project_dir.join(format!(
        ".auto-kicad-generated-drc-{}-{stamp}",
        std::process::id()
    ));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).map_err(|e| e.to_string())?;
    let work = crate::LiveShadowExport(root);
    let path = work.0.join(
        source
            .board_path
            .file_name()
            .ok_or("missing board filename")?,
    );
    fs::write(&path, snapshot.text()).map_err(|e| e.to_string())?;
    fs::write(path.with_extension("kicad_pro"), &compiled_project).map_err(|e| e.to_string())?;
    if let Some(bytes) = &rules {
        fs::write(path.with_extension("kicad_dru"), bytes).map_err(|e| e.to_string())?;
    }
    fs::write(work.0.join("fp-lib-table"), expected.table()).map_err(|e| e.to_string())?;
    let connectivity = if connectivity_required {
        Some(
            kicad_checks::connectivity::check_saved_connectivity(
                &path,
                &kicad_checks::DrcPolicy {
                    allowed_root: work.0.clone(),
                    timeout,
                },
            )
            .map_err(|e| format!("private DRC connectivity failed: {e:?}"))?,
        )
    } else {
        None
    };
    let report = kicad_checks::run_pcb_drc_complete(
        &path,
        &kicad_checks::DrcPolicy {
            allowed_root: work.0.clone(),
            timeout,
        },
    )
    .map_err(|e| format!("private generated DRC failed: {e:?}"))?;
    if let Some(proof) = &connectivity {
        proof
            .verify_saved_inputs(
                &path,
                &kicad_checks::DrcPolicy {
                    allowed_root: work.0.clone(),
                    timeout,
                },
            )
            .map_err(|e| format!("private DRC connectivity input changed: {e:?}"))?;
    }
    let settings_after = native_settings(source)?;
    for (label, changed) in [
        (
            "source PCB",
            read(&source.board_path, true)?.as_ref() != Some(&board),
        ),
        (
            "source project",
            read(&source.board_path.with_extension("kicad_pro"), true)?.as_ref() != Some(&project),
        ),
        (
            "source rules",
            read(&source.board_path.with_extension("kicad_dru"), false)? != rules,
        ),
        (
            "private PCB",
            read(&path, true)?.as_deref() != Some(snapshot.text().as_bytes()),
        ),
        (
            "private project",
            read(&path.with_extension("kicad_pro"), true)?.as_ref() != Some(&compiled_project),
        ),
        (
            "private rules",
            read(&path.with_extension("kicad_dru"), false)? != rules,
        ),
        (
            "private library table",
            read(&work.0.join("fp-lib-table"), true)?.as_deref()
                != Some(expected.table().as_bytes()),
        ),
        (
            "native settings",
            !same_settings(&settings_after, &settings),
        ),
        ("process environment", environment()? != env),
        (
            "native library sources",
            &libraries(client, source, Some(expected))? != expected,
        ),
    ] {
        if changed {
            return Err(format!("private generated DRC input changed: {label}"));
        }
    }
    let current = TeardropRefillObservation::capture(client, &source.board_filename)?;
    if current.native() != observed.native()
        || current.text().raw_text() != observed.text().raw_text()
    {
        return Err("native source changed during private DRC".into());
    }
    fs::remove_dir_all(&work.0).map_err(|e| format!("private DRC cleanup failed: {e}"))?;
    Ok((
        report,
        PrivateDrcEvidence {
            snapshot,
            project_input_sha256: sha(&project),
            project_snapshot_sha256: sha(&compiled_project),
            rules_sha256: rules.as_ref().map(|b| sha(b)),
            libraries: expected.clone(),
            native_settings: settings,
            native_settings_after: settings_after,
            environment_sha256: env,
            connectivity,
        },
    ))
}

impl PrivateDrcEvidence {
    pub(super) fn connectivity_for(
        &self,
        live: &kicad_adapter::copper_connectivity::VerifiedCopperConnectivity,
    ) -> Result<&kicad_checks::connectivity::ClosedConnectivityEvidence, String> {
        let proof = self
            .connectivity
            .as_ref()
            .ok_or("private DRC has no native connectivity evidence")?;
        let raw = |digest: &str| digest.strip_prefix("sha256:").unwrap_or(digest).to_owned();
        if raw(self.snapshot.input_sha256()) != live.closed().board_sha256()
            || raw(&self.project_input_sha256) != live.closed().project_sha256()
            || raw(self.snapshot.snapshot_sha256()) != proof.board_sha256()
            || raw(&self.project_snapshot_sha256) != proof.project_sha256()
        {
            return Err(
                "private DRC connectivity input is not bound to the live saved source".into(),
            );
        }
        Ok(proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_settings_retain_all_fields_except_ui_working_directory() {
        let raw = br#"{"system":{"working_dir":"/old","future":1.12345678901234567890},"environment":{"vars":{"CUSTOM":"source"}}}"#;
        let changed = String::from_utf8(raw.to_vec())
            .unwrap()
            .replace("/old", "/new");
        assert_eq!(
            settings_digest(raw, true).unwrap(),
            settings_digest(changed.as_bytes(), true).unwrap()
        );
        assert_ne!(
            settings_digest(raw, false).unwrap(),
            settings_digest(changed.as_bytes(), false).unwrap()
        );
        for (from, to) in [
            ("source", "changed"),
            ("1.12345678901234567890", "1.12345678901234567891"),
            ("future", "unknown"),
        ] {
            assert_ne!(
                settings_digest(raw, true).unwrap(),
                settings_digest(changed.replace(from, to).as_bytes(), true).unwrap()
            );
        }
        for bad in [
            r#"{"system":{"working_dir":1}}"#,
            r#"{"system":{"working_dir":"a","working_dir":"b"}}"#,
        ] {
            assert!(settings_digest(bad.as_bytes(), true).is_err());
        }
    }
    #[test]
    fn private_project_changes_only_the_bound_project_variable() {
        let raw=br#"{"unknown":{"n":1.123456789012345678901234567890},"text_variables":{"KEEP":"literal","KIPRJMOD":"old"},"array":[3,2,1]}"#;
        let result = project_with_root(raw, "/source/root").unwrap();
        let a: RawMap = serde_json::from_slice(raw).unwrap();
        let b: RawMap = serde_json::from_slice(&result).unwrap();
        for key in ["unknown", "array"] {
            assert_eq!(a.0[key].get(), b.0[key].get());
        }
        let vars: serde_json::Value = serde_json::from_str(b.0["text_variables"].get()).unwrap();
        assert_eq!(
            vars,
            serde_json::json!({"KEEP":"literal","KIPRJMOD":"/source/root"})
        );
        for bad in [
            r#"{"x":1,"x":2}"#,
            r#"{"text_variables":{"x":"a","x":"b"}}"#,
            r#"{"text_variables":{"x":1}}"#,
        ] {
            assert!(project_with_root(bad.as_bytes(), "/source").is_err());
        }
    }
}
