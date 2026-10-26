//! Copper-only layer identity for native track construction.
use crate::{IpcError, ObservedBoardLayer, board_types};
use protobuf::Enum;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopperLayer(board_types::BoardLayer);

impl CopperLayer {
    pub const FRONT: Self = Self(board_types::BoardLayer::BL_F_Cu);
    pub const BACK: Self = Self(board_types::BoardLayer::BL_B_Cu);

    pub fn inner(index: u8) -> Result<Self, IpcError> {
        if !(1..=30).contains(&index) {
            return Err(IpcError::InvalidObservedRouting(
                "inner copper index must be 1 through 30",
            ));
        }
        Ok(Self(
            board_types::BoardLayer::from_i32(i32::from(index) + 3)
                .expect("pinned protocol contains all 30 inner copper layers"),
        ))
    }

    pub fn as_observed(self) -> ObservedBoardLayer {
        crate::routing_copper_layer(&protobuf::EnumOrUnknown::new(self.0))
            .expect("CopperLayer contains only validated copper values")
    }

    pub(crate) fn native(self) -> board_types::BoardLayer {
        self.0
    }
}

impl TryFrom<ObservedBoardLayer> for CopperLayer {
    type Error = IpcError;
    fn try_from(value: ObservedBoardLayer) -> Result<Self, Self::Error> {
        match value {
            ObservedBoardLayer::FrontCopper => Ok(Self::FRONT),
            ObservedBoardLayer::BackCopper => Ok(Self::BACK),
            ObservedBoardLayer::InnerCopper(index) => Self::inner(index),
            _ => Err(IpcError::InvalidObservedRouting(
                "track requires a copper layer",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    #[test]
    fn copper_domain_and_native_track_receipts_are_exact() {
        use protobuf::{Message, MessageField};
        for layer in [CopperLayer::FRONT, CopperLayer::BACK]
            .into_iter()
            .chain((1..=30).map(|i| CopperLayer::inner(i).unwrap()))
        {
            assert_eq!(CopperLayer::try_from(layer.as_observed()).unwrap(), layer);
            let request = CreateTrack {
                layer,
                start_x_nm: 100,
                start_y_nm: 200,
                end_x_nm: 300,
                end_y_nm: 400,
                width_nm: 100,
                net: None,
            };
            let mut item = create_track_item(&request).unwrap();
            let mut native = board_types::Track::parse_from_bytes(&item.value).unwrap();
            assert_eq!(
                routing_copper_layer(&native.layer).unwrap(),
                layer.as_observed()
            );
            native.id = MessageField::some(base_types::KIID {
                value: "created-track".into(),
                ..Default::default()
            });
            item.value = native.write_to_bytes().unwrap();
            let mut response = editor_commands::CreateItemsResponse {
                status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
                created_items: vec![editor_commands::ItemCreationResult {
                    status: MessageField::some(editor_commands::ItemStatus {
                        code: protobuf::EnumOrUnknown::new(editor_commands::ItemStatusCode::ISC_OK),
                        ..Default::default()
                    }),
                    item: MessageField::some(item),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let token = InstanceToken::new("test").unwrap();
            assert_eq!(
                created_track_receipt(&(response.clone(), token.clone()), &request, None)
                    .unwrap()
                    .track
                    .layer,
                layer
            );
            native.layer = protobuf::EnumOrUnknown::new(if layer == CopperLayer::FRONT {
                CopperLayer::BACK.native()
            } else {
                CopperLayer::FRONT.native()
            });
            response.created_items[0].item.as_mut().unwrap().value =
                native.write_to_bytes().unwrap();
            assert!(created_track_receipt(&(response, token), &request, None).is_err());
        }
        for layer in [
            ObservedBoardLayer::InnerCopper(0),
            ObservedBoardLayer::InnerCopper(31),
            ObservedBoardLayer::FrontMask,
            ObservedBoardLayer::BackPaste,
        ] {
            assert!(CopperLayer::try_from(layer).is_err());
        }
    }
}
