//! Complete native candidate observation; source publication requires a separate approval.
use super::*;
use kicad_schematic::editing::{
    EditAction, SchematicCandidate, SchematicEdit, SymbolDefinitionEvidence, compile_edits,
};
use kicad_schematic::probes::{ConnectivityPin, PinConnectivity};
use std::sync::atomic::{AtomicBool, Ordering};

/// Immutable typed bytes and complete native before/after evidence. This cannot publish files.
pub struct SchematicEditPreview<'a> {
    source: &'a SchematicAudit,
    // Drop the audit referencing the snapshot before deleting its source tree.
    observed: SchematicAudit,
    _snapshot: tempfile::TempDir,
    candidate: SchematicCandidate,
    before_connectivity: SchematicConnectivityAudit,
    after_connectivity: SchematicConnectivityAudit,
    erc_delta: kicad_checks::erc::ErcDelta,
    evidence: Value,
    digest: String,
    valid: AtomicBool,
}

impl SchematicAudit {
    /// Resolve creation evidence through the same effective catalog as the native baseline.
    pub fn symbol_definition(&self, identifier: &str) -> Result<SymbolDefinitionEvidence, String> {
        self.verify()?;
        let resolved = self.symbols.resolve_definition(identifier)?;
        self.verify()?;
        Ok(SymbolDefinitionEvidence {
            lib_id: resolved.identifier,
            catalog_digest: resolved.catalog_digest,
            source_digest: resolved.source_digest,
            definition: resolved.definition,
        })
    }

    /// Compile against captured source bytes, then observe an isolated candidate through KiCad.
    /// Complete electrical and diagnostic differences remain explicit; this grants no write authority.
    pub fn preview_edits(
        &self,
        edits: &[SchematicEdit],
    ) -> Result<SchematicEditPreview<'_>, String> {
        self.verify()?;
        let sources = self
            .parsed
            .files
            .iter()
            .map(|file| {
                let bytes = self
                    .source
                    .files
                    .get(&self.policy.project.join(&file.artifact.relative_path))
                    .and_then(|input| input.bytes.as_deref())
                    .ok_or("missing captured schematic source")?;
                Ok((file.artifact.relative_path.clone(), bytes))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        verify_edit_sources(self, edits)?;
        let candidate = compile_edits(&self.parsed, &self.netlist, &sources, edits)?;
        let before_connectivity = self.observe_pin_connectivity()?;
        let snapshot = tempfile::Builder::new()
            .prefix("auto-kicad-schematic-preview-")
            .tempdir()
            .map_err(|error| error.to_string())?;
        let root = fs::canonicalize(snapshot.path()).map_err(|error| error.to_string())?;
        let project = root.join("project");
        // Capture only inputs already selected by the baseline audit. Unrelated files are not copied.
        for (path, input) in &self.source.files {
            if let Ok(relative) = path.strip_prefix(&self.policy.project)
                && let Some(bytes) = &input.bytes
                && path.file_name().and_then(|name| name.to_str()) != Some("sym-lib-table")
                && path.file_name().and_then(|name| name.to_str()) != Some("fp-lib-table")
            {
                let bytes = relative
                    .to_str()
                    .and_then(|name| candidate.files().get(name))
                    .map(Vec::as_slice)
                    .unwrap_or(bytes);
                write_new(&project.join(relative), bytes)?;
            }
        }
        let schematic = project.join(&self.parsed.root_relative_path);
        let table_root = schematic.parent().ok_or("missing schematic parent")?;
        let original_table_root = self
            .policy
            .schematic
            .parent()
            .ok_or("missing source parent")?;
        write_new(
            &table_root.join("sym-lib-table"),
            relocate(
                self.symbols.effective_rows()?,
                &BTreeMap::new(),
                original_table_root,
                TableKind::Symbol,
            )?
            .as_bytes(),
        )?;
        write_new(
            &table_root.join("fp-lib-table"),
            relocate(
                self.footprints.effective_rows()?,
                &BTreeMap::new(),
                original_table_root,
                TableKind::Footprint,
            )?
            .as_bytes(),
        )?;
        // Exact original project resources are allowed, with source bytes checked by both audits.
        let mut extra_roots = self.policy.extra_roots.clone();
        extra_roots.push(self.policy.project.clone());
        let policy = SchematicAuditPolicy::new(
            &project,
            &schematic,
            &self.policy.configuration,
            &self.policy.symbols,
            &self.policy.footprints,
            &extra_roots,
        )?;
        let observed = SchematicAudit::prepare(&policy)?;
        if observed.parsed != *candidate.parsed() {
            return Err("native candidate source differs from compiled hierarchy".into());
        }
        let after_connectivity = observed.observe_pin_connectivity()?;
        let erc_delta = self
            .erc
            .compare(&observed.erc)
            .map_err(|error| error.to_string())?;
        self.verify()?;
        observed.verify()?;
        verify_edit_sources(self, edits)?;
        let evidence = serde_json::json!({
            "schema_version": 1,
            "source_revision": self.revision,
            "source_artifacts": candidate.source_revision(),
            "candidate_artifacts": candidate.parsed().revision,
            "operation_digest": hash(format!("{:?}", candidate.changes()).as_bytes()),
            "source_context": self.evidence()?,
            "candidate_context": observed.evidence()?,
            "native_before": format!("{:?}", self.netlist),
            "native_after": format!("{:?}", observed.netlist),
            "erc_before": self.erc,
            "erc_after": observed.erc,
            "erc_delta": erc_delta,
            "connectivity_before": connectivity_json(before_connectivity.connectivity()),
            "connectivity_after": connectivity_json(after_connectivity.connectivity()),
            "publication_authorized": false,
        });
        let digest = hash(&serde_json::to_vec(&evidence).map_err(|error| error.to_string())?);
        Ok(SchematicEditPreview {
            source: self,
            observed,
            _snapshot: snapshot,
            candidate,
            before_connectivity,
            after_connectivity,
            erc_delta,
            evidence,
            digest,
            valid: AtomicBool::new(true),
        })
    }
}

impl SchematicEditPreview<'_> {
    pub fn candidate(&self) -> &SchematicCandidate {
        &self.candidate
    }
    pub fn observed(&self) -> &SchematicAudit {
        &self.observed
    }
    pub fn erc_delta(&self) -> &kicad_checks::erc::ErcDelta {
        &self.erc_delta
    }
    pub fn connectivity_before(&self) -> &PinConnectivity {
        self.before_connectivity.connectivity()
    }
    pub fn connectivity_after(&self) -> &PinConnectivity {
        self.after_connectivity.connectivity()
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }

