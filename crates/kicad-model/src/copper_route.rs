//! Explicit copper paths; geometry validation never infers missing routing choices.
use crate::{
    Coordinate, CopperLayer, DocumentSpace, ModelError, ObservedNetIdentity, RevisionDigest,
    StableId, StraightTrackIntent, ThroughViaIntent, Unit,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ROUTE_LEGS: usize = 16;
pub const MAX_ROUTE_SEGMENTS: usize = 128;
pub const MAX_ROUTE_VIAS: usize = 15;
pub const MAX_BATCH_COPPER: usize = 256;
const LIMIT_NM: i64 = 2_000_000_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePoint {
    pub x_nm: i64,
    pub y_nm: i64,
}
impl RoutePoint {
    pub fn coordinate(self) -> Coordinate {
        Coordinate {
            x: self.x_nm,
            y: self.y_nm,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        }
    }
    fn validate(self) -> Result<(), ModelError> {
        if self.x_nm.unsigned_abs() > LIMIT_NM as u64 || self.y_nm.unsigned_abs() > LIMIT_NM as u64
        {
            return Err(invalid("route coordinate exceeds bounds"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteTrackEnd {
    Start,
    End,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutingAnchor {
    Pad {
        pad_id: StableId,
        footprint_placement_id: StableId,
    },
    TrackEnd {
        source_uuid: StableId,
        end: RouteTrackEnd,
    },
    Via {
        source_uuid: StableId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteLeg {
    pub layer: CopperLayer,
    pub width_nm: i64,
    pub points: Vec<RoutePoint>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteVia {
    pub via_id: StableId,
    pub position: RoutePoint,
    pub pad_diameter_nm: i64,
    pub drill_diameter_nm: i64,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperRouteIntent {
    pub route_id: StableId,
    pub observed_revision: RevisionDigest,
    pub start: RoutingAnchor,
    pub end: RoutingAnchor,
    pub net: ObservedNetIdentity,
    pub legs: Vec<RouteLeg>,
    pub vias: Vec<RouteVia>,
}

fn invalid(reason: &'static str) -> ModelError {
    ModelError::InvalidRoutingObservation(reason)
}
fn identifier(id: &StableId) -> Result<(), ModelError> {
    if id.as_str().is_empty() || id.as_str().len() > 4096 || id.as_str().contains('\0') {
        return Err(invalid("route identity is not bounded"));
    }
    Ok(())
}
impl CopperRouteIntent {
    pub fn segment_id(&self, leg: usize, segment: usize) -> StableId {
        StableId::new(format!("{}:leg:{leg}:segment:{segment}", self.route_id))
            .expect("derived route segment identity is nonempty")
    }
    pub fn validate(&self) -> Result<(), ModelError> {
        identifier(&self.route_id)?;
        identifier(&self.net.net_id)?;
        if self.net.name.is_empty() || self.net.name.len() > 4096 || self.net.name.contains('\0') {
            return Err(invalid("route net name is not bounded"));
        }
        for anchor in [&self.start, &self.end] {
            match anchor {
                RoutingAnchor::Pad {
                    pad_id,
                    footprint_placement_id,
                } => {
                    identifier(pad_id)?;
                    identifier(footprint_placement_id)?;
                }
                RoutingAnchor::TrackEnd { source_uuid, .. }
                | RoutingAnchor::Via { source_uuid } => identifier(source_uuid)?,
            }
        }
        if self.legs.is_empty()
            || self.legs.len() > MAX_ROUTE_LEGS
            || self.vias.len() > MAX_ROUTE_VIAS
        {
            return Err(invalid("route leg/via count exceeds bounds"));
        }
        let mut ids = BTreeSet::new();
        let mut via_positions = BTreeMap::new();
        for via in &self.vias {
            identifier(&via.via_id)?;
            via.position.validate()?;
            if via.drill_diameter_nm <= 0
                || via.pad_diameter_nm > LIMIT_NM
                || via.drill_diameter_nm >= via.pad_diameter_nm
            {
                return Err(invalid(
                    "route via requires positive drill smaller than bounded diameter",
                ));
            }
            if !ids.insert(via.via_id.clone())
                || via_positions.insert(via.position, &via.via_id).is_some()
            {
                return Err(invalid("route via IDs and positions must be unique"));
            }
        }
        let mut count = 0;
        let mut used_vias = BTreeSet::new();
        for (i, leg) in self.legs.iter().enumerate() {
            if leg.width_nm <= 0
                || leg.width_nm > LIMIT_NM
                || leg.points.len() < 2
                || leg.points.len() > MAX_ROUTE_SEGMENTS + 1
            {
                return Err(invalid("route leg dimensions or point count are invalid"));
            }
            count += leg.points.len() - 1;
            if count > MAX_ROUTE_SEGMENTS {
                return Err(invalid("route segment count exceeds bounds"));
            }
            for point in &leg.points {
                point.validate()?;
            }
            for (j, pair) in leg.points.windows(2).enumerate() {
                if pair[0] == pair[1] || !ids.insert(self.segment_id(i, j)) {
                    return Err(invalid(
                        "route segment is zero length or has a duplicate identity",
                    ));
                }
            }
            if i > 0 {
                let prior = &self.legs[i - 1];
                if prior.points.last() != leg.points.first() {
                    return Err(invalid("adjacent route legs are disconnected"));
                }
                if prior.layer != leg.layer {
                    let via = via_positions.get(&leg.points[0]).ok_or(invalid(
                        "layer transition requires one explicit through via",
                    ))?;
                    used_vias.insert(*via);
                }
            }
        }
        if used_vias.len() != self.vias.len() {
            return Err(invalid("route contains an unused transition via"));
        }
        Ok(())
    }

    /// Expand explicit geometry without manufacturing pad identities for intermediate vertices.
    pub fn primitives(
        &self,
    ) -> Result<(Vec<StraightTrackIntent>, Vec<ThroughViaIntent>), ModelError> {
        self.validate()?;
        let mut tracks = Vec::new();
        for (i, leg) in self.legs.iter().enumerate() {
            for (j, pair) in leg.points.windows(2).enumerate() {
                let mut track = StraightTrackIntent::new(
                    self.segment_id(i, j),
                    pair[0].coordinate(),
                    pair[1].coordinate(),
                    leg.width_nm,
                )?;
                track.layer = leg.layer;
                track.net = Some(self.net.clone());
                tracks.push(track);
            }
        }
        let vias = self
            .vias
            .iter()
            .map(|via| {
                ThroughViaIntent::new_net_bound(
                    via.via_id.clone(),
                    via.position.coordinate(),
                    via.pad_diameter_nm,
                    via.drill_diameter_nm,
                    self.net.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((tracks, vias))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route() -> CopperRouteIntent {
        let id = |s: &str| StableId::new(s).unwrap();
        let point = |x| RoutePoint { x_nm: x, y_nm: 0 };
        CopperRouteIntent {
            route_id: id("route"),
            observed_revision: RevisionDigest::new("0".repeat(64)).unwrap(),
            start: RoutingAnchor::Via {
                source_uuid: id("start"),
            },
            end: RoutingAnchor::Via {
                source_uuid: id("end"),
            },
            net: ObservedNetIdentity {
                net_id: id("net"),
                name: "GND".into(),
            },
            legs: vec![
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 200_000,
                    points: vec![point(0), point(1_000_000)],
                },
                RouteLeg {
                    layer: "In1.Cu".parse().unwrap(),
                    width_nm: 250_000,
                    points: vec![point(1_000_000), point(2_000_000)],
                },
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 200_000,
                    points: vec![point(2_000_000), point(3_000_000)],
                },
            ],
            vias: vec![
                RouteVia {
                    via_id: id("v1"),
                    position: point(1_000_000),
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
                RouteVia {
                    via_id: id("v2"),
                    position: point(2_000_000),
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
            ],
        }
    }
    #[test]
    fn explicit_path_expansion_preserves_topology_layers_and_identity() {
        let route = route();
        let (tracks, vias) = route.primitives().unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(vias.len(), 2);
        for (i, track) in tracks.iter().enumerate() {
            assert_eq!(track.source_uuid, route.segment_id(i, 0));
            assert_eq!(track.layer, route.legs[i].layer);
            assert!(track.start_pad.is_none() && track.end_pad.is_none());
            assert_eq!(track.net.as_ref(), Some(&route.net));
        }
        let legacy = StraightTrackIntent::new(
            StableId::new("old").unwrap(),
            tracks[0].start,
            tracks[0].end,
            200_000,
        )
        .unwrap();
        let old = serde_json::to_value(&legacy).unwrap();
        assert!(old.get("layer").is_none());
        assert_eq!(
            serde_json::from_value::<StraightTrackIntent>(old)
                .unwrap()
                .layer,
            CopperLayer::FRONT
        );
        let inner = serde_json::to_value(&tracks[1]).unwrap();
        assert_eq!(inner["layer"], "In1.Cu");
        assert_eq!(
            serde_json::from_value::<StraightTrackIntent>(inner).unwrap(),
            tracks[1]
        );
    }
    #[test]
    fn incomplete_ambiguous_and_unbounded_routes_reject() {
        let original = route();
        let cases: [fn(&mut CopperRouteIntent); 11] = [
            |r| {
                r.vias.pop();
            },
            |r| r.legs[1].points[0].x_nm += 1,
            |r| r.legs[0].points[1] = r.legs[0].points[0],
            |r| r.vias[0].via_id = r.segment_id(0, 0),
            |r| r.vias[1].position = r.vias[0].position,
            |r| r.vias[0].drill_diameter_nm = r.vias[0].pad_diameter_nm,
            |r| r.legs[0].points[0].x_nm = i64::MIN,
            |r| r.legs[0].width_nm = 0,
            |r| r.legs[1].layer = CopperLayer::FRONT,
            |r| r.legs[0].points = vec![RoutePoint { x_nm: 0, y_nm: 0 }; 130],
            |r| r.legs = vec![r.legs[0].clone(); 17],
        ];
        for change in cases {
            let mut route = original.clone();
            change(&mut route);
            assert!(route.validate().is_err());
        }
        let mut same = original;
        same.legs.truncate(1);
        same.vias.clear();
        let mut next = same.legs[0].clone();
        next.points.iter_mut().for_each(|p| p.x_nm += 1_000_000);
        next.width_nm = 300_000;
        same.legs.push(next);
        assert!(same.validate().is_ok());
    }
}
