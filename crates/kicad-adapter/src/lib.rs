pub mod additive_identities;
pub mod copper_connectivity;
pub mod geometry;
pub mod geometry_additions;
pub mod geometry_candidate;
mod geometry_native;
mod geometry_summary;
pub mod refill_verification;
pub mod teardrop_refill;
pub mod zone_edits;
use kicad_engine::{
    MAX_BOARD_POSITION_NM, MAX_GRAPHIC_BOUNDING_AREA_NM2, MAX_GRAPHIC_SPAN_NM, Operation,
    OperationKind, expand_board_outline, validate_observed_routing_net,
    validate_routing_track_binding,
};
use kicad_ipc::{
    BoardGraphicLayer as IpcBoardGraphicLayer, BoardGraphicSegment as IpcBoardGraphicSegment,
    CommitId, CreateNetIdentity, CreateThroughVia, CreateTrack, CreatedBoardGraphicReceipt,
    CreatedThroughViaReceipt, CreatedTrackReceipt, FootprintSide as IpcFootprintSide,
    InstanceToken, KiCadClient, KiCadTransport, ObservedBoardGraphicSegment,
    ObservedBoardLayer as IpcBoardLayer, ObservedBoardRoutingItems as IpcBoardRoutingItems,
    ObservedBoardStackup as IpcBoardStackup, ObservedDrillShape as IpcDrillShape,
    ObservedFootprintInstance as IpcFootprintInstance, ObservedLockState as IpcLockState,
    ObservedPad as IpcPad, ObservedPadKind as IpcPadKind, ObservedPadShape as IpcPadShape,
    ObservedPadStackType as IpcPadStackType, ObservedTrack as IpcTrack,
    ObservedTrackGeometry as IpcTrackGeometry, ObservedVia as IpcVia, OpenDocument,
    OpenDocumentKind, PcbItemDiagnostic, PcbItemKind, UpdatedFootprintInstanceReceipt, VersionInfo,
};
use kicad_model::{
    Board, BoardGraphicLayer, BoardGraphicSegment, BoardLayer, BoardSide, CapabilityObservation,
    CapabilityStatus, Coordinate, Diagnostic, DiscoveredDocument, DiscoveredDocumentKind,
    DiscoverySnapshot, DocumentSpace, Drill, DrillShape, ExactAngleDegrees, ExactFiniteFloat,
    ExactFiniteRgba, FiniteRatio, FootprintLibraryIdentifier, LocalCoordinateNm, LockState,
    ModelError, ObservedBoardFootprints, ObservedBoardRouting, ObservedBoardStackup,
    ObservedFootprintPlacement, ObservedNetIdentity, ObservedPad, ObservedPadStackOuterLayer,
    ObservedPostMachiningProperties, ObservedRoutingNet, ObservedSchematicSymbol,
    ObservedSolderMaskOverrides, ObservedSolderPasteOverrides, ObservedStackupDielectricProperties,
    ObservedStackupEdge, ObservedStackupFinish, ObservedStackupImpedance, ObservedStackupLayer,
    ObservedStackupLayerIdentity, ObservedStackupLayerType, ObservedThermalSpokeSettings,
    ObservedTrack, ObservedTrackGeometry, ObservedVia, ObservedViaDrill, ObservedViaPadStack,
    ObservedZoneConnectionSettings, PadCopperShape, PadKind, PadShape, PadStackType,
    ProjectSnapshot, RevisionDigest, Schematic, SizeNm, SolderMaskMode, SolderPasteMode, StableId,
    UnconnectedLayerRemoval, Unit, ViaCoveringMode, ViaDrillCappingMode, ViaDrillFillingMode,
    ViaDrillPostMachiningMode, ViaPluggingMode, ViaType, ZoneConnectionStyle, board_net_id,
    board_track_id, board_via_id, footprint_definition_id, footprint_pad_id,
    footprint_placement_id,
};
use kicad_schematic::{NetlistDocument, ParsedSchematicProject, join_netlist_connectivity};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Convert a validated domain copper layer to its native protocol identity.
pub fn native_copper_layer(layer: kicad_model::CopperLayer) -> kicad_ipc::CopperLayer {
    match layer.board_layer() {
        BoardLayer::FrontCopper => kicad_ipc::CopperLayer::FRONT,
        BoardLayer::BackCopper => kicad_ipc::CopperLayer::BACK,
        BoardLayer::InnerCopper(index) => {
            kicad_ipc::CopperLayer::inner(index).expect("domain copper indices are bounded")
        }
        _ => unreachable!("domain CopperLayer excludes non-copper values"),
    }
}

pub mod bindings;
pub mod footprint;
pub mod library_catalog;
pub mod library_table;
pub mod lifecycle;
pub mod live_geometry;
pub mod outline;
pub mod routing_rules;
pub mod schematic_audit;
pub use footprint::{
    AllowlistedFootprint, DisabledFootprintRunner, DisposableFootprintContext,
    DisposableFootprintRunner, FootprintBackendError, FootprintPlacementReceipt, FootprintRegistry,
    FootprintSourceKind, LibraryProvenance, PreparedFootprintPlacement, ResolutionEvidence,
    ResolvedFootprint, validate_placement,
};
pub use lifecycle::{
    DisposablePcbLifecycle, FakeDisposablePcbLifecycle, FilesystemDisposablePcbLifecycle,
    KICAD_PCBNEW_CANDIDATES, LifecycleError, OwnedEditorIdentity, OwnedPcbnewLauncher,
    ShadowProject,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentObservation {
    Documents(Vec<OpenDocument>),
    Unsupported(String),
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryInput {
    pub endpoint: String,
    pub version: VersionInfo,
    pub schematic: DocumentObservation,
    pub pcb: DocumentObservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectArtifacts {
    pub project_dir: PathBuf,
    pub project_name: String,
    pub project_file: PathBuf,
    pub schematic_file: Option<PathBuf>,
    pub board_file: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ArtifactError {
    InvalidProjectDirectory,
    ProjectNotFound,
    AmbiguousProject,
    Io(std::io::Error),
    InvalidProjectJson(serde_json::Error),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProjectDirectory => formatter.write_str("invalid project directory"),
            Self::ProjectNotFound => formatter.write_str("KiCad project not found"),
            Self::AmbiguousProject => formatter.write_str("multiple KiCad projects found"),
            Self::Io(error) => write!(formatter, "artifact I/O failed: {error}"),
            Self::InvalidProjectJson(error) => {
                write!(formatter, "invalid KiCad project JSON: {error}")
            }
        }
    }
}

impl std::error::Error for ArtifactError {}

pub fn discover_project_artifacts(project_dir: &Path) -> Result<ProjectArtifacts, ArtifactError> {
    if !project_dir.is_absolute() || !project_dir.is_dir() {
        return Err(ArtifactError::InvalidProjectDirectory);
    }
    let mut projects = fs::read_dir(project_dir)
        .map_err(ArtifactError::Io)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "kicad_pro")
        })
        .collect::<Vec<_>>();
    projects.sort();
    let project_file = match projects.as_slice() {
        [] => return Err(ArtifactError::ProjectNotFound),
        [project] => project.clone(),
        _ => return Err(ArtifactError::AmbiguousProject),
    };
    let project_content = fs::read_to_string(&project_file).map_err(ArtifactError::Io)?;
    serde_json::from_str::<serde_json::Value>(&project_content)
        .map_err(ArtifactError::InvalidProjectJson)?;
    let project_name = project_file
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or(ArtifactError::ProjectNotFound)?
        .to_owned();
    let schematic = project_dir.join(format!("{project_name}.kicad_sch"));
    let board = project_dir.join(format!("{project_name}.kicad_pcb"));
    let schematic_file = validate_artifact(&schematic, "(kicad_sch")?;
    let board_file = validate_artifact(&board, "(kicad_pcb")?;
    Ok(ProjectArtifacts {
        project_dir: project_dir.to_path_buf(),
        project_name,
        project_file,
        schematic_file,
        board_file,
    })
}

fn validate_artifact(path: &Path, expected_root: &str) -> Result<Option<PathBuf>, ArtifactError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut file = fs::File::open(path).map_err(ArtifactError::Io)?;
    let mut prefix = [0_u8; 64];
    let length = file.read(&mut prefix).map_err(ArtifactError::Io)?;
    let content = std::str::from_utf8(&prefix[..length]).unwrap_or_default();
    if !content.trim_start().starts_with(expected_root) {
        return Err(ArtifactError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected KiCad artifact root: {}", path.display()),
        )));
    }
    Ok(Some(path.to_path_buf()))
}

pub fn artifact_discovery_snapshot(
    project_dir: &Path,
    endpoint: &str,
    version: VersionInfo,
) -> Result<DiscoverySnapshot, ArtifactError> {
    let artifacts = discover_project_artifacts(project_dir)?;
    let schematic = artifacts.schematic_file.as_ref().map(|file| OpenDocument {
        project_name: Some(artifacts.project_name.clone()),
        project_path: Some(artifacts.project_dir.display().to_string()),
        kind: OpenDocumentKind::Schematic {
            filename: Some(file.display().to_string()),
            sheet_path: None,
            sheet_kiids: Vec::new(),
        },
    });
    let pcb = artifacts.board_file.as_ref().map(|file| OpenDocument {
        project_name: Some(artifacts.project_name.clone()),
        project_path: Some(artifacts.project_dir.display().to_string()),
        kind: OpenDocumentKind::Pcb {
            board_filename: file.display().to_string(),
        },
    });
    let mut snapshot = discovery_snapshot(DiscoveryInput {
        endpoint: endpoint.to_owned(),
        version,
        schematic: schematic.map_or_else(
            || DocumentObservation::Unsupported("schematic artifact not found".to_owned()),
            |document| DocumentObservation::Documents(vec![document]),
        ),
        pcb: pcb.map_or_else(
            || DocumentObservation::Unsupported("PCB artifact not found".to_owned()),
            |document| DocumentObservation::Documents(vec![document]),
        ),
    })
    .map_err(|error| ArtifactError::Io(std::io::Error::other(error.to_string())))?;
    for capability in &mut snapshot.capabilities {
        if matches!(
            capability.id.as_str(),
            "schematic.documents.discover" | "pcb.documents.discover"
        ) {
            capability.detail = Some("offline artifact source".to_owned());
        }
    }
    snapshot.diagnostics.push(Diagnostic {
        code: "offline_artifact_source".to_owned(),
        message: artifacts.project_file.display().to_string(),
    });
    Ok(snapshot.canonical())
}

pub fn discovery_snapshot(input: DiscoveryInput) -> Result<DiscoverySnapshot, ModelError> {
    let mut documents = Vec::new();
    let mut capabilities = vec![CapabilityObservation {
        id: "base.get_version".to_owned(),
        status: CapabilityStatus::Verified,
        detail: None,
    }];
    let mut diagnostics = Vec::new();
    collect_observation(
        "schematic.documents.discover",
        input.schematic,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    collect_observation(
        "pcb.documents.discover",
        input.pcb,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    capabilities.push(CapabilityObservation {
        id: "schematic.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    capabilities.push(CapabilityObservation {
        id: "pcb.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    diagnostics.push(Diagnostic {
        code: "content_not_observed".to_owned(),
        message: "document discovery does not include schematic or board items".to_owned(),
    });
    Ok(DiscoverySnapshot {
        kicad_version: format!(
            "{}.{}.{}",
            input.version.major, input.version.minor, input.version.patch
        ),
        endpoint: input.endpoint,
        documents,
        capabilities,
        diagnostics,
    }
    .canonical())
}

fn collect_observation(
    capability_id: &str,
    observation: DocumentObservation,
    documents: &mut Vec<DiscoveredDocument>,
    capabilities: &mut Vec<CapabilityObservation>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ModelError> {
    match observation {
        DocumentObservation::Documents(observed) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Verified,
                detail: None,
            });
            for document in observed {
                match discovered_document(document)? {
                    Some(document) => {
                        if document.project_name.is_none() || document.project_path.is_none() {
                            diagnostics.push(Diagnostic {
                                code: "project_identity_incomplete".to_owned(),
                                message: document.document_id.to_string(),
                            });
                        }
                        documents.push(document);
                    }
                    None => diagnostics.push(Diagnostic {
                        code: "document_identity_unknown".to_owned(),
                        message: capability_id.to_owned(),
                    }),
                }
            }
        }
        DocumentObservation::Unsupported(detail) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unsupported,
                detail: Some(detail),
            });
        }
        DocumentObservation::Failed { code, message } => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unknown,
                detail: Some(code.clone()),
            });
            diagnostics.push(Diagnostic { code, message });
        }
    }
    Ok(())
}

fn push_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&value.len().to_be_bytes());
    output.extend_from_slice(value);
}

fn push_optional(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            push_field(output, value.as_bytes());
        }
        None => output.push(0),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicObservation {
    pub project_id: StableId,
    pub revision: RevisionDigest,
    pub symbols: Vec<ObservedSchematicSymbol>,
    pub capabilities: Vec<CapabilityObservation>,
    pub diagnostics: Vec<Diagnostic>,
    pub baseline_eligible: bool,
}

#[derive(Debug)]
pub enum SchematicObservationError {
    Join(kicad_schematic::ConnectivityJoinError),
}

