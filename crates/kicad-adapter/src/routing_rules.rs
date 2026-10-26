//! Bounded saved constraints and native rule evidence; custom rules remain KiCad-evaluated.
use kicad_ipc::{DocumentQuery, KiCadClient, KiCadTransport, OpenDocumentKind};
use kicad_model::{ObservedBoardStackup, RevisionDigest};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_JSON_NODES: usize = 100_000;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SavedRoutingConstraints {
    pub min_clearance_nm: Option<i64>,
    pub min_track_width_nm: Option<i64>,
    pub min_via_diameter_nm: Option<i64>,
    pub min_via_annular_width_nm: Option<i64>,
    pub min_through_hole_diameter_nm: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuleFileEvidence {
    pub path: PathBuf,
    pub present: bool,
    pub size: u64,
    pub digest: Option<RevisionDigest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SavedRoutingRules {
    pub project: RuleFileEvidence,
    pub custom_rules: RuleFileEvidence,
    pub constraints: SavedRoutingConstraints,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NetClassRuleEvidence {
    pub class_name: String,
    pub implicit: bool,
    pub constituents: Vec<String>,
    pub clearance_nm: Option<i64>,
    pub preferred_track_width_nm: Option<i64>,
    pub preferred_via_diameter_nm: Option<i64>,
    pub preferred_via_drill_nm: Option<i64>,
    pub tuning_profile: Option<String>,
    pub native_payload_digest: RevisionDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RoutingRuleEvidence {
    pub saved: SavedRoutingRules,
    pub stackup: ObservedBoardStackup,
    pub netclasses: BTreeMap<String, NetClassRuleEvidence>,
}

fn digest(bytes: &[u8]) -> RevisionDigest {
    RevisionDigest::new(hex::encode(Sha256::digest(bytes))).expect("SHA-256 is a valid digest")
}

impl RoutingRuleEvidence {
    pub fn digest(&self) -> Result<RevisionDigest, String> {
        Ok(digest(
            &serde_json::to_vec(self).map_err(|e| e.to_string())?,
        ))
    }

    /// Compare a disposable copy without confusing its path/document ID with changed rules.
    pub fn equivalent_preview_rules(&self, other: &Self) -> bool {
        let same_file = |a: &RuleFileEvidence, b: &RuleFileEvidence| {
            (a.present, a.size, &a.digest) == (b.present, b.size, &b.digest)
        };
        same_file(&self.saved.project, &other.saved.project)
            && same_file(&self.saved.custom_rules, &other.saved.custom_rules)
            && self.saved.constraints == other.saved.constraints
            && self.netclasses == other.netclasses
            && self.stackup.layers == other.stackup.layers
            && self.stackup.finish == other.stackup.finish
            && self.stackup.impedance == other.stackup.impedance
            && self.stackup.edge == other.stackup.edge
    }
}

fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (a.dev(), a.ino(), a.ctime(), a.ctime_nsec())
            != (b.dev(), b.ino(), b.ctime(), b.ctime_nsec())
        {
            return false;
        }
    }
    a.is_file() && b.is_file() && a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

fn read_file(path: &Path) -> Result<(RuleFileEvidence, Option<Vec<u8>>), String> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                RuleFileEvidence {
                    path: path.to_path_buf(),
                    present: false,
                    size: 0,
                    digest: None,
                },
                None,
            ));
        }
        Err(error) => return Err(error.to_string()),
    };
    if !before.is_file() || before.len() > MAX_FILE_BYTES {
        return Err("routing rule source must be a bounded normal file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Reject symlink races and never block while opening a replaced FIFO.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file: File = options.open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !same_metadata(&before, &opened) {
        return Err("routing rule source changed while opening".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let named = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != opened.len()
        || bytes.len() as u64 > MAX_FILE_BYTES
        || !same_metadata(&opened, &after)
        || !same_metadata(&after, &named)
    {
        return Err("routing rule source changed during bounded read".into());
    }
    Ok((
        RuleFileEvidence {
            path: path.to_path_buf(),
            present: true,
            size: bytes.len() as u64,
            digest: Some(digest(&bytes)),
        },
        Some(bytes),
    ))
}

struct Object<'a>(BTreeMap<String, &'a RawValue>);
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if values.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate project JSON key"));
                    }
                    if values.len() > MAX_JSON_NODES {
                        return Err(serde::de::Error::custom("project JSON node limit"));
                    }
                }
                Ok(Object(values))
            }
        }
        d.deserialize_map(ObjectVisitor)
    }
}

