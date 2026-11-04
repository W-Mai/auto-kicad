//! Typed intent dispatch; project authority never enters ordinary board-only compilation.
use super::*;

pub enum SynchronizationIntentPreview {
    Board(Arc<SynchronizationBoardPreview>),
    Project(Arc<SynchronizationProjectBoardPreview>),
}

impl SynchronizationBoardObservation {
    pub fn preview_intent(
        self: &Arc<Self>,
        intent: &SynchronizationIntent,
    ) -> Result<SynchronizationIntentPreview, String> {
        if intent.project_netclasses.is_empty() {
            return self
                .preview(intent)
                .map(SynchronizationIntentPreview::Board);
        }
        if intent.project_netclasses.len() > 128 {
            return Err("project netclass action bound exceeded".into());
        }
        let mut board = intent.clone();
        board.project_netclasses.clear();
        board.validate_board(true)?;
        let project = self.preview_project_netclasses(&intent.project_netclasses)?;
        if !project.candidate.has_changes() {
            // No project bytes are authorized. Keep the ordinary endpoint-class contract.
            let preview = self.preview_candidate(&board, false, true)?;
            project.verify()?;
            return Ok(SynchronizationIntentPreview::Board(preview));
        }
        self.compose_project(&board, project)
            .map(SynchronizationIntentPreview::Project)
    }
}
