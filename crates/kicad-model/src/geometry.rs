//! Bounded, explicit outline and solid-zone intent; native execution and approval are separate.
use crate::{CopperLayer, OutlineEdit, OutlinePoint, RevisionDigest, StableId};
use serde::{
    Deserialize, Serialize,
    de::{self, SeqAccess, Visitor},
};
use std::{fmt, marker::PhantomData};

pub(crate) fn bounded_vec<'de, D, T, const N: usize>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T, const N: usize>(PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> Visitor<'de> for Bounded<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "at most {N} values")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(N));
            while let Some(value) = seq.next_element()? {
                if values.len() == N {
                    return Err(de::Error::custom("geometry collection exceeds limit"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(Bounded::<T, N>(PhantomData))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneNetIdentity {
    #[schemars(with = "String")]
    pub net_id: StableId,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZoneRing {
    #[schemars(length(max = 512))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 512>")]
    pub points: Vec<OutlinePoint>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ZonePolygon {
    pub exterior: ZoneRing,
    #[schemars(length(max = 16))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 16>")]
    pub holes: Vec<ZoneRing>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZoneConnection {
    Solid,
    Thermal,
    PthThermal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IslandRemoval {
    Always,
    Never,
    BelowArea,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefillMode {
    All,
    None,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SolidZoneDefinition {
    pub name: String,
    pub net: ZoneNetIdentity,
    #[schemars(length(max = 32))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 32>")]
    #[schemars(with = "Vec<String>")]
    pub layers: Vec<CopperLayer>,
    pub polygon: ZonePolygon,
    pub priority: u32,
    pub clearance_nm: i64,
    pub min_thickness_nm: i64,
    pub connection: ZoneConnection,
    pub thermal_gap_nm: i64,
    pub thermal_spoke_width_nm: i64,
    pub island_removal: IslandRemoval,
    pub min_island_area_nm2: u64,
}

// Optional field patches retain native settings not expressible by this authoring schema.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SolidZonePatch {
    pub name: Option<String>,
    pub net: Option<ZoneNetIdentity>,
    #[serde(default, deserialize_with = "bounded_optional_layers")]
    #[schemars(with = "Option<Vec<String>>")]
    pub layers: Option<Vec<CopperLayer>>,
    pub polygon: Option<ZonePolygon>,
    pub priority: Option<u32>,
    pub clearance_nm: Option<i64>,
    pub min_thickness_nm: Option<i64>,
    pub connection: Option<ZoneConnection>,
    pub thermal_gap_nm: Option<i64>,
    pub thermal_spoke_width_nm: Option<i64>,
    pub island_removal: Option<IslandRemoval>,
    pub min_island_area_nm2: Option<u64>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ZoneEdit {
    Create {
        #[schemars(with = "String")]
        zone_id: StableId,
        definition: SolidZoneDefinition,
    },
    Update {
        #[schemars(with = "String")]
        source_uuid: StableId,
        patch: SolidZonePatch,
    },
    Delete {
        #[schemars(with = "String")]
        source_uuid: StableId,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GeometryRequest {
    #[schemars(with = "String")]
    #[serde(deserialize_with = "geometry_revision")]
    pub observed_revision: RevisionDigest,
    pub outline: Option<OutlineEdit>,
    #[schemars(length(max = 32))]
    #[serde(deserialize_with = "crate::geometry::bounded_vec::<_, _, 32>")]
    pub zones: Vec<ZoneEdit>,
    pub refill: RefillMode,
}

impl GeometryRequest {
    /// Bound complete JSON input before deserialization; collection bounds also apply to direct serde use.
    pub fn from_json(input: &str) -> Result<Self, String> {
        if input.len() > 2 * 1024 * 1024 {
            return Err("geometry request exceeds 2 MiB".into());
        }
        serde_json::from_str(input).map_err(|e| e.to_string())
    }
}

fn bounded_optional_layers<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<CopperLayer>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Layers(#[serde(deserialize_with = "bounded_vec::<_, _, 32>")] Vec<CopperLayer>);
    Ok(Option::<Layers>::deserialize(d)?.map(|l| l.0))
}

fn geometry_revision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RevisionDigest, D::Error> {
    let value = String::deserialize(d)?;
    RevisionDigest::new(value.strip_prefix("sha256:").unwrap_or(&value))
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    #[test]
    fn public_geometry_preserves_revision_roundtrip_and_bounded_strict_schema() {
        let raw = "a".repeat(64);
        let value =
            serde_json::json!({"observed_revision":raw,"outline":null,"zones":[],"refill":"all"});
        let request: GeometryRequest = serde_json::from_value(value.clone()).unwrap();
        let mut prefixed = value.clone();
        prefixed["observed_revision"] = serde_json::json!(format!("sha256:{raw}"));
        assert_eq!(request, serde_json::from_value(prefixed).unwrap());
        assert_eq!(serde_json::to_value(&request).unwrap(), value);
        for revision in [
            "sha256:bad".to_owned(),
            format!("sha256:sha256:{raw}"),
            format!("md5:{raw}"),
        ] {
            let mut bad = value.clone();
            bad["observed_revision"] = serde_json::json!(revision);
            assert!(serde_json::from_value::<GeometryRequest>(bad).is_err());
        }
        let mut bad = value.clone();
        bad["source_evidence"] = serde_json::json!("forged");
        assert!(serde_json::from_value::<GeometryRequest>(bad).is_err());
        let mut bad = value;
        bad["zones"] = serde_json::json!(vec![
            serde_json::json!({"kind":"delete","source_uuid":"zone"});
            33
        ]);
        assert!(serde_json::from_value::<GeometryRequest>(bad).is_err());
        let schema = serde_json::to_value(schemars::schema_for!(GeometryRequest)).unwrap();
        assert_eq!(schema["properties"]["zones"]["maxItems"], 32);
        assert_eq!(schema["additionalProperties"], false);
    }
}