impl std::fmt::Display for SchematicObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Join(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SchematicObservationError {}

pub fn schematic_observation(
    project: &ParsedSchematicProject,
    netlist: &NetlistDocument,
    project_id: StableId,
) -> Result<SchematicObservation, SchematicObservationError> {
    let symbols = join_netlist_connectivity(project, netlist, &project_id)
        .map_err(SchematicObservationError::Join)?
        .into_iter()
        .map(|component| component.symbol.canonical())
        .collect::<Vec<_>>();
    let locked = project.editor_lock_present || project.revision.editor_lock_present;
    let mut diagnostics = Vec::new();
    if locked {
        diagnostics.push(Diagnostic {
            code: "schematic_editor_lock_present".to_owned(),
            message: "saved schematic observation is stale and cannot be a commit baseline"
                .to_owned(),
        });
    }
    let status = if locked {
        CapabilityStatus::Unknown
    } else {
        CapabilityStatus::Verified
    };
    Ok(SchematicObservation {
        project_id,
        revision: project.revision.digest.clone(),
        symbols,
        capabilities: vec![CapabilityObservation {
            id: "schematic.saved_artifact.read".to_owned(),
            status,
            detail: locked.then_some("editor lock present".to_owned()),
        }],
        diagnostics,
        baseline_eligible: !locked,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObservedFootprintWithPads {
    pub footprint: IpcFootprintInstance,
    pub pads: Vec<IpcPad>,
}

#[derive(Debug)]
pub enum FootprintObservationError {
    DuplicateFootprintId(String),
    DuplicatePadId {
        footprint_id: String,
        pad_id: String,
    },
    PadParentMismatch {
        footprint_id: String,
        pad_id: String,
        actual_parent_id: String,
    },
    Model(ModelError),
}

impl std::fmt::Display for FootprintObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateFootprintId(id) => write!(formatter, "duplicate footprint id: {id}"),
            Self::DuplicatePadId {
                footprint_id,
                pad_id,
            } => write!(
                formatter,
                "duplicate pad id {pad_id} in footprint {footprint_id}"
            ),
            Self::PadParentMismatch {
                footprint_id,
                pad_id,
                actual_parent_id,
            } => write!(
                formatter,
                "pad {pad_id} belongs to footprint {actual_parent_id}, not {footprint_id}"
            ),
            Self::Model(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FootprintObservationError {}

impl From<ModelError> for FootprintObservationError {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

pub fn board_footprint_observation(
    board_id: StableId,
    observed: Vec<ObservedFootprintWithPads>,
) -> Result<ObservedBoardFootprints, FootprintObservationError> {
    let mut footprint_ids = BTreeSet::new();
    let mut placements = Vec::with_capacity(observed.len());
    let unbound_revision = RevisionDigest::new("0".repeat(64))?;

    for group in observed {
        let footprint_source = group.footprint.source_uuid.clone();
        if !footprint_ids.insert(footprint_source.clone()) {
            return Err(FootprintObservationError::DuplicateFootprintId(
                footprint_source,
            ));
        }
        let source_uuid = StableId::new(footprint_source.clone())?;
        let placement_id = footprint_placement_id(&board_id, &source_uuid);
        let mut pad_ids = BTreeSet::new();
        let mut pads = Vec::with_capacity(group.pads.len());
        for pad in group.pads {
            if pad.footprint_source_uuid != footprint_source {
                return Err(FootprintObservationError::PadParentMismatch {
                    footprint_id: footprint_source,
                    pad_id: pad.source_uuid,
                    actual_parent_id: pad.footprint_source_uuid,
                });
            }
            if !pad_ids.insert(pad.source_uuid.clone()) {
                return Err(FootprintObservationError::DuplicatePadId {
                    footprint_id: footprint_source,
                    pad_id: pad.source_uuid,
                });
            }
            pads.push(map_observed_pad(
                &board_id,
                &placement_id,
                pad,
                &group.footprint,
                &unbound_revision,
            )?);
        }
        let library_identifier = FootprintLibraryIdentifier {
            nickname: group.footprint.library_identifier.nickname,
            entry_name: group.footprint.library_identifier.entry_name,
        };
        let definition_id = footprint_definition_id(&library_identifier);
        placements.push(ObservedFootprintPlacement {
            placement_id,
            source_uuid,
            definition_id,
            library_identifier,
            reference: group.footprint.reference,
            value: group.footprint.value,
            position: Coordinate {
                x: group.footprint.position_x_nm,
                y: group.footprint.position_y_nm,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            orientation_mdeg: group.footprint.orientation_mdeg,
            side: map_footprint_side(group.footprint.side),
            lock_state: map_lock_state(group.footprint.lock_state),
            symbol_sheet_path: group
                .footprint
                .symbol_sheet_path
                .into_iter()
                .map(StableId::new)
                .collect::<Result<Vec<_>, _>>()?,
            pads,
            observed_revision: unbound_revision.clone(),
        });
    }

    ObservedBoardFootprints::new(board_id, placements).map_err(Into::into)
}

// KiCad 10 serializes PAD::GetPosition(), including inside footprint definitions.
fn observed_pad_local_position(
    footprint: &IpcFootprintInstance,
    x_nm: i64,
    y_nm: i64,
) -> Result<LocalCoordinateNm, ModelError> {
    let invalid = || ModelError::InvalidFootprintObservation("pad coordinate transform overflows");
    let dx = x_nm
        .checked_sub(footprint.position_x_nm)
        .ok_or_else(invalid)?;
    let dy = y_nm
        .checked_sub(footprint.position_y_nm)
        .ok_or_else(invalid)?;
    if dx.unsigned_abs() > (1_u64 << 52) || dy.unsigned_abs() > (1_u64 << 52) {
        return Err(invalid());
    }
    let angle = (footprint.orientation_mdeg.rem_euclid(360_000) as f64 / 1_000.0).to_radians();
    let x = (dx as f64 * angle.cos() - dy as f64 * angle.sin()).round() as i64;
    let mut y = (dx as f64 * angle.sin() + dy as f64 * angle.cos()).round() as i64;
    if footprint.side == IpcFootprintSide::Back {
        y = -y;
    }
    Ok(LocalCoordinateNm { x_nm: x, y_nm: y })
}

fn map_observed_pad(
    board_id: &StableId,
    placement_id: &StableId,
    pad: IpcPad,
    footprint: &IpcFootprintInstance,
    observed_revision: &RevisionDigest,
) -> Result<ObservedPad, ModelError> {
    let source_uuid = StableId::new(pad.source_uuid)?;
    let copper_shapes = pad
        .copper_shapes
        .into_iter()
        .map(|shape| {
            Ok(PadCopperShape {
                layer: map_board_layer(shape.layer),
                shape: map_pad_shape(shape.shape),
                size: SizeNm {
                    x_nm: shape.size_x_nm,
                    y_nm: shape.size_y_nm,
                },
                offset: LocalCoordinateNm {
                    x_nm: shape.offset_x_nm,
                    y_nm: shape.offset_y_nm,
                },
                corner_rounding_ratio: FiniteRatio::new(shape.corner_rounding_ratio)?,
                chamfer_ratio: FiniteRatio::new(shape.chamfer_ratio)?,
                chamfer_top_left: shape.chamfer_top_left,
                chamfer_top_right: shape.chamfer_top_right,
                chamfer_bottom_left: shape.chamfer_bottom_left,
                chamfer_bottom_right: shape.chamfer_bottom_right,
                trapezoid_delta: LocalCoordinateNm {
                    x_nm: shape.trapezoid_delta_x_nm,
                    y_nm: shape.trapezoid_delta_y_nm,
                },
                custom_anchor_shape: shape.custom_anchor_shape.map(map_pad_shape),
            })
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    let drill = pad.drill.map(|drill| Drill {
        shape: map_drill_shape(drill.shape),
        size: SizeNm {
            x_nm: drill.size_x_nm,
            y_nm: drill.size_y_nm,
        },
    });
    let net = pad.net_name.map(|name| ObservedNetIdentity {
        net_id: board_net_id(board_id, &name),
        name,
    });
    Ok(ObservedPad {
        pad_id: footprint_pad_id(placement_id, &source_uuid),
        source_uuid,
        footprint_placement_id: placement_id.clone(),
        number: pad.number,
        kind: map_pad_kind(pad.kind),
        lock_state: map_lock_state(pad.lock_state),
        position: observed_pad_local_position(footprint, pad.position_x_nm, pad.position_y_nm)?,
        stack_type: map_pad_stack_type(pad.stack_type),
        orientation_mdeg: (pad.orientation_mdeg.rem_euclid(360_000)
            - footprint.orientation_mdeg.rem_euclid(360_000))
        .rem_euclid(360_000),
        layers: pad.layers.into_iter().map(map_board_layer).collect(),
        copper_shapes,
        drill,
        net,
        observed_revision: observed_revision.clone(),
    })
}

fn map_footprint_side(side: IpcFootprintSide) -> BoardSide {
    match side {
        IpcFootprintSide::Front => BoardSide::Front,
        IpcFootprintSide::Back => BoardSide::Back,
    }
}

fn map_lock_state(state: IpcLockState) -> LockState {
    match state {
        IpcLockState::Unknown => LockState::Unknown,
        IpcLockState::Unlocked => LockState::Unlocked,
        IpcLockState::Locked => LockState::Locked,
    }
}

fn map_board_layer(layer: IpcBoardLayer) -> BoardLayer {
    match layer {
        IpcBoardLayer::FrontCopper => BoardLayer::FrontCopper,
        IpcBoardLayer::InnerCopper(index) => BoardLayer::InnerCopper(index),
        IpcBoardLayer::BackCopper => BoardLayer::BackCopper,
        IpcBoardLayer::FrontMask => BoardLayer::FrontMask,
        IpcBoardLayer::BackMask => BoardLayer::BackMask,
        IpcBoardLayer::FrontPaste => BoardLayer::FrontPaste,
        IpcBoardLayer::BackPaste => BoardLayer::BackPaste,
    }
}

fn map_pad_kind(kind: IpcPadKind) -> PadKind {
    match kind {
        IpcPadKind::Unknown => PadKind::Unknown,
        IpcPadKind::PlatedThroughHole => PadKind::PlatedThroughHole,
        IpcPadKind::SurfaceMount => PadKind::SurfaceMount,
        IpcPadKind::EdgeConnector => PadKind::EdgeConnector,
        IpcPadKind::NonPlatedThroughHole => PadKind::NonPlatedThroughHole,
    }
}

fn map_pad_stack_type(stack_type: IpcPadStackType) -> PadStackType {
    match stack_type {
        IpcPadStackType::Unknown => PadStackType::Unknown,
        IpcPadStackType::Normal => PadStackType::Normal,
        IpcPadStackType::FrontInnerBack => PadStackType::FrontInnerBack,
        IpcPadStackType::Custom => PadStackType::Custom,
    }
}

fn map_pad_shape(shape: IpcPadShape) -> PadShape {
    match shape {
        IpcPadShape::Unknown => PadShape::Unknown,
        IpcPadShape::Circle => PadShape::Circle,
        IpcPadShape::Rectangle => PadShape::Rectangle,
        IpcPadShape::Oval => PadShape::Oval,
        IpcPadShape::Trapezoid => PadShape::Trapezoid,
        IpcPadShape::RoundedRectangle => PadShape::RoundedRectangle,
        IpcPadShape::ChamferedRectangle => PadShape::ChamferedRectangle,
        IpcPadShape::Custom => PadShape::Custom,
    }
}

fn map_drill_shape(shape: IpcDrillShape) -> DrillShape {
    match shape {
        IpcDrillShape::Unknown => DrillShape::Unknown,
        IpcDrillShape::Circle => DrillShape::Circle,
        IpcDrillShape::Oblong => DrillShape::Oblong,
        IpcDrillShape::Undefined => DrillShape::Undefined,
    }
}

#[derive(Debug)]
pub enum RoutingObservationError {
    Model(ModelError),
}

impl std::fmt::Display for RoutingObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for RoutingObservationError {}
impl From<ModelError> for RoutingObservationError {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

fn routing_coordinate(point: kicad_ipc::ObservedRoutingPoint) -> Coordinate {
    Coordinate {
        x: point.x_nm,
        y: point.y_nm,
        unit: Unit::Nanometres,
        space: DocumentSpace::Board,
    }
}

fn map_routing_net(
    board_id: &StableId,
    net: Option<kicad_ipc::ObservedRoutingNet>,
) -> Option<ObservedRoutingNet> {
    net.map(|net| ObservedRoutingNet {
        net_id: (!net.name.is_empty()).then(|| board_net_id(board_id, &net.name)),
        code: net.code,
        name: net.name,
    })
}

fn map_routing_drill(drill: kicad_ipc::ObservedViaDrill) -> ObservedViaDrill {
    ObservedViaDrill {
        start_layer: map_board_layer(drill.start_layer),
        end_layer: map_board_layer(drill.end_layer),
        size: SizeNm {
            x_nm: drill.size_x_nm,
            y_nm: drill.size_y_nm,
        },
        shape: map_drill_shape(drill.shape),
        capping: match drill.capping {
            kicad_ipc::ObservedViaDrillCappingMode::Unknown => ViaDrillCappingMode::Unknown,
            kicad_ipc::ObservedViaDrillCappingMode::Capped => ViaDrillCappingMode::Capped,
            kicad_ipc::ObservedViaDrillCappingMode::Uncapped => ViaDrillCappingMode::Uncapped,
            kicad_ipc::ObservedViaDrillCappingMode::FromDesignRules => {
                ViaDrillCappingMode::FromDesignRules
            }
        },
        filling: match drill.filling {
            kicad_ipc::ObservedViaDrillFillingMode::Unknown => ViaDrillFillingMode::Unknown,
            kicad_ipc::ObservedViaDrillFillingMode::Filled => ViaDrillFillingMode::Filled,
            kicad_ipc::ObservedViaDrillFillingMode::Unfilled => ViaDrillFillingMode::Unfilled,
            kicad_ipc::ObservedViaDrillFillingMode::FromDesignRules => {
                ViaDrillFillingMode::FromDesignRules
            }
        },
    }
}

pub fn board_routing_observation(
    board_id: StableId,
    items: IpcBoardRoutingItems,
) -> Result<ObservedBoardRouting, RoutingObservationError> {
    let tracks = items
        .tracks
        .into_iter()
        .map(|track| {
            let source_uuid = StableId::new(track.source_uuid)?;
            let geometry = match track.geometry {
                IpcTrackGeometry::Segment { start, end } => ObservedTrackGeometry::Segment {
                    start: routing_coordinate(start),
                    end: routing_coordinate(end),
                },
                IpcTrackGeometry::Arc { start, mid, end } => ObservedTrackGeometry::Arc {
                    start: routing_coordinate(start),
                    mid: routing_coordinate(mid),
                    end: routing_coordinate(end),
                },
            };
            Ok(ObservedTrack {
                track_id: board_track_id(&board_id, &source_uuid),
                source_uuid,
                geometry,
                width_nm: track.width_nm,
                layer: map_board_layer(track.layer),
                lock_state: map_lock_state(track.lock_state),
                net: map_routing_net(&board_id, track.net),
                observed_revision: RevisionDigest::new("0".repeat(64))?,
            })
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    let vias = items
        .vias
        .into_iter()
        .map(|via| map_routing_via(&board_id, via))
        .collect::<Result<Vec<_>, ModelError>>()?;
    ObservedBoardRouting::new(board_id, tracks, vias).map_err(Into::into)
}

fn map_post_machining(
    properties: kicad_ipc::ObservedPostMachiningProperties,
) -> ObservedPostMachiningProperties {
    ObservedPostMachiningProperties {
        mode: match properties.mode {
            kicad_ipc::ObservedViaDrillPostMachiningMode::Unknown => {
                ViaDrillPostMachiningMode::Unknown
            }
            kicad_ipc::ObservedViaDrillPostMachiningMode::NotPostMachined => {
                ViaDrillPostMachiningMode::NotPostMachined
            }
            kicad_ipc::ObservedViaDrillPostMachiningMode::Counterbore => {
                ViaDrillPostMachiningMode::Counterbore
            }
            kicad_ipc::ObservedViaDrillPostMachiningMode::Countersink => {
                ViaDrillPostMachiningMode::Countersink
            }
        },
        size: properties.size,
        depth: properties.depth,
        angle: properties.angle,
    }
}

fn map_routing_via(board_id: &StableId, via: IpcVia) -> Result<ObservedVia, ModelError> {
    let source_uuid = StableId::new(via.source_uuid)?;
    let stack = via.pad_stack;
    let outer = |outer: kicad_ipc::ObservedPadStackOuterLayer| -> Result<ObservedPadStackOuterLayer, ModelError> {
        Ok(ObservedPadStackOuterLayer {
        solder_mask_mode: match outer.solder_mask_mode {
            kicad_ipc::ObservedSolderMaskMode::Unknown => SolderMaskMode::Unknown,
            kicad_ipc::ObservedSolderMaskMode::Masked => SolderMaskMode::Masked,
            kicad_ipc::ObservedSolderMaskMode::Unmasked => SolderMaskMode::Unmasked,
            kicad_ipc::ObservedSolderMaskMode::FromDesignRules => SolderMaskMode::FromDesignRules,
        },
        solder_paste_mode: match outer.solder_paste_mode {
            kicad_ipc::ObservedSolderPasteMode::Unknown => SolderPasteMode::Unknown,
            kicad_ipc::ObservedSolderPasteMode::Paste => SolderPasteMode::Paste,
            kicad_ipc::ObservedSolderPasteMode::NoPaste => SolderPasteMode::NoPaste,
            kicad_ipc::ObservedSolderPasteMode::FromDesignRules => SolderPasteMode::FromDesignRules,
        },
        solder_mask_settings: outer
            .solder_mask_settings
            .map(|v| ObservedSolderMaskOverrides {
                margin_nm: v.margin_nm,
            }),
        solder_paste_settings: outer
            .solder_paste_settings
            .map(|v| {
                Ok::<_, ModelError>(ObservedSolderPasteOverrides {
                    margin_nm: v.margin_nm,
                    margin_ratio: v.margin_ratio.map(FiniteRatio::new).transpose()?,
                })
            })
            .transpose()?,
        plugging_mode: match outer.plugging_mode {
            kicad_ipc::ObservedViaPluggingMode::Unknown => ViaPluggingMode::Unknown,
            kicad_ipc::ObservedViaPluggingMode::Plugged => ViaPluggingMode::Plugged,
            kicad_ipc::ObservedViaPluggingMode::Unplugged => ViaPluggingMode::Unplugged,
            kicad_ipc::ObservedViaPluggingMode::FromDesignRules => ViaPluggingMode::FromDesignRules,
        },
        covering_mode: match outer.covering_mode {
            kicad_ipc::ObservedViaCoveringMode::Unknown => ViaCoveringMode::Unknown,
            kicad_ipc::ObservedViaCoveringMode::Covered => ViaCoveringMode::Covered,
            kicad_ipc::ObservedViaCoveringMode::Uncovered => ViaCoveringMode::Uncovered,
            kicad_ipc::ObservedViaCoveringMode::FromDesignRules => ViaCoveringMode::FromDesignRules,
        },
        })
    };
    Ok(ObservedVia {
        via_id: board_via_id(board_id, &source_uuid),
        source_uuid,
        position: routing_coordinate(via.position),
        pad_stack: ObservedViaPadStack {
            stack_type: map_pad_stack_type(stack.stack_type),
            layers: stack.layers.into_iter().map(map_board_layer).collect(),
            primary_drill: stack.primary_drill.map(map_routing_drill),
            unconnected_layer_removal: match stack.unconnected_layer_removal {
                kicad_ipc::ObservedUnconnectedLayerRemoval::Unknown => {
                    UnconnectedLayerRemoval::Unknown
                }
                kicad_ipc::ObservedUnconnectedLayerRemoval::Keep => UnconnectedLayerRemoval::Keep,
                kicad_ipc::ObservedUnconnectedLayerRemoval::Remove => {
                    UnconnectedLayerRemoval::Remove
                }
                kicad_ipc::ObservedUnconnectedLayerRemoval::RemoveExceptStartAndEnd => {
                    UnconnectedLayerRemoval::RemoveExceptStartAndEnd
                }
                kicad_ipc::ObservedUnconnectedLayerRemoval::StartEndOnly => {
                    UnconnectedLayerRemoval::StartEndOnly
                }
            },
            copper_shapes: stack
                .copper_shapes
                .into_iter()
                .map(|shape| {
                    Ok(PadCopperShape {
                        layer: map_board_layer(shape.layer),
                        shape: map_pad_shape(shape.shape),
                        size: SizeNm {
                            x_nm: shape.size_x_nm,
                            y_nm: shape.size_y_nm,
                        },
                        offset: LocalCoordinateNm {
                            x_nm: shape.offset_x_nm,
                            y_nm: shape.offset_y_nm,
                        },
                        corner_rounding_ratio: FiniteRatio::new(shape.corner_rounding_ratio)?,
                        chamfer_ratio: FiniteRatio::new(shape.chamfer_ratio)?,
                        chamfer_top_left: shape.chamfer_top_left,
                        chamfer_top_right: shape.chamfer_top_right,
                        chamfer_bottom_left: shape.chamfer_bottom_left,
                        chamfer_bottom_right: shape.chamfer_bottom_right,
                        trapezoid_delta: LocalCoordinateNm {
                            x_nm: shape.trapezoid_delta_x_nm,
                            y_nm: shape.trapezoid_delta_y_nm,
                        },
                        custom_anchor_shape: shape.custom_anchor_shape.map(map_pad_shape),
                    })
                })
                .collect::<Result<Vec<_>, ModelError>>()?,
            angle_degrees: ExactAngleDegrees::new(stack.angle_degrees)?,
            front_outer_layers: stack.front_outer_layers.map(outer).transpose()?,
            back_outer_layers: stack.back_outer_layers.map(outer).transpose()?,
            zone_settings: stack
                .zone_settings
                .map(|settings| {
                    Ok::<_, ModelError>(ObservedZoneConnectionSettings {
                        style: match settings.style {
                            kicad_ipc::ObservedZoneConnectionStyle::Unknown => {
                                ZoneConnectionStyle::Unknown
                            }
                            kicad_ipc::ObservedZoneConnectionStyle::Inherited => {
                                ZoneConnectionStyle::Inherited
                            }
                            kicad_ipc::ObservedZoneConnectionStyle::None => {
                                ZoneConnectionStyle::None
                            }
                            kicad_ipc::ObservedZoneConnectionStyle::Thermal => {
                                ZoneConnectionStyle::Thermal
                            }
                            kicad_ipc::ObservedZoneConnectionStyle::Full => {
                                ZoneConnectionStyle::Full
                            }
                            kicad_ipc::ObservedZoneConnectionStyle::PlatedThroughHoleThermal => {
                                ZoneConnectionStyle::PlatedThroughHoleThermal
                            }
                        },
                        thermal_spokes: settings
                            .thermal_spokes
                            .map(|spokes| {
                                Ok::<_, ModelError>(ObservedThermalSpokeSettings {
                                    width_nm: spokes.width_nm,
                                    angle_degrees: ExactAngleDegrees::new(spokes.angle_degrees)?,
                                    gap_nm: spokes.gap_nm,
                                })
                            })
                            .transpose()?,
                    })
                })
                .transpose()?,
            secondary_drill: stack.secondary_drill.map(map_routing_drill),
            tertiary_drill: stack.tertiary_drill.map(map_routing_drill),
            front_post_machining: stack.front_post_machining.map(map_post_machining),
            back_post_machining: stack.back_post_machining.map(map_post_machining),
        },
        lock_state: map_lock_state(via.lock_state),
        net: map_routing_net(board_id, via.net),
        via_type: match via.via_type {
            kicad_ipc::ObservedViaType::Unknown => ViaType::Unknown,
            kicad_ipc::ObservedViaType::Through => ViaType::Through,
            kicad_ipc::ObservedViaType::BlindBuried => ViaType::BlindBuried,
            kicad_ipc::ObservedViaType::Micro => ViaType::Micro,
            kicad_ipc::ObservedViaType::Blind => ViaType::Blind,
            kicad_ipc::ObservedViaType::Buried => ViaType::Buried,
        },
        observed_revision: RevisionDigest::new("0".repeat(64))?,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveBoardObservation {
    pub snapshot: ProjectSnapshot,
    pub instance_token: InstanceToken,
    pub board_filename: String,
    pub board_path: PathBuf,
    pub project_dir: PathBuf,
}

#[derive(Debug)]
pub enum LiveBoardObservationError {
    NonPcbDocument,
    MissingProjectIdentity,
    MissingDocumentIdentity,
    DuplicateGraphicId(String),
    UnsupportedGraphicLayer,
    Model(ModelError),
}

impl std::fmt::Display for LiveBoardObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonPcbDocument => formatter.write_str("open document is not a PCB"),
            Self::MissingProjectIdentity => formatter.write_str("project identity is unavailable"),
            Self::MissingDocumentIdentity => {
                formatter.write_str("PCB document identity is unavailable")
            }
            Self::DuplicateGraphicId(id) => write!(formatter, "duplicate board graphic id: {id}"),
            Self::UnsupportedGraphicLayer => formatter.write_str("unsupported board graphic layer"),
            Self::Model(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LiveBoardObservationError {}

impl From<ModelError> for LiveBoardObservationError {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

fn map_stackup_connector_type(
    value: kicad_ipc::StackupConnectorType,
) -> kicad_model::StackupConnectorType {
    match value {
        kicad_ipc::StackupConnectorType::Unknown => kicad_model::StackupConnectorType::Unknown,
        kicad_ipc::StackupConnectorType::None => kicad_model::StackupConnectorType::None,
        kicad_ipc::StackupConnectorType::Plain => kicad_model::StackupConnectorType::Plain,
        kicad_ipc::StackupConnectorType::Beveled => kicad_model::StackupConnectorType::Beveled,
    }
}

fn map_stackup_dielectric_type(
    value: kicad_ipc::StackupDielectricType,
) -> kicad_model::StackupDielectricType {
    match value {
        kicad_ipc::StackupDielectricType::Unknown => kicad_model::StackupDielectricType::Unknown,
        kicad_ipc::StackupDielectricType::None => kicad_model::StackupDielectricType::None,
        kicad_ipc::StackupDielectricType::Core => kicad_model::StackupDielectricType::Core,
        kicad_ipc::StackupDielectricType::Prepreg => kicad_model::StackupDielectricType::Prepreg,
    }
}

fn map_stackup_dielectric_model(
    value: kicad_ipc::StackupDielectricModel,
) -> kicad_model::StackupDielectricModel {
    match value {
        kicad_ipc::StackupDielectricModel::Unknown => kicad_model::StackupDielectricModel::Unknown,
        kicad_ipc::StackupDielectricModel::Constant => {
            kicad_model::StackupDielectricModel::Constant
        }
        kicad_ipc::StackupDielectricModel::DjordjevicSarkar => {
            kicad_model::StackupDielectricModel::DjordjevicSarkar
        }
    }
}

pub fn board_stackup_observation(
    board_id: StableId,
    stackup: IpcBoardStackup,
) -> Result<ObservedBoardStackup, ModelError> {
    let layer_identity = |value| match value {
        kicad_ipc::ObservedStackupLayerIdentity::Undefined => {
            ObservedStackupLayerIdentity::Undefined
        }
        kicad_ipc::ObservedStackupLayerIdentity::FrontCopper => {
            ObservedStackupLayerIdentity::FrontCopper
        }
        kicad_ipc::ObservedStackupLayerIdentity::InnerCopper(index) => {
            ObservedStackupLayerIdentity::InnerCopper(index)
        }
        kicad_ipc::ObservedStackupLayerIdentity::BackCopper => {
            ObservedStackupLayerIdentity::BackCopper
        }
        kicad_ipc::ObservedStackupLayerIdentity::BackPaste => {
            ObservedStackupLayerIdentity::BackPaste
        }
        kicad_ipc::ObservedStackupLayerIdentity::FrontPaste => {
            ObservedStackupLayerIdentity::FrontPaste
        }
        kicad_ipc::ObservedStackupLayerIdentity::BackSilkscreen => {
            ObservedStackupLayerIdentity::BackSilkscreen
        }
        kicad_ipc::ObservedStackupLayerIdentity::FrontSilkscreen => {
            ObservedStackupLayerIdentity::FrontSilkscreen
        }
        kicad_ipc::ObservedStackupLayerIdentity::BackSoldermask => {
            ObservedStackupLayerIdentity::BackSoldermask
        }
        kicad_ipc::ObservedStackupLayerIdentity::FrontSoldermask => {
            ObservedStackupLayerIdentity::FrontSoldermask
        }
    };
    let layer_type = |value| match value {
        kicad_ipc::ObservedStackupLayerType::Unknown => ObservedStackupLayerType::Unknown,
        kicad_ipc::ObservedStackupLayerType::Copper => ObservedStackupLayerType::Copper,
        kicad_ipc::ObservedStackupLayerType::Dielectric => ObservedStackupLayerType::Dielectric,
        kicad_ipc::ObservedStackupLayerType::Silkscreen => ObservedStackupLayerType::Silkscreen,
        kicad_ipc::ObservedStackupLayerType::Soldermask => ObservedStackupLayerType::Soldermask,
        kicad_ipc::ObservedStackupLayerType::Solderpaste => ObservedStackupLayerType::Solderpaste,
        kicad_ipc::ObservedStackupLayerType::Undefined => ObservedStackupLayerType::Undefined,
    };
    let finish = stackup.finish.map(|finish| ObservedStackupFinish {
        type_name: finish.type_name,
    });
    let impedance = stackup.impedance.map(|impedance| ObservedStackupImpedance {
        is_controlled: impedance.is_controlled,
    });
    let edge = stackup.edge.map(|edge| ObservedStackupEdge {
        connector_present: edge.connector.is_some(),
        connector_type: edge.connector.map(map_stackup_connector_type),
        castellation: edge.castellation,
        plating: edge.plating,
    });
    let layers = stackup
        .layers
        .into_iter()
        .map(|layer| {
            let thickness_nm = layer
                .thickness_nm
                .ok_or(ModelError::InvalidStackupObservation(
                    "required layer thickness is missing",
                ))?;
            let color = layer
                .color
                .map(|color| {
                    Ok(ExactFiniteRgba {
                        red: ExactFiniteFloat::new(color.red)?,
                        green: ExactFiniteFloat::new(color.green)?,
                        blue: ExactFiniteFloat::new(color.blue)?,
                        alpha: ExactFiniteFloat::new(color.alpha)?,
                    })
                })
                .transpose()?;
            let dielectric = layer
                .dielectric
                .map(|properties| {
                    properties
                        .into_iter()
                        .map(|property| {
                            Ok(ObservedStackupDielectricProperties {
                                thickness_locked: property.thickness_locked,
                                spec_frequency: property
                                    .spec_frequency
                                    .map(ExactFiniteFloat::new)
                                    .transpose()?,
                                dielectric_model: map_stackup_dielectric_model(
                                    property.dielectric_model,
                                ),
                                epsilon_r: ExactFiniteFloat::new(property.epsilon_r)?,
                                loss_tangent: ExactFiniteFloat::new(property.loss_tangent)?,
                                material_name: property.material_name,
                                thickness_nm: property.thickness_nm.ok_or(
                                    ModelError::InvalidStackupObservation(
                                        "required dielectric thickness is missing",
                                    ),
                                )?,
                            })
                        })
                        .collect::<Result<Vec<_>, ModelError>>()
                })
                .transpose()?;
            Ok(ObservedStackupLayer {
                dielectric_type: layer.dielectric_type.map(map_stackup_dielectric_type),
                soldermask: layer
                    .soldermask
                    .map(|mask| {
                        Ok::<_, ModelError>(kicad_model::ObservedStackupSoldermask {
                            epsilon_r: ExactFiniteFloat::new(mask.epsilon_r)?,
                            loss_tangent: ExactFiniteFloat::new(mask.loss_tangent)?,
                            material_name: mask.material_name,
                            thickness_nm: mask.thickness_nm,
                        })
                    })
                    .transpose()?,
                silkscreen_material: layer.silkscreen_material,
                layer: layer_identity(layer.layer),
                enabled: layer.enabled,
                layer_type: layer_type(layer.layer_type),
                thickness_nm,
                material_name: layer.material_name,
                user_name: layer.user_name,
                color,
                dielectric,
            })
        })
        .collect::<Result<Vec<_>, ModelError>>()?;
    ObservedBoardStackup::new(board_id, finish, impedance, edge, layers)
}

pub fn live_board_observation(
    version: VersionInfo,
    document: OpenDocument,
    graphics: Vec<ObservedBoardGraphicSegment>,
    native_footprints: ObservedBoardFootprints,
    native_routing: ObservedBoardRouting,
    native_stackup: ObservedBoardStackup,
    instance_token: InstanceToken,
) -> Result<LiveBoardObservation, LiveBoardObservationError> {
    let board_filename = match &document.kind {
        OpenDocumentKind::Pcb { board_filename } if !board_filename.is_empty() => {
            board_filename.clone()
        }
        OpenDocumentKind::Pcb { .. } => {
            return Err(LiveBoardObservationError::MissingDocumentIdentity);
        }
        _ => return Err(LiveBoardObservationError::NonPcbDocument),
    };
    if document.project_path.is_none() && document.project_name.is_none() {
        return Err(LiveBoardObservationError::MissingProjectIdentity);
    }
    let project_path = document
        .project_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(LiveBoardObservationError::MissingProjectIdentity)?;
    let board_path = {
        let filename = Path::new(&board_filename);
        if filename.is_absolute() {
            filename.to_owned()
        } else {
            project_path.join(filename)
        }
    };
    let discovered = discovered_document(document.clone())?
        .ok_or(LiveBoardObservationError::MissingDocumentIdentity)?;
    let mut project_identity = Vec::new();
    push_optional(&mut project_identity, document.project_path.as_deref());
    push_optional(&mut project_identity, document.project_name.as_deref());
    let project_id = StableId::new(format!(
        "project:sha256:{}",
        hex::encode(Sha256::digest(&project_identity))
    ))?;
    let mut graphic_ids = BTreeSet::new();
    let graphics = graphics
        .into_iter()
        .map(|observed| {
            if !graphic_ids.insert(observed.graphic_id.clone()) {
                return Err(LiveBoardObservationError::DuplicateGraphicId(
                    observed.graphic_id,
                ));
            }
            let layer = match observed.segment.layer {
                IpcBoardGraphicLayer::DwgsUser => BoardGraphicLayer::DwgsUser,
                IpcBoardGraphicLayer::EdgeCuts => BoardGraphicLayer::EdgeCuts,
                IpcBoardGraphicLayer::FSilkS => {
                    return Err(LiveBoardObservationError::UnsupportedGraphicLayer);
                }
            };
            let graphic = BoardGraphicSegment {
                graphic_id: StableId::new(observed.graphic_id)?,
                start: Coordinate {
                    x: observed.segment.start_x_nm,
                    y: observed.segment.start_y_nm,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                end: Coordinate {
                    x: observed.segment.end_x_nm,
                    y: observed.segment.end_y_nm,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                width_nm: observed.segment.width_nm,
                layer,
            };
            graphic.validate()?;
            Ok(graphic)
        })
        .collect::<Result<Vec<_>, LiveBoardObservationError>>()?;
    native_footprints.validate()?;
    if native_footprints.board_id != discovered.document_id {
        return Err(ModelError::InvalidFootprintObservation(
            "native footprint board identity does not match the live board",
        )
        .into());
    }
    native_routing.validate()?;
    if native_routing.board_id != discovered.document_id {
        return Err(ModelError::InvalidRoutingObservation(
            "native routing board identity does not match the live board",
        )
        .into());
    }
    native_stackup.validate()?;
    if native_stackup.board_id != discovered.document_id {
        return Err(ModelError::InvalidStackupObservation(
            "native stackup board identity does not match the live board",
        )
        .into());
    }
    let snapshot = ProjectSnapshot {
        opaque_graphics: Vec::new(),
        project_id,
        kicad_version: version.full_version,
        capabilities: vec![
            "pcb.board_graphics.read".to_owned(),
            "pcb.board_graphics.write".to_owned(),
            "pcb.native_footprints.read".to_owned(),
            "pcb.native_routing.read".to_owned(),
            "pcb.stackup.read".to_owned(),
        ],
        schematic: Schematic::default(),
        board: Board {
            document_id: Some(discovered.document_id),
            footprints: Vec::new(),
            graphics,
        },
        native_footprints: Some(native_footprints),
        native_routing: Some(native_routing),
        native_stackup: Some(native_stackup),
    }
    .canonical();
    snapshot.validate()?;
    Ok(LiveBoardObservation {
        snapshot,
        instance_token,
        board_filename,
        board_path,
        project_dir: project_path,
    })
}

pub fn board_document_id(document: &OpenDocument) -> Result<StableId, LiveBoardObservationError> {
    match &document.kind {
        OpenDocumentKind::Pcb { board_filename } if !board_filename.is_empty() => {}
        OpenDocumentKind::Pcb { .. } => {
            return Err(LiveBoardObservationError::MissingDocumentIdentity);
        }
        _ => return Err(LiveBoardObservationError::NonPcbDocument),
    }
    discovered_document(document.clone())?
        .map(|document| document.document_id)
        .ok_or(LiveBoardObservationError::MissingDocumentIdentity)
}

fn discovered_document(document: OpenDocument) -> Result<Option<DiscoveredDocument>, ModelError> {
    let kind = match document.kind {
        OpenDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            if filename.is_none() && sheet_path.is_none() && sheet_kiids.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Schematic {
                filename,
                sheet_path,
                sheet_kiids,
            }
        }
        OpenDocumentKind::Pcb { board_filename } => {
            if board_filename.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Pcb {
                filename: board_filename,
            }
        }
        OpenDocumentKind::Other => return Ok(None),
    };
    let mut identity = Vec::new();
    push_optional(&mut identity, document.project_path.as_deref());
    push_optional(&mut identity, document.project_name.as_deref());
    match &kind {
        DiscoveredDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            push_field(&mut identity, b"schematic");
            push_optional(&mut identity, filename.as_deref());
            push_optional(&mut identity, sheet_path.as_deref());
            for kiid in sheet_kiids {
                push_field(&mut identity, kiid.as_bytes());
            }
        }
        DiscoveredDocumentKind::Pcb { filename } => {
            push_field(&mut identity, b"pcb");
            push_field(&mut identity, filename.as_bytes());
        }
        DiscoveredDocumentKind::Unknown => push_field(&mut identity, b"unknown"),
    }
    let digest = Sha256::digest(&identity);
    Ok(Some(DiscoveredDocument {
        document_id: StableId::new(format!("document:sha256:{}", hex::encode(digest)))?,
        project_name: document.project_name,
        project_path: document.project_path,
        kind,
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoardGraphicsCommitOutcome {
    CommittedAndSaved,
    CommittedButNotSaved { reason: String },
    Rejected { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardGraphicsExecutor {
    pub max_segments: usize,
}

impl Default for BoardGraphicsExecutor {
    fn default() -> Self {
        Self { max_segments: 100 }
    }
}

pub trait BoardGraphicsIpc {
    fn begin_commit(&mut self) -> Result<String, String>;
    fn create_board_graphics(
        &mut self,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
    ) -> Result<Vec<CreatedBoardGraphicReceipt>, String>;
    fn create_track(
        &mut self,
        board_filename: &str,
        track: &CreateTrack,
    ) -> Result<CreatedTrackReceipt, String>;
    fn create_through_via(
        &mut self,
        board_filename: &str,
        via: &CreateThroughVia,
    ) -> Result<CreatedThroughViaReceipt, String>;
    fn create_track_with_identity(
        &mut self,
        _filename: &str,
        _track: &CreateTrack,
        _id: &str,
    ) -> Result<CreatedTrackReceipt, String> {
        Err("explicit track identity creation is unavailable".into())
    }
    fn create_through_via_with_identity(
        &mut self,
        _filename: &str,
        _via: &CreateThroughVia,
        _id: &str,
    ) -> Result<CreatedThroughViaReceipt, String> {
        Err("explicit via identity creation is unavailable".into())
    }
    fn commit_changes(&mut self, commit: &str, message: &str) -> Result<(), String>;
    fn drop_commit(&mut self, commit: &str) -> Result<(), String>;
    fn save_board(&mut self, board_filename: &str) -> Result<(), String>;
    fn read_board_graphics(
        &mut self,
        board_filename: &str,
        layer: IpcBoardGraphicLayer,
    ) -> Result<Vec<ObservedBoardGraphicSegment>, String>;
    fn read_board_graphics_by_id(
        &mut self,
        board_filename: &str,
        graphic_ids: &[String],
        layer: IpcBoardGraphicLayer,
    ) -> Result<Vec<ObservedBoardGraphicSegment>, String>;
    fn read_board_routing(&mut self, board_filename: &str) -> Result<IpcBoardRoutingItems, String>;
    fn read_board_routing_by_id(
        &mut self,
        board_filename: &str,
        routing_ids: &[String],
    ) -> Result<IpcBoardRoutingItems, String>;
    fn require_supported_footprint_transform(
        &mut self,
        _board_filename: &str,
        _source_uuid: &str,
    ) -> Result<(), String> {
        Err("native footprint metadata support is unavailable".into())
    }
    fn update_footprint_instance(
        &mut self,
        board_filename: &str,
        source_uuid: &str,
        position_x_nm: i64,
        position_y_nm: i64,
        orientation_mdeg: i64,
    ) -> Result<UpdatedFootprintInstanceReceipt, String> {
        let _ = (
            board_filename,
            source_uuid,
            position_x_nm,
            position_y_nm,
            orientation_mdeg,
        );
        Err("existing footprint updates are unsupported by this IPC adapter".into())
    }
    fn read_footprints(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<IpcFootprintInstance>, String> {
        let _ = board_filename;
        Err("footprint observation is unsupported by this IPC adapter".into())
    }
    fn read_footprint_pads(
        &mut self,
        board_filename: &str,
        source_uuid: &str,
    ) -> Result<Vec<IpcPad>, String> {
        let _ = (board_filename, source_uuid);
        Err("pad observation is unsupported by this IPC adapter".into())
    }
    fn read_unsupported_move_copper(&mut self, _filename: &str) -> Result<Vec<String>, String> {
        Err("unsupported copper observation is unavailable".into())
    }
    fn read_pcb_item_diagnostics(
        &mut self,
        board_filename: &str,
        kinds: &[PcbItemKind],
    ) -> Result<Vec<PcbItemDiagnostic>, String> {
        let _ = (board_filename, kinds);
        Err("PCB item diagnostics are unsupported by this IPC adapter".into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePcbBatchReceipt {
    pub graphics: Vec<CreatedBoardGraphicReceipt>,
    pub tracks: Vec<CreatedTrackReceipt>,
    pub vias: Vec<CreatedThroughViaReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePcbBatch {
    pub graphics: Vec<IpcBoardGraphicSegment>,
    pub tracks: Vec<CreateTrack>,
    pub vias: Vec<CreateThroughVia>,
}

fn connected_copper_ids(
    snapshot: &ProjectSnapshot,
    placement_id: &StableId,
    tolerance_nm: i64,
) -> Result<Vec<String>, String> {
    kicad_engine::connected_footprint_copper(snapshot, placement_id, tolerance_nm)
        .map(|ids| ids.iter().map(ToString::to_string).collect())
        .map_err(|e| e.to_string())
}

fn read_move_footprints<I: BoardGraphicsIpc>(
    ipc: &mut I,
    board: &StableId,
    filename: &str,
) -> Result<ObservedBoardFootprints, String> {
    let footprints = ipc.read_footprints(filename)?;
    let mut observed = Vec::with_capacity(footprints.len());
    for footprint in footprints {
        let pads = ipc.read_footprint_pads(filename, &footprint.source_uuid)?;
        observed.push(ObservedFootprintWithPads { footprint, pads });
    }
    board_footprint_observation(board.clone(), observed).map_err(|e| e.to_string())
}

pub fn prepare_bound_native_batch(
    operations: &[Operation],
    snapshot: &ProjectSnapshot,
    expected_revision: &RevisionDigest,
) -> Result<NativePcbBatch, BoardGraphicsCommitOutcome> {
    let reject = |reason: &str| BoardGraphicsCommitOutcome::Rejected {
        reason: reason.into(),
    };
    if snapshot.revision().ok().as_ref() != Some(expected_revision) {
        return Err(reject("board observation revision is stale"));
    }
    let board = snapshot
        .board
        .document_id
        .as_ref()
        .ok_or_else(|| reject("board identity is missing"))?;
    if operations.is_empty() {
        return Err(reject("operation batch is empty"));
    }
    kicad_engine::copper_route::validate_additive_board_identities(snapshot, operations)
        .map_err(|e| reject(&e.to_string()))?;
    for operation in operations {
        if operation.targets.as_slice() != [board.clone()] {
            return Err(reject("operation target differs from observed board"));
        }
        let operation_board = match &operation.kind {
            OperationKind::AddCopperRoute { board, intent, .. } => {
                kicad_engine::compile_copper_route(snapshot, board, intent)
                    .map_err(|e| reject(&e.to_string()))?;
                board
            }
            OperationKind::AddStraightTrack { board, intent } => {
                intent.validate().map_err(|e| reject(&e.to_string()))?;
                validate_routing_track_binding(snapshot, board, intent)
                    .map_err(|e| reject(&e.to_string()))?;
                board
            }
            OperationKind::AddThroughVia { board, intent } => {
                intent.validate().map_err(|e| reject(&e.to_string()))?;
                validate_observed_routing_net(snapshot, intent.net.as_ref())
                    .map_err(|e| reject(&e.to_string()))?;
                board
            }
            OperationKind::AddBoardGraphicSegments { board, .. }
            | OperationKind::AddBoardOutline { board, .. } => board,
            _ => return Err(reject("unsupported native board operation")),
        };
        if operation_board != board {
            return Err(reject("operation board differs from observation"));
        }
    }
    let via_layers = if operations.iter().any(|op| {
        matches!(
            op.kind,
            OperationKind::AddThroughVia { .. } | OperationKind::AddCopperRoute { .. }
        )
    }) {
        match snapshot.native_stackup.as_ref() {
            Some(stackup) => kicad_model::enabled_copper_layers(stackup)
                .map_err(|e| reject(&e.to_string()))?
                .into_iter()
                .map(native_copper_layer)
                .collect(),
            None => vec![kicad_ipc::CopperLayer::FRONT, kicad_ipc::CopperLayer::BACK],
        }
    } else {
        Vec::new()
    };
    let mut graphics = Vec::new();
    let mut tracks = Vec::new();
    let mut vias = Vec::new();
    for operation in operations {
        match &operation.kind {
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                graphics.extend(segments.iter().map(|segment| IpcBoardGraphicSegment {
                    start_x_nm: segment.start.x,
                    start_y_nm: segment.start.y,
                    end_x_nm: segment.end.x,
                    end_y_nm: segment.end.y,
                    width_nm: segment.width_nm,
                    layer: match segment.layer {
                        kicad_model::BoardGraphicLayer::DwgsUser => IpcBoardGraphicLayer::DwgsUser,
                        kicad_model::BoardGraphicLayer::EdgeCuts => IpcBoardGraphicLayer::EdgeCuts,
                    },
                }));
            }
            OperationKind::AddBoardOutline {
                top_left,
                bottom_right,
                width_nm,
                ..
            } => {
                graphics.extend(
                    expand_board_outline(*top_left, *bottom_right, *width_nm)
                        .into_iter()
                        .map(|segment| IpcBoardGraphicSegment {
                            start_x_nm: segment.start.x,
                            start_y_nm: segment.start.y,
                            end_x_nm: segment.end.x,
                            end_y_nm: segment.end.y,
                            width_nm: segment.width_nm,
                            layer: match segment.layer {
                                kicad_model::BoardGraphicLayer::DwgsUser => {
                                    IpcBoardGraphicLayer::DwgsUser
                                }
                                kicad_model::BoardGraphicLayer::EdgeCuts => {
                                    IpcBoardGraphicLayer::EdgeCuts
                                }
                            },
                        }),
                );
            }
            OperationKind::AddCopperRoute { board, intent, .. } => {
                let route = kicad_engine::compile_copper_route(snapshot, board, intent)
                    .map_err(|e| reject(&e.to_string()))?;
                tracks.extend(route.tracks.into_iter().map(|track| CreateTrack {
                    layer: native_copper_layer(track.layer),
                    start_x_nm: track.start.x,
                    start_y_nm: track.start.y,
                    end_x_nm: track.end.x,
                    end_y_nm: track.end.y,
                    width_nm: track.width_nm,
                    net: Some(CreateNetIdentity {
                        name: intent.net.name.clone(),
                    }),
                }));
                vias.extend(route.vias.into_iter().map(|via| CreateThroughVia {
                    copper_layers: via_layers.clone(),
                    position_x_nm: via.position.x,
                    position_y_nm: via.position.y,
                    pad_diameter_nm: via.size_nm,
                    drill_diameter_nm: via.drill_nm,
                    net: Some(CreateNetIdentity {
                        name: intent.net.name.clone(),
                    }),
                }));
            }
            OperationKind::AddStraightTrack { intent, .. } => {
                tracks.push(CreateTrack {
                    layer: native_copper_layer(intent.layer),
                    start_x_nm: intent.start.x,
                    start_y_nm: intent.start.y,
                    end_x_nm: intent.end.x,
                    end_y_nm: intent.end.y,
                    width_nm: intent.width_nm,
                    net: intent.net.as_ref().map(|net| CreateNetIdentity {
                        name: net.name.clone(),
                    }),
                });
            }
            OperationKind::AddThroughVia { intent, .. } => {
                vias.push(CreateThroughVia {
                    copper_layers: via_layers.clone(),
                    position_x_nm: intent.position.x,
                    position_y_nm: intent.position.y,
                    pad_diameter_nm: intent.size_nm,
                    drill_diameter_nm: intent.drill_nm,
                    net: intent.net.as_ref().map(|net| CreateNetIdentity {
                        name: net.name.clone(),
                    }),
                });
            }
            _ => {
                return Err(BoardGraphicsCommitOutcome::Rejected {
                    reason: "operation kind is not supported by the PCB execution service".into(),
                });
            }
        }
    }

    if tracks.len() + vias.len() > kicad_model::copper_route::MAX_BATCH_COPPER {
        return Err(reject("batch copper count exceeds 256"));
    }
    Ok(NativePcbBatch {
        graphics,
        tracks,
        vias,
    })
}

impl<T: KiCadTransport> BoardGraphicsIpc for KiCadClient<T> {
    fn begin_commit(&mut self) -> Result<String, String> {
        KiCadClient::begin_commit(self)
            .map(|commit| commit.as_str().to_owned())
            .map_err(|error| error.to_string())
    }

    fn create_board_graphics(
        &mut self,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
    ) -> Result<Vec<CreatedBoardGraphicReceipt>, String> {
        KiCadClient::create_board_graphics(self, board_filename, graphics)
            .map_err(|error| error.to_string())
    }

    fn create_track(
        &mut self,
        board_filename: &str,
        track: &CreateTrack,
    ) -> Result<CreatedTrackReceipt, String> {
        KiCadClient::create_track(self, board_filename, track).map_err(|error| error.to_string())
    }

    fn create_through_via(
        &mut self,
        board_filename: &str,
        via: &CreateThroughVia,
    ) -> Result<CreatedThroughViaReceipt, String> {
        KiCadClient::create_through_via(self, board_filename, via)
            .map_err(|error| error.to_string())
    }

    fn create_track_with_identity(
        &mut self,
        filename: &str,
        track: &CreateTrack,
        id: &str,
    ) -> Result<CreatedTrackReceipt, String> {
        KiCadClient::create_track_with_identity(self, filename, track, id)
            .map_err(|e| e.to_string())
    }
    fn create_through_via_with_identity(
        &mut self,
        filename: &str,
        via: &CreateThroughVia,
        id: &str,
    ) -> Result<CreatedThroughViaReceipt, String> {
        KiCadClient::create_through_via_with_identity(self, filename, via, id)
            .map_err(|e| e.to_string())
    }

    fn commit_changes(&mut self, commit: &str, message: &str) -> Result<(), String> {
        let commit = CommitId::new(commit).map_err(|error| error.to_string())?;
        KiCadClient::commit_changes(self, &commit, message).map_err(|error| error.to_string())
    }

    fn drop_commit(&mut self, commit: &str) -> Result<(), String> {
        let commit = CommitId::new(commit).map_err(|error| error.to_string())?;
        KiCadClient::drop_commit(self, &commit).map_err(|error| error.to_string())
    }

    fn save_board(&mut self, board_filename: &str) -> Result<(), String> {
        KiCadClient::save_board(self, board_filename).map_err(|error| error.to_string())
    }

    fn read_board_graphics(
        &mut self,
        board_filename: &str,
        layer: IpcBoardGraphicLayer,
    ) -> Result<Vec<ObservedBoardGraphicSegment>, String> {
        KiCadClient::get_board_graphics_on_layer(self, board_filename, layer)
            .map_err(|error| error.to_string())
    }

    fn read_board_graphics_by_id(
        &mut self,
        board_filename: &str,
        graphic_ids: &[String],
        layer: IpcBoardGraphicLayer,
    ) -> Result<Vec<ObservedBoardGraphicSegment>, String> {
        KiCadClient::get_board_graphics_by_id(self, board_filename, graphic_ids, layer)
            .map_err(|error| error.to_string())
    }

    fn read_board_routing(&mut self, board_filename: &str) -> Result<IpcBoardRoutingItems, String> {
        KiCadClient::get_board_routing(self, board_filename).map_err(|error| error.to_string())
    }

    fn read_board_routing_by_id(
        &mut self,
        board_filename: &str,
        routing_ids: &[String],
    ) -> Result<IpcBoardRoutingItems, String> {
        KiCadClient::get_board_routing_by_id(self, board_filename, routing_ids)
            .map_err(|error| error.to_string())
    }

    fn require_supported_footprint_transform(
        &mut self,
        board_filename: &str,
        source_uuid: &str,
    ) -> Result<(), String> {
        live_geometry::require_supported_footprint_transform(self, board_filename, source_uuid)
    }

    fn update_footprint_instance(
        &mut self,
        board_filename: &str,
        source_uuid: &str,
        position_x_nm: i64,
        position_y_nm: i64,
        orientation_mdeg: i64,
    ) -> Result<UpdatedFootprintInstanceReceipt, String> {
        KiCadClient::update_footprint_instance(
            self,
            board_filename,
            source_uuid,
            position_x_nm,
            position_y_nm,
            orientation_mdeg,
        )
        .map_err(|error| error.to_string())
    }

    fn read_footprints(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<IpcFootprintInstance>, String> {
        KiCadClient::get_footprint_instances(self, board_filename)
            .map_err(|error| error.to_string())
    }

    fn read_footprint_pads(
        &mut self,
        board_filename: &str,
        source_uuid: &str,
    ) -> Result<Vec<IpcPad>, String> {
        KiCadClient::get_footprint_definition_pads(self, board_filename, source_uuid)
            .map_err(|error| error.to_string())
    }

    fn read_unsupported_move_copper(&mut self, filename: &str) -> Result<Vec<String>, String> {
        self.get_unsupported_move_copper(filename)
            .map_err(|e| e.to_string())
    }
    fn read_pcb_item_diagnostics(
        &mut self,
        board_filename: &str,
        kinds: &[PcbItemKind],
    ) -> Result<Vec<PcbItemDiagnostic>, String> {
        KiCadClient::get_pcb_item_diagnostics(self, board_filename, kinds)
            .map_err(|error| error.to_string())
    }
}

impl BoardGraphicsExecutor {
    fn execute_move_existing_footprint<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operation: &Operation,
        snapshot: &kicad_model::ProjectSnapshot,
        expected_revision: &RevisionDigest,
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        let OperationKind::MoveExistingFootprint {
            board,
            placement_id,
            source_uuid,
            position,
            orientation_mdeg,
            copper_tolerance_nm,
            ..
        } = &operation.kind
        else {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "operation is not an existing footprint transform".into(),
            };
        };
        if snapshot.revision().ok().as_ref() != Some(expected_revision)
            || snapshot.board.document_id.as_ref() != Some(board)
            || operation.targets.as_slice()
                != [board.clone(), placement_id.clone(), source_uuid.clone()]
        {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "footprint transform observation identity is stale".into(),
            };
        }
        let affected = match connected_copper_ids(snapshot, placement_id, *copper_tolerance_nm) {
            Ok(ids) => ids,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Rejected { reason };
            }
        };
        if !affected.is_empty() {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: format!(
                    "connected_copper_requires_separate_edit: {}",
                    affected.join(",")
                ),
            };
        }
        let zones = match ipc.read_unsupported_move_copper(board_filename) {
            Ok(items) => items,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: format!("unsupported_copper_interaction: {reason}"),
                };
            }
        };
        if !zones.is_empty() {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "unsupported_copper_interaction: board contains zones or unsupported copper graphics".into(),
            };
        }
        let expected = match kicad_engine::expected_footprint_move(snapshot, &operation.kind) {
            Ok(expected) => expected,
            Err(e) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: e.to_string(),
                };
            }
        };
        if let Err(reason) =
            ipc.require_supported_footprint_transform(board_filename, source_uuid.as_str())
        {
            return BoardGraphicsCommitOutcome::Rejected { reason };
        }
        let before_footprints = match read_move_footprints(ipc, board, board_filename) {
            Ok(items) => items,
            Err(reason) => return BoardGraphicsCommitOutcome::Rejected { reason },
        };
        let before_routing = match ipc.read_board_routing(board_filename) {
            Ok(items) => items,
            Err(reason) => return BoardGraphicsCommitOutcome::Rejected { reason },
        };
        if snapshot.native_footprints.as_ref() != Some(&before_footprints)
            || board_routing_observation(board.clone(), before_routing.clone())
                .ok()
                .as_ref()
                != snapshot.native_routing.as_ref()
        {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "footprint or routing observation changed before update".into(),
            };
        }
        let commit = match ipc.begin_commit() {
            Ok(commit) => commit,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let receipt = match ipc.update_footprint_instance(
            board_filename,
            &source_uuid.to_string(),
            position.x,
            position.y,
            *orientation_mdeg,
        ) {
            Ok(receipt) => receipt,
            Err(reason) => return drop_native_pcb_commit(ipc, &commit, reason),
        };
        if receipt.source_uuid != source_uuid.to_string()
            || receipt.position_x_nm != position.x
            || receipt.position_y_nm != position.y
            || receipt.orientation_mdeg != *orientation_mdeg
        {
            return drop_native_pcb_commit(
                ipc,
                &commit,
                "updated footprint receipt did not match the approved transform".into(),
            );
        }
        if let Err(reason) = ipc.commit_changes(&commit, message) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        if let Err(reason) = ipc.save_board(board_filename) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        let after_footprints = match read_move_footprints(ipc, board, board_filename) {
            Ok(items) => items,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let after_routing = match ipc.read_board_routing(board_filename) {
            Ok(items) => items,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        if expected.native_footprints.as_ref() != Some(&after_footprints)
            || board_routing_observation(board.clone(), after_routing)
                .ok()
                .as_ref()
                != snapshot.native_routing.as_ref()
        {
            return BoardGraphicsCommitOutcome::Unknown {
                reason: "saved footprint or routing set differs from the approved transform".into(),
            };
        }
        BoardGraphicsCommitOutcome::CommittedAndSaved
    }

    pub fn execute_native_batch<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
        tracks: &[CreateTrack],
        vias: &[CreateThroughVia],
        message: &str,
    ) -> Result<NativePcbBatchReceipt, BoardGraphicsCommitOutcome> {
        self.validate_native_batch(board_filename, graphics, tracks, vias)
            .map_err(|reason| BoardGraphicsCommitOutcome::Rejected { reason })?;
        let commit = ipc
            .begin_commit()
            .map_err(|reason| BoardGraphicsCommitOutcome::Unknown { reason })?;
        let receipt = self
            .create_native_batch_in_transaction(ipc, board_filename, graphics, tracks, vias)
            .map_err(|reason| drop_native_pcb_commit(ipc, &commit, reason))?;
        if let Err(reason) = ipc.commit_changes(&commit, message) {
            return Err(BoardGraphicsCommitOutcome::Unknown { reason });
        }
        if let Err(reason) = ipc.save_board(board_filename) {
            return Err(BoardGraphicsCommitOutcome::Unknown { reason });
        }
        verify_native_pcb_batch(ipc, board_filename, &receipt)
            .map_err(|reason| BoardGraphicsCommitOutcome::Unknown { reason })?;
        Ok(receipt)
    }

    fn validate_native_batch(
        &self,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
        tracks: &[CreateTrack],
        vias: &[CreateThroughVia],
    ) -> Result<(), String> {
        let item_count = graphics.len() + tracks.len() + vias.len();
        if board_filename.is_empty() || item_count == 0 || item_count > self.max_segments {
            return Err("native PCB batch is empty, oversized, or has no board filename".into());
        }
        if graphics.iter().any(|graphic| {
            graphic.width_nm <= 0
                || (graphic.start_x_nm, graphic.start_y_nm) == (graphic.end_x_nm, graphic.end_y_nm)
                || !matches!(
                    graphic.layer,
                    IpcBoardGraphicLayer::DwgsUser | IpcBoardGraphicLayer::EdgeCuts
                )
                || [
                    graphic.start_x_nm,
                    graphic.start_y_nm,
                    graphic.end_x_nm,
                    graphic.end_y_nm,
                ]
                .iter()
                .any(|value| value.unsigned_abs() > MAX_BOARD_POSITION_NM as u64)
        }) || tracks.iter().any(|track| {
            track.width_nm <= 0
                || (track.start_x_nm, track.start_y_nm) == (track.end_x_nm, track.end_y_nm)
                || [
                    track.start_x_nm,
                    track.start_y_nm,
                    track.end_x_nm,
                    track.end_y_nm,
                ]
                .iter()
                .any(|value| value.unsigned_abs() > MAX_BOARD_POSITION_NM as u64)
        }) || vias.iter().any(|via| {
            via.validate().is_err()
                || via.pad_diameter_nm <= 0
                || via.drill_diameter_nm <= 0
                || via.drill_diameter_nm > via.pad_diameter_nm
                || via.position_x_nm.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                || via.position_y_nm.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
        }) {
            return Err("native PCB batch contains invalid or out-of-bounds items".into());
        }

        Ok(())
    }

    /// Create a bounded additive batch inside a caller-owned native transaction.
    /// Errors may follow partial creation; this method never retries or changes the transaction lifecycle.
    /// Receipts do not establish saved preservation, refill, connectivity, DRC or approval.
    pub fn create_native_batch_in_transaction<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
        tracks: &[CreateTrack],
        vias: &[CreateThroughVia],
    ) -> Result<NativePcbBatchReceipt, String> {
        self.create_native_batch_with_identities(ipc, board_filename, graphics, tracks, vias, None)
    }

    pub fn create_native_batch_with_identities<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
        tracks: &[CreateTrack],
        vias: &[CreateThroughVia],
        identities: Option<&crate::additive_identities::AdditiveCreationIdentities>,
    ) -> Result<NativePcbBatchReceipt, String> {
        if let Some(ids) = identities {
            ids.validate_batch(&NativePcbBatch {
                graphics: graphics.to_vec(),
                tracks: tracks.to_vec(),
                vias: vias.to_vec(),
            })?;
        }
        self.validate_native_batch(board_filename, graphics, tracks, vias)?;
        let graphic_receipts = if graphics.is_empty() {
            Vec::new()
        } else {
            match ipc.create_board_graphics(board_filename, graphics) {
                Ok(receipts)
                    if receipts.len() == graphics.len()
                        && receipts.iter().enumerate().all(|(index, receipt)| {
                            receipt.request_index == index && receipt.segment == graphics[index]
                        }) =>
                {
                    receipts
                }
                Ok(_) => {
                    return Err("created board graphic receipt did not match the request".into());
                }
                Err(reason) => return Err(reason),
            }
        };
        let mut track_receipts = Vec::with_capacity(tracks.len());
        for (index, track) in tracks.iter().enumerate() {
            let created = match identities {
                Some(ids) => {
                    ipc.create_track_with_identity(board_filename, track, ids.track(index))
                }
                None => ipc.create_track(board_filename, track),
            };
            match created {
                Ok(receipt)
                    if receipt.request_index == 0
                        && legacy_track_receipt_matches(&receipt.track, track)
                        && identities.is_none_or(|ids| receipt.source_uuid == ids.track(index)) =>
                {
                    track_receipts.push(receipt);
                }
                Ok(_) => {
                    return Err("created track receipt did not match the request".into());
                }
                Err(reason) => return Err(reason),
            }
        }
        let mut via_receipts = Vec::with_capacity(vias.len());
        for (index, via) in vias.iter().enumerate() {
            let created = match identities {
                Some(ids) => {
                    ipc.create_through_via_with_identity(board_filename, via, ids.via(index))
                }
                None => ipc.create_through_via(board_filename, via),
            };
            match created {
                Ok(receipt) => {
                    if receipt.request_index == 0
                        && legacy_via_receipt_matches(&receipt.via, via)
                        && identities.is_none_or(|ids| receipt.source_uuid == ids.via(index))
                    {
                        via_receipts.push(receipt);
                    } else {
                        return Err(format!(
                            "created via receipt did not match the request: index={} actual=({}, {}, {}, {}, {:?}) expected=({}, {}, {}, {}, {:?})",
                            receipt.request_index,
                            receipt.via.position_x_nm,
                            receipt.via.position_y_nm,
                            receipt.via.pad_diameter_nm,
                            receipt.via.drill_diameter_nm,
                            receipt.via.net.as_ref().map(|net| net.name.as_str()),
                            via.position_x_nm,
                            via.position_y_nm,
                            via.pad_diameter_nm,
                            via.drill_diameter_nm,
                            via.net.as_ref().map(|net| net.name.as_str()),
                        ));
                    }
                }
                Err(reason) => return Err(reason),
            }
        }
        let receipt = NativePcbBatchReceipt {
            graphics: graphic_receipts,
            tracks: track_receipts,
            vias: via_receipts,
        };
        Ok(receipt)
    }

    pub fn execute<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operation: &OperationKind,
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        self.execute_kinds(ipc, &[operation], board_filename, message)
    }

    pub fn execute_bound<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operation: &Operation,
        snapshot: &kicad_model::ProjectSnapshot,
        expected_revision: &RevisionDigest,
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        if matches!(operation.kind, OperationKind::MoveExistingFootprint { .. }) {
            return self.execute_move_existing_footprint(
                ipc,
                operation,
                snapshot,
                expected_revision,
                board_filename,
                message,
            );
        }
        if matches!(
            operation.kind,
            OperationKind::AddStraightTrack { .. }
                | OperationKind::AddThroughVia { .. }
                | OperationKind::AddCopperRoute { .. }
        ) {
            return self.execute_bound_batch(
                ipc,
                std::slice::from_ref(operation),
                snapshot,
                expected_revision,
                board_filename,
                message,
            );
        }
        self.execute(ipc, &operation.kind, board_filename, message)
    }

    pub fn execute_bound_batch<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operations: &[Operation],
        snapshot: &kicad_model::ProjectSnapshot,
        expected_revision: &RevisionDigest,
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        if operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::MoveExistingFootprint { .. }))
        {
            let [operation] = operations else {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "existing footprint movement requires its own transaction".into(),
                };
            };
            return self.execute_bound(
                ipc,
                operation,
                snapshot,
                expected_revision,
                board_filename,
                message,
            );
        }
        let batch = match prepare_bound_native_batch(operations, snapshot, expected_revision) {
            Ok(batch) => batch,
            Err(outcome) => return outcome,
        };
        match self.execute_native_batch(
            ipc,
            board_filename,
            &batch.graphics,
            &batch.tracks,
            &batch.vias,
            message,
        ) {
            Ok(_) => BoardGraphicsCommitOutcome::CommittedAndSaved,
            Err(outcome) => outcome,
        }
    }

    pub fn execute_batch<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operations: &[Operation],
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        let kinds = operations
            .iter()
            .map(|operation| &operation.kind)
            .collect::<Vec<_>>();
        self.execute_kinds(ipc, &kinds, board_filename, message)
    }

    fn execute_kinds<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operations: &[&OperationKind],
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        if board_filename.is_empty() {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "board filename is empty".into(),
            };
        }
        if operations.is_empty() {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "operation batch is empty".into(),
            };
        }
        let has_routing = operations.iter().any(|operation| {
            matches!(
                operation,
                OperationKind::AddStraightTrack { .. } | OperationKind::AddThroughVia { .. }
            )
        });
        if has_routing {
            if operations.len() > self.max_segments {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "routing operation count exceeded bounds".into(),
                };
            }
            if operations.iter().any(|operation| {
                !matches!(
                    operation,
                    OperationKind::AddStraightTrack { .. } | OperationKind::AddThroughVia { .. }
                )
            }) {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "routing and graphics operations cannot share a transaction".into(),
                };
            }
            return self.execute_routing(ipc, operations, board_filename, message);
        }
        let mut segments = Vec::new();
        for operation in operations {
            match operation {
                OperationKind::AddBoardGraphicSegments {
                    segments: operation_segments,
                    ..
                } => segments.extend(operation_segments.iter().cloned()),
                OperationKind::AddBoardOutline {
                    top_left,
                    bottom_right,
                    width_nm,
                    ..
                } => segments.extend(expand_board_outline(*top_left, *bottom_right, *width_nm)),
                _ => {
                    return BoardGraphicsCommitOutcome::Rejected {
                        reason: "operation is not board graphics".into(),
                    };
                }
            }
        }
        if segments.is_empty() || segments.len() > self.max_segments {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic segment count exceeded bounds".into(),
            };
        }
        if segments.iter().any(|segment| {
            segment.validate().is_err()
                || !matches!(
                    segment.layer,
                    BoardGraphicLayer::DwgsUser | BoardGraphicLayer::EdgeCuts
                )
        }) {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic segment is invalid or uses an unsupported layer".into(),
            };
        }
        let min_x = segments
            .iter()
            .map(|segment| segment.start.x.min(segment.end.x))
            .min()
            .unwrap();
        let max_x = segments
            .iter()
            .map(|segment| segment.start.x.max(segment.end.x))
            .max()
            .unwrap();
        let min_y = segments
            .iter()
            .map(|segment| segment.start.y.min(segment.end.y))
            .min()
            .unwrap();
        let max_y = segments
            .iter()
            .map(|segment| segment.start.y.max(segment.end.y))
            .max()
            .unwrap();
        let width = i128::from(max_x) - i128::from(min_x);
        let height = i128::from(max_y) - i128::from(min_y);
        let area = width.checked_mul(height);
        if width > MAX_GRAPHIC_SPAN_NM
            || height > MAX_GRAPHIC_SPAN_NM
            || area.is_none_or(|area| area > MAX_GRAPHIC_BOUNDING_AREA_NM2)
        {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic bounds exceeded".into(),
            };
        }
        let graphics = segments
            .iter()
            .map(|segment| IpcBoardGraphicSegment {
                start_x_nm: segment.start.x,
                start_y_nm: segment.start.y,
                end_x_nm: segment.end.x,
                end_y_nm: segment.end.y,
                width_nm: segment.width_nm,
                layer: match segment.layer {
                    BoardGraphicLayer::DwgsUser => IpcBoardGraphicLayer::DwgsUser,
                    BoardGraphicLayer::EdgeCuts => IpcBoardGraphicLayer::EdgeCuts,
                },
            })
            .collect::<Vec<_>>();
        let commit = match ipc.begin_commit() {
            Ok(commit) => commit,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let receipts = match ipc.create_board_graphics(board_filename, &graphics) {
            Ok(receipts) => receipts,
            Err(reason) => {
                return match ipc.drop_commit(&commit) {
                    Ok(()) => BoardGraphicsCommitOutcome::Rejected { reason },
                    Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
                        reason: format!("create failed: {reason}; drop failed: {drop_reason}"),
                    },
                };
            }
        };
        if receipts.len() != graphics.len()
            || receipts.iter().enumerate().any(|(index, receipt)| {
                receipt.request_index != index || receipt.segment != graphics[index]
            })
        {
            return match ipc.drop_commit(&commit) {
                Ok(()) => BoardGraphicsCommitOutcome::Rejected {
                    reason: "created board graphic receipt did not match the request".into(),
                },
                Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("receipt mismatch; drop failed: {drop_reason}"),
                },
            };
        }
        if let Err(reason) = ipc.commit_changes(&commit, message) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        if let Err(reason) = ipc.save_board(board_filename) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        let read_layer = if segments
            .iter()
            .all(|segment| segment.layer == BoardGraphicLayer::EdgeCuts)
        {
            IpcBoardGraphicLayer::EdgeCuts
        } else {
            IpcBoardGraphicLayer::DwgsUser
        };
        let observed = match ipc.read_board_graphics(board_filename, read_layer) {
            Ok(observed) => observed,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let mut matched = BTreeSet::new();
        for receipt in &receipts {
            let candidates = observed
                .iter()
                .filter(|item| item.graphic_id == receipt.graphic_id);
            let mut candidates = candidates;
            let Some(item) = candidates.next() else {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved board graphic {} is missing", receipt.graphic_id),
                };
            };
            if candidates.next().is_some() || !matched.insert(receipt.graphic_id.clone()) {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved board graphic {} is not unique", receipt.graphic_id),
                };
            }
            if item.segment != receipt.segment {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved board graphic {} does not match", receipt.graphic_id),
                };
            }
        }
        BoardGraphicsCommitOutcome::CommittedAndSaved
    }

    fn execute_routing<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operations: &[&OperationKind],
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        let mut requests = Vec::with_capacity(operations.len());
        for operation in operations {
            let request = match operation {
                OperationKind::AddStraightTrack { intent, .. } => {
                    if intent.validate().is_err()
                        || intent.start.x.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                        || intent.start.y.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                        || intent.end.x.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                        || intent.end.y.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                    {
                        return BoardGraphicsCommitOutcome::Rejected {
                            reason: "straight track intent is invalid or out of bounds".into(),
                        };
                    }
                    RoutingRequest::Track(CreateTrack {
                        layer: native_copper_layer(intent.layer),
                        start_x_nm: intent.start.x,
                        start_y_nm: intent.start.y,
                        end_x_nm: intent.end.x,
                        end_y_nm: intent.end.y,
                        width_nm: intent.width_nm,
                        net: intent.net.as_ref().map(|net| CreateNetIdentity {
                            name: net.name.clone(),
                        }),
                    })
                }
                OperationKind::AddThroughVia { intent, .. } => {
                    if intent.validate().is_err()
                        || intent.position.x.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                        || intent.position.y.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                    {
                        return BoardGraphicsCommitOutcome::Rejected {
                            reason: "through via intent is invalid or out of bounds".into(),
                        };
                    }
                    RoutingRequest::Via(CreateThroughVia {
                        copper_layers: vec![
                            kicad_ipc::CopperLayer::FRONT,
                            kicad_ipc::CopperLayer::BACK,
                        ],
                        position_x_nm: intent.position.x,
                        position_y_nm: intent.position.y,
                        pad_diameter_nm: intent.size_nm,
                        drill_diameter_nm: intent.drill_nm,
                        net: intent.net.as_ref().map(|net| CreateNetIdentity {
                            name: net.name.clone(),
                        }),
                    })
                }
                _ => unreachable!("routing batch was checked before execution"),
            };
            requests.push(request);
        }
        let commit = match ipc.begin_commit() {
            Ok(commit) => commit,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let mut track_receipts = Vec::new();
        let mut via_receipts = Vec::new();
        for request in &requests {
            let result = match request {
                RoutingRequest::Track(track) => {
                    ipc.create_track(board_filename, track).map(|receipt| {
                        let matches = receipt.request_index == 0 && receipt.track == *track;
                        track_receipts.push(receipt);
                        matches
                    })
                }
                RoutingRequest::Via(via) => {
                    ipc.create_through_via(board_filename, via).map(|receipt| {
                        let matches = receipt.request_index == 0 && receipt.via == *via;
                        via_receipts.push(receipt);
                        matches
                    })
                }
            };
            match result {
                Ok(true) => {}
                Ok(false) => {
                    return match ipc.drop_commit(&commit) {
                        Ok(()) => BoardGraphicsCommitOutcome::Rejected {
                            reason: "created routing receipt did not match the request".into(),
                        },
                        Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
                            reason: format!("routing receipt mismatch; drop failed: {drop_reason}"),
                        },
                    };
                }
                Err(reason) => {
                    return match ipc.drop_commit(&commit) {
                        Ok(()) => BoardGraphicsCommitOutcome::Rejected { reason },
                        Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
                            reason: format!(
                                "routing create failed: {reason}; drop failed: {drop_reason}"
                            ),
                        },
                    };
                }
            }
        }
        if let Err(reason) = ipc.commit_changes(&commit, message) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        if let Err(reason) = ipc.save_board(board_filename) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        let routing_ids = track_receipts
            .iter()
            .map(|receipt| receipt.source_uuid.clone())
            .chain(
                via_receipts
                    .iter()
                    .map(|receipt| receipt.source_uuid.clone()),
            )
            .collect::<Vec<_>>();
        let observed = match ipc.read_board_routing_by_id(board_filename, &routing_ids) {
            Ok(observed) => observed,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        let mut matched_tracks = BTreeSet::new();
        for receipt in &track_receipts {
            let candidates = observed
                .tracks
                .iter()
                .filter(|track| track.source_uuid == receipt.source_uuid);
            let mut candidates = candidates;
            let Some(track) = candidates.next() else {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved track {} is missing", receipt.source_uuid),
                };
            };
            if candidates.next().is_some() || !matched_tracks.insert(receipt.source_uuid.clone()) {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved track {} is not unique", receipt.source_uuid),
                };
            }
            let expected_net = operations.iter().find_map(|operation| match operation {
                OperationKind::AddStraightTrack { intent, .. }
                    if intent.source_uuid.as_str() == receipt.source_uuid =>
                {
                    intent.net.as_ref()
                }
                _ => None,
            });
            if !observed_track_matches(track, receipt, expected_net) {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved track {} does not match", receipt.source_uuid),
                };
            }
        }
        let mut matched_vias = BTreeSet::new();
        for receipt in &via_receipts {
            let candidates = observed
                .vias
                .iter()
                .filter(|via| via.source_uuid == receipt.source_uuid);
            let mut candidates = candidates;
            let Some(via) = candidates.next() else {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved via {} is missing", receipt.source_uuid),
                };
            };
            if candidates.next().is_some() || !matched_vias.insert(receipt.source_uuid.clone()) {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved via {} is not unique", receipt.source_uuid),
                };
            }
            let expected_net = operations.iter().find_map(|operation| match operation {
                OperationKind::AddThroughVia { intent, .. }
                    if intent.source_uuid.as_str() == receipt.source_uuid =>
                {
                    intent.net.as_ref()
                }
                _ => None,
            });
            if !observed_via_matches(via, receipt, expected_net) {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("saved via {} does not match", receipt.source_uuid),
                };
            }
        }
        BoardGraphicsCommitOutcome::CommittedAndSaved
    }
}

