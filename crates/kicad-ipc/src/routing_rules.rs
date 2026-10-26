use super::*;
use std::collections::{BTreeMap, BTreeSet};

const QUERY: &str = "type.googleapis.com/kiapi.board.commands.GetNetClassForNets";
const REPLY: &str = "type.googleapis.com/kiapi.board.commands.NetClassForNetsResponse";

/// Preferred via dimensions, not hard manufacturing constraints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedNetClassViaSize {
    pub diameter_nm: i64,
    pub drill_nm: i64,
}

/// Effective native netclass evidence. Missing fields remain unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedNetClassRules {
    pub class_name: String,
    pub implicit: bool,
    pub constituents: Vec<String>,
    pub clearance_nm: Option<i64>,
    pub preferred_track_width_nm: Option<i64>,
    pub preferred_via: Option<ObservedNetClassViaSize>,
    pub tuning_profile: Option<String>,
    /// Complete native bytes retain unsupported settings and unknown fields for approval evidence.
    pub native_payload: Vec<u8>,
}

fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedRouting(reason)
}

fn distance(value: Option<&base_types::Distance>, zero: bool) -> Result<Option<i64>, IpcError> {
    value
        .map(|value| {
            if value.value_nm < if zero { 0 } else { 1 } || value.value_nm > 2_000_000_000 {
                Err(invalid("invalid netclass distance"))
            } else {
                Ok(value.value_nm)
            }
        })
        .transpose()
}

fn via_size(stack: &board_types::PadStack) -> Option<ObservedNetClassViaSize> {
    // Report only uniform equal-axis size preferences; retain all native geometry in the payload.
    let hole = stack.drill.as_ref()?.diameter.as_ref()?;
    if hole.x_nm <= 0 || hole.x_nm != hole.y_nm || hole.x_nm > 2_000_000_000 {
        return None;
    }
    let size = stack.copper_layers.first()?.size.as_ref()?;
    if size.x_nm <= hole.x_nm
        || size.x_nm != size.y_nm
        || size.x_nm > 2_000_000_000
        || stack
            .copper_layers
            .iter()
            .any(|layer| layer.size.as_ref() != Some(size))
    {
        return None;
    }
    Some(ObservedNetClassViaSize {
        diameter_nm: size.x_nm,
        drill_nm: hole.x_nm,
    })
}

fn observe(class: project_settings::NetClass) -> Result<ObservedNetClassRules, IpcError> {
    let implicit = match class.type_.enum_value() {
        Ok(project_settings::NetClassType::NCT_EXPLICIT) => false,
        Ok(project_settings::NetClassType::NCT_IMPLICIT) => true,
        _ => return Err(invalid("unknown netclass kind")),
    };
    if class.name.len() > 4096
        || (!implicit && class.name.is_empty())
        || class.constituents.len() > 64
        || class
            .constituents
            .iter()
            .any(|name| name.is_empty() || name.len() > 4096)
        || class.constituents.iter().collect::<BTreeSet<_>>().len() != class.constituents.len()
    {
        return Err(invalid("invalid netclass identity"));
    }
    let board = class.board.as_ref();
    let clearance_nm = distance(board.and_then(|b| b.clearance.as_ref()), true)?;
    let preferred_track_width_nm = distance(board.and_then(|b| b.track_width.as_ref()), false)?;
    let tuning_profile = board.and_then(|b| b.tuning_profile.clone());
    if tuning_profile
        .as_ref()
        .is_some_and(|name| name.len() > 4096)
    {
        return Err(invalid("netclass tuning profile exceeds limit"));
    }
    let payload = class
        .write_to_bytes()
        .map_err(|_| IpcError::MalformedEnvelope)?;
    if payload.len() > 1024 * 1024 {
        return Err(invalid("netclass payload exceeds limit"));
    }
    Ok(ObservedNetClassRules {
        class_name: class.name.clone(),
        implicit,
        constituents: class.constituents.clone(),
        clearance_nm,
        preferred_track_width_nm,
        preferred_via: board.and_then(|b| b.via_stack.as_ref()).and_then(via_size),
        tuning_profile,
        native_payload: payload,
    })
}

