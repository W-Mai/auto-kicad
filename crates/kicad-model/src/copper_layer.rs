//! Canonical copper layers with explicit observed-stackup validation.
use crate::{BoardLayer, ModelError, ObservedBoardStackup, ObservedStackupLayerIdentity};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CopperLayer(BoardLayer);

impl CopperLayer {
    pub const FRONT: Self = Self(BoardLayer::FrontCopper);
    pub const BACK: Self = Self(BoardLayer::BackCopper);
    pub fn board_layer(self) -> BoardLayer {
        self.0
    }
    pub fn require_enabled_on(self, stackup: &ObservedBoardStackup) -> Result<(), ModelError> {
        stackup.validate()?;
        let identity = match self.0 {
            BoardLayer::FrontCopper => ObservedStackupLayerIdentity::FrontCopper,
            BoardLayer::BackCopper => ObservedStackupLayerIdentity::BackCopper,
            BoardLayer::InnerCopper(index) => ObservedStackupLayerIdentity::InnerCopper(index),
            _ => unreachable!("CopperLayer excludes non-copper layers"),
        };
        if !stackup
            .layers
            .iter()
            .any(|layer| layer.enabled && layer.layer == identity)
        {
            return Err(ModelError::InvalidRoutingObservation(
                "requested copper layer is not enabled in the observed stackup",
            ));
        }
        Ok(())
    }
}

impl Default for CopperLayer {
    fn default() -> Self {
        Self::FRONT
    }
}

impl TryFrom<BoardLayer> for CopperLayer {
    type Error = ModelError;
    fn try_from(value: BoardLayer) -> Result<Self, Self::Error> {
        match value {
            BoardLayer::FrontCopper | BoardLayer::BackCopper | BoardLayer::InnerCopper(1..=30) => {
                Ok(Self(value))
            }
            _ => Err(ModelError::InvalidRoutingObservation(
                "expected F.Cu, B.Cu or In1.Cu through In30.Cu",
            )),
        }
    }
}

impl FromStr for CopperLayer {
    type Err = ModelError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "F.Cu" => Ok(Self::FRONT),
            "B.Cu" => Ok(Self::BACK),
            _ => {
                let index = value
                    .strip_prefix("In")
                    .and_then(|s| s.strip_suffix(".Cu"))
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|i| (1..=30).contains(i) && value == format!("In{i}.Cu"))
                    .ok_or(ModelError::InvalidRoutingObservation(
                        "copper layer name must be canonical",
                    ))?;
                Self::try_from(BoardLayer::InnerCopper(index))
            }
        }
    }
}
impl TryFrom<String> for CopperLayer {
    type Error = ModelError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<CopperLayer> for String {
    fn from(value: CopperLayer) -> Self {
        value.to_string()
    }
}
impl fmt::Display for CopperLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            BoardLayer::FrontCopper => f.write_str("F.Cu"),
            BoardLayer::BackCopper => f.write_str("B.Cu"),
            BoardLayer::InnerCopper(index) => write!(f, "In{index}.Cu"),
            _ => unreachable!("CopperLayer excludes non-copper layers"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_layer_domain_roundtrips_and_rejects_aliases() {
        for name in ["F.Cu".to_owned(), "B.Cu".to_owned()]
            .into_iter()
            .chain((1..=30).map(|i| format!("In{i}.Cu")))
        {
            let layer: CopperLayer = name.parse().unwrap();
            assert_eq!(layer.to_string(), name);
            let encoded = serde_json::to_string(&layer).unwrap();
            assert_eq!(
                serde_json::from_str::<CopperLayer>(&encoded).unwrap(),
                layer
            );
        }
        for name in [
            "f.cu",
            "In0.Cu",
            "In31.Cu",
            "In01.Cu",
            "In+1.Cu",
            "F.Mask",
            "Edge.Cuts",
            " F.Cu",
            "FrontCopper",
        ] {
            assert!(name.parse::<CopperLayer>().is_err());
            assert!(serde_json::from_value::<CopperLayer>(serde_json::json!(name)).is_err());
        }
        for layer in [
            BoardLayer::FrontMask,
            BoardLayer::BackPaste,
            BoardLayer::InnerCopper(0),
            BoardLayer::InnerCopper(31),
        ] {
            assert!(CopperLayer::try_from(layer).is_err());
        }
    }
}
