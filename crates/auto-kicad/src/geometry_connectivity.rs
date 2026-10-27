//! Immutable report evidence tied to native connectivity and the exact DRC inputs.
use crate::geometry_drc::PrivateDrcEvidence;
use kicad_adapter::{
    LiveBoardObservation, copper_connectivity::VerifiedCopperConnectivity,
    geometry::GeometryObservation,
};
use kicad_checks::{CompleteDrcReport, DrcPolicy, connectivity::ClosedConnectivityEvidence};
use kicad_ipc::{KiCadClient, NngTransport};
use kicad_model::RevisionDigest;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize)]
pub struct ReportConnectivity {
    live: VerifiedCopperConnectivity,
    drc_input: ClosedConnectivityEvidence,
    report_digest: RevisionDigest,
}

pub(super) fn capture(
    client: &mut KiCadClient<NngTransport>,
    source: &LiveBoardObservation,
    observed: &GeometryObservation,
    policy: &DrcPolicy,
) -> Result<VerifiedCopperConnectivity, String> {
    let saved =
        crate::geometry_drc::read(&source.board_path, true)?.ok_or("missing connectivity PCB")?;
    let text = std::str::from_utf8(&saved).map_err(|e| e.to_string())?;
    VerifiedCopperConnectivity::capture(
        client,
        &source.board_filename,
        observed,
        text,
        &source.board_path,
        policy,
    )
}

impl ReportConnectivity {
    pub(super) fn bind(
        live: VerifiedCopperConnectivity,
        report: &CompleteDrcReport,
        private: Option<&PrivateDrcEvidence>,
    ) -> Result<Self, String> {
        let drc_input = match private {
            Some(private) => private.connectivity_for(&live)?.clone(),
            None => live.closed().clone(),
        };
        if drc_input.endpoints() != live.closed().endpoints()
            || drc_input.components() != live.closed().components()
        {
            return Err("DRC input physical components differ from the bound native editor".into());
        }
        Ok(Self {
            live,
            drc_input,
            report_digest: report_digest(report)?,
        })
    }
}

fn report_digest(report: &CompleteDrcReport) -> Result<RevisionDigest, String> {
    Ok(crate::digest_bytes(
        &serde_json::to_vec(report).map_err(|e| e.to_string())?,
    ))
}

pub(super) fn equivalent(
    before: &CompleteDrcReport,
    after: &CompleteDrcReport,
    identities: &BTreeMap<String, String>,
    before_proof: Option<&ReportConnectivity>,
    after_proof: Option<&ReportConnectivity>,
) -> Result<bool, String> {
    match (before_proof, after_proof) {
        (None, None) => before
            .equivalent_to(after, identities)
            .map_err(|e| e.to_string()),
        (Some(a), Some(b)) => {
            if a.report_digest != report_digest(before)? || b.report_digest != report_digest(after)?
            {
                return Err(
                    "connectivity evidence belongs to a different complete DRC report".into(),
                );
            }
            before
                .equivalent_with_closed_connectivity(after, identities, &a.drc_input, &b.drc_input)
                .map_err(|e| e.to_string())
        }
        _ => Err("approved and current DRC use different connectivity evidence contracts".into()),
    }
}
