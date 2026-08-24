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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub footprints: Vec<Footprint>,
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
    Serialization(serde_json::Error),
}

impl Display for ModelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyStableId => formatter.write_str("stable id must not be empty"),
            Self::InvalidRevisionDigest => {
                formatter.write_str("revision digest must be 64 hexadecimal characters")
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
}
