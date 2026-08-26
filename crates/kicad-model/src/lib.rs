use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
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
            Self::InvalidBoardGraphic(message) => formatter.write_str(message),
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
}