/// Commit and save one native edit; callers must verify complete saved postconditions.
pub fn execute_native_edit_transaction<I: BoardGraphicsIpc>(
    ipc: &mut I,
    board_filename: &str,
    message: &str,
    update: impl FnOnce(&mut I) -> Result<(), String>,
) -> BoardGraphicsCommitOutcome {
    let commit = match ipc.begin_commit() {
        Ok(commit) => commit,
        Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
    };
    if let Err(reason) = update(ipc) {
        return drop_native_pcb_commit(ipc, &commit, reason);
    }
    if let Err(reason) = ipc.commit_changes(&commit, message) {
        return BoardGraphicsCommitOutcome::Unknown { reason };
    }
    if let Err(reason) = ipc.save_board(board_filename) {
        return BoardGraphicsCommitOutcome::Unknown { reason };
    }
    BoardGraphicsCommitOutcome::CommittedAndSaved
}

fn drop_native_pcb_commit<I: BoardGraphicsIpc>(
    ipc: &mut I,
    commit: &str,
    reason: String,
) -> BoardGraphicsCommitOutcome {
    match ipc.drop_commit(commit) {
        Ok(()) => BoardGraphicsCommitOutcome::Rejected { reason },
        Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
            reason: format!("native PCB create failed: {reason}; drop failed: {drop_reason}"),
        },
    }
}

