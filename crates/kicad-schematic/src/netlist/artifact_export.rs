//! Fixed native artifact recipes with frozen process inputs and bounded output capture.
use super::*;

#[derive(Clone, Debug)]
pub enum NativeArtifactRecipe {
    Gerber { layers: Vec<String> },
    Drill,
    Bom,
    SchematicPdf,
}

impl NetlistRunner {
    /// Generate a fixed artifact recipe into an owned destination; this grants no publication authority.
    pub fn run_artifact_export(
        &self,
        project_root: &Path,
        input: &Path,
        output: &Path,
        recipe: &NativeArtifactRecipe,
    ) -> Result<(), NetlistRunnerError> {
        let root = canonical_directory(project_root)?;
        let input = match recipe {
            NativeArtifactRecipe::Gerber { .. } | NativeArtifactRecipe::Drill => {
                canonical_board(&root, input)?
            }
            _ => canonical_schematic(&root, input)?,
        };
        let folder = matches!(
            recipe,
            NativeArtifactRecipe::Gerber { .. } | NativeArtifactRecipe::Drill
        );
        let output_root = if folder {
            let target = canonical_directory(output)?;
            if fs::read_dir(&target)
                .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?
                .next()
                .is_some()
            {
                return Err(NetlistRunnerError::TemporaryOutput(
                    "artifact destination is not empty".into(),
                ));
            }
            target
        } else {
            let parent = canonical_directory(
                output
                    .parent()
                    .ok_or(NetlistRunnerError::InvalidProjectRoot)?,
            )?;
            if output.file_name().is_none()
                || output.exists()
                || fs::symlink_metadata(output).is_ok()
            {
                return Err(NetlistRunnerError::TemporaryOutput(
                    "artifact file already exists".into(),
                ));
            }
            parent
        };
        self.verify_context()?;
        if self.context.is_none() {
            return Err(NetlistRunnerError::ExecutionContext(
                "artifact export requires a frozen context".into(),
            ));
        }
        let capture = Builder::new()
            .prefix("auto-kicad-export-command-")
            .tempdir()
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
        let stdout_path = capture.path().join("stdout.log");
        let stderr_path = capture.path().join("stderr.log");
        let stdout = File::create(&stdout_path)
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
        let stderr = File::create(&stderr_path)
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
        let mut command = Command::new(canonical_executable(&self.executable)?);
        self.context
            .as_ref()
            .expect("checked context")
            .apply(&mut command, &root);
        match recipe {
            NativeArtifactRecipe::Gerber { layers } => {
                let supported = |layer: &str| {
                    matches!(
                        layer,
                        "F.Cu" | "B.Cu" | "F.Mask" | "B.Mask" | "F.SilkS" | "B.SilkS" | "Edge.Cuts"
                    ) || layer
                        .strip_prefix("In")
                        .and_then(|value| value.strip_suffix(".Cu"))
                        .and_then(|value| value.parse::<u8>().ok())
                        .is_some_and(|number| (1..=30).contains(&number))
                };
                let distinct = layers.iter().collect::<std::collections::BTreeSet<_>>();
                if layers.is_empty()
                    || layers.len() > 38
                    || distinct.len() != layers.len()
                    || layers.iter().any(|layer| !supported(layer))
                {
                    return Err(NetlistRunnerError::TemporaryOutput(
                        "unsupported or repeated fabrication layer".into(),
                    ));
                }
                command
                    .args([
                        "pcb",
                        "export",
                        "gerbers",
                        "--precision",
                        "6",
                        "--no-protel-ext",
                        "--layers",
                    ])
                    .arg(layers.join(","));
            }
            NativeArtifactRecipe::Drill => {
                command.args([
                    "pcb",
                    "export",
                    "drill",
                    "--format",
                    "excellon",
                    "--drill-origin",
                    "absolute",
                    "--excellon-units",
                    "mm",
                    "--excellon-zeros-format",
                    "decimal",
                    "--excellon-oval-format",
                    "route",
                    "--excellon-separate-th",
                ]);
            }
            NativeArtifactRecipe::Bom => {
                command.args([
                    "sch",
                    "export",
                    "bom",
                    "--fields",
                    "Reference,Value,Footprint,QUANTITY,DNP",
                    "--labels",
                    "Reference,Value,Footprint,Quantity,DNP",
                    "--group-by",
                    "",
                    "--sort-field",
                    "Reference",
                    "--exclude-dnp",
                    "--ref-range-delimiter",
                    "",
                    "--keep-tabs",
                    "--keep-line-breaks",
                ]);
            }
            NativeArtifactRecipe::SchematicPdf => {
                command.args([
                    "sch",
                    "export",
                    "pdf",
                    "--black-and-white",
                    "--no-background-color",
                    "--exclude-pdf-metadata",
                ]);
            }
        }
        command
            .arg("--output")
            .arg(output)
            .arg(&input)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        configure_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| NetlistRunnerError::Spawn(e.to_string()))?;
        let started = Instant::now();
        let outcome = loop {
            if let Err(error) = artifact_output_bound(&output_root)
                .and_then(|_| enforce_output_size(&stdout_path, self.max_stderr_bytes, true))
                .and_then(|_| enforce_output_size(&stderr_path, self.max_stderr_bytes, false))
            {
                terminate_child_tree(&mut child);
                break Err(error);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(error) => {
                    terminate_child_tree(&mut child);
                    break Err(NetlistRunnerError::Wait(error.to_string()));
                }
            }
            if started.elapsed() >= self.timeout.min(Duration::from_secs(120)) {
                terminate_child_tree(&mut child);
                break Err(NetlistRunnerError::Timeout {
                    stdout: read_diagnostic(&stdout_path, self.max_stderr_bytes),
                    stderr: read_diagnostic(&stderr_path, self.max_stderr_bytes),
                });
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let status = outcome?;
        self.verify_context()?;
        artifact_output_bound(&output_root)?;
        if !status.success() {
            return Err(NetlistRunnerError::ProcessFailed {
                status: status.code(),
                stdout: read_diagnostic(&stdout_path, self.max_stderr_bytes),
                stderr: read_diagnostic(&stderr_path, self.max_stderr_bytes),
            });
        }
        Ok(())
    }
}

fn artifact_output_bound(root: &Path) -> Result<(), NetlistRunnerError> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut total = 0u64;
    let mut count = 0;
    while let Some((directory, depth)) = pending.pop() {
        if depth > 4 {
            return Err(NetlistRunnerError::TemporaryOutput(
                "artifact depth exceeds bound".into(),
            ));
        }
        for entry in fs::read_dir(&directory)
            .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?
        {
            let entry = entry.map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| NetlistRunnerError::TemporaryOutput(e.to_string()))?;
            if metadata.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if metadata.is_file() {
                count += 1;
                total = total.saturating_add(metadata.len());
                if count > 512 || metadata.len() > 64 * 1024 * 1024 || total > 256 * 1024 * 1024 {
                    return Err(NetlistRunnerError::TemporaryOutput(
                        "artifact output exceeds byte/file bounds".into(),
                    ));
                }
            } else {
                return Err(NetlistRunnerError::TemporaryOutput(
                    "artifact output contains a symlink or special file".into(),
                ));
            }
        }
    }
    Ok(())
}
