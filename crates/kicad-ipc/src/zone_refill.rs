//! Fixed version-bound refill dispatch with read-only access after an uncertain result.
use crate::*;

/// An action response, not proof that copper was successfully filled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZoneFillDispatchReceipt {
    instance_token: InstanceToken,
    board_filename: String,
}
impl ZoneFillDispatchReceipt {
    pub fn instance_token(&self) -> &InstanceToken {
        &self.instance_token
    }
    pub fn board_filename(&self) -> &str {
        &self.board_filename
    }
}

/// Owns the uncertain client without exposing mutating operations or a retry entry point.
pub struct UncertainZoneFill<T> {
    client: KiCadClient<T>,
    filename: String,
    reason: String,
}
impl<T: KiCadTransport> UncertainZoneFill<T> {
    pub fn reason(&self) -> &str {
        &self.reason
    }
    pub fn observe_geometry(&mut self) -> Result<NativeBoardGeometry, IpcError> {
        self.client.capture_board_geometry(&self.filename)
    }
    pub fn observe_live_text(&mut self) -> Result<String, IpcError> {
        self.client.live_board_text(&self.filename)
    }
}

pub enum ZoneFillAttempt<T> {
    /// No action request was sent. Prior definition commits still require their own outcome classification.
    NotDispatched {
        client: KiCadClient<T>,
        reason: String,
    },
    /// Dispatch returned; complete independent verification is mandatory before reporting success.
    Returned {
        client: KiCadClient<T>,
        receipt: ZoneFillDispatchReceipt,
    },
    /// The action may still be running. No automatic retry, save, reload or rollback is available.
    Uncertain(UncertainZoneFill<T>),
}