fn validate_json(raw: &RawValue, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if depth > 32 || *nodes > MAX_JSON_NODES {
        return Err("project JSON depth/node limit".into());
    }
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let object: Object<'_> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
            for child in object.0.values() {
                validate_json(child, depth + 1, nodes)?;
            }
        }
        Some(b'[') => {
            let array: Vec<&RawValue> =
                serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
            if array.len() > MAX_JSON_NODES {
                return Err("project JSON node limit".into());
            }
            for child in array {
                validate_json(child, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn mm_to_nm(raw: &RawValue) -> Result<i64, String> {
    let number = raw.get();
    if number.len() > 128 || !number.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return Err("constraint must be a bounded nonnegative JSON number".into());
    }
    let (mantissa, exponent) = number.split_once(['e', 'E']).map_or((number, 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or(i32::MAX))
    });
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let joined = format!("{whole}{fraction}");
    let mut digits = joined.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        return Ok(0);
    }
    let scale = i64::from(exponent) - fraction.len() as i64 + 6;
    if scale < 0 {
        let remove = usize::try_from(-scale).map_err(|_| "constraint precision exceeds limit")?;
        if remove >= digits.len()
            || !digits.as_bytes()[digits.len() - remove..]
                .iter()
                .all(|b| *b == b'0')
        {
            return Err("constraint is not exactly representable in nanometres".into());
        }
        digits.truncate(digits.len() - remove);
    } else {
        if scale > 10 || digits.len() + scale as usize > 10 {
            return Err("constraint exceeds coordinate bound".into());
        }
        digits.extend(std::iter::repeat_n('0', scale as usize));
    }
    let value = digits
        .parse::<i64>()
        .map_err(|_| "invalid constraint number")?;
    if value > 2_000_000_000 {
        return Err("constraint exceeds coordinate bound".into());
    }
    Ok(value)
}

fn constraints(bytes: &[u8]) -> Result<SavedRoutingConstraints, String> {
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    validate_json(raw, 0, &mut 0)?;
    let mut object: Object<'_> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
    for key in ["board", "design_settings", "rules"] {
        let Some(value) = object.0.get(key) else {
            return Ok(SavedRoutingConstraints::default());
        };
        object = serde_json::from_str(value.get())
            .map_err(|e| format!("invalid saved constraints at {key}: {e}"))?;
    }
    let value = |name: &str| object.0.get(name).map(|raw| mm_to_nm(raw)).transpose();
    Ok(SavedRoutingConstraints {
        min_clearance_nm: value("min_clearance")?,
        min_track_width_nm: value("min_track_width")?,
        min_via_diameter_nm: value("min_via_diameter")?,
        min_via_annular_width_nm: value("min_via_annular_width")?,
        min_through_hole_diameter_nm: value("min_through_hole_diameter")?,
    })
}

/// Read only the adjacent saved project and custom-rule files; absence remains explicit evidence.
pub fn read_saved_routing_rules(board: &Path) -> Result<SavedRoutingRules, String> {
    if !board.is_absolute()
        || board.extension().and_then(|s| s.to_str()) != Some("kicad_pcb")
        || !fs::symlink_metadata(board)
            .map_err(|e| e.to_string())?
            .is_file()
    {
        return Err("routing rules require an absolute normal PCB file".into());
    }
    let board = fs::canonicalize(board).map_err(|e| e.to_string())?;
    let (project, bytes) = read_file(&board.with_extension("kicad_pro"))?;
    let constraints = bytes
        .as_deref()
        .map(constraints)
        .transpose()?
        .unwrap_or_default();
    let (custom_rules, _) = read_file(&board.with_extension("kicad_dru"))?;
    Ok(SavedRoutingRules {
        project,
        custom_rules,
        constraints,
    })
}

