use crate::{BoardFootprintRequest, digest_bytes};
use kicad_adapter::bindings::{
    SchematicFootprintBinding, bind_schematic_footprint, verify_placed_pin_pad_binding,
};
use kicad_adapter::footprint::SourceFootprintTemplate;
use kicad_adapter::{
    AllowlistedFootprint, FootprintRegistry, LiveBoardObservation, ObservedFootprintWithPads,
};
use kicad_ipc::{KiCadClient, KiCadTransport};
use kicad_model::{
    AllowlistedFootprintPlacement, FootprintLibraryIdentifier, FootprintSourceBinding,
    ObservedFootprintPlacement, StableId,
};
use std::path::{Path, PathBuf};

pub(crate) fn registry(
    project: &Path,
    placement: &AllowlistedFootprintPlacement,
) -> Result<FootprintRegistry, String> {
    let library = AllowlistedFootprint::from_identifier(&placement.library_identifier)
        .ok_or("footprint is not allowlisted")?;
    let root = match library {
        AllowlistedFootprint::OfficialLed1210 | AllowlistedFootprint::OfficialResistor0603 => {
            let root = std::env::var_os("AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT")
                .map(PathBuf::from)
                .or_else(|| {
                    [
                        "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints",
                        "/usr/share/kicad/footprints",
                    ]
                    .into_iter()
                    .map(PathBuf::from)
                    .find(|root| root.is_dir())
                })
                .ok_or("official KiCad footprint library is unavailable")?;
            root.join(format!("{}.pretty", placement.library_identifier.nickname))
        }
        AllowlistedFootprint::ProjectTypeC => std::env::var_os("AUTO_KICAD_PROJECT_FOOTPRINT_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| project.join("Type-C.pretty")),
    };
    FootprintRegistry::new(&root, &root).map_err(|e| e.to_string())
}

pub(crate) fn prepare(
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
    let registry = registry(&observation.project_dir, &placement)?;
    let library = AllowlistedFootprint::from_identifier(&placement.library_identifier)
        .ok_or("unsupported footprint")?;
    let resolved = registry.resolve(library).map_err(|e| e.to_string())?;
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
    }));
    placement.validate().map_err(|e| e.to_string())?;
    verify_sources(&observation.project_dir, &placement)?;
    Ok(placement)
}

pub(crate) fn verify_sources(
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
    let registry = registry(&project, placement)?;
    let library = AllowlistedFootprint::from_identifier(&placement.library_identifier)
        .ok_or("unsupported footprint")?;
    let resolved = registry.resolve(library).map_err(|e| e.to_string())?;
    if resolved.provenance.source_digest != source.library_digest
        || resolved.provenance.source_size != source.library_size
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
    let key =
        |pad: &kicad_model::ObservedPad| (pad.number.clone(), pad.position.x_nm, pad.position.y_nm);
    expected.pads.sort_by_key(&key);
    let mut actual_pads = actual.pads.clone();
    actual_pads.sort_by_key(&key);
    if expected.pads.len() != actual_pads.len() {
        return Err("created pad count differs from source template".into());
    }
    for (expected_pad, actual_pad) in expected.pads.iter_mut().zip(&actual_pads) {
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

pub(crate) fn execute<T: KiCadTransport>(
    client: &mut KiCadClient<T>,
    filename: &str,
    board: &StableId,
    prepared: &SourceFootprintTemplate,
) -> Result<ObservedFootprintPlacement, kicad_adapter::BoardGraphicsCommitOutcome> {
    use kicad_adapter::BoardGraphicsCommitOutcome as Outcome;
    let unknown = |e: String| Outcome::Unknown { reason: e };
    let commit = client.begin_commit().map_err(|e| unknown(e.to_string()))?;
    let p = &prepared.placement;
    let receipt = match client.create_footprint_instance(
        filename,
        &prepared.template,
        p.position.x,
        p.position.y,
        &p.reference,
        &p.value,
    ) {
        Ok(receipt) => receipt,
        Err(error) => {
            return Err(match client.drop_commit(&commit) {
                Ok(()) => Outcome::Rejected {
                    reason: error.to_string(),
                },
                Err(drop_error) => unknown(format!("{error}; rollback failed: {drop_error}")),
            });
        }
    };
    client
        .commit_changes(&commit, "auto-kicad source-bound footprint placement")
        .map_err(|e| unknown(e.to_string()))?;
    client
        .save_board(filename)
        .map_err(|e| unknown(e.to_string()))?;
    let mut items = client
        .get_footprint_instances(filename)
        .map_err(|e| unknown(e.to_string()))?
        .into_iter()
        .filter(|item| item.source_uuid == receipt.source_uuid);
    let footprint = items
        .next()
        .ok_or_else(|| unknown("created footprint is missing".into()))?;
    if items.next().is_some() {
        return Err(unknown("created footprint identity is ambiguous".into()));
    }
    let pads = client
        .get_footprint_definition_pads(filename, &receipt.source_uuid)
        .map_err(|e| unknown(e.to_string()))?;
    verify_created(
        prepared,
        ObservedFootprintWithPads { footprint, pads },
        board,
    )
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
