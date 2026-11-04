use std::path::PathBuf;

use crate::presentation::state::agent_session::{ForkPosition, ThinkingLevel, UiAnswerView};
use crate::presentation::state::model::{
    ApprovalId, Basis, CandidateId, DeliveryMode, DraftId, GitId, Intent as ObjectiveIntent,
    ModelRef, ObjectiveId, Reference, RepoPath, SearchScope, SessionId, Side, WorkspaceId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartPoint {
    Target,
    Commit(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftRevisionRef {
    pub draft_id: DraftId,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    Exit,
    Logout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    RetryConnection,
    OpenWorkspace {
        path: PathBuf,
    },
    LoadObjective {
        objective_id: ObjectiveId,
    },
    LoadActivity {
        objective_id: ObjectiveId,
        before: Option<u64>,
    },
    LoadChanges {
        objective_id: ObjectiveId,
        basis: Basis,
    },
    LoadDiff {
        objective_id: ObjectiveId,
        basis: Basis,
        path: RepoPath,
    },
    OpenFile {
        objective_id: ObjectiveId,
        basis: Basis,
        path: RepoPath,
        side: Side,
        offset: u64,
    },
    LoadDrafts {
        objective_id: ObjectiveId,
    },
    LoadInputs {
        objective_id: ObjectiveId,
    },
    Search {
        workspace_id: WorkspaceId,
        scope: SearchScope,
        objective_id: Option<ObjectiveId>,
        basis: Option<Basis>,
        query: String,
    },
    CreateObjective {
        workspace_id: WorkspaceId,
        title: String,
        intent: ObjectiveIntent,
        integration_target: String,
        start: StartPoint,
    },
    UpdateObjective {
        objective_id: ObjectiveId,
        expected_intent_revision: u64,
        intent: ObjectiveIntent,
    },
    StartExecution {
        objective_id: ObjectiveId,
        model: ModelRef,
        thinking: Option<ThinkingLevel>,
    },
    Cancel {
        objective_id: ObjectiveId,
    },
    Resume {
        objective_id: ObjectiveId,
    },
    SendMessage {
        objective_id: ObjectiveId,
        text: String,
        mode: DeliveryMode,
    },
    SaveDraft {
        objective_id: ObjectiveId,
        draft_id: DraftId,
        expected_revision: u64,
        reference: Reference,
        body: String,
    },
    DeleteDraft {
        draft_id: DraftId,
        expected_revision: u64,
    },
    SubmitFeedback {
        objective_id: ObjectiveId,
        destination_session_id: SessionId,
        association_revision: u64,
        drafts: Vec<DraftRevisionRef>,
        mode: DeliveryMode,
        note: Option<String>,
    },
    Freeze {
        objective_id: ObjectiveId,
    },
    ReturnForRevision {
        objective_id: ObjectiveId,
        candidate_id: CandidateId,
    },
    Verify {
        objective_id: ObjectiveId,
        candidate_id: CandidateId,
        argv: Vec<String>,
    },
    Approve {
        objective_id: ObjectiveId,
        candidate_id: CandidateId,
        target_commit: GitId,
        override_reason: Option<String>,
    },
    Refresh {
        objective_id: ObjectiveId,
        candidate_id: CandidateId,
    },
    Integrate {
        objective_id: ObjectiveId,
        approval_id: ApprovalId,
    },
    Complete {
        objective_id: ObjectiveId,
    },
    Discard {
        objective_id: ObjectiveId,
    },
    Cleanup {
        objective_id: ObjectiveId,
        confirm_data_loss: bool,
    },
    DeleteHistory {
        objective_id: ObjectiveId,
    },
    UpdateSettings {
        default_model: ModelRef,
    },
    LoadModels {
        provider: Option<String>,
    },
    LoadSessionTree {
        objective_id: ObjectiveId,
    },
    NavigateSession {
        objective_id: ObjectiveId,
        entry_id: String,
    },
    ForkSession {
        objective_id: ObjectiveId,
        entry_id: String,
        position: ForkPosition,
    },
    ImportSession {
        objective_id: ObjectiveId,
        path: PathBuf,
    },
    ExportSession {
        objective_id: ObjectiveId,
    },
    CompactSession {
        objective_id: ObjectiveId,
    },
    ReloadSession {
        objective_id: ObjectiveId,
    },
    ConfigureSession {
        objective_id: ObjectiveId,
        model: ModelRef,
        thinking: ThinkingLevel,
    },
    LoadResources,
    DecideResource {
        name: String,
        sha256: String,
        approve: bool,
    },
    ImportProfile {
        source: PathBuf,
    },
    RespondDialog {
        objective_id: ObjectiveId,
        ui_id: String,
        generation: u64,
        answer: UiAnswerView,
    },
    OpenTerminal {
        objective_id: ObjectiveId,
        cols: u16,
        rows: u16,
    },
    TerminalInput {
        objective_id: ObjectiveId,
        terminal_id: String,
        data: String,
    },
    ResizeTerminal {
        objective_id: ObjectiveId,
        terminal_id: String,
        cols: u16,
        rows: u16,
    },
    CloseTerminal {
        objective_id: ObjectiveId,
        terminal_id: String,
    },
    StopService {
        reason: StopReason,
    },
}

impl Intent {
    pub fn label(&self) -> &'static str {
        match self {
            Self::RetryConnection => "Retry connection",
            Self::OpenWorkspace { .. } => "Open workspace",
            Self::LoadObjective { .. } => "Load Objective",
            Self::LoadActivity { .. } => "Load activity",
            Self::LoadChanges { .. } => "Load Change Set",
            Self::LoadDiff { .. } => "Load diff",
            Self::OpenFile { .. } => "Open file",
            Self::LoadDrafts { .. } => "Load drafts",
            Self::LoadInputs { .. } => "Load inputs",
            Self::Search { .. } => "Search",
            Self::CreateObjective { .. } => "Create Objective",
            Self::UpdateObjective { .. } => "Edit intent",
            Self::StartExecution { .. } => "Start agent",
            Self::Cancel { .. } => "Cancel",
            Self::Resume { .. } => "Resume",
            Self::SendMessage { .. } => "Send message",
            Self::SaveDraft { .. } => "Save draft",
            Self::DeleteDraft { .. } => "Delete draft",
            Self::SubmitFeedback { .. } => "Submit feedback",
            Self::Freeze { .. } => "Freeze for review",
            Self::ReturnForRevision { .. } => "Return for revision",
            Self::Verify { .. } => "Run check",
            Self::Approve { .. } => "Approve",
            Self::Refresh { .. } => "Refresh against target",
            Self::Integrate { .. } => "Integrate",
            Self::Complete { .. } => "Complete without changes",
            Self::Discard { .. } => "Discard",
            Self::Cleanup { .. } => "Clean up",
            Self::DeleteHistory { .. } => "Delete Git history",
            Self::UpdateSettings { .. } => "Save default model",
            Self::LoadModels { .. } => "Load models",
            Self::LoadSessionTree { .. } => "Load session tree",
            Self::NavigateSession { .. } => "Go to session entry",
            Self::ForkSession {
                position: ForkPosition::Before,
                ..
            } => "Fork session",
            Self::ForkSession {
                position: ForkPosition::At,
                ..
            } => "Clone session",
            Self::ImportSession { .. } => "Import session",
            Self::ExportSession { .. } => "Export session",
            Self::CompactSession { .. } => "Compact context",
            Self::ReloadSession { .. } => "Reload session resources",
            Self::ConfigureSession { .. } => "Change model and thinking",
            Self::LoadResources => "Load profile resources",
            Self::DecideResource { approve: true, .. } => "Approve extension",
            Self::DecideResource { approve: false, .. } => "Reject extension",
            Self::ImportProfile { .. } => "Import pi profile",
            Self::RespondDialog { .. } => "Answer extension dialog",
            Self::OpenTerminal { .. } => "Open terminal",
            Self::TerminalInput { .. } => "Send terminal input",
            Self::ResizeTerminal { .. } => "Resize terminal",
            Self::CloseTerminal { .. } => "Close terminal",
            Self::StopService { .. } => "Exit and stop the service",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::OpenTerminal { .. }
                | Self::TerminalInput { .. }
                | Self::ResizeTerminal { .. }
                | Self::CloseTerminal { .. }
        )
    }

    pub fn terminal_objective(&self) -> Option<&ObjectiveId> {
        match self {
            Self::OpenTerminal { objective_id, .. }
            | Self::TerminalInput { objective_id, .. }
            | Self::ResizeTerminal { objective_id, .. }
            | Self::CloseTerminal { objective_id, .. } => Some(objective_id),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::LoadObjective { .. }
                | Self::LoadActivity { .. }
                | Self::LoadChanges { .. }
                | Self::LoadDiff { .. }
                | Self::OpenFile { .. }
                | Self::LoadDrafts { .. }
                | Self::LoadInputs { .. }
                | Self::Search { .. }
                | Self::LoadModels { .. }
                | Self::LoadSessionTree { .. }
                | Self::LoadResources
        )
    }
}

pub trait IntentSink {
    fn submit(&self, intent: Intent);
}

#[cfg(test)]
#[derive(Default)]
pub struct RecordingSink {
    pub intents: std::cell::RefCell<Vec<Intent>>,
}

#[cfg(test)]
impl IntentSink for RecordingSink {
    fn submit(&self, intent: Intent) {
        self.intents.borrow_mut().push(intent);
    }
}