pub fn verify_native_pcb_batch<I: BoardGraphicsIpc>(
    ipc: &mut I,
    board_filename: &str,
    receipt: &NativePcbBatchReceipt,
) -> Result<(), String> {
    let mut observed_graphics = Vec::new();
    for layer in [
        IpcBoardGraphicLayer::DwgsUser,
        IpcBoardGraphicLayer::EdgeCuts,
    ] {
        let ids = receipt
            .graphics
            .iter()
            .filter(|graphic| graphic.segment.layer == layer)
            .map(|graphic| graphic.graphic_id.clone())
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            observed_graphics.extend(ipc.read_board_graphics_by_id(board_filename, &ids, layer)?);
        }
    }
    let mut matched = BTreeSet::new();
    for expected in &receipt.graphics {
        let mut candidates = observed_graphics
            .iter()
            .filter(|actual| actual.graphic_id == expected.graphic_id);
        let Some(actual) = candidates.next() else {
            return Err(format!(
                "saved board graphic {} is missing",
                expected.graphic_id
            ));
        };
        if candidates.next().is_some() || !matched.insert(expected.graphic_id.clone()) {
            return Err(format!(
                "saved board graphic {} is not unique",
                expected.graphic_id
            ));
        }
        if actual.segment != expected.segment {
            return Err(format!(
                "saved board graphic {} does not match",
                expected.graphic_id
            ));
        }
    }
    read_verified_native_routing(ipc, board_filename, receipt).map(|_| ())
}

