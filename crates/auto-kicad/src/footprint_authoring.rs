use crate::library_settings::{self, SharedLibrarySettings};
use crate::{BoardFootprintRequest, digest_bytes};
use kicad_adapter::bindings::{
    SchematicFootprintBinding, bind_schematic_footprint, verify_placed_pin_pad_binding,
};
use kicad_adapter::footprint::SourceFootprintTemplate;
use kicad_adapter::{FootprintRegistry, LiveBoardObservation, ObservedFootprintWithPads};
use kicad_ipc::{KiCadClient, KiCadTransport};
use kicad_model::{
    AllowlistedFootprintPlacement, FootprintLibraryIdentifier, FootprintSourceBinding,
    ObservedFootprintPlacement, StableId,
};
use std::path::Path;

pub(crate) fn registry(
    settings: &SharedLibrarySettings,
    project: &Path,
    version: &str,
) -> Result<FootprintRegistry, String> {
    library_settings::registry(settings, project, version)
}

pub(crate) fn prepare(
    settings: &SharedLibrarySettings,
    request: &BoardFootprintRequest,
    observation: &LiveBoardObservation,
) -> Result<AllowlistedFootprintPlacement, String> {
    let mut placement = AllowlistedFootprintPlacement::new(
        StableId::new(&request.target_id).map_err(|e| e.to_string())?,
        FootprintLibraryIdentifier {
            nickname: request.library_nickname.clone(),
            entry_name: request.library_entry_name.clone(),
        },
        kicad_model::Coordinate {
            x: request.x_nm,
            y: request.y_nm,
            unit: kicad_model::Unit::Nanometres,
            space: kicad_model::DocumentSpace::Board,
        },
        &request.reference,
        &request.value,
    )
    .map_err(|e| e.to_string())?;
    kicad_adapter::validate_placement(&placement).map_err(|e| e.to_string())?;
    let registry = registry(
        settings,
        &observation.project_dir,
        &observation.snapshot.kicad_version,
    )?;
    let resolved = registry
        .resolve_identifier(&placement.library_identifier)
        .map_err(|e| e.to_string())?;
    let schematic_path = observation.board_path.with_extension("kicad_sch");
    let source = kicad_schematic::load_schematic_project(
        &observation.project_dir,
        &schematic_path,
        &observation.snapshot.kicad_version,
        1,
        kicad_schematic::Limits::default(),
    )
    .map_err(|e| e.to_string())?;
    if source.editor_lock_present {
        return Err("close the schematic editor before source binding".into());
    }
    let netlist = kicad_schematic::NetlistRunner::production()
        .map_err(|e| e.to_string())?
        .run(&observation.project_dir, &schematic_path)
        .map_err(|e| e.to_string())?;
    let binding = bind_schematic_footprint(
        &source,
        &netlist.document,
        &observation.snapshot,
        &placement,
    )?;
    placement.source = Some(Box::new(FootprintSourceBinding {
        schematic: source.revision,
        symbol: binding.symbol,
        symbol_path: binding.symbol_path,
        pad_nets: binding.pad_nets,
        library_digest: resolved.provenance.source_digest,
        library_size: resolved.provenance.source_size,
        resolution_digest: Some(
            kicad_model::RevisionDigest::new(
                &resolved
                    .provenance
                    .catalog_evidence
                    .as_ref()
                    .ok_or("catalog evidence missing")?
                    .resolution_digest,
            )
            .map_err(|e| e.to_string())?,
        ),
    }));
    placement.validate().map_err(|e| e.to_string())?;
    verify_sources(settings, &observation.project_dir, &placement)?;
    Ok(placement)
}

