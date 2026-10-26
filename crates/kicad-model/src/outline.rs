//! Serializable outline intent; compilation and native topology verification are separate gates.
use crate::StableId;
use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct OutlinePoint {
    pub x_nm: i64,
    pub y_nm: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutlineLoop {
    #[schemars(with = "String")]
    pub loop_id: StableId,
    #[schemars(length(max = 512))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 512>")]
    pub edges: Vec<OutlineEdge>,
    pub width_nm: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutlineEdit {
    #[schemars(length(max = 100))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 100>")]
    #[schemars(with = "Vec<String>")]
    pub remove_ids: Vec<StableId>,
    #[schemars(length(max = 32))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 32>")]
    pub loops: Vec<OutlineLoop>,
}