/// Compare newly created routing across reload, accounting for native via serialization.
/// Call only after both observations have passed exact creation-receipt validation.
pub fn created_routing_matches_reloaded(
    before: &IpcBoardRoutingItems,
    after: &IpcBoardRoutingItems,
) -> bool {
    fn canonical(mut routing: IpcBoardRoutingItems) -> IpcBoardRoutingItems {
        for via in &mut routing.vias {
            if via.via_type != kicad_ipc::ObservedViaType::Through
                || via.pad_stack.stack_type != IpcPadStackType::FrontInnerBack
            {
                continue;
            }
            let diameter = via
                .pad_stack
                .copper_shapes
                .iter()
                .find(|s| {
                    s.layer == IpcBoardLayer::FrontCopper
                        && s.shape == IpcPadShape::Circle
                        && s.size_x_nm == s.size_y_nm
                        && s.size_x_nm > 0
                })
                .map(|s| s.size_x_nm);
            for shape in &mut via.pad_stack.copper_shapes {
                if shape.shape != IpcPadShape::Circle {
                    continue;
                }
                // The custom anchor is unused for a circular pad.
                shape.custom_anchor_shape = None;
                if shape.layer == IpcBoardLayer::InnerCopper(1)
                    && shape.size_x_nm == 0
                    && shape.size_y_nm == 0
                    && let Some(size) = diameter
                {
                    shape.size_x_nm = size;
                    shape.size_y_nm = size;
                }
            }
        }
        routing
    }
    canonical(before.clone()) == canonical(after.clone())
}

pub fn read_verified_native_routing<I: BoardGraphicsIpc>(
    ipc: &mut I,
    board_filename: &str,
    receipt: &NativePcbBatchReceipt,
) -> Result<IpcBoardRoutingItems, String> {
    let routing_ids = receipt
        .tracks
        .iter()
        .map(|track| track.source_uuid.clone())
        .chain(receipt.vias.iter().map(|via| via.source_uuid.clone()))
        .collect::<Vec<_>>();
    let observed_routing = if routing_ids.is_empty() {
        IpcBoardRoutingItems::default()
    } else {
        ipc.read_board_routing_by_id(board_filename, &routing_ids)?
    };
    if observed_routing.tracks.len() != receipt.tracks.len()
        || observed_routing.vias.len() != receipt.vias.len()
    {
        return Err("routing readback count differs from creation receipts".into());
    }
    let mut matched = BTreeSet::new();
    for expected in &receipt.tracks {
        let mut candidates = observed_routing
            .tracks
            .iter()
            .filter(|actual| actual.source_uuid == expected.source_uuid);
        let Some(actual) = candidates.next() else {
            return Err(format!("saved track {} is missing", expected.source_uuid));
        };
        if candidates.next().is_some() || !matched.insert(expected.source_uuid.clone()) {
            return Err(format!(
                "saved track {} is not unique",
                expected.source_uuid
            ));
        }
        if !observed_track_matches(actual, expected, None) {
            return Err(format!(
                "saved track {} does not match",
                expected.source_uuid
            ));
        }
    }
    for expected in &receipt.vias {
        let mut candidates = observed_routing
            .vias
            .iter()
            .filter(|actual| actual.source_uuid == expected.source_uuid);
        let Some(actual) = candidates.next() else {
            return Err(format!("saved via {} is missing", expected.source_uuid));
        };
        if candidates.next().is_some() || !matched.insert(expected.source_uuid.clone()) {
            return Err(format!("saved via {} is not unique", expected.source_uuid));
        }
        if !observed_via_matches(actual, expected, None) {
            return Err(format!("saved via {} does not match", expected.source_uuid));
        }
    }
    Ok(observed_routing)
}

fn legacy_track_receipt_matches(receipt: &CreateTrack, requested: &CreateTrack) -> bool {
    receipt.layer == requested.layer
        && receipt.start_x_nm == requested.start_x_nm
        && receipt.start_y_nm == requested.start_y_nm
        && receipt.end_x_nm == requested.end_x_nm
        && receipt.end_y_nm == requested.end_y_nm
        && receipt.width_nm == requested.width_nm
        && requested.net.as_ref().is_none_or(|expected| {
            receipt
                .net
                .as_ref()
                .is_some_and(|actual| actual.name == expected.name && !actual.name.is_empty())
        })
}

fn observed_track_matches(
    observed: &IpcTrack,
    receipt: &CreatedTrackReceipt,
    expected_net: Option<&kicad_model::ObservedNetIdentity>,
) -> bool {
    let IpcTrackGeometry::Segment { start, end } = &observed.geometry else {
        return false;
    };
    let requested = normalized_track_geometry(
        receipt.track.start_x_nm,
        receipt.track.start_y_nm,
        receipt.track.end_x_nm,
        receipt.track.end_y_nm,
    );
    let expected_name = expected_net
        .map(|net| net.name.as_str())
        .or_else(|| receipt.track.net.as_ref().map(|net| net.name.as_str()));
    let net_matches = expected_name.is_none_or(|expected| {
        observed
            .net
            .as_ref()
            .is_some_and(|actual| actual.name == expected && !actual.name.is_empty())
    });
    normalized_track_geometry(start.x_nm, start.y_nm, end.x_nm, end.y_nm) == requested
        && observed.width_nm == receipt.track.width_nm
        && observed.layer == receipt.track.layer.as_observed()
        && net_matches
}

fn normalized_track_geometry(
    start_x: i64,
    start_y: i64,
    end_x: i64,
    end_y: i64,
) -> ((i64, i64), (i64, i64)) {
    let start = (start_x, start_y);
    let end = (end_x, end_y);
    if start <= end {
        (start, end)
    } else {
        (end, start)
    }
}

fn legacy_via_receipt_matches(receipt: &CreateThroughVia, requested: &CreateThroughVia) -> bool {
    receipt.copper_layers == requested.copper_layers
        && receipt.position_x_nm == requested.position_x_nm
        && receipt.position_y_nm == requested.position_y_nm
        && receipt.pad_diameter_nm == requested.pad_diameter_nm
        && receipt.drill_diameter_nm == requested.drill_diameter_nm
        && requested.net.as_ref().is_none_or(|expected| {
            receipt
                .net
                .as_ref()
                .is_some_and(|actual| actual.name == expected.name && !actual.name.is_empty())
        })
}

fn observed_via_matches(
    observed: &IpcVia,
    receipt: &CreatedThroughViaReceipt,
    expected_net: Option<&kicad_model::ObservedNetIdentity>,
) -> bool {
    let via = &receipt.via;
    let mut expected_layers = via
        .copper_layers
        .iter()
        .map(|layer| layer.as_observed())
        .collect::<Vec<_>>();
    expected_layers.sort();
    let pad_shapes = &observed.pad_stack.copper_shapes;
    let expected_pad_layers = [
        IpcBoardLayer::FrontCopper,
        IpcBoardLayer::InnerCopper(1),
        IpcBoardLayer::BackCopper,
    ];
    let exact_pad_shapes = via.pad_diameter_nm > 0
        && pad_shapes.len() == expected_pad_layers.len()
        && pad_shapes
            .iter()
            .zip(expected_pad_layers)
            .all(|(actual, expected_layer)| {
                let expected_size = if expected_layer == IpcBoardLayer::InnerCopper(1) {
                    actual.size_x_nm == 0 || actual.size_x_nm == via.pad_diameter_nm
                } else {
                    actual.size_x_nm == via.pad_diameter_nm
                };
                actual.layer == expected_layer
                    && actual.shape == kicad_ipc::ObservedPadShape::Circle
                    && expected_size
                    && actual.size_x_nm == actual.size_y_nm
            });
    let Some(drill) = observed.pad_stack.primary_drill.as_ref() else {
        return false;
    };
    let expected_name = expected_net
        .map(|net| net.name.as_str())
        .or_else(|| receipt.via.net.as_ref().map(|net| net.name.as_str()));
    let net_matches = expected_name.is_none_or(|expected| {
        observed
            .net
            .as_ref()
            .is_some_and(|actual| actual.name == expected && !actual.name.is_empty())
    });
    observed.position.x_nm == via.position_x_nm
        && observed.position.y_nm == via.position_y_nm
        && net_matches
        && observed.via_type == kicad_ipc::ObservedViaType::Through
        && observed.pad_stack.stack_type == IpcPadStackType::FrontInnerBack
        && observed.pad_stack.layers == expected_layers
        && exact_pad_shapes
        && drill.start_layer == IpcBoardLayer::FrontCopper
        && drill.end_layer == IpcBoardLayer::BackCopper
        && drill.shape == IpcDrillShape::Circle
        && drill.size_x_nm == via.drill_diameter_nm
        && drill.size_y_nm == via.drill_diameter_nm
}

#[derive(Clone)]
enum RoutingRequest {
    Track(CreateTrack),
    Via(CreateThroughVia),
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_model::{
        BoardGraphicSegment, Coordinate, DocumentSpace, RevisionDigest, SchematicArtifactFile,
        SchematicArtifactRevision, Unit,
    };
    use kicad_schematic::{
        InstancePath, NetlistComponent, NetlistNet, NetlistNode, ParsedSchematic, Position,
        RawSymbol, RawSymbolPin,
    };

    fn version() -> VersionInfo {
        VersionInfo {
            major: 10,
            minor: 0,
            patch: 4,
            full_version: "10.0.4".to_owned(),
        }
    }

    fn schematic(filename: &str) -> OpenDocument {
        OpenDocument {
            project_name: None,
            project_path: None,
            kind: OpenDocumentKind::Schematic {
                filename: Some(filename.to_owned()),
                sheet_path: None,
                sheet_kiids: Vec::new(),
            },
        }
    }

    fn input(documents: Vec<OpenDocument>) -> DiscoveryInput {
        DiscoveryInput {
            endpoint: "ipc:///tmp/kicad/api.sock".to_owned(),
            version: version(),
            schematic: DocumentObservation::Documents(documents),
            pcb: DocumentObservation::Unsupported("no handler".to_owned()),
        }
    }

    fn pcb() -> OpenDocument {
        OpenDocument {
            project_name: Some("xLock".to_owned()),
            project_path: Some("/projects/xLock".to_owned()),
            kind: OpenDocumentKind::Pcb {
                board_filename: "xLock.kicad_pcb".to_owned(),
            },
        }
    }

    #[test]
    fn board_document_id_accepts_only_usable_pcb_documents() {
        let document = pcb();
        let expected = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        assert_eq!(board_document_id(&document).unwrap(), expected);

        let mut missing_identity = document;
        missing_identity.kind = OpenDocumentKind::Pcb {
            board_filename: String::new(),
        };
        assert!(matches!(
            board_document_id(&missing_identity),
            Err(LiveBoardObservationError::MissingDocumentIdentity)
        ));
        assert!(matches!(
            board_document_id(&schematic("xLock.kicad_sch")),
            Err(LiveBoardObservationError::NonPcbDocument)
        ));
    }

    fn schematic_fixture(locked: bool) -> (ParsedSchematicProject, NetlistDocument) {
        let artifact = SchematicArtifactFile::new(
            "root.kicad_sch",
            1,
            RevisionDigest::new("a".repeat(64)).unwrap(),
        )
        .unwrap();
        let revision =
            SchematicArtifactRevision::new(vec![artifact.clone()], "10.0.4", 1, locked).unwrap();
        let symbol = RawSymbol {
            source_uuid: "symbol-uuid".to_owned(),
            lib_id: "Device:R".to_owned(),
            lib_name: None,
            position: Position {
                x: 1.0,
                y: 2.0,
                orientation_degrees: Some(0.0),
            },
            unit: 1,
            body_style: None,
            reference: "R1".to_owned(),
            value: "10k".to_owned(),
            footprint: Some("Resistor_SMD:R_0603".to_owned()),
            on_board: Some(true),
            in_bom: Some(true),
            dnp: Some(false),
            pins: vec![RawSymbolPin {
                number: "1".to_owned(),
                source_uuid: "pin-uuid".to_owned(),
            }],
            instance_paths: vec![InstancePath {
                project: "demo".to_owned(),
                path: "/file-uuid/root/symbol-uuid".to_owned(),
                reference: Some("R1".to_owned()),
                unit: Some(1),
                page: Some("1".to_owned()),
            }],
        };
        let parsed_file = ParsedSchematic {
            format_version: "20250114".to_owned(),
            generator: "kicad".to_owned(),
            generator_version: Some("10.0.4".to_owned()),
            file_uuid: "file-uuid".to_owned(),
            sheets: Vec::new(),
            symbols: vec![symbol],
            diagnostics: Vec::new(),
            artifact,
        };
        let project = ParsedSchematicProject {
            root_relative_path: "root.kicad_sch".to_owned(),
            files: vec![parsed_file],
            revision,
            editor_lock_present: locked,
        };
        let netlist = NetlistDocument {
            export_version: "D".to_owned(),
            sheets: Vec::new(),
            components: vec![NetlistComponent {
                units: None,
                reference: "R1".to_owned(),
                value: "10k".to_owned(),
                footprint: Some("Resistor_SMD:R_0603".to_owned()),
                symbol_uuid: Some("symbol-uuid".to_owned()),
                sheet_path: "/root".to_owned(),
                sheet_uuid_path: "/root/symbol-uuid".to_owned(),
                library: "Device".to_owned(),
                library_part: "R".to_owned(),
            }],
            nets: vec![NetlistNet {
                code: "1".to_owned(),
                name: "GND".to_owned(),
                class: None,
                nodes: vec![NetlistNode {
                    reference: "R1".to_owned(),
                    pin: "1".to_owned(),
                    pin_function: "passive".to_owned(),
                    pin_type: "passive".to_owned(),
                }],
            }],
        };
        (project, netlist)
    }

