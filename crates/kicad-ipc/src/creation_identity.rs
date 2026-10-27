//! Explicit identities for server-planned copper creation.
use super::*;
use protobuf::Message;

fn validate(id: &str) -> Result<(), IpcError> {
    let bytes = id.as_bytes();
    if bytes.len() != 36
        || !bytes.iter().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                *b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(b)
            }
        })
        || id == "00000000-0000-0000-0000-000000000000"
    {
        return Err(IpcError::InvalidObservedRouting(
            "invalid explicit creation UUID",
        ));
    }
    Ok(())
}

impl<T: KiCadTransport> KiCadClient<T> {
    /// Create one track with an exact identity; callers must preflight the complete namespace.
    pub fn create_track_with_identity(
        &mut self,
        filename: &str,
        track: &CreateTrack,
        id: &str,
    ) -> Result<CreatedTrackReceipt, IpcError> {
        validate(id)?;
        let mut item = create_track_item(track)?;
        let mut payload = board_types::Track::parse_from_bytes(&item.value)
            .map_err(|_| IpcError::MalformedEnvelope)?;
        payload.id = Some(base_types::KIID {
            value: id.into(),
            ..Default::default()
        })
        .into();
        item.value = payload
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?;
        let response = self.create_items(filename, vec![item])?;
        let receipt = created_track_receipt(&response, track, track.net.as_ref())?;
        if receipt.source_uuid != id {
            return Err(IpcError::InvalidObservedRouting(
                "created track UUID differs from intent",
            ));
        }
        self.token = Some(response.1);
        Ok(receipt)
    }

    /// Create one through via with an exact identity; no anonymous fallback is permitted.
    pub fn create_through_via_with_identity(
        &mut self,
        filename: &str,
        via: &CreateThroughVia,
        id: &str,
    ) -> Result<CreatedThroughViaReceipt, IpcError> {
        validate(id)?;
        let mut item = create_through_via_item(via)?;
        let mut payload = board_types::Via::parse_from_bytes(&item.value)
            .map_err(|_| IpcError::MalformedEnvelope)?;
        payload.id = Some(base_types::KIID {
            value: id.into(),
            ..Default::default()
        })
        .into();
        item.value = payload
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?;
        let response = self.create_items(filename, vec![item])?;
        let receipt = created_through_via_receipt(&response, via, via.net.as_ref())?;
        if receipt.source_uuid != id {
            return Err(IpcError::InvalidObservedRouting(
                "created via UUID differs from intent",
            ));
        }
        self.token = Some(response.1);
        Ok(receipt)
    }
}
