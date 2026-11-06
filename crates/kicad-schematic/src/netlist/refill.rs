//! Frozen native refill on a private saved-board copy.
use super::*;

/// Native output bytes and diagnostics; content matching and publication require separate proofs.
#[derive(Clone, Debug)]
pub struct NativeRefillCapture {
    input_sha256: String,
    output: String,
    report: kicad_checks::DrcReport,
}
impl NativeRefillCapture {
    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }
    pub fn output_board(&self) -> &str {
        &self.output
    }
    pub fn report(&self) -> &kicad_checks::DrcReport {
        &self.report
    }
}

impl NetlistRunner {
    /// Refill an owned copy under the captured execution context, preserving supplied inputs.
    /// The caller owns source/resource/configuration binding and complete final schematic parity.
    pub fn run_private_pcb_refill(
        &self,
        project_root: &Path,
        board: &Path,
    ) -> Result<NativeRefillCapture, NetlistRunnerError> {
        let root = canonical_directory(project_root)?;
        let board = canonical_board(&root, board)?;
        if self.context.is_none() {
            return Err(NetlistRunnerError::ExecutionContext(
                "refill requires a frozen native context".into(),
            ));
        }
        self.verify_context()?;
        let read = |path: &Path| -> Result<Vec<u8>, NetlistRunnerError> {
            let limit = if path.extension().and_then(|value| value.to_str()) == Some("kicad_pcb") {
                kicad_checks::connectivity::SavedBoardProfile::Synchronization.board_bytes()
            } else {
                32 * 1024 * 1024
            };
            let metadata = fs::symlink_metadata(path)
                .map_err(|e| NetlistRunnerError::OutputRead(e.to_string()))?;
            if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
                return Err(NetlistRunnerError::OutputRead(
                    "refill input exceeds regular-file bound".into(),
                ));
            }
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|e| NetlistRunnerError::OutputRead(e.to_string()))?
                .take(limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| NetlistRunnerError::OutputRead(e.to_string()))?;
            if bytes.len() as u64 != metadata.len() {
                return Err(NetlistRunnerError::OutputRead(
                    "refill input changed during capture".into(),
                ));
            }
            Ok(bytes)
        };
        let original = read(&board)?;
        let project_path = board.with_extension("kicad_pro");
        let project = read(&project_path)?;
        let rules_path = board.with_extension("kicad_dru");
        let rules = match fs::symlink_metadata(&rules_path) {
            Ok(_) => Some(read(&rules_path)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(NetlistRunnerError::OutputRead(e.to_string())),
        };
        let owner = Builder::new()
            .prefix("auto-kicad-native-refill-")
            .tempdir_in(&root)
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
        let private = fs::canonicalize(owner.path())
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
        let copy = private.join(board.file_name().ok_or(NetlistRunnerError::InvalidBoard)?);
        let write = |path: &Path, bytes: &[u8]| {
            fs::write(path, bytes).map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))
        };
        write(&copy, &original)?;
        write(&copy.with_extension("kicad_pro"), &project)?;
        if let Some(bytes) = &rules {
            write(&copy.with_extension("kicad_dru"), bytes)?;
        }
        let raw = self.run_check_capture(&private, &copy, NativeCheck::PcbRefill)?;
        let report = kicad_checks::DrcReport::parse(&raw)?;
        if report.kicad_version != "10.0.7"
            || Path::new(&report.source).file_name() != copy.file_name()
        {
            return Err(NetlistRunnerError::OutputRead(
                "refill report native version or input identity differs".into(),
            ));
        }
        let output = read(&copy)?;
        if read(&board)? != original
            || read(&project_path)? != project
            || fs::symlink_metadata(&rules_path).is_ok() != rules.is_some()
            || rules.as_ref().is_some_and(|bytes| {
                !read(&rules_path).is_ok_and(|actual| actual.as_slice() == bytes.as_slice())
            })
            || read(&copy.with_extension("kicad_pro"))? != project
            || fs::symlink_metadata(copy.with_extension("kicad_dru")).is_ok() != rules.is_some()
            || rules.as_ref().is_some_and(|bytes| {
                !read(&copy.with_extension("kicad_dru"))
                    .is_ok_and(|actual| actual.as_slice() == bytes.as_slice())
            })
        {
            return Err(NetlistRunnerError::OutputRead(
                "refill changed protected inputs".into(),
            ));
        }
        self.verify_context()?;
        use sha2::{Digest, Sha256};
        Ok(NativeRefillCapture {
            input_sha256: hex::encode(Sha256::digest(&original)),
            output: String::from_utf8(output)
                .map_err(|e| NetlistRunnerError::OutputRead(e.to_string()))?,
            report,
        })
    }
}
