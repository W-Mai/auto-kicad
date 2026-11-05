pub mod owned_temp;
use std::fmt::{Display, Formatter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub transport: Transport,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            transport: Transport::Stdio,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transport {
    Stdio,
}

impl Display for Transport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio => formatter.write_str("stdio"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorrelationId(String);

impl CorrelationId {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RuntimeError::InvalidCorrelationId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    InvalidCorrelationId,
    ConnectionUnavailable,
    ProtocolDecodeFailed,
    KiCadStatusError,
    UnsupportedCapability,
    TargetNotFound,
    TargetAmbiguous,
    PreconditionsFailed,
    RevisionStale,
    ValidationFailed,
    ApprovalRequired,
    PolicyDenied,
    SnapshotFailed,
    CommitFailed,
}

impl RuntimeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidCorrelationId => "invalid_correlation_id",
            Self::ConnectionUnavailable => "connection_unavailable",
            Self::ProtocolDecodeFailed => "protocol_decode_failed",
            Self::KiCadStatusError => "kicad_status_error",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::TargetNotFound => "target_not_found",
            Self::TargetAmbiguous => "target_ambiguous",
            Self::PreconditionsFailed => "precondition_failed",
            Self::RevisionStale => "revision_stale",
            Self::ValidationFailed => "validation_failed",
            Self::ApprovalRequired => "approval_required",
            Self::PolicyDenied => "policy_denied",
            Self::SnapshotFailed => "snapshot_failed",
            Self::CommitFailed => "commit_failed",
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for RuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_uses_stdio() {
        assert_eq!(RuntimeConfig::default().transport, Transport::Stdio);
    }

    #[test]
    fn correlation_id_rejects_empty_values() {
        assert_eq!(
            CorrelationId::new(""),
            Err(RuntimeError::InvalidCorrelationId)
        );
    }

    #[test]
    fn errors_expose_stable_codes() {
        assert_eq!(RuntimeError::RevisionStale.code(), "revision_stale");
    }
}
