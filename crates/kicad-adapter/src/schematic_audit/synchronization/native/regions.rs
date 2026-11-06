//! Sealed native ownership of authored zones, separate from endpoint partition graph edges.
use super::*;
use kicad_checks::connectivity::ClosedConnectivityEvidence;

const REGION_WORKER: &str = include_str!("../../../../helpers/synchronization_regions.py");

// Preserve the exact legacy worker identity and validation body for existing baselines.
fn large_region_worker() -> String {
    format!(
        "namespace = {{'__name__': 'auto_kicad_region_reader'}}\nexec({}, namespace)\nnamespace['MAX_INPUT'] = 128 * 1024 * 1024\nnamespace['MAX_ENDPOINTS'] = 100_000\nnamespace['main']()\n",
        serde_json::to_string(REGION_WORKER).expect("embedded region reader is a string")
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeZone {
    uuid: String,
    net: String,
    locked: bool,
    rule_area: bool,
    layers: Vec<String>,
    filled: bool,
    terminals: Vec<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionReport {
    schema_version: u32,
    native_version: String,
    board_sha256: String,
    project_sha256: String,
    endpoints: BTreeMap<String, String>,
    zones: Vec<NativeZone>,
    generated_count: usize,
}

/// Additive restart evidence for the explicit region profile; it supplies no resource roots.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::schematic_audit::synchronization) struct RegionRecoveryEvidence {
    worker_sha256: String,
    native: RegionReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated_saved_signature: Option<String>,
}
impl RegionRecoveryEvidence {
    pub(in crate::schematic_audit::synchronization) fn validate(&self) -> Result<(), String> {
        let legacy = self.worker_sha256 == hash(REGION_WORKER.as_bytes());
        let large = self.worker_sha256 == hash(large_region_worker().as_bytes());
        let max_endpoints = if large { 100_000 } else { 4096 };
        if (!legacy && !large)
            || self.native.schema_version != 1
            || self.native.native_version != "10.0.7"
            || self.native.endpoints.len() > max_endpoints
            || self.native.zones.len() + self.native.generated_count > 256
        {
            return Err("unsupported region restoration baseline or worker".into());
        }
        if (self.native.generated_count > 0) != self.generated_saved_signature.is_some()
            || self
                .generated_saved_signature
                .as_ref()
                .is_some_and(|value| {
                    value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit())
                })
        {
            return Err("incomplete generated native restoration signature".into());
        }
        Ok(())
    }
    pub(in crate::schematic_audit::synchronization) fn saved_signature(
        &self,
    ) -> Result<Option<&str>, String> {
        self.validate()?;
        Ok(self.generated_saved_signature.as_deref())
    }
    pub(in crate::schematic_audit::synchronization) fn verify_observation(
        &self,
        actual: &NativeRegionOwnership,
    ) -> Result<(), String> {
        self.validate()?;
        if self.native != actual.report {
            return Err(
                "restored native region ownership differs from the original baseline".into(),
            );
        }
        Ok(())
    }
}

pub(in crate::schematic_audit::synchronization) struct NativeRegionOwnership {
    report: RegionReport,
    worker: NativeWorkerReceipt,
    context: NativeExecutionContext,
    inputs: Inputs,
    config: PathBuf,
    config_names: Vec<PathBuf>,
    _owner: runtime::owned_temp::TempDir,
}

impl NativeRegionOwnership {
    pub(in crate::schematic_audit::synchronization) fn same_native(&self, other: &Self) -> bool {
        self.report == other.report
    }
    pub(in crate::schematic_audit::synchronization) fn recovery_evidence(
        &self,
        native: &NativeBoardProjection,
    ) -> Result<RegionRecoveryEvidence, String> {
        if native.report.board_sha256 != self.report.board_sha256
            || native.report.project_sha256 != self.report.project_sha256
        {
            return Err("region recovery signature belongs to another native input".into());
        }
        Ok(RegionRecoveryEvidence {
            worker_sha256: self.worker.worker_sha256.clone(),
            native: self.report.clone(),
            generated_saved_signature: native.generated_saved_signature()?,
        })
    }
    pub(in crate::schematic_audit::synchronization) fn require_board(
        &self,
        board: &board::SavedSynchronizationBoard,
    ) -> Result<(), String> {
        if self.report.board_sha256 != board.digest() {
            return Err("native region ownership belongs to another board".into());
        }
        Ok(())
    }
    pub(in crate::schematic_audit::synchronization) fn require_input(
        &self,
        board: &board::SavedSynchronizationBoard,
        connectivity: &ClosedConnectivityEvidence,
    ) -> Result<(), String> {
        if self.report.board_sha256 != board.digest()
            || self.report.board_sha256 != connectivity.board_sha256()
            || self.report.project_sha256 != connectivity.project_sha256()
        {
            return Err("region ownership belongs to different saved inputs".into());
        }
        Ok(())
    }
    pub(in crate::schematic_audit::synchronization) fn contacted_endpoints(
        &self,
    ) -> BTreeSet<String> {
        self.report
            .zones
            .iter()
            .flat_map(|zone| zone.terminals.iter().cloned())
            .collect()
    }
    pub(in crate::schematic_audit::synchronization) fn verify(
        &self,
        audit: &SchematicAudit,
    ) -> Result<(), String> {
        self.worker.verify(audit, &self.context)?;
        self.inputs.verify()?;
        if config_names(&self.config)? != self.config_names {
            return Err("native region configuration namespace changed".into());
        }
        Ok(())
    }

    pub(in crate::schematic_audit::synchronization) fn verify_prefix(
        &self,
        audit: &SchematicAudit,
        prefix: &crate::schematic_audit::source_transition::VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        audit.verify_publication_prefix(prefix)?;
        self.worker.verify_execution(&self.context)?;
        self.inputs.verify()?;
        if config_names(&self.config)? != self.config_names {
            return Err("native region configuration namespace changed".into());
        }
        Ok(())
    }

    pub(in crate::schematic_audit::synchronization) fn evidence(&self) -> Result<Value, String> {
        Ok(
            serde_json::json!({"schema_version":1,"worker_sha256":self.worker.worker_sha256,
            "native":self.report,"execution":self.context.digest(),"source_files_read_only":true}),
        )
    }

    /// Zone ownership may constrain a selection but must never create a graph union across islands.
    pub(in crate::schematic_audit::synchronization) fn validate_net_changes(
        &self,
        changes: &[board::SynchronizationCopperNet],
        final_endpoints: &BTreeMap<String, String>,
        replacements: &BTreeMap<String, String>,
        source_nets: &BTreeSet<String>,
    ) -> Result<(), String> {
        let mut selections = BTreeMap::new();
        for change in changes {
            let zone = self
                .report
                .zones
                .iter()
                .find(|zone| zone.uuid == change.uuid)
                .ok_or("unknown authored zone net target")?;
            if selections.insert(&change.uuid, &change.net).is_some()
                || zone.locked
                || zone.rule_area
                || zone.net == change.net
                || !source_nets.contains(&change.net)
                || zone
                    .layers
                    .iter()
                    .any(|layer| layer.parse::<kicad_model::CopperLayer>().is_err())
            {
                return Err(
                    "repeated, locked, non-copper, redundant or foreign zone net selection".into(),
                );
            }
        }
        for zone in &self.report.zones {
            let desired = selections
                .get(&zone.uuid)
                .map(|name| name.as_str())
                .unwrap_or(&zone.net);
            for terminal in &zone.terminals {
                let mapped = replacements.get(terminal).unwrap_or(terminal);
                let final_net = final_endpoints.get(mapped).ok_or_else(|| {
                    format!("zone_contact_removal_requires_separate_edit: {terminal}")
                })?;
                if final_net != desired {
                    return Err(format!(
                        "zone_islands_have_conflicting_desired_nets: {}: {terminal}",
                        zone.uuid
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(in crate::schematic_audit::synchronization) fn capture_regions(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    board: &board::SavedSynchronizationBoard,
    project: &InputFile,
    rules: &InputFile,
    connectivity: &ClosedConnectivityEvidence,
) -> Result<NativeRegionOwnership, String> {
    audit.verify()?;
    if Path::new(filename).file_name() != Some(filename) {
        return Err("native region capture requires one board basename".into());
    }
    let owner = runtime::owned_temp::Builder::new()
        .prefix("auto-kicad-sync-regions-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
    let config = root.join("config/10.0");
    fs::create_dir_all(&config).map_err(|e| e.to_string())?;
    for path in &audit.inner.private_config_names {
        let bytes = audit
            .inner
            .private_inputs
            .files
            .get(path)
            .and_then(|value| value.bytes.as_deref())
            .ok_or("missing region configuration source")?;
        write_new(
            &config.join(path.file_name().ok_or("missing configuration name")?),
            bytes,
        )?;
    }
    let runner = audit
        .inner
        .runner
        .clone()
        .freeze_context(Some(config.parent().ok_or("missing configuration parent")?))
        .map_err(|e| e.to_string())?;
    let context = runner
        .execution_context()
        .ok_or("missing region execution context")?
        .clone();
    let path = root.join(filename);
    write_new(&path, board.text().as_bytes())?;
    write_new(
        &path.with_extension("kicad_pro"),
        project
            .bytes
            .as_deref()
            .ok_or("missing region project bytes")?,
    )?;
    if let Some(bytes) = rules.bytes.as_deref() {
        write_new(&path.with_extension("kicad_dru"), bytes)?;
    }
    let large;
    let selected = if board.text().len() > 32 * 1024 * 1024 || connectivity.endpoints().len() > 4096
    {
        large = large_region_worker();
        large.as_str()
    } else {
        REGION_WORKER
    };
    let (bytes, worker) = run_fixed_worker(audit, &context, &root, selected, &[&path])?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("native region report exceeds bound".into());
    }
    let report: RegionReport = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let expected = connectivity
        .endpoints()
        .iter()
        .map(|endpoint| (endpoint.uuid().to_owned(), endpoint.net().to_owned()))
        .collect::<BTreeMap<_, _>>();
    if report.schema_version != 1
        || report.native_version != "10.0.7"
        || report.board_sha256 != board.digest()
        || Some(&report.project_sha256) != project.digest.as_ref()
        || report.endpoints != expected
        || report.zones.len() != board.zones()?.len()
        || report.zones.len() + report.generated_count > 256
        || report.generated_count
            != crate::live_geometry::TeardropTextPartition::parse_synchronization(board.text())?
                .generated_count()
    {
        return Err("native region input or complete endpoint/zone inventory differs".into());
    }
    let mut observed = BTreeSet::new();
    for zone in &report.zones {
        let original = board
            .zones()?
            .get(&zone.uuid)
            .ok_or("native region has an unknown authored identity")?;
        if !observed.insert(&zone.uuid)
            || zone.net != original.net
            || zone.locked != original.locked
            || zone.rule_area != original.rule_area
            || zone.layers != original.layers
            || zone.terminals.len() > 100_000
            || zone.terminals.windows(2).any(|pair| pair[0] >= pair[1])
            || zone
                .terminals
                .iter()
                .any(|terminal| expected.get(terminal) != Some(&zone.net))
        {
            return Err(
                "native zone ownership disagrees with exact saved definitions/endpoints".into(),
            );
        }
    }
    let mut inputs = Inputs::synchronization();
    inputs.track(&path)?;
    inputs.track(&path.with_extension("kicad_pro"))?;
    inputs.track(&path.with_extension("kicad_dru"))?;
    inputs.track(&root.join("stdout.json"))?;
    let config_names = config_names(&config)?;
    for path in &config_names {
        inputs.track(path)?;
    }
    let result = NativeRegionOwnership {
        report,
        worker,
        context,
        inputs,
        config,
        config_names,
        _owner: owner,
    };
    result.verify(audit)?;
    Ok(result)
}