    /// Any observed drift permanently invalidates this preview, including after source restoration.
    pub fn verify(&self) -> Result<(), String> {
        if !self.valid.load(Ordering::Acquire) {
            return Err("schematic preview was invalidated; prepare a fresh preview".into());
        }
        let result = self
            .source
            .verify()
            .and_then(|()| self.observed.verify())
            .and_then(|()| {
                let edits = self
                    .candidate
                    .changes()
                    .iter()
                    .map(|change| SchematicEdit {
                        file: change.file.clone(),
                        scope: kicad_schematic::editing::EditScope::FileAllInstances,
                        action: change.action.clone(),
                    })
                    .collect::<Vec<_>>();
                verify_edit_sources(self.source, &edits)
            });
        if result.is_err() {
            self.valid.store(false, Ordering::Release);
        }
        result
    }
}

fn verify_edit_sources(audit: &SchematicAudit, edits: &[SchematicEdit]) -> Result<(), String> {
    for edit in edits {
        match &edit.action {
            EditAction::CreateSymbol { symbol, .. } => {
                let current = audit.symbol_definition(&symbol.evidence.lib_id)?;
                if current != symbol.evidence {
                    return Err(
                        "symbol creation evidence differs from the effective catalog".into(),
                    );
                }
                if symbol.footprint.as_ref().is_some_and(|id| !id.is_empty()) {
                    return Err(
                        "created-symbol footprint requires separate compatibility evidence".into(),
                    );
                }
            }
            EditAction::AssignFootprint {
                footprint,
                evidence,
                ..
            } => {
                let (nickname, entry_name) = footprint
                    .split_once(':')
                    .ok_or("invalid footprint identifier")?;
                let current = audit.footprints.resolve(&FootprintLibraryIdentifier {
                    nickname: nickname.into(),
                    entry_name: entry_name.into(),
                })?;
                if current.catalog_digest != evidence.catalog_digest
                    || current.source_digest != evidence.source_digest
                {
                    return Err(
                        "footprint assignment evidence differs from the effective catalog".into(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn pin_json(pin: &ConnectivityPin) -> Value {
    let unit = |key: &kicad_schematic::pins::SourceUnitKey| {
        serde_json::json!({
            "file": key.file, "sheet_path": key.sheet_path, "symbol_uuid": key.symbol_uuid,
        })
    };
    match pin {
        ConnectivityPin::Component(key) => serde_json::json!({
            "kind": "component", "units": key.component_units.iter().map(unit).collect::<Vec<_>>(), "number": key.number,
        }),
        ConnectivityPin::Virtual { source, number } => serde_json::json!({
            "kind": "virtual", "source": unit(source), "number": number,
        }),
    }
}

fn connectivity_json(connectivity: &PinConnectivity) -> Value {
    serde_json::json!({
        "nets": connectivity.nets().iter().map(|net| serde_json::json!({
            "name": net.name, "class": net.class, "pins": net.pins.iter().map(pin_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "isolated_no_connect": connectivity.isolated_no_connect().iter().map(pin_json).collect::<Vec<_>>(),
    })
}
