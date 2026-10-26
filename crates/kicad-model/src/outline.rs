//! Serializable outline intent; compilation and native topology verification are separate gates.
use crate::StableId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlinePoint {
    pub x_nm: i64,
    pub y_nm: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutlineEdge {
    Segment {
        start: OutlinePoint,
        end: OutlinePoint,
    },
    Arc {
        start: OutlinePoint,
        mid: OutlinePoint,
        end: OutlinePoint,
    },
}
impl OutlineEdge {
    pub fn start(&self) -> OutlinePoint {
        match *self {
            Self::Segment { start, .. } | Self::Arc { start, .. } => start,
        }
    }
    pub fn end(&self) -> OutlinePoint {
        match *self {
            Self::Segment { end, .. } | Self::Arc { end, .. } => end,
        }
    }
    pub fn control_points(&self) -> Vec<OutlinePoint> {
        match *self {
            Self::Segment { start, end } => vec![start, end],
            Self::Arc { start, mid, end } => vec![start, mid, end],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineLoop {
    pub loop_id: StableId,
    pub edges: Vec<OutlineEdge>,
    pub width_nm: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlineEdit {
    pub remove_ids: Vec<StableId>,
    pub loops: Vec<OutlineLoop>,
}