impl<T: KiCadTransport> KiCadClient<T> {
    /// Read the exact effective class set on the same editor and open board before and after querying.
    pub fn get_effective_netclass_rules(
        &mut self,
        board_filename: &str,
        net_names: &[String],
    ) -> Result<BTreeMap<String, ObservedNetClassRules>, IpcError> {
        let names = net_names.iter().cloned().collect::<BTreeSet<_>>();
        if board_filename.is_empty()
            || net_names.is_empty()
            || net_names.len() > 64
            || names.len() != net_names.len()
            || net_names
                .iter()
                .any(|name| name.is_empty() || name.len() > 4096 || name.contains('\0'))
        {
            return Err(invalid(
                "netclass query requires bounded unique nonempty nets and a board",
            ));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, board_filename)?;
        let response: board_commands::NetClassForNetsResponse = self.bound_item_command(
            QUERY,
            REPLY,
            board_commands::GetNetClassForNets {
                net: names
                    .iter()
                    .map(|name| board_types::Net {
                        name: name.clone(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        )?;
        if response.classes.keys().cloned().collect::<BTreeSet<_>>() != names {
            return Err(invalid(
                "effective netclass response omitted or added requested nets",
            ));
        }
        let observed = response
            .classes
            .into_iter()
            .map(|(name, class)| observe(class).map(|value| (name, value)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        if self.require_current_board(&token, board_filename)? != document {
            return Err(invalid("project identity changed during netclass query"));
        }
        Ok(observed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Wire {
        replies: VecDeque<ApiResponse>,
        requests: Vec<ApiRequest>,
    }
    impl KiCadTransport for Wire {
        fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
            self.requests.push(request);
            self.replies
                .pop_front()
                .ok_or(IpcError::MissingResponseMessage)
        }
    }
    fn response<M: Message>(url: &str, message: M, token: &str) -> ApiResponse {
        ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: token.into(),
                ..Default::default()
            }),
            message: MessageField::some(Any {
                type_url: url.into(),
                value: message.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: ApiStatusCode::AS_OK.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn board(name: &str, token: &str) -> ApiResponse {
        response(
            "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse",
            editor_commands::GetOpenDocumentsResponse {
                documents: vec![board_document(name)],
                ..Default::default()
            },
            token,
        )
    }
    fn class() -> project_settings::NetClass {
        project_settings::NetClass {
            name: "Power".into(),
            type_: project_settings::NetClassType::NCT_EXPLICIT.into(),
            board: MessageField::some(project_settings::NetClassBoardSettings {
                track_width: MessageField::some(base_types::Distance {
                    value_nm: 400_000,
                    ..Default::default()
                }),
                clearance: MessageField::some(base_types::Distance {
                    value_nm: 200_000,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn classes(name: &str, token: &str) -> ApiResponse {
        response(
            REPLY,
            board_commands::NetClassForNetsResponse {
                classes: std::collections::HashMap::from([(name.into(), class())]),
                ..Default::default()
            },
            token,
        )
    }
    fn client(replies: Vec<ApiResponse>) -> KiCadClient<Wire> {
        let mut client = KiCadClient::new(
            Wire {
                replies: replies.into(),
                requests: vec![],
            },
            ClientConfig::new("ipc:///tmp/rule-test", "rule-test").unwrap(),
        );
        client.token = Some(InstanceToken::new("same").unwrap());
        client
    }
    #[test]
    fn rules_require_exact_nets_and_stable_document_and_instance() {
        let good = vec![
            board("main.kicad_pcb", "same"),
            classes("GND", "same"),
            board("main.kicad_pcb", "same"),
        ];
        let mut valid = client(good.clone());
        let result = valid
            .get_effective_netclass_rules("main.kicad_pcb", &["GND".into()])
            .unwrap();
        assert_eq!(result["GND"].preferred_track_width_nm, Some(400_000));
        assert_eq!(result["GND"].clearance_nm, Some(200_000));
        assert_eq!(valid.transport.requests.len(), 3);
        let mut moved_document = board_document("main.kicad_pcb");
        moved_document.project = MessageField::some(base_types::ProjectSpecifier {
            name: "main".into(),
            path: "/other/project".into(),
            ..Default::default()
        });
        let moved = response(
            "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse",
            editor_commands::GetOpenDocumentsResponse {
                documents: vec![moved_document],
                ..Default::default()
            },
            "same",
        );
        for (index, replacement) in [
            (2, moved),
            (0, board("other.kicad_pcb", "same")),
            (1, classes("OTHER", "same")),
            (1, classes("GND", "changed")),
            (2, board("other.kicad_pcb", "same")),
            (2, board("main.kicad_pcb", "changed")),
        ] {
            let mut replies = good.clone();
            replies[index] = replacement;
            let mut rejected = client(replies);
            assert!(
                rejected
                    .get_effective_netclass_rules("main.kicad_pcb", &["GND".into()])
                    .is_err()
            );
            assert_eq!(rejected.token, Some(InstanceToken::new("same").unwrap()));
        }
        let mut bounded = client(vec![]);
        assert!(
            bounded
                .get_effective_netclass_rules("main.kicad_pcb", &["GND".into(), "GND".into()])
                .is_err()
        );
        assert!(bounded.transport.requests.is_empty());
    }
    #[test]
    fn missing_settings_stay_absent_and_unknown_native_fields_remain_evidence() {
        let mut native = class();
        native.board = MessageField::none();
        native
            .special_fields
            .mut_unknown_fields()
            .add_varint(1000, 42);
        let result = observe(native.clone()).unwrap();
        assert!(result.preferred_track_width_nm.is_none() && result.clearance_nm.is_none());
        assert_eq!(
            project_settings::NetClass::parse_from_bytes(&result.native_payload).unwrap(),
            native
        );
        native.type_ = project_settings::NetClassType::NCT_UNKNOWN.into();
        assert!(observe(native).is_err());
    }
}
