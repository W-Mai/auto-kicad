use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct StableId(String);

impl StableId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ModelError::EmptyStableId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for StableId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl Display for StableId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Unit {
    Nanometres,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DocumentSpace {
    Schematic,
    Board,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Coordinate {
    pub x: i64,
    pub y: i64,
    pub unit: Unit,
    pub space: DocumentSpace,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReferenceResolution {
    Resolved(StableId),
    Unresolved {
        query: String,
    },
    Ambiguous {
        query: String,
        candidates: Vec<StableId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    pub source_uuid: StableId,
    pub observed_revision: RevisionDigest,
    pub number: String,
    pub name: Option<String>,
    pub net: ReferenceResolution,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Net {
    pub source_uuid: StableId,
    pub observed_revision: RevisionDigest,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub source_uuid: StableId,
    pub observed_revision: RevisionDigest,
    pub sheet_path: String,
    pub reference: Option<String>,
    pub value: Option<String>,
    pub position: Coordinate,
    pub pins: Vec<Pin>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub source_uuid: StableId,
    pub observed_revision: RevisionDigest,
    pub reference: Option<String>,
    pub value: Option<String>,
    pub position: Coordinate,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Schematic {
    pub nets: Vec<Net>,
    pub symbols: Vec<Symbol>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchematicArtifactFile {
    pub relative_path: String,
    pub size: u64,
    pub content_digest: RevisionDigest,
}

impl SchematicArtifactFile {
    pub fn new(
        relative_path: impl Into<String>,
        size: u64,
        content_digest: RevisionDigest,
    ) -> Result<Self, ModelError> {
        let file = Self {
            relative_path: relative_path.into(),
            size,
            content_digest,
        };
        file.validate()?;
        Ok(file)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let segments = self.relative_path.split('/').collect::<Vec<_>>();
        if self.relative_path.is_empty()
            || self.relative_path.contains('\\')
            || self.relative_path.starts_with('/')
            || self.relative_path.contains(':')
            || segments
                .iter()
                .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
        {
            return Err(ModelError::InvalidSchematicArtifactPath);
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for SchematicArtifactFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawFile {
            relative_path: String,
            size: u64,
            content_digest: RevisionDigest,
        }

        let raw = RawFile::deserialize(deserializer)?;
        Self::new(raw.relative_path, raw.size, raw.content_digest).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SchematicArtifactRevision {
    pub digest: RevisionDigest,
    pub files: Vec<SchematicArtifactFile>,
    pub kicad_cli_version: String,
    pub extractor_schema_version: u32,
    pub editor_lock_present: bool,
}

impl SchematicArtifactRevision {
    pub fn new(
        files: Vec<SchematicArtifactFile>,
        kicad_cli_version: impl Into<String>,
        extractor_schema_version: u32,
        editor_lock_present: bool,
    ) -> Result<Self, ModelError> {
        let mut revision = Self {
            digest: RevisionDigest::new("0".repeat(64))?,
            files,
            kicad_cli_version: kicad_cli_version.into(),
            extractor_schema_version,
            editor_lock_present,
        };
        revision.canonicalize_files()?;
        revision.digest = revision.calculate_digest()?;
        Ok(revision)
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        let mut canonical = self.clone();
        canonical.canonicalize_files()?;
        if canonical.files != self.files {
            return Err(ModelError::NonCanonicalSchematicArtifacts);
        }
        if self.digest != self.calculate_digest()? {
            return Err(ModelError::SchematicArtifactDigestMismatch);
        }
        Ok(())
    }

    fn canonicalize_files(&mut self) -> Result<(), ModelError> {
        for file in &self.files {
            file.validate()?;
        }
        self.files
            .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        let unique_paths = self
            .files
            .iter()
            .map(|file| file.relative_path.as_str())
            .collect::<BTreeSet<_>>();
        if unique_paths.len() != self.files.len() {
            return Err(ModelError::DuplicateSchematicArtifactPath);
        }
        if self.kicad_cli_version.is_empty() || self.extractor_schema_version == 0 {
            return Err(ModelError::InvalidSchematicArtifactRevision);
        }
        Ok(())
    }

    fn calculate_digest(&self) -> Result<RevisionDigest, ModelError> {
        #[derive(Serialize)]
        struct DigestInput<'a> {
            files: &'a [SchematicArtifactFile],
            kicad_cli_version: &'a str,
            extractor_schema_version: u32,
        }

        let bytes = serde_json::to_vec(&DigestInput {
            files: &self.files,
            kicad_cli_version: &self.kicad_cli_version,
            extractor_schema_version: self.extractor_schema_version,
        })
        .map_err(ModelError::Serialization)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        RevisionDigest::new(hex::encode(hasher.finalize()))
    }
}

impl<'de> Deserialize<'de> for SchematicArtifactRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawRevision {
            digest: RevisionDigest,
            files: Vec<SchematicArtifactFile>,
            kicad_cli_version: String,
            extractor_schema_version: u32,
            editor_lock_present: bool,
        }

        let raw = RawRevision::deserialize(deserializer)?;
        let revision = Self::new(
            raw.files,
            raw.kicad_cli_version,
            raw.extractor_schema_version,
            raw.editor_lock_present,
        )
        .map_err(serde::de::Error::custom)?;
        if revision.digest != raw.digest {
            return Err(serde::de::Error::custom(
                ModelError::SchematicArtifactDigestMismatch,
            ));
        }
        Ok(revision)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedSchematicPin {
    pub pin_id: StableId,
    pub source_uuid: StableId,
    pub number: String,
    pub name: Option<String>,
    pub electrical_type: Option<String>,
    pub net: ReferenceResolution,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedSchematicSymbol {
    pub symbol_id: StableId,
    pub source_uuid: StableId,
    pub sheet_instance_path: String,
    pub reference: String,
    pub value: String,
    pub lib_identifier: String,
    pub footprint_field: Option<String>,
    pub unit: u32,
    pub pins: Vec<ObservedSchematicPin>,
    pub observed_revision: RevisionDigest,
}

impl ObservedSchematicSymbol {
    pub fn canonical(&self) -> Self {
        let mut symbol = self.clone();
        symbol.pins.sort_by(|left, right| {
            (&left.number, &left.source_uuid).cmp(&(&right.number, &right.source_uuid))
        });
        for pin in &mut symbol.pins {
            if let ReferenceResolution::Ambiguous { candidates, .. } = &mut pin.net {
                candidates.sort();
            }
        }
        symbol
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BoardSide {
    Front,
    Back,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum LockState {
    Unknown,
    Unlocked,
    Locked,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct FootprintLibraryIdentifier {
    pub nickname: String,
    pub entry_name: String,
}

impl FootprintLibraryIdentifier {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.entry_name.is_empty() {
            return Err(ModelError::InvalidFootprintObservation(
                "library entry name must not be empty",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BoardLayer {
    FrontCopper,
    InnerCopper(u8),
    BackCopper,
    FrontMask,
    BackMask,
    FrontPaste,
    BackPaste,
}

impl BoardLayer {
    pub fn validate(&self) -> Result<(), ModelError> {
        if matches!(self, Self::InnerCopper(index) if !(1..=30).contains(index)) {
            return Err(ModelError::InvalidFootprintObservation(
                "inner copper layer index must be between 1 and 30",
            ));
        }
        Ok(())
    }

    fn is_copper(self) -> bool {
        matches!(
            self,
            Self::FrontCopper | Self::InnerCopper(_) | Self::BackCopper
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalCoordinateNm {
    pub x_nm: i64,
    pub y_nm: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SizeNm {
    pub x_nm: i64,
    pub y_nm: i64,
}

impl SizeNm {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.x_nm <= 0 || self.y_nm <= 0 {
            return Err(ModelError::InvalidFootprintObservation(
                "size dimensions must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PadKind {
    Unknown,
    PlatedThroughHole,
    SurfaceMount,
    EdgeConnector,
    NonPlatedThroughHole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PadShape {
    Unknown,
    Circle,
    Rectangle,
    Oval,
    Trapezoid,
    RoundedRectangle,
    ChamferedRectangle,
    Custom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FiniteRatio(u64);

impl FiniteRatio {
    pub fn new(value: f64) -> Result<Self, ModelError> {
        if !value.is_finite() {
            return Err(ModelError::InvalidFootprintObservation(
                "pad geometry ratio must be finite",
            ));
        }
        let value = if value == 0.0 { 0.0 } else { value };
        Ok(Self(value.to_bits()))
    }

    pub fn as_f64(self) -> f64 {
        f64::from_bits(self.0)
    }
}

impl Serialize for FiniteRatio {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for FiniteRatio {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PadCopperShape {
    pub layer: BoardLayer,
    pub shape: PadShape,
    pub size: SizeNm,
    pub offset: LocalCoordinateNm,
    pub corner_rounding_ratio: FiniteRatio,
    pub chamfer_ratio: FiniteRatio,
    pub chamfer_top_left: bool,
    pub chamfer_top_right: bool,
    pub chamfer_bottom_left: bool,
    pub chamfer_bottom_right: bool,
    pub trapezoid_delta: LocalCoordinateNm,
    pub custom_anchor_shape: Option<PadShape>,
}

impl PadCopperShape {
    pub fn validate(&self) -> Result<(), ModelError> {
        self.layer.validate()?;
        if !self.layer.is_copper() {
            return Err(ModelError::InvalidFootprintObservation(
                "pad copper shape must use a copper layer",
            ));
        }
        self.size.validate()?;
        if !matches!(
            self.custom_anchor_shape,
            None | Some(PadShape::Circle | PadShape::Rectangle)
        ) {
            return Err(ModelError::InvalidFootprintObservation(
                "custom pad anchor must be a circle or rectangle",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DrillShape {
    Unknown,
    Circle,
    Oblong,
    Undefined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Drill {
    pub shape: DrillShape,
    pub size: SizeNm,
}

impl Drill {
    pub fn validate(&self) -> Result<(), ModelError> {
        self.size.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedNetIdentity {
    pub net_id: StableId,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PadStackType {
    Unknown,
    Normal,
    FrontInnerBack,
    Custom,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedPad {
    pub pad_id: StableId,
    pub source_uuid: StableId,
    pub footprint_placement_id: StableId,
    pub number: String,
    pub kind: PadKind,
    pub lock_state: LockState,
    pub position: LocalCoordinateNm,
    pub stack_type: PadStackType,
    pub orientation_mdeg: i64,
    pub layers: Vec<BoardLayer>,
    pub copper_shapes: Vec<PadCopperShape>,
    pub drill: Option<Drill>,
    pub net: Option<ObservedNetIdentity>,
    pub observed_revision: RevisionDigest,
}

impl ObservedPad {
    pub fn canonical(&self) -> Self {
        let mut pad = self.clone();
        pad.layers.sort();
        pad.copper_shapes.sort_by_key(|shape| shape.layer);
        pad
    }

    pub fn validate(
        &self,
        board_id: &StableId,
        footprint_placement_id: &StableId,
        observed_revision: &RevisionDigest,
    ) -> Result<(), ModelError> {
        if &self.footprint_placement_id != footprint_placement_id {
            return Err(ModelError::InvalidFootprintObservation(
                "pad parent footprint does not match placement",
            ));
        }
        if &self.observed_revision != observed_revision {
            return Err(ModelError::InvalidFootprintObservation(
                "pad revision does not match placement revision",
            ));
        }
        if self.pad_id != footprint_pad_id(footprint_placement_id, &self.source_uuid) {
            return Err(ModelError::InvalidFootprintObservation(
                "pad id does not match its source identity",
            ));
        }
        for layer in &self.layers {
            layer.validate()?;
        }
        if self.layers.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(ModelError::InvalidFootprintObservation(
                "pad layers must be unique and canonically ordered",
            ));
        }
        for shape in &self.copper_shapes {
            shape.validate()?;
        }
        if self
            .copper_shapes
            .windows(2)
            .any(|pair| pair[0].layer >= pair[1].layer)
        {
            return Err(ModelError::InvalidFootprintObservation(
                "pad copper shapes must have unique canonical layers",
            ));
        }
        if let Some(drill) = self.drill {
            drill.validate()?;
        }
        if let Some(net) = &self.net
            && (net.name.is_empty() || net.net_id != board_net_id(board_id, &net.name))
        {
            return Err(ModelError::InvalidFootprintObservation(
                "pad net identity does not match its board and name",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedFootprintPlacement {
    pub placement_id: StableId,
    pub source_uuid: StableId,
    pub definition_id: StableId,
    pub library_identifier: FootprintLibraryIdentifier,
    pub reference: String,
    pub value: String,
    pub position: Coordinate,
    pub orientation_mdeg: i64,
    pub side: BoardSide,
    pub lock_state: LockState,
    pub symbol_sheet_path: Vec<StableId>,
    pub pads: Vec<ObservedPad>,
    pub observed_revision: RevisionDigest,
}

impl ObservedFootprintPlacement {
    pub fn canonical(&self) -> Self {
        let mut placement = self.clone();
        placement.pads = placement.pads.iter().map(ObservedPad::canonical).collect();
        placement
            .pads
            .sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        placement
    }

    pub fn validate(&self, board_id: &StableId) -> Result<(), ModelError> {
        self.library_identifier.validate()?;
        if self.position.unit != Unit::Nanometres || self.position.space != DocumentSpace::Board {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint position must use nanometres in board space",
            ));
        }
        if self.placement_id != footprint_placement_id(board_id, &self.source_uuid) {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint placement id does not match its source identity",
            ));
        }
        if self.definition_id != footprint_definition_id(&self.library_identifier) {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint definition id does not match its library identity",
            ));
        }
        if self
            .pads
            .windows(2)
            .any(|pair| pair[0].source_uuid >= pair[1].source_uuid)
        {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint pads must have unique canonical source identities",
            ));
        }
        for pad in &self.pads {
            pad.validate(board_id, &self.placement_id, &self.observed_revision)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedBoardFootprints {
    pub board_id: StableId,
    pub placements: Vec<ObservedFootprintPlacement>,
    pub observed_revision: RevisionDigest,
}

impl ObservedBoardFootprints {
    pub fn new(
        board_id: StableId,
        placements: Vec<ObservedFootprintPlacement>,
    ) -> Result<Self, ModelError> {
        let mut observation = Self {
            board_id,
            placements,
            observed_revision: zero_revision_digest(),
        }
        .canonical();
        let revision = observation.content_revision()?;
        observation.bind_revision(&revision);
        observation.validate()?;
        Ok(observation)
    }

    pub fn canonical(&self) -> Self {
        let mut observation = self.clone();
        observation.placements = observation
            .placements
            .iter()
            .map(ObservedFootprintPlacement::canonical)
            .collect();
        observation
            .placements
            .sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        observation
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self
            .placements
            .windows(2)
            .any(|pair| pair[0].source_uuid >= pair[1].source_uuid)
        {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint placements must have unique canonical source identities",
            ));
        }
        if self.observed_revision != self.content_revision()? {
            return Err(ModelError::InvalidFootprintObservation(
                "footprint observation revision does not match its content",
            ));
        }
        for placement in &self.placements {
            if placement.observed_revision != self.observed_revision {
                return Err(ModelError::InvalidFootprintObservation(
                    "footprint placement revision does not match its observation",
                ));
            }
            placement.validate(&self.board_id)?;
        }
        Ok(())
    }

    fn content_revision(&self) -> Result<RevisionDigest, ModelError> {
        let mut payload = self.canonical();
        payload.bind_revision(&zero_revision_digest());
        let bytes = serde_json::to_vec(&(
            "observed-board-footprints-v1",
            &payload.board_id,
            &payload.placements,
        ))
        .map_err(ModelError::Serialization)?;
        Ok(RevisionDigest(hex::encode(Sha256::digest(bytes))))
    }

    fn bind_revision(&mut self, revision: &RevisionDigest) {
        self.observed_revision = revision.clone();
        for placement in &mut self.placements {
            placement.observed_revision = revision.clone();
            for pad in &mut placement.pads {
                pad.observed_revision = revision.clone();
            }
        }
    }
}

fn zero_revision_digest() -> RevisionDigest {
    RevisionDigest("0".repeat(64))
}

pub fn footprint_placement_id(board_id: &StableId, source_uuid: &StableId) -> StableId {
    derived_stable_id(
        "footprint-placement-v1",
        &[board_id.as_str(), source_uuid.as_str()],
    )
}

pub fn footprint_definition_id(library_identifier: &FootprintLibraryIdentifier) -> StableId {
    derived_stable_id(
        "footprint-definition-v1",
        &[&library_identifier.nickname, &library_identifier.entry_name],
    )
}

pub fn footprint_pad_id(placement_id: &StableId, source_uuid: &StableId) -> StableId {
    derived_stable_id(
        "footprint-pad-v1",
        &[placement_id.as_str(), source_uuid.as_str()],
    )
}

pub fn board_net_id(board_id: &StableId, name: &str) -> StableId {
    derived_stable_id("board-net-v1", &[board_id.as_str(), name])
}

pub fn schematic_symbol_id(
    project_id: &StableId,
    sheet_instance_path: &str,
    source_uuid: &StableId,
) -> StableId {
    derived_stable_id(
        "schematic-symbol-v1",
        &[
            project_id.as_str(),
            sheet_instance_path,
            source_uuid.as_str(),
        ],
    )
}

pub fn schematic_pin_id(symbol_id: &StableId, source_uuid: &StableId) -> StableId {
    derived_stable_id(
        "schematic-pin-v1",
        &[symbol_id.as_str(), source_uuid.as_str()],
    )
}

fn derived_stable_id(domain: &str, parts: &[&str]) -> StableId {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    StableId(hex::encode(hasher.finalize()))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum BoardGraphicLayer {
    DwgsUser,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BoardGraphicSegment {
    pub graphic_id: StableId,
    pub start: Coordinate,
    pub end: Coordinate,
    pub width_nm: i64,
    pub layer: BoardGraphicLayer,
}

impl BoardGraphicSegment {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.start.unit != Unit::Nanometres || self.end.unit != Unit::Nanometres {
            return Err(ModelError::InvalidBoardGraphic(
                "coordinates must use nanometres",
            ));
        }
        if self.start.space != DocumentSpace::Board || self.end.space != DocumentSpace::Board {
            return Err(ModelError::InvalidBoardGraphic(
                "coordinates must use board space",
            ));
        }
        if self.width_nm <= 0 {
            return Err(ModelError::InvalidBoardGraphic("width must be positive"));
        }
        if self.start.x == self.end.x && self.start.y == self.end.y {
            return Err(ModelError::InvalidBoardGraphic(
                "segment endpoints must differ",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub document_id: Option<StableId>,
    pub footprints: Vec<Footprint>,
    #[serde(default)]
    pub graphics: Vec<BoardGraphicSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CapabilityStatus {
    Verified,
    Unsupported,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityObservation {
    pub id: String,
    pub status: CapabilityStatus,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DiscoveredDocumentKind {
    Schematic {
        filename: Option<String>,
        sheet_path: Option<String>,
        sheet_kiids: Vec<String>,
    },
    Pcb {
        filename: String,
    },
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredDocument {
    pub document_id: StableId,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub kind: DiscoveredDocumentKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoverySnapshot {
    pub kicad_version: String,
    pub endpoint: String,
    pub documents: Vec<DiscoveredDocument>,
    pub capabilities: Vec<CapabilityObservation>,
    pub diagnostics: Vec<Diagnostic>,
}

impl DiscoverySnapshot {
    pub fn canonical(&self) -> Self {
        let mut snapshot = self.clone();
        snapshot
            .documents
            .sort_by(|left, right| left.document_id.cmp(&right.document_id));
        snapshot
            .capabilities
            .sort_by(|left, right| left.id.cmp(&right.id));
        snapshot
            .diagnostics
            .sort_by(|left, right| (&left.code, &left.message).cmp(&(&right.code, &right.message)));
        snapshot
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ModelError> {
        serde_json::to_vec(&self.canonical()).map_err(ModelError::Serialization)
    }

    pub fn revision(&self) -> Result<RevisionDigest, ModelError> {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_bytes()?);
        Ok(RevisionDigest(hex::encode(hasher.finalize())))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectSnapshot {
    pub project_id: StableId,
    pub kicad_version: String,
    pub capabilities: Vec<String>,
    pub schematic: Schematic,
    pub board: Board,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_footprints: Option<ObservedBoardFootprints>,
}

impl ProjectSnapshot {
    pub fn canonical(&self) -> Self {
        let mut snapshot = self.clone();
        snapshot.capabilities.sort();
        snapshot
            .schematic
            .nets
            .sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        snapshot.schematic.symbols.sort_by(|left, right| {
            (&left.sheet_path, &left.source_uuid).cmp(&(&right.sheet_path, &right.source_uuid))
        });
        for symbol in &mut snapshot.schematic.symbols {
            symbol.pins.sort_by(|left, right| {
                (&left.number, &left.source_uuid).cmp(&(&right.number, &right.source_uuid))
            });
            for pin in &mut symbol.pins {
                if let ReferenceResolution::Ambiguous { candidates, .. } = &mut pin.net {
                    candidates.sort();
                }
            }
        }
        snapshot
            .board
            .footprints
            .sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
        snapshot.board.graphics.sort_by_key(|graphic| {
            (
                graphic.start.x,
                graphic.start.y,
                graphic.end.x,
                graphic.end.y,
                graphic.width_nm,
                graphic.layer,
                graphic.graphic_id.clone(),
            )
        });
        snapshot.native_footprints = snapshot
            .native_footprints
            .as_ref()
            .map(ObservedBoardFootprints::canonical);
        snapshot
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if let Some(native_footprints) = &self.native_footprints {
            native_footprints.canonical().validate()?;
            if self.board.document_id.as_ref() != Some(&native_footprints.board_id) {
                return Err(ModelError::InvalidFootprintObservation(
                    "native footprint board identity does not match the project board",
                ));
            }
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ModelError> {
        serde_json::to_vec(&self.canonical()).map_err(ModelError::Serialization)
    }

    pub fn revision(&self) -> Result<RevisionDigest, ModelError> {
        self.validate()?;
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_bytes()?);
        Ok(RevisionDigest(hex::encode(hasher.finalize())))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RevisionDigest(String);

impl RevisionDigest {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ModelError::InvalidRevisionDigest);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RevisionDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl Display for RevisionDigest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "sha256:{}", self.0)
    }
}

#[derive(Debug)]
pub enum ModelError {
    EmptyStableId,
    InvalidRevisionDigest,
    InvalidSchematicArtifactPath,
    DuplicateSchematicArtifactPath,
    InvalidSchematicArtifactRevision,
    NonCanonicalSchematicArtifacts,
    SchematicArtifactDigestMismatch,
    InvalidBoardGraphic(&'static str),
    InvalidFootprintObservation(&'static str),
    Serialization(serde_json::Error),
}

impl Display for ModelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyStableId => formatter.write_str("stable id must not be empty"),
            Self::InvalidRevisionDigest => {
                formatter.write_str("revision digest must be 64 hexadecimal characters")
            }
            Self::InvalidSchematicArtifactPath => formatter
                .write_str("schematic artifact path must be a normalized, project-relative path"),
            Self::DuplicateSchematicArtifactPath => {
                formatter.write_str("schematic artifact paths must be unique")
            }
            Self::InvalidSchematicArtifactRevision => formatter.write_str(
                "schematic artifact revision requires a CLI version and nonzero schema version",
            ),
            Self::NonCanonicalSchematicArtifacts => {
                formatter.write_str("schematic artifact files must be canonically ordered")
            }
            Self::SchematicArtifactDigestMismatch => {
                formatter.write_str("schematic artifact revision digest does not match its inputs")
            }
            Self::InvalidBoardGraphic(message) | Self::InvalidFootprintObservation(message) => {
                formatter.write_str(message)
            }
            Self::Serialization(error) => {
                write!(formatter, "snapshot serialization failed: {error}")
            }
        }
    }
}

impl std::error::Error for ModelError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> StableId {
        StableId::new(value).unwrap()
    }

    fn revision(value: char) -> RevisionDigest {
        RevisionDigest::new(value.to_string().repeat(64)).unwrap()
    }

    fn snapshot(symbols: Vec<Symbol>) -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: id("project"),
            kicad_version: "10.0.5".to_owned(),
            capabilities: vec!["board.read".to_owned(), "schematic.read".to_owned()],
            schematic: Schematic {
                nets: vec![],
                symbols,
            },
            board: Board::default(),
            native_footprints: None,
        }
    }

    fn symbol(uuid: &str, reference: &str) -> Symbol {
        Symbol {
            source_uuid: id(uuid),
            observed_revision: revision('a'),
            sheet_path: "/".to_owned(),
            reference: Some(reference.to_owned()),
            value: Some("10k".to_owned()),
            position: Coordinate {
                x: 1,
                y: 2,
                unit: Unit::Nanometres,
                space: DocumentSpace::Schematic,
            },
            pins: vec![],
        }
    }

    #[test]
    fn canonical_revision_is_independent_of_insertion_order() {
        let first = snapshot(vec![symbol("b", "R2"), symbol("a", "R1")]);
        let second = snapshot(vec![symbol("a", "R1"), symbol("b", "R2")]);
        assert_eq!(first.revision().unwrap(), second.revision().unwrap());
    }

    #[test]
    fn stable_id_deserialization_preserves_nonempty_invariant() {
        assert!(serde_json::from_str::<StableId>("\"\"").is_err());
        assert_eq!(
            serde_json::from_str::<StableId>("\"source\"").unwrap(),
            id("source")
        );
    }

    #[test]
    fn stable_id_is_separate_from_human_label() {
        let first = snapshot(vec![symbol("a", "R1")]);
        let second = snapshot(vec![symbol("a", "R2")]);
        assert_ne!(first.revision().unwrap(), second.revision().unwrap());
        assert_eq!(
            first.schematic.symbols[0].source_uuid,
            second.schematic.symbols[0].source_uuid
        );
    }

    #[test]
    fn coordinates_carry_unit_and_document_space() {
        let coordinate = symbol("a", "R1").position;
        assert_eq!(coordinate.unit, Unit::Nanometres);
        assert_eq!(coordinate.space, DocumentSpace::Schematic);
    }

    #[test]
    fn ambiguous_references_are_preserved_and_canonicalized() {
        let mut pin = Pin {
            source_uuid: id("p"),
            observed_revision: revision('a'),
            number: "1".to_owned(),
            name: None,
            net: ReferenceResolution::Ambiguous {
                query: "GND".to_owned(),
                candidates: vec![id("net-b"), id("net-a")],
            },
        };
        let symbol = Symbol {
            pins: vec![pin.clone()],
            ..symbol("a", "R1")
        };
        let canonical = snapshot(vec![symbol]).canonical();
        pin.net = ReferenceResolution::Ambiguous {
            query: "GND".to_owned(),
            candidates: vec![id("net-a"), id("net-b")],
        };
        assert_eq!(canonical.schematic.symbols[0].pins[0].net, pin.net);
    }

    #[test]
    fn canonical_revision_sorts_nets_by_stable_id() {
        let mut first = snapshot(vec![]);
        first.schematic.nets = vec![
            Net {
                source_uuid: id("net-b"),
                observed_revision: revision('a'),
                name: Some("GND".to_owned()),
            },
            Net {
                source_uuid: id("net-a"),
                observed_revision: revision('a'),
                name: Some("VCC".to_owned()),
            },
        ];
        let mut second = first.clone();
        second.schematic.nets.reverse();
        assert_eq!(first.revision().unwrap(), second.revision().unwrap());
    }

    #[test]
    fn revision_digest_rejects_invalid_values() {
        assert!(RevisionDigest::new("short").is_err());
        assert!(RevisionDigest::new("z".repeat(64)).is_err());
        assert_eq!(
            RevisionDigest::new("A".repeat(64)).unwrap().as_str(),
            "a".repeat(64)
        );
    }

    fn graphic(graphic_id: &str) -> BoardGraphicSegment {
        BoardGraphicSegment {
            graphic_id: id(graphic_id),
            start: Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            end: Coordinate {
                x: 1,
                y: 1,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            width_nm: 1,
            layer: BoardGraphicLayer::DwgsUser,
        }
    }

    #[test]
    fn board_graphic_identity_changes_revision_and_order_does_not() {
        let mut first = snapshot(vec![]);
        first.board.document_id = Some(id("board"));
        first.board.graphics = vec![graphic("b"), graphic("a")];
        let mut reordered = first.clone();
        reordered.board.graphics.reverse();
        assert_eq!(first.revision().unwrap(), reordered.revision().unwrap());
        reordered.board.graphics[0].graphic_id = id("changed");
        assert_ne!(first.revision().unwrap(), reordered.revision().unwrap());
    }

    #[test]
    fn old_board_snapshot_deserializes_without_new_fields() {
        let board: Board = serde_json::from_value(serde_json::json!({
            "footprints": []
        }))
        .unwrap();
        assert_eq!(board.document_id, None);
        assert!(board.graphics.is_empty());
    }

    #[test]
    fn board_graphic_validation_rejects_invalid_geometry() {
        assert!(graphic("valid").validate().is_ok());
        let mut invalid = graphic("invalid");
        invalid.end = invalid.start;
        assert!(matches!(
            invalid.validate(),
            Err(ModelError::InvalidBoardGraphic(
                "segment endpoints must differ"
            ))
        ));
    }

    fn artifact(path: &str, digest: char) -> SchematicArtifactFile {
        SchematicArtifactFile::new(path, 42, revision(digest)).unwrap()
    }

    fn observed_symbol(sheet_path: &str, footprint_field: Option<&str>) -> ObservedSchematicSymbol {
        let project_id = id("project");
        let source_uuid = id("symbol-source");
        let symbol_id = schematic_symbol_id(&project_id, sheet_path, &source_uuid);
        let pin_source_uuid = id("pin-source");
        ObservedSchematicSymbol {
            symbol_id: symbol_id.clone(),
            source_uuid,
            sheet_instance_path: sheet_path.to_owned(),
            reference: "R1".to_owned(),
            value: "10k".to_owned(),
            lib_identifier: "Device:R".to_owned(),
            footprint_field: footprint_field.map(str::to_owned),
            unit: 1,
            pins: vec![ObservedSchematicPin {
                pin_id: schematic_pin_id(&symbol_id, &pin_source_uuid),
                source_uuid: pin_source_uuid,
                number: "1".to_owned(),
                name: Some("~".to_owned()),
                electrical_type: Some("passive".to_owned()),
                net: ReferenceResolution::Unresolved {
                    query: "unconnected".to_owned(),
                },
            }],
            observed_revision: revision('a'),
        }
    }

    #[test]
    fn old_project_snapshot_deserializes_without_artifact_observation_types() {
        let old = serde_json::json!({
            "project_id": "project",
            "kicad_version": "10.0.5",
            "capabilities": [],
            "schematic": {"nets": [], "symbols": []},
            "board": {"footprints": []}
        });
        let snapshot: ProjectSnapshot = serde_json::from_value(old).unwrap();
        assert!(snapshot.schematic.symbols.is_empty());
        assert_eq!(snapshot.native_footprints, None);
        assert!(
            serde_json::to_value(&snapshot)
                .unwrap()
                .get("native_footprints")
                .is_none()
        );
        assert_eq!(
            snapshot.canonical_bytes().unwrap(),
            br#"{"project_id":"project","kicad_version":"10.0.5","capabilities":[],"schematic":{"nets":[],"symbols":[]},"board":{"document_id":null,"footprints":[],"graphics":[]}}"#
        );
    }

    #[test]
    fn artifact_revision_is_order_independent_and_lock_is_gating_metadata() {
        let first = SchematicArtifactRevision::new(
            vec![
                artifact("power.kicad_sch", 'b'),
                artifact("xLock.kicad_sch", 'a'),
            ],
            "10.0.5",
            1,
            false,
        )
        .unwrap();
        let locked = SchematicArtifactRevision::new(
            vec![
                artifact("xLock.kicad_sch", 'a'),
                artifact("power.kicad_sch", 'b'),
            ],
            "10.0.5",
            1,
            true,
        )
        .unwrap();
        assert_eq!(first.files, locked.files);
        assert_eq!(first.digest, locked.digest);
        assert_ne!(first.editor_lock_present, locked.editor_lock_present);
        assert!(first.validate().is_ok());
        assert!(locked.validate().is_ok());
    }

    #[test]
    fn symbol_and_pin_canonicalization_is_order_independent() {
        let mut first = observed_symbol("/root/sheet", None);
        let mut second_pin = first.pins[0].clone();
        second_pin.source_uuid = id("pin-source-2");
        second_pin.pin_id = schematic_pin_id(&first.symbol_id, &second_pin.source_uuid);
        second_pin.number = "2".to_owned();
        first.pins.insert(0, second_pin);
        let mut second = first.clone();
        second.pins.reverse();
        assert_eq!(first.canonical(), second.canonical());
    }

    #[test]
    fn symbol_identity_is_separated_by_sheet_instance_path() {
        let first = observed_symbol("/root/sheet-a", None);
        let second = observed_symbol("/root/sheet-b", None);
        assert_eq!(first.source_uuid, second.source_uuid);
        assert_ne!(first.symbol_id, second.symbol_id);
        assert_ne!(first.pins[0].pin_id, second.pins[0].pin_id);
    }

    #[test]
    fn footprint_field_preserves_absent_and_explicit_empty_states() {
        let absent = observed_symbol("/root", None);
        let empty = observed_symbol("/root", Some(""));
        assert_eq!(absent.footprint_field, None);
        assert_eq!(empty.footprint_field.as_deref(), Some(""));
        assert_ne!(
            serde_json::to_value(absent).unwrap(),
            serde_json::to_value(empty).unwrap()
        );
    }

    #[test]
    fn artifact_paths_and_serialized_digests_are_validated() {
        for path in [
            "",
            "../x.kicad_sch",
            "./x.kicad_sch",
            "/x.kicad_sch",
            "a\\b.kicad_sch",
        ] {
            assert!(SchematicArtifactFile::new(path, 1, revision('a')).is_err());
        }
        assert!(SchematicArtifactFile::new("sheets/power.kicad_sch", 1, revision('a')).is_ok());

        let valid = SchematicArtifactRevision::new(
            vec![artifact("xLock.kicad_sch", 'a')],
            "10.0.5",
            1,
            false,
        )
        .unwrap();
        let mut forged = serde_json::to_value(valid).unwrap();
        forged["digest"] = serde_json::Value::String("f".repeat(64));
        assert!(serde_json::from_value::<SchematicArtifactRevision>(forged).is_err());
    }

    fn copper_shape(layer: BoardLayer, shape: PadShape) -> PadCopperShape {
        PadCopperShape {
            layer,
            shape,
            size: SizeNm {
                x_nm: 1_000_000,
                y_nm: 500_000,
            },
            offset: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
            corner_rounding_ratio: FiniteRatio::new(0.0).unwrap(),
            chamfer_ratio: FiniteRatio::new(0.0).unwrap(),
            chamfer_top_left: false,
            chamfer_top_right: false,
            chamfer_bottom_left: false,
            chamfer_bottom_right: false,
            trapezoid_delta: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
            custom_anchor_shape: None,
        }
    }

    fn observed_footprint() -> (StableId, ObservedFootprintPlacement) {
        let board_id = id("board");
        let source_uuid = id("footprint-source");
        let placement_id = footprint_placement_id(&board_id, &source_uuid);
        let library_identifier = FootprintLibraryIdentifier {
            nickname: "Resistor_SMD".to_owned(),
            entry_name: "R_0603_1608Metric".to_owned(),
        };
        let observed_revision = revision('b');
        let pad_source_uuid = id("pad-source");
        let pad = ObservedPad {
            pad_id: footprint_pad_id(&placement_id, &pad_source_uuid),
            source_uuid: pad_source_uuid,
            footprint_placement_id: placement_id.clone(),
            number: "1".to_owned(),
            kind: PadKind::SurfaceMount,
            lock_state: LockState::Unlocked,
            position: LocalCoordinateNm {
                x_nm: -750_000,
                y_nm: 0,
            },
            stack_type: PadStackType::Normal,
            orientation_mdeg: 0,
            layers: vec![
                BoardLayer::FrontCopper,
                BoardLayer::FrontMask,
                BoardLayer::FrontPaste,
            ],
            copper_shapes: vec![copper_shape(BoardLayer::FrontCopper, PadShape::Rectangle)],
            drill: None,
            net: Some(ObservedNetIdentity {
                net_id: board_net_id(&board_id, "GND"),
                name: "GND".to_owned(),
            }),
            observed_revision: observed_revision.clone(),
        };
        let definition_id = footprint_definition_id(&library_identifier);
        (
            board_id,
            ObservedFootprintPlacement {
                placement_id,
                source_uuid,
                definition_id,
                library_identifier,
                reference: "R1".to_owned(),
                value: "10k".to_owned(),
                position: Coordinate {
                    x: 10_000_000,
                    y: 20_000_000,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                orientation_mdeg: 90_000,
                side: BoardSide::Front,
                lock_state: LockState::Unlocked,
                symbol_sheet_path: vec![id("root-sheet"), id("child-sheet")],
                pads: vec![pad],
                observed_revision,
            },
        )
    }

    fn snapshot_with_native(native_footprints: ObservedBoardFootprints) -> ProjectSnapshot {
        let mut snapshot = snapshot(Vec::new());
        snapshot.board.document_id = Some(native_footprints.board_id.clone());
        snapshot.native_footprints = Some(native_footprints);
        snapshot
    }

    #[test]
    fn project_snapshot_native_footprints_are_canonical_and_revision_bound() {
        let (board_id, mut footprint) = observed_footprint();
        let mut second_pad = footprint.pads[0].clone();
        second_pad.source_uuid = id("another-pad");
        second_pad.pad_id = footprint_pad_id(&footprint.placement_id, &second_pad.source_uuid);
        footprint.pads.push(second_pad);
        let native = ObservedBoardFootprints::new(board_id, vec![footprint]).unwrap();
        let first = snapshot_with_native(native.clone());
        let mut reordered_native = native;
        reordered_native.placements[0].pads.reverse();
        let reordered = snapshot_with_native(reordered_native);
        assert_eq!(first.revision().unwrap(), reordered.revision().unwrap());
        assert_eq!(first.canonical(), reordered.canonical());

        let mut changed_placement = first.native_footprints.as_ref().unwrap().placements[0].clone();
        changed_placement.value = "changed".to_owned();
        let changed_native = ObservedBoardFootprints::new(
            first.board.document_id.clone().unwrap(),
            vec![changed_placement],
        )
        .unwrap();
        let changed = snapshot_with_native(changed_native);
        assert_ne!(first.revision().unwrap(), changed.revision().unwrap());
    }

    #[test]
    fn project_snapshot_rejects_mismatched_or_forged_native_footprints() {
        let (board_id, footprint) = observed_footprint();
        let native = ObservedBoardFootprints::new(board_id, vec![footprint]).unwrap();
        let mut mismatched = snapshot_with_native(native.clone());
        mismatched.board.document_id = Some(id("other-board"));
        assert!(matches!(
            mismatched.validate(),
            Err(ModelError::InvalidFootprintObservation(
                "native footprint board identity does not match the project board"
            ))
        ));

        let mut forged = snapshot_with_native(native);
        forged.native_footprints.as_mut().unwrap().observed_revision = revision('f');
        assert!(matches!(
            forged.revision(),
            Err(ModelError::InvalidFootprintObservation(
                "footprint observation revision does not match its content"
            ))
        ));
    }

    #[test]
    fn footprint_observation_round_trips_and_ids_are_stable() {
        let (board_id, footprint) = observed_footprint();
        assert!(footprint.validate(&board_id).is_ok());
        let decoded: ObservedFootprintPlacement =
            serde_json::from_slice(&serde_json::to_vec(&footprint).unwrap()).unwrap();
        assert_eq!(decoded, footprint);
        assert_eq!(
            decoded.pads[0].layers,
            vec![
                BoardLayer::FrontCopper,
                BoardLayer::FrontMask,
                BoardLayer::FrontPaste,
            ]
        );
        assert_eq!(
            footprint.placement_id,
            footprint_placement_id(&board_id, &footprint.source_uuid)
        );
        assert_ne!(
            footprint.placement_id, footprint.definition_id,
            "derived identity domains must remain separate"
        );
        let mut changed_revision = footprint.clone();
        changed_revision.observed_revision = revision('c');
        changed_revision.pads[0].observed_revision = revision('c');
        assert_ne!(
            serde_json::to_vec(&footprint).unwrap(),
            serde_json::to_vec(&changed_revision).unwrap()
        );
    }

    #[test]
    fn footprint_definition_identity_is_stable_across_boards() {
        let (first_board_id, footprint) = observed_footprint();
        let second_board_id = id("second-board");
        let mut second_placement = footprint.clone();
        second_placement.placement_id =
            footprint_placement_id(&second_board_id, &second_placement.source_uuid);
        second_placement.pads[0].footprint_placement_id = second_placement.placement_id.clone();
        second_placement.pads[0].pad_id = footprint_pad_id(
            &second_placement.placement_id,
            &second_placement.pads[0].source_uuid,
        );
        second_placement.pads[0].net.as_mut().unwrap().net_id =
            board_net_id(&second_board_id, "GND");

        assert!(footprint.validate(&first_board_id).is_ok());
        assert!(second_placement.validate(&second_board_id).is_ok());
        assert_eq!(footprint.definition_id, second_placement.definition_id);
        assert_ne!(footprint.placement_id, second_placement.placement_id);
        assert_eq!(
            footprint.definition_id,
            footprint_definition_id(&footprint.library_identifier)
        );
    }

    #[test]
    fn footprint_canonicalization_preserves_pad_layers_and_per_layer_shapes() {
        let (board_id, mut footprint) = observed_footprint();
        footprint.pads[0].layers = vec![
            BoardLayer::FrontPaste,
            BoardLayer::FrontCopper,
            BoardLayer::FrontMask,
        ];
        footprint.pads[0].copper_shapes = vec![
            copper_shape(BoardLayer::BackCopper, PadShape::Oval),
            copper_shape(BoardLayer::FrontCopper, PadShape::Rectangle),
        ];
        let mut second = footprint.pads[0].clone();
        second.source_uuid = id("another-pad");
        second.pad_id = footprint_pad_id(&footprint.placement_id, &second.source_uuid);
        second.number.clear();
        second.copper_shapes.clear();
        footprint.pads.insert(0, second);
        let canonical = footprint.canonical();
        assert!(canonical.validate(&board_id).is_ok());
        assert_eq!(canonical.pads[0].number, "");
        assert_eq!(
            canonical.pads[1].layers,
            vec![
                BoardLayer::FrontCopper,
                BoardLayer::FrontMask,
                BoardLayer::FrontPaste,
            ]
        );
        assert_eq!(canonical.pads[1].copper_shapes.len(), 2);
        assert_eq!(
            canonical.pads[1].copper_shapes[0].layer,
            BoardLayer::FrontCopper
        );
    }

    #[test]
    fn empty_and_repeated_pad_numbers_are_allowed() {
        let (board_id, mut footprint) = observed_footprint();
        footprint.pads[0].number.clear();
        let mut second = footprint.pads[0].clone();
        second.source_uuid = id("second-pad");
        second.pad_id = footprint_pad_id(&footprint.placement_id, &second.source_uuid);
        footprint.pads.push(second);
        assert!(footprint.canonical().validate(&board_id).is_ok());
    }

    #[test]
    fn footprint_validation_rejects_invalid_context_geometry_and_order() {
        let (board_id, footprint) = observed_footprint();
        let mut cases = Vec::new();

        let mut invalid = footprint.clone();
        invalid.position.space = DocumentSpace::Schematic;
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.library_identifier.entry_name.clear();
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.placement_id = id("forged-placement");
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].pad_id = id("forged-pad");
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].footprint_placement_id = id("other-placement");
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].observed_revision = revision('c');
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].layers.push(BoardLayer::FrontMask);
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].layers.swap(0, 2);
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].layers = vec![BoardLayer::InnerCopper(31)];
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].copper_shapes[0].layer = BoardLayer::FrontMask;
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].copper_shapes[0].size.x_nm = 0;
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].drill = Some(Drill {
            shape: DrillShape::Oblong,
            size: SizeNm {
                x_nm: 0,
                y_nm: 300_000,
            },
        });
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0].copper_shapes[0].layer = BoardLayer::InnerCopper(31);
        cases.push(invalid);
        let mut invalid = footprint.clone();
        invalid.pads[0]
            .copper_shapes
            .push(copper_shape(BoardLayer::FrontCopper, PadShape::Oval));
        cases.push(invalid);
        let mut invalid = footprint;
        invalid.pads.push(invalid.pads[0].clone());
        cases.push(invalid);

        assert!(cases.iter().all(|case| case.validate(&board_id).is_err()));
    }

    #[test]
    fn finite_ratio_is_canonical_and_numeric_in_json() {
        let zero = FiniteRatio::new(0.0).unwrap();
        let negative_zero = FiniteRatio::new(-0.0).unwrap();
        assert_eq!(zero, negative_zero);
        assert_eq!(serde_json::to_value(zero).unwrap(), serde_json::json!(0.0));
        for value in [-0.1, 0.25, 1.1] {
            let ratio = FiniteRatio::new(value).unwrap();
            assert_eq!(
                serde_json::from_value::<FiniteRatio>(serde_json::json!(value)).unwrap(),
                ratio
            );
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(FiniteRatio::new(invalid).is_err());
        }
    }

    #[test]
    fn extended_pad_geometry_and_stack_semantics_are_preserved() {
        let (board_id, mut footprint) = observed_footprint();
        let pad = &mut footprint.pads[0];
        pad.stack_type = PadStackType::FrontInnerBack;
        pad.orientation_mdeg = 45_000;
        pad.copper_shapes = vec![
            {
                let mut shape = copper_shape(BoardLayer::FrontCopper, PadShape::RoundedRectangle);
                shape.corner_rounding_ratio = FiniteRatio::new(0.25).unwrap();
                shape
            },
            copper_shape(BoardLayer::InnerCopper(1), PadShape::Oval),
            {
                let mut shape = copper_shape(BoardLayer::BackCopper, PadShape::ChamferedRectangle);
                shape.chamfer_ratio = FiniteRatio::new(0.2).unwrap();
                shape.chamfer_top_left = true;
                shape.chamfer_bottom_right = true;
                shape
            },
        ];
        let canonical = footprint.canonical();
        assert!(canonical.validate(&board_id).is_ok());
        assert_eq!(canonical.pads[0].stack_type, PadStackType::FrontInnerBack);
        assert_eq!(canonical.pads[0].orientation_mdeg, 45_000);
        assert_eq!(canonical.pads[0].copper_shapes.len(), 3);
        assert_eq!(
            canonical.pads[0].copper_shapes[1].layer,
            BoardLayer::InnerCopper(1),
            "the representative inner-layer entry must not be expanded"
        );
        let decoded: ObservedFootprintPlacement =
            serde_json::from_value(serde_json::to_value(&canonical).unwrap()).unwrap();
        assert_eq!(decoded, canonical);
    }

    #[test]
    fn custom_anchor_shape_is_limited_to_supported_values() {
        let mut invalid = copper_shape(BoardLayer::FrontCopper, PadShape::Custom);
        invalid.custom_anchor_shape = Some(PadShape::Oval);
        assert!(invalid.validate().is_err());

        let mut valid = copper_shape(BoardLayer::FrontCopper, PadShape::Custom);
        valid.custom_anchor_shape = Some(PadShape::Circle);
        assert!(valid.validate().is_ok());
        valid.custom_anchor_shape = Some(PadShape::Rectangle);
        assert!(valid.validate().is_ok());
    }

    fn second_observed_footprint(
        board_id: &StableId,
        footprint: &ObservedFootprintPlacement,
    ) -> ObservedFootprintPlacement {
        let mut second = footprint.clone();
        second.source_uuid = id("second-footprint-source");
        second.placement_id = footprint_placement_id(board_id, &second.source_uuid);
        second.reference = "R2".to_owned();
        second.pads[0].source_uuid = id("second-footprint-pad-source");
        second.pads[0].footprint_placement_id = second.placement_id.clone();
        second.pads[0].pad_id = footprint_pad_id(&second.placement_id, &second.pads[0].source_uuid);
        second
    }

    #[test]
    fn board_footprint_observation_revision_is_order_independent_and_content_bound() {
        let (board_id, footprint) = observed_footprint();
        let second = second_observed_footprint(&board_id, &footprint);
        let first_order =
            ObservedBoardFootprints::new(board_id.clone(), vec![footprint.clone(), second.clone()])
                .unwrap();
        let reverse_order =
            ObservedBoardFootprints::new(board_id.clone(), vec![second, footprint]).unwrap();
        assert_eq!(first_order, reverse_order);
        assert!(first_order.validate().is_ok());
        assert!(first_order.placements.iter().all(|placement| {
            placement.observed_revision == first_order.observed_revision
                && placement
                    .pads
                    .iter()
                    .all(|pad| pad.observed_revision == first_order.observed_revision)
        }));

        let mut changed_placement = first_order.placements[0].clone();
        changed_placement.orientation_mdeg += 1;
        let changed = ObservedBoardFootprints::new(
            board_id,
            vec![changed_placement, first_order.placements[1].clone()],
        )
        .unwrap();
        assert_ne!(first_order.observed_revision, changed.observed_revision);
    }

    #[test]
    fn board_footprint_observation_rejects_forged_net_identity() {
        let (board_id, footprint) = observed_footprint();
        let observation = ObservedBoardFootprints::new(board_id.clone(), vec![footprint]).unwrap();

        let mut forged = observation.placements[0].clone();
        forged.pads[0].net.as_mut().unwrap().net_id = id("forged-net");
        assert!(ObservedBoardFootprints::new(board_id.clone(), vec![forged]).is_err());

        let mut forged = observation.placements[0].clone();
        forged.pads[0].net.as_mut().unwrap().name = "+3V3".to_owned();
        assert!(ObservedBoardFootprints::new(board_id.clone(), vec![forged]).is_err());

        let mut forged = observation.placements[0].clone();
        forged.pads[0].net.as_mut().unwrap().name.clear();
        assert!(ObservedBoardFootprints::new(board_id, vec![forged]).is_err());
    }

    #[test]
    fn board_footprint_observation_rejects_forged_revisions() {
        let (board_id, footprint) = observed_footprint();
        let observation = ObservedBoardFootprints::new(board_id, vec![footprint]).unwrap();

        let mut forged = observation.clone();
        forged.observed_revision = revision('f');
        assert!(forged.validate().is_err());

        let mut forged = observation;
        forged.placements[0].observed_revision = revision('f');
        forged.placements[0].pads[0].observed_revision = revision('f');
        assert!(forged.validate().is_err());
    }

    #[test]
    fn board_net_identity_is_stable_and_board_scoped() {
        let first = board_net_id(&id("board"), "GND");
        assert_eq!(first, board_net_id(&id("board"), "GND"));
        assert_ne!(first, board_net_id(&id("other-board"), "GND"));
        assert_ne!(first, board_net_id(&id("board"), "+3V3"));
    }

    #[test]
    fn old_board_snapshot_shape_is_unchanged() {
        let board: Board = serde_json::from_value(serde_json::json!({"footprints": []})).unwrap();
        assert_eq!(
            serde_json::to_value(board).unwrap(),
            serde_json::json!({
                "document_id": null,
                "footprints": [],
                "graphics": []
            })
        );
    }
}