/// Capture stable saved/native evidence without saving the editor or rewriting project rules.
pub fn observe_routing_rules<T: KiCadTransport>(
    ipc: &mut KiCadClient<T>,
    board: &Path,
    nets: &[String],
) -> Result<RoutingRuleEvidence, String> {
    let before = read_saved_routing_rules(board)?;
    let board = fs::canonicalize(board).map_err(|e| e.to_string())?;
    let filename = board
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("PCB filename is not UTF-8")?;
    let token = ipc
        .instance_token()
        .cloned()
        .ok_or("routing observation requires a bound instance")?;
    let documents = ipc
        .get_open_documents(DocumentQuery::Pcb)
        .map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&token) || documents.len() != 1 {
        return Err("routing observation instance/document changed".into());
    }
    let document = &documents[0];
    if !matches!(&document.kind,OpenDocumentKind::Pcb {board_filename} if board_filename==filename)
        || document.project_path.as_ref().map(Path::new) != board.parent()
    {
        return Err("routing observation project does not match saved source".into());
    }
    let stackup = ipc.get_board_stackup(filename).map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&token) {
        return Err("routing observation instance changed".into());
    }
    let classes = ipc
        .get_effective_netclass_rules(filename, nets)
        .map_err(|e| e.to_string())?;
    let repeated = ipc
        .get_effective_netclass_rules(filename, nets)
        .map_err(|e| e.to_string())?;
    let final_stackup = ipc.get_board_stackup(filename).map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&token) {
        return Err("routing observation instance changed".into());
    }
    let final_documents = ipc
        .get_open_documents(DocumentQuery::Pcb)
        .map_err(|e| e.to_string())?;
    if ipc.instance_token() != Some(&token)
        || final_documents != documents
        || stackup != final_stackup
        || classes != repeated
        || read_saved_routing_rules(&board)? != before
    {
        return Err("routing rules changed while observing; save/reload and inspect again".into());
    }
    let board_id = crate::board_document_id(document).map_err(|e| e.to_string())?;
    let stackup = crate::board_stackup_observation(board_id, stackup).map_err(|e| e.to_string())?;
    let netclasses = classes
        .into_iter()
        .map(|(net, class)| {
            (
                net,
                NetClassRuleEvidence {
                    class_name: class.class_name,
                    implicit: class.implicit,
                    constituents: class.constituents,
                    clearance_nm: class.clearance_nm,
                    preferred_track_width_nm: class.preferred_track_width_nm,
                    preferred_via_diameter_nm: class
                        .preferred_via
                        .as_ref()
                        .map(|via| via.diameter_nm),
                    preferred_via_drill_nm: class.preferred_via.as_ref().map(|via| via.drill_nm),
                    tuning_profile: class.tuning_profile,
                    native_payload_digest: digest(&class.native_payload),
                },
            )
        })
        .collect();
    Ok(RoutingRuleEvidence {
        saved: before,
        stackup,
        netclasses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "saved-rules-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::write(path.join("board.kicad_pcb"), "fixture").unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
        fn board(&self) -> PathBuf {
            self.0.join("board.kicad_pcb")
        }
        fn project(&self) -> PathBuf {
            self.0.join("board.kicad_pro")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn decimal_constraints_are_exact_bounded_and_presence_sensitive() {
        for (source, expected) in [
            ("0", 0),
            ("0.2", 200000),
            ("2e-1", 200000),
            ("0.000001", 1),
            ("1.2300000", 1230000),
            ("2000", 2000000000),
        ] {
            let raw: &RawValue = serde_json::from_str(source).unwrap();
            assert_eq!(mm_to_nm(raw).unwrap(), expected);
        }
        for source in [
            "-1",
            "0.0000001",
            "0.20000000000000001",
            "1e100",
            "1e-100",
            "2000.000001",
            "null",
            "\"0.2\"",
            "true",
        ] {
            let raw: &RawValue = serde_json::from_str(source).unwrap();
            assert!(mm_to_nm(raw).is_err(), "accepted {source}");
        }
        assert_eq!(constraints(b"{}").unwrap().min_track_width_nm, None);
        assert_eq!(
            constraints(br#"{"board":{"design_settings":{"rules":{"min_clearance":0}}}}"#)
                .unwrap()
                .min_clearance_nm,
            Some(0)
        );
    }
    #[test]
    fn whole_project_duplicate_keys_and_malformed_structure_reject() {
        for bytes in [
            br#"{"board":{},"board":{}}"#.as_slice(),
            br#"{"unrelated":{"a":1,"a":2}}"#,
            br#"{"board":null}"#,
            br#"{"board":{"design_settings":{"rules":[]}}}"#,
            b"{} {}",
        ] {
            assert!(constraints(bytes).is_err());
        }
        let deep = format!("{}0{}", "[".repeat(34), "]".repeat(34));
        assert!(constraints(deep.as_bytes()).is_err());
    }
    #[test]
    fn source_presence_content_and_unknown_settings_are_bound() {
        let fixture = Fixture::new();
        let absent = read_saved_routing_rules(&fixture.board()).unwrap();
        assert!(!absent.project.present && !absent.custom_rules.present);
        fs::write(fixture.project(), "{}").unwrap();
        let present = read_saved_routing_rules(&fixture.board()).unwrap();
        assert_ne!(absent, present);
        fs::write(fixture.project(), "{\"unknown\":42}").unwrap();
        let changed = read_saved_routing_rules(&fixture.board()).unwrap();
        assert_ne!(changed, present);
        assert_eq!(changed.constraints, present.constraints);
        fs::write(fixture.0.join("board.kicad_dru"), "(version 1)").unwrap();
        assert_ne!(read_saved_routing_rules(&fixture.board()).unwrap(), changed);
        fs::write(fixture.project(), "{\"unknown\":42}").unwrap();
        assert_eq!(fs::read(fixture.project()).unwrap(), b"{\"unknown\":42}");
    }
    #[test]
    fn rejects_nonfiles_symlinks_and_oversized_sources() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.project()).unwrap();
        assert!(read_saved_routing_rules(&fixture.board()).is_err());
        fs::remove_dir(fixture.project()).unwrap();
        let file = File::create(fixture.project()).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(read_saved_routing_rules(&fixture.board()).is_err());
        drop(file);
        fs::remove_file(fixture.project()).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fixture.board(), fixture.project()).unwrap();
            assert!(read_saved_routing_rules(&fixture.board()).is_err());
            fs::remove_file(fixture.project()).unwrap();
            let path =
                std::ffi::CString::new(fixture.project().as_os_str().as_encoded_bytes()).unwrap();
            // The fixture path is owned and contains no interior NUL bytes.
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            assert!(read_saved_routing_rules(&fixture.board()).is_err());
        }
    }
}
