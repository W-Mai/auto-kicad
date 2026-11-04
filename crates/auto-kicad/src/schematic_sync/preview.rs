//! Session-owned alternatives for exact board-only and project/board approval.
use super::*;
use crate::recovery::{ProjectWriter, SynchronizationPublicationFailure};
use kicad_adapter::schematic_audit::synchronization::{
    MatchedSynchronizationProjectBoardPreview, SynchronizationIntentPreview,
    SynchronizationProjectBoardExpectation, SynchronizationProjectBoardPreview,
    board::SynchronizationDifference,
};

/// Existing board expectation JSON remains unchanged; project expectations add required fields.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Expectation {
    Project(SynchronizationProjectBoardExpectation),
    Board(SynchronizationExpectation),
}

pub(super) enum Preview {
    Board(Arc<SynchronizationBoardPreview>),
    Project(Arc<SynchronizationProjectBoardPreview>),
}
pub(super) enum Matched {
    Board(MatchedSynchronizationPreview),
    Project(MatchedSynchronizationProjectBoardPreview),
}
impl From<SynchronizationIntentPreview> for Preview {
    fn from(value: SynchronizationIntentPreview) -> Self {
        match value {
            SynchronizationIntentPreview::Board(p) => Self::Board(p),
            SynchronizationIntentPreview::Project(p) => Self::Project(p),
        }
    }
}
impl Preview {
    pub(super) fn verify(&self) -> Result<(), String> {
        match self {
            Self::Board(p) => p.verify(),
            Self::Project(p) => p.verify(),
        }
    }
    pub(super) fn digest(&self) -> &str {
        match self {
            Self::Board(p) => p.digest(),
            Self::Project(p) => p.digest(),
        }
    }
    pub(super) fn evidence(&self) -> &Value {
        match self {
            Self::Board(p) => p.evidence(),
            Self::Project(p) => p.evidence(),
        }
    }
    pub(super) fn differences(&self) -> &[SynchronizationDifference] {
        match self {
            Self::Board(p) => p.candidate().differences(),
            Self::Project(p) => p.board_candidate().differences(),
        }
    }
    pub(super) fn drc(&self) -> &kicad_checks::CompleteDrcDelta {
        match self {
            Self::Board(p) => p.drc_delta(),
            Self::Project(p) => p.drc_delta(),
        }
    }
    pub(super) fn has_changes(&self) -> bool {
        match self {
            Self::Board(p) => p.candidate().has_changes(),
            Self::Project(p) => {
                p.board_candidate().has_changes() || p.project_candidate().has_changes()
            }
        }
    }
    pub(super) fn project_data(&self) -> Option<Value> {
        match self {
            Self::Board(_) => None,
            Self::Project(p) => Some(json!({
            "project_differences":p.project_candidate().differences(),"netclass_delta":p.netclass_delta()})),
        }
    }
    pub(super) fn match_expectation(&self, expectation: Expectation) -> Result<Matched, String> {
        match (self, expectation) {
            (Self::Board(p), Expectation::Board(e)) => p.match_expectation(e).map(Matched::Board),
            (Self::Project(p), Expectation::Project(e)) => {
                p.match_expectation(e).map(Matched::Project)
            }
            _ => Err("synchronization expectation does not match the plan publication kind".into()),
        }
    }
}
impl Matched {
    pub(super) fn preview(&self) -> Preview {
        match self {
            Self::Board(p) => Preview::Board(p.preview().clone()),
            Self::Project(p) => Preview::Project(p.preview().clone()),
        }
    }
    pub(super) fn digest(&self) -> &str {
        match self {
            Self::Board(p) => p.digest(),
            Self::Project(p) => p.digest(),
        }
    }
    pub(super) fn expectation(&self) -> Value {
        match self {
            Self::Board(p) => json!(p.expectation()),
            Self::Project(p) => json!(p.expectation()),
        }
    }
    pub(super) fn publish(
        self,
        writer: &ProjectWriter,
        hash: &str,
        approval: &str,
    ) -> Result<Value, SynchronizationPublicationFailure> {
        match self {
            Self::Board(m) => writer
                .publish_matched_synchronization(hash, approval, m)
                .map(|p| json!(p)),
            Self::Project(m) => writer
                .publish_matched_project_synchronization(hash, approval, m)
                .map(|p| json!(p)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expectation_alternatives_keep_legacy_json_and_require_complete_project_records() {
        let board = json!({"differences":[],"drc":{"introduced":[],"resolved":[]}});
        let parsed: Expectation = serde_json::from_value(board.clone()).unwrap();
        assert!(matches!(parsed, Expectation::Board(_)));
        assert_eq!(serde_json::to_value(parsed).unwrap(), board);
        let project = json!({"differences":[],"project_differences":[],
            "netclasses":{"source_nets":{},"configured":{},"effective":{}},
            "drc":{"introduced":[],"resolved":[]}});
        let parsed: Expectation = serde_json::from_value(project.clone()).unwrap();
        assert!(matches!(parsed, Expectation::Project(_)));
        assert_eq!(serde_json::to_value(parsed).unwrap(), project);
        for key in ["source_nets", "configured", "effective"] {
            let mut missing = project.clone();
            missing["netclasses"].as_object_mut().unwrap().remove(key);
            assert!(serde_json::from_value::<Expectation>(missing).is_err());
        }
        for key in ["project_bytes", "proof", "allowed_nets"] {
            let mut extra = project.clone();
            extra[key] = json!({});
            assert!(serde_json::from_value::<Expectation>(extra).is_err());
        }
        let schema = serde_json::to_value(schemars::schema_for!(Expectation)).unwrap();
        assert_eq!(schema["anyOf"].as_array().unwrap().len(), 2);
        for name in [
            "SynchronizationExpectation",
            "SynchronizationProjectBoardExpectation",
            "ProjectNetclassDelta",
            "ProjectNetclassDifference",
            "ConfiguredClass",
            "EffectiveClass",
            "ClassDimension",
            "ClassText",
            "ClassIdentity",
        ] {
            assert_eq!(
                schema["$defs"][name]["additionalProperties"], false,
                "{name}"
            );
        }
    }

    #[test]
    fn class_inspection_is_opt_in_and_every_project_action_is_a_closed_shape() {
        let raw = r#"{"kind":"observe","project_root":"/project","root_schematic":"/project/root.kicad_sch"}"#;
        let inspect: Inspection = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&inspect).unwrap(), raw);
        let mut requested: Value = serde_json::from_str(raw).unwrap();
        requested["project_netclasses"] = true.into();
        assert!(serde_json::from_value::<Inspection>(requested).is_ok());
        let schema = serde_json::to_value(schemars::schema_for!(Request)).unwrap();
        let actions = schema["$defs"]["ProjectNetclassChange"]["oneOf"]
            .as_array()
            .unwrap();
        assert_eq!(actions.len(), 7);
        for action in actions {
            assert_eq!(action["additionalProperties"], false);
        }
        let required = schema["$defs"]["SynchronizationIntent"]["required"]
            .as_array()
            .unwrap();
        assert!(!required.iter().any(|name| name == "project_netclasses"));
    }
}