pub(crate) fn verify_sources(
    settings: &SharedLibrarySettings,
    project: &Path,
    placement: &AllowlistedFootprintPlacement,
) -> Result<FootprintRegistry, String> {
    placement.validate().map_err(|e| e.to_string())?;
    let source = placement
        .source
        .as_ref()
        .ok_or("footprint source evidence is missing")?;
    let project = std::fs::canonicalize(project).map_err(|e| e.to_string())?;
    for artifact in &source.schematic.files {
        let path = std::fs::canonicalize(project.join(&artifact.relative_path))
            .map_err(|e| e.to_string())?;
        if !path.starts_with(&project)
            || !path.is_file()
            || path.extension().and_then(|s| s.to_str()) != Some("kicad_sch")
        {
            return Err("schematic source escaped the project".into());
        }
        let lock = path.with_file_name(format!(
            "~{}.lck",
            path.file_name()
                .ok_or("missing filename")?
                .to_string_lossy()
        ));
        if lock.exists() {
            return Err("schematic editor lock invalidates footprint source".into());
        }
        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if size != artifact.size || size > 32 * 1024 * 1024 {
            return Err("schematic source size changed".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if digest_bytes(&bytes) != artifact.content_digest {
            return Err("schematic source changed".into());
        }
    }
    let registry = registry(settings, &project, &source.schematic.kicad_cli_version)?;
    let resolved = registry
        .resolve_identifier(&placement.library_identifier)
        .map_err(|e| e.to_string())?;
    if resolved.provenance.source_digest != source.library_digest
        || resolved.provenance.source_size != source.library_size
        || source.resolution_digest.as_ref().map(|d| d.as_str())
            != resolved
                .provenance
                .catalog_evidence
                .as_ref()
                .map(|e| e.resolution_digest.as_str())
        || source.resolution_digest.is_none()
    {
        return Err("footprint library source changed".into());
    }
    Ok(registry)
}

pub(crate) fn bind_template(
    mut prepared: SourceFootprintTemplate,
) -> Result<SourceFootprintTemplate, String> {
    let source = prepared
        .placement
        .source
        .as_ref()
        .ok_or("missing footprint source")?;
    if prepared.resolved.provenance.source_digest != source.library_digest
        || prepared.resolved.provenance.source_size != source.library_size
        || source.resolution_digest.as_ref().map(|d| d.as_str())
            != prepared
                .resolved
                .provenance
                .catalog_evidence
                .as_ref()
                .map(|e| e.resolution_digest.as_str())
        || source.resolution_digest.is_none()
    {
        return Err("captured template source differs from plan".into());
    }
    prepared.template = prepared
        .template
        .with_association(kicad_ipc::FootprintAssociation {
            symbol_path: source.symbol_path.iter().map(ToString::to_string).collect(),
            pad_nets: source.pad_nets.clone(),
        })
        .map_err(|e| e.to_string())?;
    Ok(prepared)
}

fn verify_created(
    prepared: &SourceFootprintTemplate,
    group: ObservedFootprintWithPads,
    child_mapping: &std::collections::BTreeMap<String, String>,
    board: &StableId,
) -> Result<ObservedFootprintPlacement, String> {
    let actual = kicad_adapter::board_footprint_observation(board.clone(), vec![group])
        .map_err(|e| e.to_string())?
        .placements
        .remove(0);
    let mut expected =
        kicad_adapter::board_footprint_observation(board.clone(), vec![prepared.observed.clone()])
            .map_err(|e| e.to_string())?
            .placements
            .remove(0);
    let source = prepared
        .placement
        .source
        .as_ref()
        .ok_or("missing footprint source")?;
    let binding = SchematicFootprintBinding {
        schematic_revision: source.schematic.digest.clone(),
        board_revision: actual.observed_revision.clone(),
        symbol: source.symbol.clone(),
        symbol_path: source.symbol_path.clone(),
        pad_nets: source.pad_nets.clone(),
    };
    verify_placed_pin_pad_binding(&binding, &prepared.placement, &actual, board)?;
    expected.source_uuid = actual.source_uuid.clone();
    expected.placement_id = actual.placement_id.clone();
    expected.observed_revision = actual.observed_revision.clone();
    expected.symbol_sheet_path = source.symbol_path.clone();
    for pad in &mut expected.pads {
        pad.source_uuid = StableId::new(
            child_mapping
                .get(pad.source_uuid.as_str())
                .ok_or("physical pad correspondence missing")?,
        )
        .map_err(|e| e.to_string())?;
    }
    let key = |pad: &kicad_model::ObservedPad| pad.source_uuid.clone();
    expected.pads.sort_by_key(&key);
    let mut actual_pads = actual.pads.clone();
    actual_pads.sort_by_key(&key);
    if expected.pads.len() != actual_pads.len() {
        return Err("created pad count differs from source template".into());
    }
    for (expected_pad, actual_pad) in expected.pads.iter_mut().zip(&actual_pads) {
        if expected_pad.source_uuid != actual_pad.source_uuid {
            return Err("created physical pad identity differs from source template".into());
        }
        expected_pad.pad_id = actual_pad.pad_id.clone();
        expected_pad.source_uuid = actual_pad.source_uuid.clone();
        expected_pad.footprint_placement_id = actual.placement_id.clone();
        expected_pad.observed_revision = actual_pad.observed_revision.clone();
        expected_pad.net = source
            .pad_nets
            .get(&expected_pad.number)
            .and_then(|net| net.as_ref())
            .map(|name| kicad_model::ObservedNetIdentity {
                net_id: kicad_model::board_net_id(board, name),
                name: name.clone(),
            });
    }
    expected
        .pads
        .sort_by(|a, b| a.source_uuid.cmp(&b.source_uuid));
    if expected.canonical() != actual.canonical() {
        return Err("created footprint geometry differs from source template".into());
    }
    Ok(actual)
}

fn create_verified<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board: &StableId,
    prepared: &SourceFootprintTemplate,
) -> Result<ObservedFootprintPlacement, String> {
    let p = &prepared.placement;
    let receipt = client
        .create_footprint_instance(
            filename,
            &prepared.template,
            p.position.x,
            p.position.y,
            &p.reference,
            &p.value,
        )
        .map_err(|e| e.to_string())?;
    let child_mapping = prepared
        .template
        .created_child_mapping(
            &receipt.native,
            p.position.x,
            p.position.y,
            &p.reference,
            &p.value,
        )
        .map_err(|e| e.to_string())?
        .ok_or("native child correspondence differs")?;
    verify_created(
        prepared,
        ObservedFootprintWithPads {
            footprint: receipt.footprint,
            pads: receipt.pads,
        },
        &child_mapping,
        board,
    )
}

fn resolve_connections(
    snapshot: &kicad_model::ProjectSnapshot,
    targets: &std::collections::BTreeMap<StableId, StableId>,
    connections: &[kicad_engine::PlannedPadConnection],
) -> Result<Vec<kicad_engine::Operation>, String> {
    use kicad_engine::{Operation, OperationKind, Precondition, RiskClass};
    use kicad_model::{RoutingEndpointRef, StraightTrackIntent};
    let board = snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?;
    let footprints = snapshot
        .native_footprints
        .as_ref()
        .ok_or("missing footprint observation")?;
    let endpoint = |pin: &kicad_engine::PlannedPadRef| -> Result<_, String> {
        let uuid = targets
            .get(&pin.target_id)
            .ok_or("assembly target is missing")?;
        let placement = footprints
            .placements
            .iter()
            .find(|p| &p.source_uuid == uuid)
            .ok_or("created assembly placement is missing")?;
        let mut pads = placement
            .pads
            .iter()
            .filter(|pad| pad.number == pin.pad_number);
        let pad = pads.next().ok_or("assembly pad is missing")?;
        if pads.next().is_some() {
            return Err("assembly pad number is ambiguous".into());
        }
        if !pad.layers.contains(&kicad_model::BoardLayer::FrontCopper) {
            return Err("assembly connection requires front copper pads".into());
        }
        Ok((
            RoutingEndpointRef {
                pad_id: pad.pad_id.clone(),
                footprint_placement_id: placement.placement_id.clone(),
            },
            kicad_engine::pad_board_position(placement, pad).map_err(|e| e.to_string())?,
            pad.net.clone().ok_or("assembly pad has no net")?,
        ))
    };
    connections
        .iter()
        .map(|connection| {
            let (start_pad, start, net) = endpoint(&connection.start)?;
            let (end_pad, end, end_net) = endpoint(&connection.end)?;
            if net != end_net {
                return Err("created assembly pad nets differ".into());
            }
            let intent = StraightTrackIntent::new_endpoint_bound(
                connection.track_id.clone(),
                start,
                end,
                connection.width_nm,
                start_pad,
                end_pad,
                net,
            )
            .map_err(|e| e.to_string())?;
            kicad_engine::validate_routing_track_binding(snapshot, board, &intent)
                .map_err(|e| e.to_string())?;
            Ok(Operation {
                operation_id: connection.track_id.clone(),
                kind: OperationKind::AddStraightTrack {
                    board: board.clone(),
                    intent,
                },
                targets: vec![board.clone()],
                preconditions: vec![Precondition::Revision(
                    snapshot.revision().map_err(|e| e.to_string())?,
                )],
                expected_effects: vec!["connect verified assembly pads".into()],
                risk_class: RiskClass::Medium,
                idempotency_key: connection.track_id.to_string(),
                source_run_id: "assembly-receipt-resolution".into(),
            })
        })
        .collect()
}

pub(crate) fn execute<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    observation: &LiveBoardObservation,
    prepared: &[SourceFootprintTemplate],
    connections: &[kicad_engine::PlannedPadConnection],
) -> Result<kicad_model::ProjectSnapshot, kicad_adapter::BoardGraphicsCommitOutcome> {
    use kicad_adapter::BoardGraphicsCommitOutcome as Outcome;
    let unknown = |reason: String| Outcome::Unknown { reason };
    let board = observation
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or_else(|| Outcome::Rejected {
            reason: "missing board identity".into(),
        })?;
    let filename = &observation.board_filename;
    let source_native = client
        .capture_footprint_instance_templates(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let commit = client.begin_commit().map_err(|e| unknown(e.to_string()))?;
    let result = (|| -> Result<_, String> {
        let mut expected = observation.clone();
        let mut targets = std::collections::BTreeMap::new();
        for template in prepared {
            let added = create_verified(client, filename, board, template)?;
            if targets
                .insert(
                    template.placement.target_id.clone(),
                    added.source_uuid.clone(),
                )
                .is_some()
            {
                return Err("assembly target is duplicated".into());
            }
            expected.snapshot = expected_snapshot(&expected.snapshot, added)?;
        }
        if connections.is_empty() {
            return Ok((
                expected,
                Vec::new(),
                kicad_adapter::NativePcbBatchReceipt {
                    graphics: vec![],
                    tracks: vec![],
                    vias: vec![],
                },
            ));
        }
        let operations = resolve_connections(&expected.snapshot, &targets, connections)?;
        let batch = kicad_adapter::prepare_bound_native_batch(
            &operations,
            &expected.snapshot,
            &expected.snapshot.revision().map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("assembly routing is invalid: {e:?}"))?;
        let mut receipts = kicad_adapter::NativePcbBatchReceipt {
            graphics: vec![],
            tracks: vec![],
            vias: vec![],
        };
        for track in &batch.tracks {
            let receipt = client
                .create_track(filename, track)
                .map_err(|e| e.to_string())?;
            if receipt.request_index != 0 || receipt.track != *track {
                return Err("assembly track receipt differs from resolved connection".into());
            }
            receipts.tracks.push(receipt);
        }
        Ok((expected, operations, receipts))
    })();
    let (expected, operations, receipts) = match result {
        Ok(expected) => expected,
        Err(reason) => {
            return Err(match client.drop_commit(&commit) {
                Ok(()) => Outcome::Rejected { reason },
                Err(error) => unknown(format!("{reason}; rollback failed: {error}")),
            });
        }
    };
    client
        .commit_changes(&commit, "auto-kicad source-bound PCB authoring")
        .map_err(|e| unknown(e.to_string()))?;
    client
        .save_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let saved_native = client
        .capture_footprint_instance_templates(filename)
        .map_err(|e| unknown(e.to_string()))?;
    if saved_native.len() != source_native.len() + prepared.len() {
        return Err(unknown(
            "saved footprint set differs from placement plan".into(),
        ));
    }
    for original in &source_native {
        let saved = saved_native
            .iter()
            .find(|item| item.metadata().source_uuid == original.metadata().source_uuid)
            .ok_or_else(|| unknown("existing footprint disappeared during placement".into()))?;
        if !original
            .matches_saved_instance(saved)
            .map_err(|e| unknown(e.to_string()))?
        {
            return Err(unknown(
                "existing native footprint changed during placement".into(),
            ));
        }
    }
    for template in prepared {
        let placement = &template.placement;
        let placed = expected
            .snapshot
            .native_footprints
            .as_ref()
            .and_then(|f| {
                f.placements
                    .iter()
                    .find(|f| f.reference == placement.reference)
            })
            .ok_or_else(|| unknown("placed footprint identity missing".into()))?;
        let saved = saved_native
            .iter()
            .find(|item| item.metadata().source_uuid == placed.source_uuid.as_str())
            .ok_or_else(|| unknown("placed native footprint missing".into()))?;
        if !template
            .template
            .matches_created_instance(
                saved,
                placement.position.x,
                placement.position.y,
                &placement.reference,
                &placement.value,
            )
            .map_err(|e| unknown(e.to_string()))?
        {
            return Err(unknown(
                "saved native footprint differs from the approved source template".into(),
            ));
        }
    }
    if operations.is_empty() {
        return Ok(expected.snapshot);
    }
    let routing = kicad_adapter::read_verified_native_routing(client, filename, &receipts)
        .map_err(unknown)?;
    crate::expected_native_shadow_snapshot(&expected, &operations, &receipts, routing)
        .map_err(unknown)
}

pub(crate) fn expected_snapshot(
    before: &kicad_model::ProjectSnapshot,
    added: ObservedFootprintPlacement,
) -> Result<kicad_model::ProjectSnapshot, String> {
    let mut expected = before.clone();
    let mut footprints = expected
        .native_footprints
        .take()
        .ok_or("missing baseline footprints")?;
    let mut ids = before
        .board
        .graphics
        .iter()
        .map(|graphic| graphic.graphic_id.clone())
        .chain(before.native_routing.iter().flat_map(|routing| {
            routing
                .tracks
                .iter()
                .map(|track| track.source_uuid.clone())
                .chain(routing.vias.iter().map(|via| via.source_uuid.clone()))
        }))
        .chain(footprints.placements.iter().flat_map(|placement| {
            std::iter::once(placement.source_uuid.clone())
                .chain(placement.pads.iter().map(|pad| pad.source_uuid.clone()))
        }))
        .collect::<std::collections::BTreeSet<_>>();
    for id in
        std::iter::once(&added.source_uuid).chain(added.pads.iter().map(|pad| &pad.source_uuid))
    {
        if !ids.insert(id.clone()) {
            return Err("footprint receipt reused an existing native identity".into());
        }
    }
    footprints.placements.push(added);
    expected.native_footprints = Some(
        kicad_model::ObservedBoardFootprints::new(footprints.board_id, footprints.placements)
            .map_err(|e| e.to_string())?,
    );
    expected.validate().map_err(|e| e.to_string())?;
    Ok(expected.canonical())
}

pub(crate) fn source_evidence(
    settings: &SharedLibrarySettings,
    project: &Path,
    placement: &AllowlistedFootprintPlacement,
) -> Result<serde_json::Value, String> {
    let registry = verify_sources(settings, project, placement)?;
    let resolved = registry
        .resolve_identifier(&placement.library_identifier)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(resolved.provenance.catalog_evidence).map_err(|e| e.to_string())
}

pub(crate) fn prepare_shadow_library_table(
    settings: &SharedLibrarySettings,
    project: &Path,
    shadow: &Path,
    operations: &[kicad_engine::Operation],
    existing: Option<&kicad_model::ProjectSnapshot>,
) -> Result<(), String> {
    use std::io::Write;
    let placements = operations
        .iter()
        .flat_map(|operation| operation.kind.footprint_placements())
        .collect::<Vec<_>>();
    let registry = if let Some(first) = placements.first() {
        verify_sources(settings, project, first)?
    } else if crate::geometry::contains(operations) {
        let version = &existing
            .ok_or("geometry requires an observed source")?
            .kicad_version;
        crate::library_settings::registry(settings, project, version)?
    } else {
        return Ok(());
    };
    let mut roots = std::collections::BTreeMap::new();
    let mut insert = |resolved: kicad_adapter::footprint::ResolvedFootprint| -> Result<(), String> {
        let nickname = resolved.identifier.nickname;
        let root = resolved.provenance.registry_root;
        if let Some(previous) = roots.insert(nickname, root.clone())
            && previous != root
        {
            return Err("one library nickname resolves to conflicting roots".into());
        }
        Ok(())
    };
    for placement in &placements {
        verify_sources(settings, project, placement)?;
        insert(
            registry
                .resolve_identifier(&placement.library_identifier)
                .map_err(|e| e.to_string())?,
        )?;
    }
    for footprint in existing
        .into_iter()
        .flat_map(|s| s.native_footprints.iter())
        .flat_map(|f| &f.placements)
    {
        if let Ok(resolved) = registry.resolve_identifier(&footprint.library_identifier) {
            insert(resolved)?;
        }
    }
    let mut table = String::from("(fp_lib_table (version 7)\n");
    for (nickname, root) in roots {
        let name = serde_json::to_string(&nickname).map_err(|e| e.to_string())?;
        let path = serde_json::to_string(root.to_str().ok_or("library path is not UTF-8")?)
            .map_err(|e| e.to_string())?;
        table.push_str(&format!(
            "  (lib (name {name}) (type \"KiCad\") (uri {path}) (options \"\") (descr \"\"))\n"
        ));
    }
    table.push_str(")\n");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(shadow.join("fp-lib-table"))
        .map_err(|e| e.to_string())?;
    file.write_all(table.as_bytes()).map_err(|e| e.to_string())
}