impl<T: KiCadTransport> KiCadClient<T> {
    /// Consume the client so an uncertain refill cannot accidentally be followed by writes. This fixed action is available only after this client's native transaction has ended.
    pub fn request_zone_refill(mut self, filename: &str) -> ZoneFillAttempt<T> {
        let preflight = (|| {
            if self.native_commit_open {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "refill requires an ended native transaction",
                ));
            }
            if self.config.timeout.is_zero() || self.config.timeout > Duration::from_secs(60) {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "refill transport timeout must be bounded by 60 seconds",
                ));
            }
            let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
            let document = self.require_current_board(&token, filename)?;
            let version: base_commands::GetVersionResponse = self.bound_item_command(
                GET_VERSION_TYPE_URL,
                "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
                base_commands::GetVersion::default(),
            )?;
            if version.version.as_ref().is_none_or(|v| {
                v.full_version != "10.0.7" || (v.major, v.minor, v.patch) != (10, 0, 7)
            }) {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "refill requires exact KiCad 10.0.7",
                ));
            }
            if self.require_current_board(&token, filename)? != document {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "refill document changed",
                ));
            }
            Ok((token, document))
        })();
        let (token, document) = match preflight {
            Ok(value) => value,
            Err(error) => {
                return ZoneFillAttempt::NotDispatched {
                    client: self,
                    reason: error.to_string(),
                };
            }
        };
        let action = (|| {
            let response: editor_commands::RunActionResponse = self.bound_item_command(
                "type.googleapis.com/kiapi.common.commands.RunAction",
                "type.googleapis.com/kiapi.common.commands.RunActionResponse",
                editor_commands::RunAction {
                    action: "pcbnew.ZoneFiller.zoneFillAll".into(),
                    ..Default::default()
                },
            )?;
            if response.status.enum_value() != Ok(editor_commands::RunActionStatus::RAS_OK) {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "zone fill dispatch did not return RAS_OK",
                ));
            }
            if self.require_current_board(&token, filename)? != document {
                return Err(IpcError::InvalidObservedBoardGraphic(
                    "document changed during fill action",
                ));
            }
            Ok(())
        })();
        match action {
            Ok(()) => ZoneFillAttempt::Returned {
                client: self,
                receipt: ZoneFillDispatchReceipt {
                    instance_token: token,
                    board_filename: filename.into(),
                },
            },
            Err(error) => ZoneFillAttempt::Uncertain(UncertainZoneFill {
                client: self,
                filename: filename.into(),
                reason: error.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct Wire {
        mode: u8,
        actions: Rc<RefCell<usize>>,
    }
    fn reply<M: Message>(kind: &str, value: M) -> ApiResponse {
        ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: "same".into(),
                ..Default::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: ApiStatusCode::AS_OK.into(),
                ..Default::default()
            }),
            message: MessageField::some(Any {
                type_url: kind.into(),
                value: value.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    impl KiCadTransport for Wire {
        fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
            match request.message.as_ref().unwrap().type_url.as_str() {
                GET_OPEN_DOCUMENTS_TYPE_URL => Ok(reply(
                    "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse",
                    editor_commands::GetOpenDocumentsResponse {
                        documents: vec![board_document("board.kicad_pcb")],
                        ..Default::default()
                    },
                )),
                GET_VERSION_TYPE_URL => Ok(reply(
                    "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
                    base_commands::GetVersionResponse {
                        version: MessageField::some(base_types::KiCadVersion {
                            major: 10,
                            minor: 0,
                            patch: if self.mode == 3 { 6 } else { 7 },
                            full_version: if self.mode == 3 { "10.0.6" } else { "10.0.7" }.into(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                "type.googleapis.com/kiapi.common.commands.RunAction" => {
                    *self.actions.borrow_mut() += 1;
                    let action = editor_commands::RunAction::parse_from_bytes(
                        &request.message.unwrap().value,
                    )
                    .unwrap();
                    assert_eq!(action.action, "pcbnew.ZoneFiller.zoneFillAll");
                    if self.mode == 1 {
                        return Err(IpcError::Transport(nng::Error::TimedOut));
                    }
                    Ok(reply(
                        "type.googleapis.com/kiapi.common.commands.RunActionResponse",
                        editor_commands::RunActionResponse {
                            status: if self.mode == 2 {
                                editor_commands::RunActionStatus::RAS_INVALID
                            } else {
                                editor_commands::RunActionStatus::RAS_OK
                            }
                            .into(),
                            ..Default::default()
                        },
                    ))
                }
                GET_ITEMS_TYPE_URL => Err(IpcError::Status(
                    ApiStatusCode::AS_BUSY,
                    "still executing".into(),
                )),
                _ => panic!("unexpected command"),
            }
        }
    }
    #[test]
    fn dispatch_is_version_bound_and_uncertain_clients_allow_only_observation() {
        for mode in 0..5 {
            let actions = Rc::new(RefCell::new(0));
            let mut client = KiCadClient::new(
                Wire {
                    mode,
                    actions: actions.clone(),
                },
                ClientConfig::new("unused", "test").unwrap(),
            );
            client.token = Some(InstanceToken::new("same").unwrap());
            client.native_commit_open = mode == 4;
            match client.request_zone_refill("board.kicad_pcb") {
                ZoneFillAttempt::Returned { receipt, .. } => {
                    assert_eq!(mode, 0);
                    assert_eq!(receipt.board_filename(), "board.kicad_pcb");
                }
                ZoneFillAttempt::NotDispatched { .. } => {
                    assert!(mode >= 3);
                    assert_eq!(*actions.borrow(), 0);
                }
                ZoneFillAttempt::Uncertain(mut reader) => {
                    assert!(matches!(mode, 1 | 2));
                    assert!(reader.observe_geometry().is_err());
                    assert_eq!(*actions.borrow(), 1);
                }
            }
        }
    }
    #[test]
    fn production_transport_disables_automatic_resend() {
        let address = format!(
            "inproc://no-refill-retry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        let server = Socket::new(Protocol::Rep0).unwrap();
        server.listen(&address).unwrap();
        let transport = NngTransport::connect(ClientConfig::new(address, "test").unwrap()).unwrap();
        assert_eq!(
            transport
                .socket
                .get_opt::<nng::options::protocol::reqrep::ResendTime>()
                .unwrap(),
            None
        );
    }
}