    #[test]
    fn pad_board_coordinates_round_trip_through_local_observation() {
        for side in [IpcFootprintSide::Front, IpcFootprintSide::Back] {
            for angle in [0, 90_000, 180_000, 270_000, 37_500] {
                let mut group = observed_footprint("fp", "pad");
                group.footprint.side = side;
                group.footprint.orientation_mdeg = angle;
                group.pads[0].position_x_nm = 12_825_000;
                group.pads[0].position_y_nm = 34_250_000;
                let observation =
                    board_footprint_observation(StableId::new("board").unwrap(), vec![group])
                        .unwrap();
                let placement = &observation.placements[0];
                let actual =
                    kicad_engine::pad_board_position(placement, &placement.pads[0]).unwrap();
                assert!((actual.x - 12_825_000).abs() <= 1);
                assert!((actual.y - 34_250_000).abs() <= 1);
                if side == IpcFootprintSide::Front && angle == 90_000 {
                    assert_eq!(
                        placement.pads[0].position,
                        LocalCoordinateNm {
                            x_nm: -250_000,
                            y_nm: 825_000
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn schematic_footprint_binding_rejects_conflicting_sources_and_pad_networks() {
        let (project, netlist) = schematic_fixture(false);
        let board_id = StableId::new("board").unwrap();
        let board = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.6".into(),
            capabilities: vec![],
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(board_id.clone()),
                ..Board::default()
            },
            native_footprints: Some(
                ObservedBoardFootprints::new(board_id.clone(), vec![]).unwrap(),
            ),
            native_routing: None,
            native_stackup: None,
        };
        let placement = kicad_model::AllowlistedFootprintPlacement::new(
            StableId::new("planned-footprint").unwrap(),
            FootprintLibraryIdentifier {
                nickname: "Resistor_SMD".into(),
                entry_name: "R_0603".into(),
            },
            board_coordinate(1_000_000, 2_000_000),
            "R1",
            "10k",
        )
        .unwrap();
        let binding =
            bindings::bind_schematic_footprint(&project, &netlist, &board, &placement).unwrap();
        assert_eq!(binding.pad_nets["1"].as_deref(), Some("GND"));
        assert_eq!(
            binding.symbol_path.last(),
            Some(&binding.symbol.source_uuid)
        );
        assert_eq!(binding.schematic_revision, project.revision.digest);
        let mut sourced = placement.clone();
        sourced.source = Some(Box::new(kicad_model::FootprintSourceBinding {
            resolution_digest: None,
            schematic: project.revision.clone(),
            symbol: binding.symbol.clone(),
            symbol_path: binding.symbol_path.clone(),
            pad_nets: binding.pad_nets.clone(),
            library_digest: RevisionDigest::new("a".repeat(64)).unwrap(),
            library_size: 42,
        }));
        sourced.validate().unwrap();
        let value = serde_json::to_value(&sourced).unwrap();
        assert_eq!(
            serde_json::from_value::<kicad_model::AllowlistedFootprintPlacement>(value.clone())
                .unwrap(),
            sourced
        );
        let mut wrong_path = value.clone();
        wrong_path["source"]["symbol_path"][0] = serde_json::json!("wrong-root");
        assert!(
            serde_json::from_value::<kicad_model::AllowlistedFootprintPlacement>(wrong_path)
                .is_err()
        );
        let mut missing_pin = value;
        missing_pin["source"]["pad_nets"] = serde_json::json!({});
        assert!(
            serde_json::from_value::<kicad_model::AllowlistedFootprintPlacement>(missing_pin)
                .is_err()
        );

        let mut stale = netlist.clone();
        stale.components[0].footprint = Some("Other:Wrong".into());
        assert!(bindings::bind_schematic_footprint(&project, &stale, &board, &placement).is_err());
        let mut changed_cst = project.clone();
        changed_cst.files[0].symbols[0].footprint = Some("Other:Wrong".into());
        assert!(
            bindings::bind_schematic_footprint(&changed_cst, &netlist, &board, &placement).is_err()
        );
        let (locked, _) = schematic_fixture(true);
        assert!(bindings::bind_schematic_footprint(&locked, &netlist, &board, &placement).is_err());
        let mut actual = board_footprint_observation(
            board_id.clone(),
            vec![observed_footprint("new-footprint", "new-pad")],
        )
        .unwrap()
        .placements
        .remove(0);
        actual.reference = placement.reference.clone();
        actual.value = placement.value.clone();
        actual.position = placement.position;
        actual.library_identifier = placement.library_identifier.clone();
        actual.definition_id = footprint_definition_id(&actual.library_identifier);
        actual.symbol_sheet_path = binding.symbol_path.clone();
        let joined =
            bindings::verify_placed_pin_pad_binding(&binding, &placement, &actual, &board_id)
                .unwrap();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].pad_id, actual.pads[0].pad_id);
        actual.pads[0].net.as_mut().unwrap().name = "VCC".into();
        assert!(
            bindings::verify_placed_pin_pad_binding(&binding, &placement, &actual, &board_id)
                .is_err()
        );
        actual.pads[0].net.as_mut().unwrap().name = "GND".into();
        actual.pads[0].number = "2".into();
        assert!(
            bindings::verify_placed_pin_pad_binding(&binding, &placement, &actual, &board_id)
                .is_err()
        );
    }

    #[test]
    fn schematic_observation_maps_connectivity_and_is_deterministic() {
        let (project, netlist) = schematic_fixture(false);
        let first =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        let second =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        assert_eq!(first, second);
        assert!(first.baseline_eligible);
        assert_eq!(first.capabilities[0].status, CapabilityStatus::Verified);
        assert!(matches!(
            first.symbols[0].pins[0].net,
            kicad_model::ReferenceResolution::Resolved(_)
        ));
        assert_eq!(
            first.symbols[0].footprint_field.as_deref(),
            Some("Resistor_SMD:R_0603")
        );
    }

    #[test]
    fn schematic_observation_lock_is_not_a_verified_baseline() {
        let (project, netlist) = schematic_fixture(true);
        let observation =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        assert!(!observation.baseline_eligible);
        assert_eq!(
            observation.capabilities[0].status,
            CapabilityStatus::Unknown
        );
        assert_eq!(
            observation.diagnostics[0].code,
            "schematic_editor_lock_present"
        );
    }

    #[test]
    fn schematic_observation_propagates_join_errors() {
        let (project, mut netlist) = schematic_fixture(false);
        netlist.nets[0].nodes[0].reference = "X1".to_owned();
        assert!(matches!(
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()),
            Err(SchematicObservationError::Join(
                kicad_schematic::ConnectivityJoinError::UnknownNode { .. }
            ))
        ));
    }

    fn observed_footprint(source_uuid: &str, pad_source_uuid: &str) -> ObservedFootprintWithPads {
        ObservedFootprintWithPads {
            footprint: IpcFootprintInstance {
                source_uuid: source_uuid.to_owned(),
                position_x_nm: 12_000_000,
                position_y_nm: 34_000_000,
                orientation_mdeg: 90_000,
                side: IpcFootprintSide::Back,
                lock_state: IpcLockState::Locked,
                library_identifier: kicad_ipc::ObservedFootprintLibraryIdentifier {
                    nickname: "Package_QFP".to_owned(),
                    entry_name: "LQFP-48".to_owned(),
                },
                reference: "U1".to_owned(),
                value: "MCU".to_owned(),
                symbol_sheet_path: vec!["root-kiid".to_owned(), "child-kiid".to_owned()],
            },
            pads: vec![IpcPad {
                source_uuid: pad_source_uuid.to_owned(),
                footprint_source_uuid: source_uuid.to_owned(),
                number: "1".to_owned(),
                kind: IpcPadKind::PlatedThroughHole,
                lock_state: IpcLockState::Unlocked,
                position_x_nm: 11_750_000,
                position_y_nm: 34_750_000,
                stack_type: IpcPadStackType::FrontInnerBack,
                orientation_mdeg: 45_000,
                layers: vec![
                    IpcBoardLayer::FrontCopper,
                    IpcBoardLayer::InnerCopper(1),
                    IpcBoardLayer::BackCopper,
                ],
                copper_shapes: vec![
                    kicad_ipc::ObservedPadCopperShape {
                        layer: IpcBoardLayer::FrontCopper,
                        shape: IpcPadShape::RoundedRectangle,
                        size_x_nm: 1_200_000,
                        size_y_nm: 800_000,
                        offset_x_nm: 10_000,
                        offset_y_nm: -20_000,
                        corner_rounding_ratio: 0.25,
                        chamfer_ratio: 0.0,
                        chamfer_top_left: false,
                        chamfer_top_right: false,
                        chamfer_bottom_left: false,
                        chamfer_bottom_right: false,
                        trapezoid_delta_x_nm: 0,
                        trapezoid_delta_y_nm: 0,
                        custom_anchor_shape: None,
                    },
                    kicad_ipc::ObservedPadCopperShape {
                        layer: IpcBoardLayer::InnerCopper(1),
                        shape: IpcPadShape::ChamferedRectangle,
                        size_x_nm: 1_000_000,
                        size_y_nm: 700_000,
                        offset_x_nm: 0,
                        offset_y_nm: 0,
                        corner_rounding_ratio: 0.0,
                        chamfer_ratio: 0.2,
                        chamfer_top_left: true,
                        chamfer_top_right: false,
                        chamfer_bottom_left: false,
                        chamfer_bottom_right: true,
                        trapezoid_delta_x_nm: 50_000,
                        trapezoid_delta_y_nm: 0,
                        custom_anchor_shape: Some(IpcPadShape::Rectangle),
                    },
                ],
                drill: Some(kicad_ipc::ObservedPadDrill {
                    shape: IpcDrillShape::Oblong,
                    size_x_nm: 400_000,
                    size_y_nm: 600_000,
                }),
                net_name: Some("GND".to_owned()),
            }],
        }
    }

    #[test]
    fn board_routing_mapping_preserves_empty_and_named_net_identity() {
        let board_id = StableId::new("board-a").unwrap();
        let track = |source_uuid: &str, name: &str| kicad_ipc::ObservedTrack {
            source_uuid: source_uuid.to_owned(),
            geometry: IpcTrackGeometry::Segment {
                start: kicad_ipc::ObservedRoutingPoint { x_nm: 0, y_nm: 0 },
                end: kicad_ipc::ObservedRoutingPoint { x_nm: 1, y_nm: 0 },
            },
            width_nm: 1,
            layer: IpcBoardLayer::FrontCopper,
            lock_state: IpcLockState::Unlocked,
            net: Some(kicad_ipc::ObservedRoutingNet {
                code: Some(0),
                name: name.to_owned(),
            }),
        };
        let observation = board_routing_observation(
            board_id.clone(),
            IpcBoardRoutingItems {
                tracks: vec![track("empty-net", ""), track("named-net", "GND")],
                vias: Vec::new(),
            },
        )
        .unwrap();
        let empty = observation
            .tracks
            .iter()
            .find(|track| track.source_uuid.as_str() == "empty-net")
            .unwrap()
            .net
            .as_ref()
            .unwrap();
        assert_eq!(empty.net_id, None);
        assert_eq!(empty.code, Some(0));
        assert_eq!(empty.name, "");
        let named = observation
            .tracks
            .iter()
            .find(|track| track.source_uuid.as_str() == "named-net")
            .unwrap()
            .net
            .as_ref()
            .unwrap();
        assert_eq!(named.net_id, Some(board_net_id(&board_id, "GND")));
    }

    #[test]
    fn board_footprint_mapping_preserves_identity_geometry_and_revision() {
        let board_id = StableId::new("board-a").unwrap();
        let observation = board_footprint_observation(
            board_id.clone(),
            vec![observed_footprint("footprint-a", "pad-a")],
        )
        .unwrap();
        assert!(observation.validate().is_ok());
        let placement = &observation.placements[0];
        assert_eq!(
            placement.placement_id,
            footprint_placement_id(&board_id, &placement.source_uuid)
        );
        assert_eq!(
            placement.definition_id,
            footprint_definition_id(&placement.library_identifier)
        );
        assert_eq!(
            (placement.position.x, placement.position.y),
            (12_000_000, 34_000_000)
        );
        assert_eq!(placement.position.unit, Unit::Nanometres);
        assert_eq!(placement.position.space, DocumentSpace::Board);
        assert_eq!(placement.orientation_mdeg, 90_000);
        assert_eq!(placement.side, BoardSide::Back);
        assert_eq!(placement.lock_state, LockState::Locked);
        assert_eq!(placement.reference, "U1");
        assert_eq!(placement.value, "MCU");
        assert_eq!(placement.library_identifier.nickname, "Package_QFP");
        assert_eq!(placement.library_identifier.entry_name, "LQFP-48");
        assert_eq!(
            placement
                .symbol_sheet_path
                .iter()
                .map(StableId::as_str)
                .collect::<Vec<_>>(),
            ["root-kiid", "child-kiid"]
        );
        let pad = &placement.pads[0];
        assert_eq!(
            pad.pad_id,
            footprint_pad_id(&placement.placement_id, &pad.source_uuid)
        );
        assert_eq!(
            pad.position,
            LocalCoordinateNm {
                x_nm: -750_000,
                y_nm: 250_000
            }
        );
        assert_eq!(pad.kind, PadKind::PlatedThroughHole);
        assert_eq!(pad.lock_state, LockState::Unlocked);
        assert_eq!(pad.stack_type, PadStackType::FrontInnerBack);
        assert_eq!(pad.orientation_mdeg, 315_000);
        assert_eq!(
            pad.layers,
            [
                BoardLayer::FrontCopper,
                BoardLayer::InnerCopper(1),
                BoardLayer::BackCopper
            ]
        );
        assert_eq!(pad.copper_shapes.len(), 2);
        assert_eq!(pad.copper_shapes[0].corner_rounding_ratio.as_f64(), 0.25);
        assert_eq!(pad.copper_shapes[1].layer, BoardLayer::InnerCopper(1));
        assert_eq!(pad.copper_shapes[1].chamfer_ratio.as_f64(), 0.2);
        assert!(pad.copper_shapes[1].chamfer_top_left);
        assert!(pad.copper_shapes[1].chamfer_bottom_right);
        assert_eq!(
            pad.copper_shapes[1].trapezoid_delta,
            LocalCoordinateNm {
                x_nm: 50_000,
                y_nm: 0
            }
        );
        assert_eq!(
            pad.copper_shapes[1].custom_anchor_shape,
            Some(PadShape::Rectangle)
        );
        assert_eq!(pad.drill.unwrap().shape, DrillShape::Oblong);
        assert_eq!(
            pad.net.as_ref().unwrap().net_id,
            board_net_id(&board_id, "GND")
        );
        assert_eq!(placement.observed_revision, observation.observed_revision);
        assert_eq!(pad.observed_revision, observation.observed_revision);
    }

    #[test]
    fn board_footprint_mapping_is_order_independent_and_board_scoped() {
        let board_id = StableId::new("board-a").unwrap();
        let first = observed_footprint("footprint-a", "pad-a");
        let second = observed_footprint("footprint-b", "pad-b");
        let forward =
            board_footprint_observation(board_id.clone(), vec![first.clone(), second.clone()])
                .unwrap();
        let reverse = board_footprint_observation(board_id.clone(), vec![second, first]).unwrap();
        assert_eq!(forward, reverse);
        assert_eq!(
            forward.placements[0].pads[0].net.as_ref().unwrap().net_id,
            forward.placements[1].pads[0].net.as_ref().unwrap().net_id
        );
        let other_board = board_footprint_observation(
            StableId::new("board-b").unwrap(),
            vec![observed_footprint("footprint-a", "pad-a")],
        )
        .unwrap();
        assert_ne!(
            forward.placements[0].pads[0].net.as_ref().unwrap().net_id,
            other_board.placements[0].pads[0]
                .net
                .as_ref()
                .unwrap()
                .net_id
        );
    }

    #[test]
    fn board_footprint_mapping_rejects_duplicate_and_mismatched_identity() {
        let board_id = StableId::new("board-a").unwrap();
        let group = observed_footprint("footprint-a", "pad-a");
        assert!(matches!(
            board_footprint_observation(board_id.clone(), vec![group.clone(), group]),
            Err(FootprintObservationError::DuplicateFootprintId(_))
        ));
        let mut duplicate_pad = observed_footprint("footprint-a", "pad-a");
        duplicate_pad.pads.push(duplicate_pad.pads[0].clone());
        assert!(matches!(
            board_footprint_observation(board_id.clone(), vec![duplicate_pad]),
            Err(FootprintObservationError::DuplicatePadId { .. })
        ));
        let mut mismatched = observed_footprint("footprint-a", "pad-a");
        mismatched.pads[0].footprint_source_uuid = "other-footprint".to_owned();
        assert!(matches!(
            board_footprint_observation(board_id, vec![mismatched]),
            Err(FootprintObservationError::PadParentMismatch { .. })
        ));
    }

    #[test]
    fn board_footprint_mapping_rejects_invalid_values() {
        let board_id = StableId::new("board-a").unwrap();
        let mut invalid_ratio = observed_footprint("footprint-a", "pad-a");
        invalid_ratio.pads[0].copper_shapes[0].corner_rounding_ratio = f64::NAN;
        assert!(matches!(
            board_footprint_observation(board_id.clone(), vec![invalid_ratio]),
            Err(FootprintObservationError::Model(_))
        ));
        let mut invalid_anchor = observed_footprint("footprint-a", "pad-a");
        invalid_anchor.pads[0].copper_shapes[0].custom_anchor_shape = Some(IpcPadShape::Oval);
        assert!(matches!(
            board_footprint_observation(board_id.clone(), vec![invalid_anchor]),
            Err(FootprintObservationError::Model(_))
        ));
        let invalid_id = observed_footprint("", "pad-a");
        assert!(matches!(
            board_footprint_observation(board_id, vec![invalid_id]),
            Err(FootprintObservationError::Model(_))
        ));
    }

    #[test]
    fn board_footprint_mapping_allows_empty_and_repeated_pad_numbers() {
        let mut group = observed_footprint("footprint-a", "pad-a");
        group.pads[0].number.clear();
        let mut second = group.pads[0].clone();
        second.source_uuid = "pad-b".to_owned();
        group.pads.push(second);
        let observation =
            board_footprint_observation(StableId::new("board-a").unwrap(), vec![group]).unwrap();
        assert_eq!(observation.placements[0].pads.len(), 2);
        assert!(
            observation.placements[0]
                .pads
                .iter()
                .all(|pad| pad.number.is_empty())
        );
    }

    fn observed_graphic(id: &str, x: i64) -> ObservedBoardGraphicSegment {
        ObservedBoardGraphicSegment {
            graphic_id: id.to_owned(),
            segment: IpcBoardGraphicSegment {
                start_x_nm: x,
                start_y_nm: 2,
                end_x_nm: x + 10,
                end_y_nm: 4,
                width_nm: 100_000,
                layer: IpcBoardGraphicLayer::DwgsUser,
            },
        }
    }

    fn native_footprints(document: &OpenDocument) -> ObservedBoardFootprints {
        let board_id = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        board_footprint_observation(board_id, vec![observed_footprint("footprint-a", "pad-a")])
            .unwrap()
    }

    fn native_routing(document: &OpenDocument) -> ObservedBoardRouting {
        let board_id = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        ObservedBoardRouting::new(board_id, Vec::new(), Vec::new()).unwrap()
    }

    fn native_stackup(document: &OpenDocument) -> ObservedBoardStackup {
        let board_id = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        ObservedBoardStackup::new(
            board_id,
            None,
            None,
            None,
            vec![ObservedStackupLayer {
                dielectric_type: None,
                soldermask: None,
                silkscreen_material: None,
                layer: ObservedStackupLayerIdentity::FrontCopper,
                enabled: true,
                layer_type: ObservedStackupLayerType::Copper,
                thickness_nm: 35_000,
                material_name: "Copper".to_owned(),
                user_name: "F.Cu".to_owned(),
                color: None,
                dielectric: None,
            }],
        )
        .unwrap()
    }

    fn ipc_stackup_layer(
        layer: kicad_ipc::ObservedStackupLayerIdentity,
        layer_type: kicad_ipc::ObservedStackupLayerType,
        thickness_nm: i64,
        user_name: &str,
    ) -> kicad_ipc::ObservedStackupLayer {
        kicad_ipc::ObservedStackupLayer {
            dielectric_type: None,
            soldermask: None,
            silkscreen_material: None,
            layer,
            enabled: true,
            layer_type,
            thickness_nm: Some(thickness_nm),
            color: None,
            material_name: String::new(),
            user_name: user_name.to_owned(),
            dielectric: (layer_type == kicad_ipc::ObservedStackupLayerType::Dielectric)
                .then(Vec::new),
        }
    }

    fn xlock_ipc_stackup() -> IpcBoardStackup {
        use kicad_ipc::ObservedStackupLayerIdentity as Identity;
        use kicad_ipc::ObservedStackupLayerType as LayerType;

        let mut front_mask = ipc_stackup_layer(
            Identity::FrontSoldermask,
            LayerType::Soldermask,
            10_000,
            "F.Mask",
        );
        front_mask.color = Some(kicad_ipc::ObservedStackupColor {
            red: 0.0,
            green: 0.5,
            blue: 1.0,
            alpha: 0.0,
        });
        let mut core =
            ipc_stackup_layer(Identity::Undefined, LayerType::Dielectric, 100_000, "Core");
        core.material_name = "FR4".to_owned();
        core.dielectric = Some(vec![kicad_ipc::ObservedStackupDielectricProperties {
            thickness_locked: false,
            spec_frequency: None,
            dielectric_model: Default::default(),
            epsilon_r: 4.2,
            loss_tangent: 0.0,
            material_name: "FR4".to_owned(),
            thickness_nm: Some(100_000),
        }]);
        let mut disabled_inner =
            ipc_stackup_layer(Identity::InnerCopper(1), LayerType::Copper, 0, "In1.Cu");
        disabled_inner.enabled = false;
        let zero_prepreg =
            ipc_stackup_layer(Identity::Undefined, LayerType::Dielectric, 0, "Prepreg");
        IpcBoardStackup {
            finish: Some(kicad_ipc::ObservedStackupFinish {
                type_name: "ENIG".to_owned(),
            }),
            impedance: Some(kicad_ipc::ObservedStackupImpedance {
                is_controlled: false,
            }),
            edge: Some(kicad_ipc::ObservedStackupEdge {
                connector: Some(kicad_ipc::StackupConnectorType::Unknown),
                castellation: Some(false),
                plating: None,
            }),
            layers: vec![
                ipc_stackup_layer(
                    Identity::FrontSilkscreen,
                    LayerType::Silkscreen,
                    12_000,
                    "F.SilkS",
                ),
                front_mask,
                ipc_stackup_layer(Identity::FrontCopper, LayerType::Copper, 35_000, "F.Cu"),
                core,
                disabled_inner,
                zero_prepreg,
                ipc_stackup_layer(Identity::BackCopper, LayerType::Copper, 35_000, "B.Cu"),
                ipc_stackup_layer(
                    Identity::BackSoldermask,
                    LayerType::Soldermask,
                    10_000,
                    "B.Mask",
                ),
                ipc_stackup_layer(
                    Identity::BackSilkscreen,
                    LayerType::Silkscreen,
                    12_000,
                    "B.SilkS",
                ),
            ],
        }
    }

    #[test]
    fn board_stackup_mapping_preserves_xlock_fields_order_and_presence() {
        let board_id = StableId::new("xlock-board").unwrap();
        let observation = board_stackup_observation(board_id.clone(), xlock_ipc_stackup()).unwrap();
        assert_eq!(observation.board_id, board_id);
        assert_eq!(observation.finish.as_ref().unwrap().type_name, "ENIG");
        assert!(!observation.impedance.as_ref().unwrap().is_controlled);
        assert_eq!(
            observation.edge,
            Some(ObservedStackupEdge {
                connector_present: true,
                connector_type: Some(kicad_model::StackupConnectorType::Unknown),
                castellation: Some(false),
                plating: None,
            })
        );
        assert_eq!(
            observation
                .layers
                .iter()
                .map(|layer| layer.layer)
                .collect::<Vec<_>>(),
            [
                ObservedStackupLayerIdentity::FrontSilkscreen,
                ObservedStackupLayerIdentity::FrontSoldermask,
                ObservedStackupLayerIdentity::FrontCopper,
                ObservedStackupLayerIdentity::Undefined,
                ObservedStackupLayerIdentity::InnerCopper(1),
                ObservedStackupLayerIdentity::Undefined,
                ObservedStackupLayerIdentity::BackCopper,
                ObservedStackupLayerIdentity::BackSoldermask,
                ObservedStackupLayerIdentity::BackSilkscreen,
            ]
        );
        let color = observation.layers[1].color.unwrap();
        assert_eq!(
            [
                color.red.as_f64(),
                color.green.as_f64(),
                color.blue.as_f64(),
                color.alpha.as_f64(),
            ],
            [0.0, 0.5, 1.0, 0.0]
        );
        let dielectric = observation.layers[3].dielectric.as_ref().unwrap();
        assert_eq!(dielectric[0].epsilon_r.as_f64(), 4.2);
        assert_eq!(dielectric[0].loss_tangent.as_f64(), 0.0);
        assert_eq!(dielectric[0].thickness_nm, 100_000);
        assert!(!observation.layers[4].enabled);
        assert_eq!(observation.layers[4].thickness_nm, 0);
        assert_eq!(observation.layers[5].thickness_nm, 0);
        assert_eq!(observation.layers[5].dielectric, Some(Vec::new()));
    }

    #[test]
    fn board_stackup_revision_is_board_scoped_content_sensitive_and_not_forgeable() {
        let first =
            board_stackup_observation(StableId::new("board-a").unwrap(), xlock_ipc_stackup())
                .unwrap();
        let other_board =
            board_stackup_observation(StableId::new("board-b").unwrap(), xlock_ipc_stackup())
                .unwrap();
        let mut changed_ipc = xlock_ipc_stackup();
        changed_ipc.layers.swap(3, 5);
        let changed =
            board_stackup_observation(StableId::new("board-a").unwrap(), changed_ipc).unwrap();
        assert_ne!(first.observed_revision, other_board.observed_revision);
        assert_ne!(first.observed_revision, changed.observed_revision);

        let mut forged = first;
        forged.observed_revision = other_board.observed_revision;
        assert!(matches!(
            forged.validate(),
            Err(ModelError::InvalidStackupObservation(
                "stackup observation revision does not match its board and content"
            ))
        ));
    }

    #[test]
    fn board_stackup_mapping_preserves_legacy_optional_absence() {
        let mut legacy = xlock_ipc_stackup();
        legacy.finish = None;
        legacy.impedance = None;
        legacy.edge = None;
        legacy.layers[1].color = None;
        let observation =
            board_stackup_observation(StableId::new("board").unwrap(), legacy).unwrap();
        assert_eq!(observation.finish, None);
        assert_eq!(observation.impedance, None);
        assert_eq!(observation.edge, None);
        assert_eq!(observation.layers[1].color, None);
    }

    #[test]
    fn live_board_observation_maps_identity_geometry_and_capabilities() {
        let document = pcb();
        let native_footprints = native_footprints(&document);
        let observation = live_board_observation(
            version(),
            document,
            vec![observed_graphic("graphic-1", 1)],
            native_footprints,
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let discovered = discovered_document(pcb()).unwrap().unwrap();
        assert_eq!(observation.board_filename, "xLock.kicad_pcb");
        assert_eq!(observation.instance_token.as_str(), "token");
        assert_eq!(
            observation.snapshot.board.document_id,
            Some(discovered.document_id)
        );
        assert_eq!(
            observation.snapshot.capabilities,
            [
                "pcb.board_graphics.read",
                "pcb.board_graphics.write",
                "pcb.native_footprints.read",
                "pcb.native_routing.read",
                "pcb.stackup.read",
            ]
        );
        let stackup = observation.snapshot.native_stackup.as_ref().unwrap();
        assert_eq!(stackup.layers[0].user_name, "F.Cu");
        let native = observation.snapshot.native_footprints.as_ref().unwrap();
        assert_eq!(
            observation.snapshot.board.document_id.as_ref(),
            Some(&native.board_id)
        );
        assert_eq!(native.placements.len(), 1);
        assert!(observation.snapshot.schematic.nets.is_empty());
        assert!(observation.snapshot.schematic.symbols.is_empty());
        assert!(observation.snapshot.board.footprints.is_empty());
        let graphic = &observation.snapshot.board.graphics[0];
        assert_eq!(graphic.graphic_id.as_str(), "graphic-1");
        assert_eq!((graphic.start.x, graphic.end.x), (1, 11));
        assert_eq!(graphic.start.space, DocumentSpace::Board);
        assert_eq!(graphic.width_nm, 100_000);
    }

    #[test]
    fn live_board_revision_is_independent_of_graphic_and_footprint_order() {
        let document = pcb();
        let board_id = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        let footprint_a = observed_footprint("footprint-a", "pad-a");
        let footprint_b = observed_footprint("footprint-b", "pad-b");
        let first_native = board_footprint_observation(
            board_id.clone(),
            vec![footprint_b.clone(), footprint_a.clone()],
        )
        .unwrap();
        let second_native =
            board_footprint_observation(board_id, vec![footprint_a, footprint_b]).unwrap();
        let first = live_board_observation(
            version(),
            document.clone(),
            vec![observed_graphic("b", 20), observed_graphic("a", 1)],
            first_native,
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let second = live_board_observation(
            version(),
            document.clone(),
            vec![observed_graphic("a", 1), observed_graphic("b", 20)],
            second_native,
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(
            first.snapshot.revision().unwrap(),
            second.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn live_board_revision_changes_with_native_pad_content() {
        let document = pcb();
        let board_id = discovered_document(document.clone())
            .unwrap()
            .unwrap()
            .document_id;
        let first_native = native_footprints(&document);
        let mut changed = observed_footprint("footprint-a", "pad-a");
        changed.pads[0].number = "2".to_owned();
        let changed_native = board_footprint_observation(board_id, vec![changed]).unwrap();
        let first = live_board_observation(
            version(),
            document.clone(),
            Vec::new(),
            first_native,
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let changed = live_board_observation(
            version(),
            document,
            Vec::new(),
            changed_native,
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        assert_ne!(
            first.snapshot.revision().unwrap(),
            changed.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn live_board_observation_rejects_native_footprints_from_another_board() {
        let error = live_board_observation(
            version(),
            pcb(),
            Vec::new(),
            board_footprint_observation(
                StableId::new("other-board").unwrap(),
                vec![observed_footprint("footprint-a", "pad-a")],
            )
            .unwrap(),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LiveBoardObservationError::Model(ModelError::InvalidFootprintObservation(
                "native footprint board identity does not match the live board"
            ))
        ));
    }

    #[test]
    fn live_board_observation_rejects_stackup_from_another_board_or_forged_revision() {
        let document = pcb();
        let wrong_board = ObservedBoardStackup::new(
            StableId::new("other-board").unwrap(),
            None,
            None,
            None,
            native_stackup(&document).layers,
        )
        .unwrap();
        let error = live_board_observation(
            version(),
            document.clone(),
            Vec::new(),
            native_footprints(&document),
            native_routing(&document),
            wrong_board,
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LiveBoardObservationError::Model(ModelError::InvalidStackupObservation(
                "native stackup board identity does not match the live board"
            ))
        ));

        let mut forged = native_stackup(&document);
        forged.observed_revision = RevisionDigest::new("0".repeat(64)).unwrap();
        let error = live_board_observation(
            version(),
            document.clone(),
            Vec::new(),
            native_footprints(&document),
            native_routing(&document),
            forged,
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LiveBoardObservationError::Model(ModelError::InvalidStackupObservation(
                "stackup observation revision does not match its board and content"
            ))
        ));
    }

    #[test]
    fn live_board_snapshot_is_stackup_content_and_order_sensitive() {
        let document = pcb();
        let board_id = board_document_id(&document).unwrap();
        let first_stackup =
            board_stackup_observation(board_id.clone(), xlock_ipc_stackup()).unwrap();
        let mut changed_ipc = xlock_ipc_stackup();
        changed_ipc.layers.swap(3, 5);
        let reordered_stackup = board_stackup_observation(board_id, changed_ipc).unwrap();
        let first = live_board_observation(
            version(),
            document.clone(),
            Vec::new(),
            native_footprints(&document),
            native_routing(&document),
            first_stackup,
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let reordered = live_board_observation(
            version(),
            document.clone(),
            Vec::new(),
            native_footprints(&document),
            native_routing(&document),
            reordered_stackup,
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        assert_ne!(first.snapshot, reordered.snapshot);
        assert_ne!(
            first.snapshot.revision().unwrap(),
            reordered.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn legacy_project_snapshot_keeps_stackup_absent() {
        let legacy = serde_json::json!({
            "project_id": "project",
            "kicad_version": "10.0.5",
            "capabilities": [],
            "schematic": {"nets": [], "symbols": []},
            "board": {"footprints": []}
        });
        let snapshot: ProjectSnapshot = serde_json::from_value(legacy).unwrap();
        assert_eq!(snapshot.native_stackup, None);
        assert!(
            serde_json::to_value(snapshot)
                .unwrap()
                .get("native_stackup")
                .is_none()
        );
    }

    #[test]
    fn live_board_observation_rejects_duplicate_or_invalid_graphic_ids() {
        let document = pcb();
        let duplicate = live_board_observation(
            version(),
            document.clone(),
            vec![observed_graphic("same", 1), observed_graphic("same", 20)],
            native_footprints(&document),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            duplicate,
            LiveBoardObservationError::DuplicateGraphicId(ref id) if id == "same"
        ));
        let invalid = live_board_observation(
            version(),
            document.clone(),
            vec![observed_graphic("", 1)],
            native_footprints(&document),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            invalid,
            LiveBoardObservationError::Model(ModelError::EmptyStableId)
        ));
    }

    #[test]
    fn live_board_observation_rejects_missing_project_identity() {
        let mut document = pcb();
        document.project_name = None;
        document.project_path = None;
        let error = live_board_observation(
            version(),
            document,
            Vec::new(),
            native_footprints(&pcb()),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LiveBoardObservationError::MissingProjectIdentity
        ));
    }

    #[test]
    fn live_board_observation_rejects_non_pcb_document() {
        let error = live_board_observation(
            version(),
            schematic("xLock.kicad_sch"),
            Vec::new(),
            native_footprints(&pcb()),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(error, LiveBoardObservationError::NonPcbDocument));

        let mut document = pcb();
        document.kind = OpenDocumentKind::Other;
        let error = live_board_observation(
            version(),
            document,
            Vec::new(),
            native_footprints(&pcb()),
            native_routing(&pcb()),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(error, LiveBoardObservationError::NonPcbDocument));
    }

    #[test]
    fn digest_is_independent_of_document_insertion_order() {
        let first = discovery_snapshot(input(vec![
            schematic("b.kicad_sch"),
            schematic("a.kicad_sch"),
        ]))
        .unwrap();
        let second = discovery_snapshot(input(vec![
            schematic("a.kicad_sch"),
            schematic("b.kicad_sch"),
        ]))
        .unwrap();
        assert_eq!(first.revision().unwrap(), second.revision().unwrap());
    }

    #[test]
    fn missing_project_identity_remains_explicit() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        assert_eq!(snapshot.documents[0].project_name, None);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "project_identity_incomplete" })
        );
    }

    #[test]
    fn artifact_discovery_finds_same_named_project_documents() {
        let root = std::env::temp_dir().join(format!(
            "kicad-adapter-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("fixture.kicad_pro"), "{}").unwrap();
        fs::write(root.join("fixture.kicad_sch"), "(kicad_sch)").unwrap();
        fs::write(root.join("fixture.kicad_pcb"), "(kicad_pcb)").unwrap();
        let snapshot =
            artifact_discovery_snapshot(&root, "ipc:///tmp/kicad/api.sock", version()).unwrap();
        assert_eq!(snapshot.documents.len(), 2);
        assert!(snapshot.capabilities.iter().any(|capability| {
            capability.id == "schematic.documents.discover"
                && capability.detail.as_deref() == Some("offline artifact source")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_pcb_and_unprobed_items_are_not_advertised_as_verified() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        let pcb = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "pcb.documents.discover")
            .unwrap();
        let items = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "schematic.items.read")
            .unwrap();
        assert_eq!(pcb.status, CapabilityStatus::Unsupported);
        assert_eq!(items.status, CapabilityStatus::Unknown);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "content_not_observed" })
        );
    }

    #[test]
    fn native_edit_transaction_drops_partial_updates_and_reports_ambiguity() {
        for fault in 0..6 {
            let mut ipc = FakeGraphicsIpc::default();
            match fault {
                1 => ipc.begin = Err("begin lost".into()),
                3 => ipc.drop = Err("drop lost".into()),
                4 => ipc.commit = Err("commit lost".into()),
                5 => ipc.save = Err("save lost".into()),
                _ => {}
            }
            let outcome =
                execute_native_edit_transaction(&mut ipc, "board.kicad_pcb", "edit", |ipc| {
                    ipc.calls.push("update-first".into());
                    ipc.calls.push("update-second".into());
                    if fault == 2 || fault == 3 {
                        Err("second item rejected".into())
                    } else {
                        Ok(())
                    }
                });
            match fault {
                0 => assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved),
                2 => assert!(matches!(
                    outcome,
                    BoardGraphicsCommitOutcome::Rejected { .. }
                )),
                _ => assert!(matches!(
                    outcome,
                    BoardGraphicsCommitOutcome::Unknown { .. }
                )),
            }
            let expected = match fault {
                1 => vec!["begin"],
                2 | 3 => vec!["begin", "update-first", "update-second", "drop:c1"],
                4 => vec!["begin", "update-first", "update-second", "commit:c1"],
                _ => vec![
                    "begin",
                    "update-first",
                    "update-second",
                    "commit:c1",
                    "save:board.kicad_pcb",
                ],
            };
            assert_eq!(ipc.calls, expected, "fault {fault}");
        }
    }

    struct FakeGraphicsIpc {
        move_group: Option<ObservedFootprintWithPads>,
        move_metadata_error: Option<String>,
        update_error: Option<String>,
        corrupt_move: bool,
        begin: Result<String, String>,
        create: Result<(), String>,
        track: Result<(), String>,
        via: Result<(), String>,
        via_calls: usize,
        fail_via_on_call: Option<usize>,
        routing_receipt_matches: bool,
        commit: Result<(), String>,
        drop: Result<(), String>,
        save: Result<(), String>,
        calls: Vec<String>,
        graphics: Vec<IpcBoardGraphicSegment>,
        observed: Option<Result<Vec<ObservedBoardGraphicSegment>, String>>,
        routing_observed: Option<Result<IpcBoardRoutingItems, String>>,
    }

    impl Default for FakeGraphicsIpc {
        fn default() -> Self {
            Self {
                move_group: None,
                move_metadata_error: None,
                update_error: None,
                corrupt_move: false,
                begin: Ok("c1".into()),
                create: Ok(()),
                track: Ok(()),
                via: Ok(()),
                via_calls: 0,
                fail_via_on_call: None,
                routing_receipt_matches: true,
                commit: Ok(()),
                drop: Ok(()),
                save: Ok(()),
                calls: Vec::new(),
                graphics: Vec::new(),
                observed: None,
                routing_observed: None,
            }
        }
    }

    impl BoardGraphicsIpc for FakeGraphicsIpc {
        fn require_supported_footprint_transform(
            &mut self,
            _: &str,
            _: &str,
        ) -> Result<(), String> {
            match &self.move_metadata_error {
                Some(reason) => Err(reason.clone()),
                None => Ok(()),
            }
        }

        fn read_unsupported_move_copper(&mut self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }

        fn read_footprints(&mut self, _: &str) -> Result<Vec<IpcFootprintInstance>, String> {
            Ok(vec![
                self.move_group
                    .as_ref()
                    .ok_or("no move fixture")?
                    .footprint
                    .clone(),
            ])
        }
        fn read_footprint_pads(&mut self, _: &str, _: &str) -> Result<Vec<IpcPad>, String> {
            Ok(self
                .move_group
                .as_ref()
                .ok_or("no move fixture")?
                .pads
                .clone())
        }
        fn read_pcb_item_diagnostics(
            &mut self,
            _: &str,
            _: &[PcbItemKind],
        ) -> Result<Vec<PcbItemDiagnostic>, String> {
            Ok(vec![])
        }
        fn update_footprint_instance(
            &mut self,
            _: &str,
            source: &str,
            x: i64,
            y: i64,
            angle: i64,
        ) -> Result<UpdatedFootprintInstanceReceipt, String> {
            self.calls.push("update".into());
            if let Some(error) = &self.update_error {
                return Err(error.clone());
            }
            let group = self.move_group.as_mut().unwrap();
            assert_eq!(
                angle, group.footprint.orientation_mdeg,
                "mock supports translation only"
            );
            for pad in &mut group.pads {
                pad.position_x_nm += x - group.footprint.position_x_nm;
                pad.position_y_nm += y - group.footprint.position_y_nm;
                if self.corrupt_move {
                    pad.number = "corrupt".into();
                }
            }
            group.footprint.position_x_nm = x;
            group.footprint.position_y_nm = y;
            Ok(UpdatedFootprintInstanceReceipt {
                footprint: group.footprint.clone(),
                source_uuid: source.into(),
                position_x_nm: x,
                position_y_nm: y,
                orientation_mdeg: angle,
            })
        }
        fn begin_commit(&mut self) -> Result<String, String> {
            self.calls.push("begin".into());
            self.begin.clone()
        }

        fn create_board_graphics(
            &mut self,
            filename: &str,
            graphics: &[IpcBoardGraphicSegment],
        ) -> Result<Vec<CreatedBoardGraphicReceipt>, String> {
            self.calls.push(format!("create:{filename}"));
            self.graphics = graphics.to_vec();
            self.create.clone().map(|_| {
                graphics
                    .iter()
                    .enumerate()
                    .map(|(request_index, segment)| CreatedBoardGraphicReceipt {
                        request_index,
                        graphic_id: format!("g{}", request_index + 1),
                        segment: segment.clone(),
                        lock_state: kicad_ipc::ObservedLockState::Unlocked,
                    })
                    .collect()
            })
        }

        fn create_track(
            &mut self,
            filename: &str,
            track: &CreateTrack,
        ) -> Result<CreatedTrackReceipt, String> {
            self.calls.push(format!("track:{filename}"));
            self.track.clone().map(|_| CreatedTrackReceipt {
                request_index: usize::from(!self.routing_receipt_matches),
                source_uuid: "track-uuid".into(),
                track: track.clone(),
                lock_state: kicad_ipc::ObservedLockState::Unlocked,
            })
        }

        fn create_through_via(
            &mut self,
            filename: &str,
            via: &CreateThroughVia,
        ) -> Result<CreatedThroughViaReceipt, String> {
            self.calls.push(format!("via:{filename}"));
            self.via_calls += 1;
            if self.fail_via_on_call == Some(self.via_calls) {
                return Err("injected second via failure".into());
            }
            self.via.clone().map(|_| CreatedThroughViaReceipt {
                request_index: usize::from(!self.routing_receipt_matches),
                source_uuid: "via-uuid".into(),
                via: via.clone(),
                lock_state: kicad_ipc::ObservedLockState::Unlocked,
            })
        }

        fn commit_changes(&mut self, commit: &str, _: &str) -> Result<(), String> {
            self.calls.push(format!("commit:{commit}"));
            self.commit.clone()
        }

        fn drop_commit(&mut self, commit: &str) -> Result<(), String> {
            self.calls.push(format!("drop:{commit}"));
            self.drop.clone()
        }

        fn save_board(&mut self, filename: &str) -> Result<(), String> {
            self.calls.push(format!("save:{filename}"));
            self.save.clone()
        }

        fn read_board_graphics(
            &mut self,
            filename: &str,
            _layer: IpcBoardGraphicLayer,
        ) -> Result<Vec<ObservedBoardGraphicSegment>, String> {
            self.calls.push(format!("read:{filename}"));
            if let Some(observed) = &self.observed {
                return observed.clone();
            }
            Ok(self
                .graphics
                .iter()
                .enumerate()
                .map(|(index, segment)| ObservedBoardGraphicSegment {
                    graphic_id: format!("g{}", index + 1),
                    segment: segment.clone(),
                })
                .collect())
        }

        fn read_board_graphics_by_id(
            &mut self,
            filename: &str,
            graphic_ids: &[String],
            _layer: IpcBoardGraphicLayer,
        ) -> Result<Vec<ObservedBoardGraphicSegment>, String> {
            self.calls.push(format!("read-by-id:{filename}"));
            let observed = if let Some(observed) = &self.observed {
                observed.clone()?
            } else {
                self.graphics
                    .iter()
                    .enumerate()
                    .map(|(index, segment)| ObservedBoardGraphicSegment {
                        graphic_id: format!("g{}", index + 1),
                        segment: segment.clone(),
                    })
                    .collect()
            };
            Ok(observed
                .into_iter()
                .filter(|graphic| graphic_ids.contains(&graphic.graphic_id))
                .collect())
        }

        fn read_board_routing(&mut self, filename: &str) -> Result<IpcBoardRoutingItems, String> {
            self.calls.push(format!("routing-read:{filename}"));
            self.routing_observed
                .clone()
                .ok_or_else(|| "routing observation not configured".to_owned())?
        }

        fn read_board_routing_by_id(
            &mut self,
            filename: &str,
            routing_ids: &[String],
        ) -> Result<IpcBoardRoutingItems, String> {
            self.calls.push(format!("routing-read-by-id:{filename}"));
            let mut observed = self
                .routing_observed
                .clone()
                .ok_or_else(|| "routing observation not configured".to_owned())??;
            observed
                .tracks
                .retain(|track| routing_ids.contains(&track.source_uuid));
            observed
                .vias
                .retain(|via| routing_ids.contains(&via.source_uuid));
            Ok(observed)
        }
    }

    fn graphic() -> BoardGraphicSegment {
        BoardGraphicSegment {
            graphic_id: StableId::new("g1").unwrap(),
            start: Coordinate {
                x: 1,
                y: 2,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            end: Coordinate {
                x: 3,
                y: 4,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            width_nm: 100,
            layer: BoardGraphicLayer::DwgsUser,
        }
    }

    fn graphics_operation() -> OperationKind {
        OperationKind::AddBoardGraphicSegments {
            board: StableId::new("board").unwrap(),
            segments: vec![graphic()],
        }
    }

    #[test]
    fn footprint_batch_dispatch_preserves_transaction_and_postcondition_failures() {
        let document = pcb();
        let board = board_document_id(&document).unwrap();
        let mut group = observed_footprint("fp", "pad");
        group.footprint.lock_state = IpcLockState::Unlocked;
        let empty = IpcBoardRoutingItems {
            tracks: vec![],
            vias: vec![],
        };
        let snapshot = live_board_observation(
            version(),
            document,
            vec![],
            board_footprint_observation(board.clone(), vec![group.clone()]).unwrap(),
            board_routing_observation(board.clone(), empty.clone()).unwrap(),
            native_stackup(&pcb()),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap()
        .snapshot;
        let target = &snapshot.native_footprints.as_ref().unwrap().placements[0];
        let operation = Operation {
            operation_id: StableId::new("move").unwrap(),
            kind: OperationKind::MoveExistingFootprint {
                board: board.clone(),
                placement_id: target.placement_id.clone(),
                source_uuid: target.source_uuid.clone(),
                position: board_coordinate(13_000_000, 34_000_000),
                orientation_mdeg: 90_000,
                copper_tolerance_nm: kicad_engine::FOOTPRINT_COPPER_TOLERANCE_NM,
                native_evidence: RevisionDigest::new("a".repeat(64)).unwrap(),
            },
            targets: vec![
                board,
                target.placement_id.clone(),
                target.source_uuid.clone(),
            ],
            preconditions: vec![],
            expected_effects: vec!["rigid transform".into()],
            risk_class: kicad_engine::RiskClass::Medium,
            idempotency_key: "move".into(),
            source_run_id: "test".into(),
        };
        for fault in 0..7 {
            let mut ipc = FakeGraphicsIpc {
                move_group: Some(group.clone()),
                routing_observed: Some(Ok(empty.clone())),
                ..Default::default()
            };
            match fault {
                1 => ipc.update_error = Some("immutable".into()),
                2 => ipc.save = Err("save failed".into()),
                3 => ipc.corrupt_move = true,
                4 => {
                    ipc.update_error = Some("timeout".into());
                    ipc.drop = Err("drop failed".into());
                }
                5 => ipc.commit = Err("editor process exited".into()),
                6 => ipc.move_metadata_error = Some("native unit metadata is unsupported".into()),
                _ => {}
            }
            let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
                &mut ipc,
                std::slice::from_ref(&operation),
                &snapshot,
                &snapshot.revision().unwrap(),
                "board.kicad_pcb",
                "move",
            );
            match fault {
                0 => assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved),
                6 => {
                    assert!(matches!(
                        outcome,
                        BoardGraphicsCommitOutcome::Rejected { .. }
                    ));
                    assert!(
                        ipc.calls.is_empty(),
                        "metadata rejection must precede BeginCommit"
                    );
                    continue;
                }
                1 => {
                    assert!(matches!(
                        outcome,
                        BoardGraphicsCommitOutcome::Rejected { .. }
                    ));
                    assert_eq!(ipc.calls.last().unwrap(), "drop:c1");
                }
                _ => assert!(
                    matches!(outcome, BoardGraphicsCommitOutcome::Unknown { .. }),
                    "{outcome:?}"
                ),
            }
            assert_eq!(ipc.calls.iter().filter(|c| *c == "update").count(), 1);
        }
        let mut ipc = FakeGraphicsIpc::default();
        assert!(matches!(
            BoardGraphicsExecutor::default().execute_bound_batch(
                &mut ipc,
                &[operation.clone(), operation],
                &snapshot,
                &snapshot.revision().unwrap(),
                "board.kicad_pcb",
                "mixed"
            ),
            BoardGraphicsCommitOutcome::Rejected { .. }
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn graphics_executor_rejects_invalid_input_before_ipc() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            ..Default::default()
        };
        let mut invalid = graphic();
        invalid.start.space = DocumentSpace::Schematic;
        let operation = OperationKind::AddBoardGraphicSegments {
            board: StableId::new("board").unwrap(),
            segments: vec![invalid],
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "draw",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected { .. }
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn graphics_executor_reports_create_rejection_after_drop() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Err("create failed".into()),
            drop: Ok(()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected {
                reason: "create failed".into()
            }
        );
        assert_eq!(ipc.calls, ["begin", "create:board.kicad_pcb", "drop:c1"]);
    }

    #[test]
    fn graphics_executor_reports_unknown_when_create_and_drop_fail() {
        let mut ipc = FakeGraphicsIpc {
            create: Err("create failed".into()),
            drop: Err("drop failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "create failed: create failed; drop failed: drop failed".into()
            }
        );
    }

    #[test]
    fn graphics_executor_reports_unknown_when_commit_transport_fails() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Ok(()),
            commit: Err("transport lost".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "transport lost".into()
            }
        );
    }

    #[test]
    fn graphics_executor_reports_committed_but_not_saved() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Ok(()),
            commit: Ok(()),
            save: Err("save failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "save failed".into()
            }
        );
        assert_eq!(ipc.graphics[0].layer, IpcBoardGraphicLayer::DwgsUser);
    }

    #[test]
    fn graphics_executor_rejects_missing_saved_postcondition() {
        let mut ipc = FakeGraphicsIpc {
            observed: Some(Ok(Vec::new())),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("missing"))
        );
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "create:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "read:board.kicad_pcb"
            ]
        );
    }

    #[test]
    fn graphics_executor_writes_and_reads_edge_cuts_outline_layer() {
        let outline = expand_board_outline(
            Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            Coordinate {
                x: 1_000,
                y: 500,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            10,
        );
        let operation = OperationKind::AddBoardOutline {
            board: StableId::new("board").unwrap(),
            top_left: outline[0].start,
            bottom_right: outline[2].start,
            width_nm: 10,
        };
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "outline",
        );
        assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved);
        assert!(
            ipc.graphics
                .iter()
                .all(|segment| { segment.layer == IpcBoardGraphicLayer::EdgeCuts })
        );
        assert_eq!(
            ipc.calls.last().map(String::as_str),
            Some("read:board.kicad_pcb")
        );
    }

    #[test]
    fn graphics_executor_rejects_mismatched_saved_postcondition() {
        let mut mismatched = IpcBoardGraphicSegment {
            start_x_nm: 1,
            start_y_nm: 2,
            end_x_nm: 30,
            end_y_nm: 40,
            width_nm: 100,
            layer: IpcBoardGraphicLayer::DwgsUser,
        };
        mismatched.end_x_nm = 31;
        let mut ipc = FakeGraphicsIpc {
            observed: Some(Ok(vec![ObservedBoardGraphicSegment {
                graphic_id: "g1".into(),
                segment: mismatched,
            }])),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("does not match"))
        );
    }

    fn board_coordinate(x: i64, y: i64) -> Coordinate {
        Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        }
    }

    fn observed_track_for_test(
        source_uuid: &str,
        start: (i64, i64),
        end: (i64, i64),
        width_nm: i64,
    ) -> kicad_ipc::ObservedTrack {
        kicad_ipc::ObservedTrack {
            source_uuid: source_uuid.to_owned(),
            geometry: IpcTrackGeometry::Segment {
                start: kicad_ipc::ObservedRoutingPoint {
                    x_nm: start.0,
                    y_nm: start.1,
                },
                end: kicad_ipc::ObservedRoutingPoint {
                    x_nm: end.0,
                    y_nm: end.1,
                },
            },
            width_nm,
            layer: IpcBoardLayer::FrontCopper,
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            net: None,
        }
    }

    fn observed_via_for_test(source_uuid: &str) -> kicad_ipc::ObservedVia {
        let copper_shape = |layer, size_nm| kicad_ipc::ObservedPadCopperShape {
            layer,
            shape: IpcPadShape::Circle,
            size_x_nm: size_nm,
            size_y_nm: size_nm,
            offset_x_nm: 0,
            offset_y_nm: 0,
            corner_rounding_ratio: 0.0,
            chamfer_ratio: 0.0,
            chamfer_top_left: false,
            chamfer_top_right: false,
            chamfer_bottom_left: false,
            chamfer_bottom_right: false,
            trapezoid_delta_x_nm: 0,
            trapezoid_delta_y_nm: 0,
            custom_anchor_shape: None,
        };
        kicad_ipc::ObservedVia {
            source_uuid: source_uuid.to_owned(),
            position: kicad_ipc::ObservedRoutingPoint { x_nm: 5, y_nm: 6 },
            pad_stack: kicad_ipc::ObservedViaPadStack {
                stack_type: IpcPadStackType::FrontInnerBack,
                layers: vec![IpcBoardLayer::FrontCopper, IpcBoardLayer::BackCopper],
                primary_drill: Some(kicad_ipc::ObservedViaDrill {
                    start_layer: IpcBoardLayer::FrontCopper,
                    end_layer: IpcBoardLayer::BackCopper,
                    size_x_nm: 100,
                    size_y_nm: 100,
                    shape: IpcDrillShape::Circle,
                    capping: kicad_ipc::ObservedViaDrillCappingMode::Unknown,
                    filling: kicad_ipc::ObservedViaDrillFillingMode::Unknown,
                }),
                unconnected_layer_removal: kicad_ipc::ObservedUnconnectedLayerRemoval::Unknown,
                copper_shapes: vec![
                    copper_shape(IpcBoardLayer::FrontCopper, 200),
                    copper_shape(IpcBoardLayer::InnerCopper(1), 0),
                    copper_shape(IpcBoardLayer::BackCopper, 200),
                ],
                angle_degrees: 0.0,
                front_outer_layers: None,
                back_outer_layers: None,
                zone_settings: None,
                secondary_drill: None,
                tertiary_drill: None,
                front_post_machining: None,
                back_post_machining: None,
            },
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            net: None,
            via_type: kicad_ipc::ObservedViaType::Through,
        }
    }

    fn via_operation_for_test() -> OperationKind {
        OperationKind::AddThroughVia {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::ThroughViaIntent::new(
                StableId::new("via").unwrap(),
                board_coordinate(5, 6),
                200,
                100,
            )
            .unwrap(),
        }
    }

    fn track_operation_for_test() -> OperationKind {
        OperationKind::AddStraightTrack {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::StraightTrackIntent::new(
                StableId::new("track").unwrap(),
                board_coordinate(1, 2),
                board_coordinate(3, 4),
                100,
            )
            .unwrap(),
        }
    }

    #[test]
    fn native_pcb_batch_uses_one_commit_save_and_exact_postcondition_reads() {
        let graphics = vec![IpcBoardGraphicSegment {
            start_x_nm: 0,
            start_y_nm: 0,
            end_x_nm: 10,
            end_y_nm: 0,
            width_nm: 100,
            layer: IpcBoardGraphicLayer::EdgeCuts,
        }];
        let tracks = vec![CreateTrack {
            layer: kicad_ipc::CopperLayer::FRONT,
            start_x_nm: 1,
            start_y_nm: 2,
            end_x_nm: 3,
            end_y_nm: 4,
            width_nm: 100,
            net: None,
        }];
        let vias = vec![CreateThroughVia {
            copper_layers: vec![kicad_ipc::CopperLayer::FRONT, kicad_ipc::CopperLayer::BACK],
            position_x_nm: 5,
            position_y_nm: 6,
            pad_diameter_nm: 200,
            drill_diameter_nm: 100,
            net: None,
        }];
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: vec![observed_track_for_test("track-uuid", (3, 4), (1, 2), 100)],
                vias: vec![observed_via_for_test("via-uuid")],
            })),
            ..Default::default()
        };
        let receipt = BoardGraphicsExecutor::default()
            .execute_native_batch(
                &mut ipc,
                "board.kicad_pcb",
                &graphics,
                &tracks,
                &vias,
                "native batch",
            )
            .unwrap();
        assert_eq!(receipt.graphics.len(), 1);
        assert_eq!(receipt.tracks.len(), 1);
        assert_eq!(receipt.vias.len(), 1);
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "create:board.kicad_pcb",
                "track:board.kicad_pcb",
                "via:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "read-by-id:board.kicad_pcb",
                "routing-read-by-id:board.kicad_pcb",
            ]
        );
    }

    #[test]
    fn additive_creation_leaves_transaction_lifecycle_to_caller() {
        let track = CreateTrack {
            layer: kicad_ipc::CopperLayer::FRONT,
            start_x_nm: 1,
            start_y_nm: 2,
            end_x_nm: 3,
            end_y_nm: 4,
            width_nm: 100,
            net: None,
        };
        let via = CreateThroughVia {
            copper_layers: vec![kicad_ipc::CopperLayer::FRONT, kicad_ipc::CopperLayer::BACK],
            position_x_nm: 5,
            position_y_nm: 6,
            pad_diameter_nm: 200,
            drill_diameter_nm: 100,
            net: None,
        };
        let graphic = IpcBoardGraphicSegment {
            start_x_nm: 0,
            start_y_nm: 0,
            end_x_nm: 10,
            end_y_nm: 0,
            width_nm: 100,
            layer: IpcBoardGraphicLayer::DwgsUser,
        };
        // Definite rejection, partial writes and uncertain responses share the same ownership boundary.
        for fault in 0..6 {
            let mut ipc = FakeGraphicsIpc::default();
            match fault {
                1 => ipc.create = Err("graphic transport timeout".into()),
                2 => ipc.track = Err("track transport timeout".into()),
                3 => ipc.via = Err("via transport timeout".into()),
                4 => ipc.routing_receipt_matches = false,
                _ => {}
            }
            let mut input_track = track.clone();
            if fault == 5 {
                input_track.width_nm = 0;
            }
            let outcome = BoardGraphicsExecutor::default().create_native_batch_in_transaction(
                &mut ipc,
                "board.kicad_pcb",
                std::slice::from_ref(&graphic),
                &[input_track],
                std::slice::from_ref(&via),
            );
            assert_eq!(outcome.is_ok(), fault == 0, "fault {fault}");
            let calls: &[&str] = match fault {
                0 | 3 => &[
                    "create:board.kicad_pcb",
                    "track:board.kicad_pcb",
                    "via:board.kicad_pcb",
                ],
                1 => &["create:board.kicad_pcb"],
                2 | 4 => &["create:board.kicad_pcb", "track:board.kicad_pcb"],
                _ => &[],
            };
            assert_eq!(ipc.calls, calls, "fault {fault}");
        }
    }

    #[test]
    fn native_pcb_batch_drops_nonzero_single_item_receipt_before_commit() {
        let mut ipc = FakeGraphicsIpc {
            routing_receipt_matches: false,
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default()
            .execute_native_batch(
                &mut ipc,
                "board.kicad_pcb",
                &[],
                &[CreateTrack {
                    layer: kicad_ipc::CopperLayer::FRONT,
                    start_x_nm: 1,
                    start_y_nm: 2,
                    end_x_nm: 3,
                    end_y_nm: 4,
                    width_nm: 100,
                    net: None,
                }],
                &[],
                "native batch",
            )
            .unwrap_err();
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Rejected { reason } if reason.contains("track receipt"))
        );
        assert_eq!(ipc.calls, vec!["begin", "track:board.kicad_pcb", "drop:c1"]);
    }

    #[test]
    fn routing_executor_accepts_exact_saved_track_observation() {
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: vec![observed_track_for_test("track-uuid", (3, 4), (1, 2), 100)],
                vias: Vec::new(),
            })),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &track_operation_for_test(),
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved);
    }

    #[test]
    fn routing_executor_accepts_exact_saved_through_via_observation() {
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: Vec::new(),
                vias: vec![observed_via_for_test("via-uuid")],
            })),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &via_operation_for_test(),
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved);
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "via:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "routing-read-by-id:board.kicad_pcb",
            ]
        );
    }

    #[test]
    fn created_route_reload_normalizes_only_unused_circle_fields_and_inner_placeholder() {
        let before = IpcBoardRoutingItems {
            tracks: vec![],
            vias: vec![observed_via_for_test("via")],
        };
        let mut after = before.clone();
        let diameter = after.vias[0].pad_stack.copper_shapes[0].size_x_nm;
        for shape in &mut after.vias[0].pad_stack.copper_shapes {
            shape.custom_anchor_shape = Some(kicad_ipc::ObservedPadShape::Rectangle);
            if shape.layer == IpcBoardLayer::InnerCopper(1) {
                shape.size_x_nm = diameter;
                shape.size_y_nm = diameter;
            }
        }
        assert!(created_routing_matches_reloaded(&before, &after));
        let cases: [fn(&mut IpcVia); 5] = [
            |v| v.position.x_nm += 1,
            |v| v.pad_stack.copper_shapes[1].size_x_nm += 1,
            |v| v.pad_stack.copper_shapes[0].offset_x_nm += 1,
            |v| v.pad_stack.primary_drill.as_mut().unwrap().size_x_nm += 1,
            |v| v.pad_stack.layers.push(IpcBoardLayer::InnerCopper(2)),
        ];
        for change in cases {
            let mut invalid = after.clone();
            change(&mut invalid.vias[0]);
            assert!(!created_routing_matches_reloaded(&before, &invalid));
        }
    }

    #[test]
    fn observed_via_match_requires_canonical_two_layer_shapes() {
        let receipt = CreatedThroughViaReceipt {
            request_index: 0,
            source_uuid: "via-uuid".into(),
            via: CreateThroughVia {
                copper_layers: vec![kicad_ipc::CopperLayer::FRONT, kicad_ipc::CopperLayer::BACK],
                position_x_nm: 5,
                position_y_nm: 6,
                pad_diameter_nm: 200,
                drill_diameter_nm: 100,
                net: None,
            },
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
        };
        let canonical = observed_via_for_test("via-uuid");
        assert!(observed_via_matches(&canonical, &receipt, None));

        let mut wrong_stack_layers = canonical.clone();
        wrong_stack_layers
            .pad_stack
            .layers
            .insert(1, IpcBoardLayer::InnerCopper(1));
        assert!(!observed_via_matches(&wrong_stack_layers, &receipt, None));

        let mut wrong_shape_order = canonical.clone();
        wrong_shape_order.pad_stack.copper_shapes.swap(0, 2);
        assert!(!observed_via_matches(&wrong_shape_order, &receipt, None));

        let mut reopened_inner_shape = canonical;
        reopened_inner_shape.pad_stack.copper_shapes[1].size_x_nm = 200;
        reopened_inner_shape.pad_stack.copper_shapes[1].size_y_nm = 200;
        assert!(observed_via_matches(&reopened_inner_shape, &receipt, None));

        reopened_inner_shape.pad_stack.copper_shapes[1].size_x_nm = 199;
        reopened_inner_shape.pad_stack.copper_shapes[1].size_y_nm = 199;
        assert!(!observed_via_matches(&reopened_inner_shape, &receipt, None));
    }

    #[test]
    fn routing_executor_rejects_missing_saved_track_observation() {
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems::default())),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &track_operation_for_test(),
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("missing"))
        );
    }

    #[test]
    fn routing_executor_rejects_mismatched_saved_track_observation() {
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: vec![observed_track_for_test("track-uuid", (1, 2), (3, 4), 101)],
                vias: Vec::new(),
            })),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &track_operation_for_test(),
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("does not match"))
        );
    }

    #[test]
    fn routing_executor_rejects_duplicate_saved_track_observation() {
        let track = observed_track_for_test("track-uuid", (1, 2), (3, 4), 100);
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: vec![track.clone(), track],
                vias: Vec::new(),
            })),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &track_operation_for_test(),
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("not unique"))
        );
    }

    #[test]
    fn routing_executor_creates_track_and_via_but_reports_unknown_without_observation() {
        let operations = vec![
            Operation {
                operation_id: StableId::new("op-track").unwrap(),
                kind: OperationKind::AddStraightTrack {
                    board: StableId::new("board").unwrap(),
                    intent: kicad_model::StraightTrackIntent::new(
                        StableId::new("track").unwrap(),
                        board_coordinate(1, 2),
                        board_coordinate(3, 4),
                        100,
                    )
                    .unwrap(),
                },
                targets: Vec::new(),
                preconditions: Vec::new(),
                expected_effects: Vec::new(),
                risk_class: kicad_engine::RiskClass::Low,
                idempotency_key: "track".into(),
                source_run_id: "test".into(),
            },
            Operation {
                operation_id: StableId::new("op-via").unwrap(),
                kind: OperationKind::AddThroughVia {
                    board: StableId::new("board").unwrap(),
                    intent: kicad_model::ThroughViaIntent::new(
                        StableId::new("via").unwrap(),
                        board_coordinate(5, 6),
                        200,
                        100,
                    )
                    .unwrap(),
                },
                targets: Vec::new(),
                preconditions: Vec::new(),
                expected_effects: Vec::new(),
                risk_class: kicad_engine::RiskClass::Low,
                idempotency_key: "via".into(),
                source_run_id: "test".into(),
            },
        ];
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute_batch(
            &mut ipc,
            &operations,
            "board.kicad_pcb",
            "route",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown { reason }
                if reason.contains("routing observation not configured")
        ));
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "track:board.kicad_pcb",
                "via:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "routing-read-by-id:board.kicad_pcb",
            ]
        );
    }

    #[test]
    fn routing_executor_drops_commit_after_create_failure() {
        let operation = OperationKind::AddThroughVia {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::ThroughViaIntent::new(
                StableId::new("via").unwrap(),
                board_coordinate(5, 6),
                200,
                100,
            )
            .unwrap(),
        };
        let mut ipc = FakeGraphicsIpc {
            via: Err("create failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected {
                reason: "create failed".into()
            }
        );
        assert_eq!(ipc.calls, vec!["begin", "via:board.kicad_pcb", "drop:c1"]);
    }

    #[test]
    fn routing_executor_reports_unknown_when_receipt_mismatch_cannot_be_dropped() {
        let operation = OperationKind::AddStraightTrack {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::StraightTrackIntent::new(
                StableId::new("track").unwrap(),
                board_coordinate(1, 2),
                board_coordinate(3, 4),
                100,
            )
            .unwrap(),
        };
        let mut ipc = FakeGraphicsIpc {
            routing_receipt_matches: false,
            drop: Err("drop failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Unknown { reason } if reason.contains("receipt mismatch") && reason.contains("drop failed"))
        );
        assert_eq!(ipc.calls, vec!["begin", "track:board.kicad_pcb", "drop:c1"]);
    }

    #[test]
    fn routing_executor_reports_unknown_when_save_fails_after_commit() {
        let operation = OperationKind::AddThroughVia {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::ThroughViaIntent::new(
                StableId::new("via").unwrap(),
                board_coordinate(5, 6),
                200,
                100,
            )
            .unwrap(),
        };
        let mut ipc = FakeGraphicsIpc {
            save: Err("save failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "save failed".into()
            }
        );
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "via:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb"
            ]
        );
    }

    #[test]
    fn routing_executor_rejects_mixed_and_oversized_batches_before_begin_commit() {
        let track = Operation {
            operation_id: StableId::new("op-track").unwrap(),
            kind: OperationKind::AddStraightTrack {
                board: StableId::new("board").unwrap(),
                intent: kicad_model::StraightTrackIntent::new(
                    StableId::new("track").unwrap(),
                    board_coordinate(1, 2),
                    board_coordinate(3, 4),
                    100,
                )
                .unwrap(),
            },
            targets: Vec::new(),
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "track".into(),
            source_run_id: "test".into(),
        };
        let graphic = Operation {
            operation_id: StableId::new("op-graphic").unwrap(),
            kind: graphics_operation(),
            targets: Vec::new(),
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "graphic".into(),
            source_run_id: "test".into(),
        };
        let mut ipc = FakeGraphicsIpc::default();
        let mixed = BoardGraphicsExecutor::default().execute_batch(
            &mut ipc,
            &[track.clone(), graphic],
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(mixed, BoardGraphicsCommitOutcome::Rejected { reason } if reason.contains("cannot share"))
        );
        assert!(ipc.calls.is_empty());

        let oversized = BoardGraphicsExecutor { max_segments: 1 }.execute_batch(
            &mut ipc,
            &[track.clone(), track],
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(oversized, BoardGraphicsCommitOutcome::Rejected { reason } if reason.contains("exceeded bounds"))
        );
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn routing_executor_rejects_invalid_intent_before_begin_commit() {
        let operation = OperationKind::AddStraightTrack {
            board: StableId::new("board").unwrap(),
            intent: kicad_model::StraightTrackIntent {
                layer: kicad_model::CopperLayer::FRONT,
                source_uuid: StableId::new("track").unwrap(),
                start: board_coordinate(1, 2),
                end: board_coordinate(1, 2),
                width_nm: 100,
                start_pad: None,
                end_pad: None,
                net: None,
            },
        };
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "route",
        );
        assert!(
            matches!(outcome, BoardGraphicsCommitOutcome::Rejected { reason } if reason.contains("invalid"))
        );
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn routing_readback_rejects_changed_networks_with_server_assigned_ids() {
        let receipt = CreatedTrackReceipt {
            request_index: 0,
            source_uuid: "server-track".into(),
            lock_state: IpcLockState::Unlocked,
            track: CreateTrack {
                layer: kicad_ipc::CopperLayer::FRONT,
                start_x_nm: 1,
                start_y_nm: 2,
                end_x_nm: 3,
                end_y_nm: 4,
                width_nm: 100,
                net: Some(CreateNetIdentity { name: "GND".into() }),
            },
        };
        let mut track = IpcTrack {
            source_uuid: "server-track".into(),
            geometry: IpcTrackGeometry::Segment {
                start: kicad_ipc::ObservedRoutingPoint { x_nm: 1, y_nm: 2 },
                end: kicad_ipc::ObservedRoutingPoint { x_nm: 3, y_nm: 4 },
            },
            width_nm: 100,
            layer: IpcBoardLayer::FrontCopper,
            lock_state: IpcLockState::Unlocked,
            net: Some(kicad_ipc::ObservedRoutingNet {
                code: Some(1),
                name: "GND".into(),
            }),
        };
        assert!(observed_track_matches(&track, &receipt, None));
        track.net.as_mut().unwrap().name = "VCC".into();
        assert!(!observed_track_matches(&track, &receipt, None));
        track.net = None;
        assert!(!observed_track_matches(&track, &receipt, None));
        let via_receipt = CreatedThroughViaReceipt {
            request_index: 0,
            source_uuid: "server-via".into(),
            lock_state: IpcLockState::Unlocked,
            via: CreateThroughVia {
                copper_layers: vec![kicad_ipc::CopperLayer::FRONT, kicad_ipc::CopperLayer::BACK],
                position_x_nm: 5,
                position_y_nm: 6,
                pad_diameter_nm: 200,
                drill_diameter_nm: 100,
                net: Some(CreateNetIdentity { name: "GND".into() }),
            },
        };
        let mut via = observed_via_for_test("server-via");
        via.net = Some(kicad_ipc::ObservedRoutingNet {
            code: Some(1),
            name: "GND".into(),
        });
        assert!(observed_via_matches(&via, &via_receipt, None));
        via.net.as_mut().unwrap().name = "VCC".into();
        assert!(!observed_via_matches(&via, &via_receipt, None));
    }

    #[test]
    fn bound_batch_preserves_graphics_only_execution_path() {
        let board_id = StableId::new("board").unwrap();
        let snapshot = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: Vec::new(),
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(board_id.clone()),
                ..Board::default()
            },
            native_footprints: None,
            native_routing: None,
            native_stackup: None,
        };
        let operation = Operation {
            operation_id: StableId::new("op-graphic").unwrap(),
            kind: OperationKind::AddBoardGraphicSegments {
                board: board_id,
                segments: vec![graphic()],
            },
            targets: vec![snapshot.board.document_id.clone().unwrap()],
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "graphic".into(),
            source_run_id: "test".into(),
        };
        let revision = snapshot.revision().unwrap();
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
            &mut ipc,
            &[operation],
            &snapshot,
            &revision,
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved);
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "create:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "read-by-id:board.kicad_pcb",
            ]
        );
    }

    #[test]
    fn bound_batch_rejects_partial_binding_before_begin_commit() {
        let board_id = StableId::new("board").unwrap();
        let snapshot = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: Vec::new(),
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(board_id.clone()),
                ..Board::default()
            },
            native_footprints: None,
            native_routing: None,
            native_stackup: None,
        };
        let operation = Operation {
            operation_id: StableId::new("op-track").unwrap(),
            kind: track_operation_for_test(),
            targets: vec![snapshot.board.document_id.clone().unwrap()],
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "track".into(),
            source_run_id: "test".into(),
        };
        let revision = snapshot.revision().unwrap();
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
            &mut ipc,
            &[operation],
            &snapshot,
            &revision,
            "board.kicad_pcb",
            "route",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected { reason }
                if reason.contains("binding is required") || reason.contains("was not observed")
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn bound_batch_accepts_observed_net_via_and_reads_back_by_id() {
        let board_id = StableId::new("board-a").unwrap();
        let footprints = board_footprint_observation(
            board_id.clone(),
            vec![observed_footprint("footprint-a", "pad-a")],
        )
        .unwrap();
        let net = ObservedNetIdentity {
            net_id: board_net_id(&board_id, "GND"),
            name: "GND".into(),
        };
        let snapshot = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: Vec::new(),
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(board_id.clone()),
                ..Board::default()
            },
            native_footprints: Some(footprints),
            native_routing: None,
            native_stackup: None,
        };
        let operation = Operation {
            operation_id: StableId::new("op-via").unwrap(),
            kind: OperationKind::AddThroughVia {
                board: board_id,
                intent: kicad_model::ThroughViaIntent::new_net_bound(
                    StableId::new("via").unwrap(),
                    board_coordinate(5, 6),
                    200,
                    100,
                    net,
                )
                .unwrap(),
            },
            targets: vec![snapshot.board.document_id.clone().unwrap()],
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "via".into(),
            source_run_id: "test".into(),
        };
        let mut observed_via = observed_via_for_test("via-uuid");
        observed_via.net = Some(kicad_ipc::ObservedRoutingNet {
            code: Some(1),
            name: "GND".into(),
        });
        let mut ipc = FakeGraphicsIpc {
            routing_observed: Some(Ok(IpcBoardRoutingItems {
                tracks: Vec::new(),
                vias: vec![observed_via],
            })),
            ..Default::default()
        };
        let revision = snapshot.revision().unwrap();
        let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
            &mut ipc,
            &[operation],
            &snapshot,
            &revision,
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(outcome, BoardGraphicsCommitOutcome::CommittedAndSaved);
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "via:board.kicad_pcb",
                "commit:c1",
                "save:board.kicad_pcb",
                "routing-read-by-id:board.kicad_pcb",
            ]
        );
    }

    #[test]
    fn bound_batch_rejects_stale_revision_before_begin_commit() {
        let snapshot = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: Vec::new(),
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(StableId::new("board").unwrap()),
                ..Board::default()
            },
            native_footprints: None,
            native_routing: None,
            native_stackup: None,
        };
        let stale_revision = RevisionDigest::new("f".repeat(64)).unwrap();
        assert_ne!(snapshot.revision().unwrap(), stale_revision);
        let operation = Operation {
            operation_id: StableId::new("op-via").unwrap(),
            kind: via_operation_for_test(),
            targets: vec![snapshot.board.document_id.clone().unwrap()],
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "via".into(),
            source_run_id: "test".into(),
        };
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
            &mut ipc,
            &[operation],
            &snapshot,
            &stale_revision,
            "board.kicad_pcb",
            "route",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected { reason }
                if reason.contains("revision is stale")
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn bound_batch_rejects_unobserved_net_before_begin_commit() {
        let board_id = StableId::new("board-a").unwrap();
        let snapshot = ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: Vec::new(),
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(board_id.clone()),
                ..Board::default()
            },
            native_footprints: Some(
                board_footprint_observation(
                    board_id.clone(),
                    vec![observed_footprint("footprint-a", "pad-a")],
                )
                .unwrap(),
            ),
            native_routing: None,
            native_stackup: None,
        };
        let operation = Operation {
            operation_id: StableId::new("op-via").unwrap(),
            kind: OperationKind::AddThroughVia {
                board: board_id.clone(),
                intent: kicad_model::ThroughViaIntent::new_net_bound(
                    StableId::new("via").unwrap(),
                    board_coordinate(5, 6),
                    200,
                    100,
                    ObservedNetIdentity {
                        net_id: board_net_id(&board_id, "VCC"),
                        name: "VCC".into(),
                    },
                )
                .unwrap(),
            },
            targets: vec![snapshot.board.document_id.clone().unwrap()],
            preconditions: Vec::new(),
            expected_effects: Vec::new(),
            risk_class: kicad_engine::RiskClass::Low,
            idempotency_key: "via".into(),
            source_run_id: "test".into(),
        };
        let revision = snapshot.revision().unwrap();
        let mut ipc = FakeGraphicsIpc::default();
        let outcome = BoardGraphicsExecutor::default().execute_bound_batch(
            &mut ipc,
            &[operation],
            &snapshot,
            &revision,
            "board.kicad_pcb",
            "route",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected { reason }
                if reason.contains("binding is required") || reason.contains("was not observed")
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn routing_batch_reports_rejection_after_partial_create() {
        let operations = [
            Operation {
                operation_id: StableId::new("op-via-1").unwrap(),
                kind: via_operation_for_test(),
                targets: Vec::new(),
                preconditions: Vec::new(),
                expected_effects: Vec::new(),
                risk_class: kicad_engine::RiskClass::Low,
                idempotency_key: "via-1".into(),
                source_run_id: "test".into(),
            },
            Operation {
                operation_id: StableId::new("op-via-2").unwrap(),
                kind: via_operation_for_test(),
                targets: Vec::new(),
                preconditions: Vec::new(),
                expected_effects: Vec::new(),
                risk_class: kicad_engine::RiskClass::Low,
                idempotency_key: "via-2".into(),
                source_run_id: "test".into(),
            },
        ];
        let mut ipc = FakeGraphicsIpc {
            fail_via_on_call: Some(2),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute_batch(
            &mut ipc,
            &operations,
            "board.kicad_pcb",
            "route",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected {
                reason: "injected second via failure".into(),
            }
        );
        assert_eq!(
            ipc.calls,
            vec![
                "begin",
                "via:board.kicad_pcb",
                "via:board.kicad_pcb",
                "drop:c1",
            ]
        );
    }
}
