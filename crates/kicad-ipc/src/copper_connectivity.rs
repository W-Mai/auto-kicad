//! Bounded same-net copper components from native connectivity.
use crate::*;
use std::collections::BTreeSet;

const CONNECTED: &str = "type.googleapis.com/kiapi.board.commands.GetConnectedItems";
const ARC: &str = "type.googleapis.com/kiapi.board.types.Arc";
fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedRouting(reason)
}

fn members(items: &[Any], source: &str, net: &str) -> Result<BTreeSet<String>, IpcError> {
    if items.is_empty() || items.len() > 4096 {
        return Err(invalid("native copper component is empty or oversized"));
    }
    let mut result = BTreeSet::new();
    for item in items {
        macro_rules! identity {
            ($kind:ty) => {{
                let value = <$kind>::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                let id = kiid_value(value.id.as_ref())
                    .ok_or(invalid("connected copper identity is missing"))?;
                if value.net.as_ref().map(|n| n.name.as_str()) != Some(net) {
                    return Err(invalid("connected copper has a different or missing net"));
                }
                id
            }};
        }
        let id = match item.type_url.as_str() {
            PAD_TYPE_URL => identity!(board_types::Pad),
            TRACK_TYPE_URL => identity!(board_types::Track),
            VIA_TYPE_URL => identity!(board_types::Via),
            ARC => identity!(board_types::Arc),
            _ => return Err(invalid("connectivity returned an unrequested object type")),
        };
        if id.len() > 4096 || id.contains('\0') || !result.insert(id) {
            return Err(invalid(
                "connected copper identity is duplicated or unbounded",
            ));
        }
    }
    if !result.contains(source) {
        return Err(invalid("native copper component omitted its source"));
    }
    Ok(result)
}

impl<T: KiCadTransport> KiCadClient<T> {
    /// Observe pads, tracks, arcs and vias in one native same-net connected component.
    /// Foreign-net shorts require DRC; this native query respects assigned net codes.
    pub fn connected_copper_ids(
        &mut self,
        filename: &str,
        source: &str,
        net: &str,
    ) -> Result<BTreeSet<String>, IpcError> {
        if [filename, source, net]
            .iter()
            .any(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
        {
            return Err(invalid(
                "copper component query requires bounded identities",
            ));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, filename)?;
        let response: editor_commands::GetItemsResponse = self.bound_item_command(
            CONNECTED,
            GET_ITEMS_RESPONSE_TYPE_URL,
            board_commands::GetConnectedItems {
                header: MessageField::some(board_item_header(filename)),
                items: vec![base_types::KIID {
                    value: source.into(),
                    ..Default::default()
                }],
                types: [
                    enums::KiCadObjectType::KOT_PCB_PAD,
                    enums::KiCadObjectType::KOT_PCB_TRACE,
                    enums::KiCadObjectType::KOT_PCB_ARC,
                    enums::KiCadObjectType::KOT_PCB_VIA,
                ]
                .into_iter()
                .map(protobuf::EnumOrUnknown::new)
                .collect(),
                ..Default::default()
            },
        )?;
        self.validate_item_reply(response.header, response.status, filename)?;
        let component = members(&response.items, source, net)?;
        if self.require_current_board(&token, filename)? != document {
            return Err(invalid(
                "copper component document changed during observation",
            ));
        }
        Ok(component)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_component_requires_exact_type_identity_and_net_evidence() {
        let item = |id: &str, net: &str| Any {
            type_url: TRACK_TYPE_URL.into(),
            value: board_types::Track {
                id: MessageField::some(base_types::KIID {
                    value: id.into(),
                    ..Default::default()
                }),
                net: MessageField::some(board_types::Net {
                    name: net.into(),
                    ..Default::default()
                }),
                ..Default::default()
            }
            .write_to_bytes()
            .unwrap(),
            ..Default::default()
        };
        let a = item("a", "GND");
        let b = item("b", "GND");
        assert_eq!(
            members(&[a.clone(), b.clone()], "a", "GND").unwrap(),
            BTreeSet::from(["a".into(), "b".into()])
        );
        for items in [
            vec![],
            vec![a.clone(), a.clone()],
            vec![b],
            vec![a.clone(), item("c", "VCC")],
            vec![a; 4097],
        ] {
            assert!(members(&items, "a", "GND").is_err());
        }
        let mut unknown = item("a", "GND");
        unknown.type_url = "unknown".into();
        assert!(members(&[unknown], "a", "GND").is_err());
    }
}
