//! Combine exact definitions and filled copper with an independently refilled saved copy.
use crate::live_geometry::{
    FilledCopperEvidence, LiveBoardText, LiveGeometryEvidence, RefillDefinitionEvidence,
};
use kicad_checks::refill::ClosedRefillResult;
use serde::Serialize;

/// Content-equivalence evidence. Native dispatch completion, pad connectivity, DRC delta and approval remain independent requirements.
#[derive(Clone, Debug, Serialize)]
pub struct VerifiedRefillContent {
    definitions: RefillDefinitionEvidence,
    copper: FilledCopperEvidence,
    saved_input_sha256: String,
    independent_output_sha256: String,
}
impl VerifiedRefillContent {
    /// Both live observations must be bound by the caller to the same editor/document before and after fill. Saved input must also pass the caller's save/reload identity verification.
    pub fn verify(
        before: &LiveGeometryEvidence,
        after: &LiveGeometryEvidence,
        independent: &ClosedRefillResult,
    ) -> Result<Self, String> {
        let definitions = RefillDefinitionEvidence::from_observation(before)?;
        if definitions != RefillDefinitionEvidence::from_observation(after)? {
            return Err("refill changed complete live definitions".into());
        }
        let saved = LiveBoardText::parse(independent.input_board())?;
        let refilled = LiveBoardText::parse(independent.output_board())?;
        if saved.closed_refill_definitions()? != refilled.closed_refill_definitions()? {
            return Err("independent refill changed saved definitions".into());
        }
        let copper = after.text.filled_copper()?;
        if copper != saved.filled_copper()? || copper != refilled.filled_copper()? {
            return Err("live, saved and independently refilled copper differ".into());
        }
        Ok(Self {
            definitions,
            copper,
            saved_input_sha256: independent.input_sha256().into(),
            independent_output_sha256: independent.output_sha256().into(),
        })
    }
}
