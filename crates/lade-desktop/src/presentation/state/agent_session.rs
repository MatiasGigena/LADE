use std::collections::{HashMap, HashSet};

use super::model::{ModelRef, ObjectiveId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [Self; 7] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "Extra high",
            Self::Max => "Maximum",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderStatus {
    Available,
    MissingCredentials,
    Restricted,
}

impl ProviderStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::MissingCredentials => "needs credentials",
            Self::Restricted => "restricted",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub auth: Vec<String>,
    pub status: ProviderStatus,
    pub reason: Option<String>,
    pub variables: Vec<String>,
    pub models: u64,
}

impl ProviderInfo {
    pub fn picker_label(&self) -> String {
        if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        }
    }

    pub fn subtitle(&self) -> String {
        let auth = if self.auth.is_empty() {
            "no authentication".to_owned()
        } else {
            self.auth.join(", ")
        };
        format!(
            "{} · {} · {auth} · {} model{}",
            self.id,
            self.status.label(),
            self.models,
            if self.models == 1 { "" } else { "s" }
        )
    }

    pub fn credential_help(&self) -> Option<String> {
        if self.status != ProviderStatus::MissingCredentials || self.variables.is_empty() {
            return None;
        }
        let id = &self.id;
        let (variables, keyring) = if self.auth.iter().any(|a| a == "custom provider") {
            (
                self.variables.join(" and "),
                format!("laded credentials set {id} --variable NAME"),
            )
        } else {
            (
                self.variables.join(" or "),
                format!("laded credentials set {id}"),
            )
        };
        Some(format!(
            "Needs {variables}.\n\
             Keyring: {keyring}\n\
             File: ~/.config/lade/credentials.env (mode 0600), one NAME=value per line\n\
             laded's environment works too. Keys are read when the agent starts."
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub thinking: Vec<ThinkingLevel>,
}

impl ModelInfo {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(self.provider.clone(), self.id.clone())
    }

    pub fn picker_label(&self) -> String {
        if self.reasoning {
            format!("{} (reasoning)", self.name)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub sdk_version: String,
    pub providers: Vec<ProviderInfo>,
    pub error: Option<String>,
}

impl Catalog {
    pub fn provider(&self, id: &str) -> Option<&ProviderInfo> {
        self.providers.iter().find(|p| p.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelChoice {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: ThinkingLevel,
}

/// Why the service reported a new Agent Session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionChangeReason {
    Start,
    Navigate,
    Fork,
    Import,
    Reload,
    Compact,
    Configure,
}

impl SessionChangeReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "started",
            Self::Navigate => "moved in the tree",
            Self::Fork => "forked",
            Self::Import => "imported",
            Self::Reload => "reloaded",
            Self::Compact => "compacted",
            Self::Configure => "reconfigured",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub generation: u64,
    pub reason: SessionChangeReason,
    pub model: ModelRef,
    pub thinking: ThinkingLevel,
    pub association_revision: Option<u64>,
    pub pi_session_id: Option<String>,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeEntryKind {
    User,
    Assistant,
    ToolResult,
    CustomMessage,
    Compaction,
    BranchSummary,
    ModelChange,
    ThinkingChange,
    Label,
    SessionInfo,
    Custom,
    Other,
}

impl TreeEntryKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "You",
            Self::Assistant => "Agent",
            Self::ToolResult => "Tool result",
            Self::CustomMessage => "Extension message",
            Self::Compaction => "Compaction",
            Self::BranchSummary => "Branch summary",
            Self::ModelChange => "Model change",
            Self::ThinkingChange => "Thinking change",
            Self::Label => "Bookmark",
            Self::SessionInfo => "Session name",
            Self::Custom => "Extension data",
            Self::Other => "Entry",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub entry_id: String,
    pub parent_id: Option<String>,
    pub kind: TreeEntryKind,
    pub label: String,
    pub bookmark: Option<String>,
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTree {
    pub entries: Vec<TreeNode>,
    pub leaf_id: Option<String>,
    pub truncated: bool,
}

impl SessionTree {
    pub fn ordered(entries: Vec<TreeNode>, leaf_id: Option<String>, truncated: bool) -> Self {
        let known: HashSet<String> = entries.iter().map(|e| e.entry_id.clone()).collect();
        let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let parent = entry.parent_id.clone().filter(|p| known.contains(p));
            children.entry(parent).or_default().push(index);
        }
        let mut ordered = Vec::with_capacity(entries.len());
        let mut entries: Vec<Option<TreeNode>> = entries.into_iter().map(Some).collect();
        let mut stack: Vec<(usize, usize)> = children
            .get(&None)
            .map(|roots| roots.iter().rev().map(|&i| (i, 0)).collect())
            .unwrap_or_default();
        while let Some((index, depth)) = stack.pop() {
            // Each entry has one parent, so it is listed, and taken, once.
            let Some(mut node) = entries[index].take() else {
                continue;
            };
            node.depth = depth;
            if let Some(kids) = children.get(&Some(node.entry_id.clone())) {
                stack.extend(kids.iter().rev().map(|&i| (i, depth + 1)));
            }
            ordered.push(node);
        }
        Self {
            entries: ordered,
            leaf_id,
            truncated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkPosition {
    Before,
    At,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    Skill,
    Prompt,
    Context,
    Extension,
    Models,
}

impl ResourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Skill => "Skill",
            Self::Prompt => "Prompt template",
            Self::Context => "Context file",
            Self::Extension => "Extension (executable)",
            Self::Models => "Custom providers",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceState {
    Active,
    PendingApproval,
    Rejected,
    Refused,
}

impl ResourceState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::PendingApproval => "needs approval",
            Self::Rejected => "rejected",
            Self::Refused => "refused",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceEntry {
    pub kind: ResourceKind,
    pub name: String,
    pub state: ResourceState,
    pub sha256: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Inventory {
    pub profile_dir: String,
    pub entries: Vec<ResourceEntry>,
    pub restrictions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiPrompt {
    Confirm { title: String, message: String },
    Select { title: String, options: Vec<String> },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}

impl UiPrompt {
    pub fn title(&self) -> &str {
        match self {
            Self::Confirm { title, .. }
            | Self::Select { title, .. }
            | Self::Input { title, .. }
            | Self::Editor { title, .. } => title,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiDialog {
    pub objective_id: ObjectiveId,
    pub ui_id: String,
    pub generation: u64,
    pub prompt: UiPrompt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAnswer {
    Confirm(bool),
    Value(String),
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    CatalogLoading,
    NoProviders {
        error: Option<String>,
    },
    NoProviderChosen,
    ModelsLoading {
        provider: String,
    },
    NoModels {
        provider: String,
    },
    NeedsCredentials {
        provider: String,
    },
    Restricted {
        provider: String,
    },
    Ready {
        model: ModelInfo,
        thinking: ThinkingLevel,
    },
}

impl Readiness {
    pub fn problem(&self) -> Option<String> {
        Some(match self {
            Self::CatalogLoading => "Loading the available models…".to_owned(),
            Self::NoProviders { error: Some(_) } => "LADE can't load models right now.".to_owned(),
            Self::NoProviders { error: None } => "No model providers are available.".to_owned(),
            Self::NoProviderChosen => "Choose a model provider.".to_owned(),
            Self::ModelsLoading { provider } => format!("Loading {provider}'s models…"),
            Self::NoModels { provider } => {
                format!("{provider} has no models available. Choose another provider.")
            }
            Self::NeedsCredentials { provider } => {
                format!("Add credentials for {provider} to start the agent.")
            }
            Self::Restricted { provider } => {
                format!("{provider} can't be used in LADE. Choose another provider.")
            }
            Self::Ready { .. } => return None,
        })
    }
}

/// What the model picker offers, decided from the Agent Session state alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPicker {
    /// The provider catalogue has not arrived yet.
    Loading,
    /// The service offers no provider; `error` says why when the service reported a failure.
    NoProviders {
        error: Option<String>,
    },
    Choices(PickerChoices),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerChoices {
    /// Why some providers may be missing from an otherwise usable catalogue.
    pub catalog_error: Option<String>,
    pub providers: Vec<ProviderInfo>,
    pub selected_provider: Option<usize>,
    /// The chosen provider's models, in dropdown order.
    pub models: Vec<ModelInfo>,
    pub model_row: ModelRow,
    pub choice: ModelChoice,
}

impl PickerChoices {
    pub fn provider(&self) -> Option<&ProviderInfo> {
        self.selected_provider.and_then(|i| self.providers.get(i))
    }
}

/// What the picker shows below the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelRow {
    /// A model is ready: offer the models and the thinking levels the chosen one supports.
    Ready {
        selected_model: Option<usize>,
        thinking: Vec<ThinkingLevel>,
        selected_thinking: Option<usize>,
    },
    /// The provider itself can't be used, and the provider row already explains why.
    Hidden,
    /// No model can be chosen yet, for this reason.
    Problem(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AgentSessionState {
    pub catalog: Option<Catalog>,
    pub models: std::collections::HashMap<String, Vec<ModelInfo>>,
    pub sessions: std::collections::HashMap<ObjectiveId, SessionState>,
    pub trees: std::collections::HashMap<ObjectiveId, SessionTree>,
    pub inventory: Option<Inventory>,
    pub dialogs: Vec<UiDialog>,
    pub exports: std::collections::HashMap<ObjectiveId, String>,
    pub choice: ModelChoice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSessionUpdate {
    Catalog(Catalog),
    Models {
        provider: String,
        models: Vec<ModelInfo>,
    },
    Session {
        objective_id: ObjectiveId,
        session: SessionState,
    },
    Tree {
        objective_id: ObjectiveId,
        tree: SessionTree,
    },
    Inventory(Inventory),
    DialogOpened(UiDialog),
    DialogClosed {
        ui_id: String,
    },
    Exported {
        objective_id: ObjectiveId,
        path: String,
    },
    Choice(ModelChoice),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentSessionChange {
    Catalog,
    Session(ObjectiveId),
    Resources,
    Dialogs,
}

impl AgentSessionState {
    pub fn apply(&mut self, update: AgentSessionUpdate) -> Vec<AgentSessionChange> {
        match update {
            AgentSessionUpdate::Catalog(catalog) => {
                self.catalog = Some(catalog);
                vec![AgentSessionChange::Catalog]
            }
            AgentSessionUpdate::Models { provider, models } => {
                self.models.insert(provider, models);
                vec![AgentSessionChange::Catalog]
            }
            AgentSessionUpdate::Session {
                objective_id,
                session,
            } => {
                if self
                    .sessions
                    .get(&objective_id)
                    .is_some_and(|s| s.generation > session.generation && session.generation > 0)
                {
                    return Vec::new();
                }
                self.sessions.insert(objective_id.clone(), session);
                vec![AgentSessionChange::Session(objective_id)]
            }
            AgentSessionUpdate::Tree { objective_id, tree } => {
                self.trees.insert(objective_id.clone(), tree);
                vec![AgentSessionChange::Session(objective_id)]
            }
            AgentSessionUpdate::Inventory(inventory) => {
                self.inventory = Some(inventory);
                vec![AgentSessionChange::Resources]
            }
            AgentSessionUpdate::DialogOpened(dialog) => {
                if self.dialogs.iter().any(|d| d.ui_id == dialog.ui_id) {
                    return Vec::new();
                }
                self.dialogs.push(dialog);
                vec![AgentSessionChange::Dialogs]
            }
            AgentSessionUpdate::DialogClosed { ui_id } => {
                let before = self.dialogs.len();
                self.dialogs.retain(|d| d.ui_id != ui_id);
                if self.dialogs.len() == before {
                    Vec::new()
                } else {
                    vec![AgentSessionChange::Dialogs]
                }
            }
            AgentSessionUpdate::Exported { objective_id, path } => {
                self.exports.insert(objective_id.clone(), path);
                vec![AgentSessionChange::Session(objective_id)]
            }
            AgentSessionUpdate::Choice(choice) => {
                if self.choice == choice {
                    return Vec::new();
                }
                self.choice = choice;
                vec![AgentSessionChange::Catalog]
            }
        }
    }

    pub fn chosen_models(&self) -> &[ModelInfo] {
        self.choice
            .provider
            .as_ref()
            .and_then(|p| self.models.get(p))
            .map_or(&[], Vec::as_slice)
    }

    pub fn readiness(&self) -> Readiness {
        let Some(catalog) = &self.catalog else {
            return Readiness::CatalogLoading;
        };
        if catalog.providers.is_empty() {
            return Readiness::NoProviders {
                error: catalog.error.clone(),
            };
        }
        let Some(id) = &self.choice.provider else {
            return Readiness::NoProviderChosen;
        };
        let info = catalog.provider(id);
        let provider = info.map_or_else(|| id.clone(), ProviderInfo::picker_label);
        match info.map(|listed| listed.status) {
            Some(ProviderStatus::MissingCredentials) => {
                return Readiness::NeedsCredentials { provider };
            }
            Some(ProviderStatus::Restricted) => return Readiness::Restricted { provider },
            Some(ProviderStatus::Available) | None => {}
        }
        match (self.models.get(id), self.chosen_model()) {
            (None, _) => Readiness::ModelsLoading { provider },
            (Some(_), None) => Readiness::NoModels { provider },
            (Some(_), Some(model)) => Readiness::Ready {
                model: model.clone(),
                thinking: self.choice.thinking,
            },
        }
    }

    pub fn model_picker(&self) -> ModelPicker {
        let Some(catalog) = &self.catalog else {
            return ModelPicker::Loading;
        };
        if catalog.providers.is_empty() {
            return ModelPicker::NoProviders {
                error: catalog.error.clone(),
            };
        }
        let choice = self.choice.clone();
        let models = self.chosen_models().to_vec();
        let model_row = match self.readiness() {
            Readiness::Ready { model, thinking } => {
                let levels = if model.thinking.is_empty() {
                    ThinkingLevel::ALL.to_vec()
                } else {
                    model.thinking
                };
                ModelRow::Ready {
                    selected_model: models.iter().position(|m| m.id == model.id),
                    selected_thinking: levels.iter().position(|l| *l == thinking),
                    thinking: levels,
                }
            }
            Readiness::NeedsCredentials { .. } | Readiness::Restricted { .. } => ModelRow::Hidden,
            other => ModelRow::Problem(other.problem().unwrap_or_default()),
        };
        ModelPicker::Choices(PickerChoices {
            catalog_error: catalog.error.clone(),
            selected_provider: choice.provider.as_ref().and_then(|p| {
                catalog
                    .providers
                    .iter()
                    .position(|provider| &provider.id == p)
            }),
            providers: catalog.providers.clone(),
            models,
            model_row,
            choice,
        })
    }

    pub fn chosen_model(&self) -> Option<&ModelInfo> {
        let models = self.chosen_models();
        self.choice
            .model
            .as_ref()
            .and_then(|id| models.iter().find(|m| &m.id == id))
            .or_else(|| models.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_generations_never_replace_newer_session_reports() {
        let mut state = AgentSessionState::default();
        let objective = ObjectiveId::new("o");
        let session = |generation| SessionState {
            generation,
            reason: SessionChangeReason::Reload,
            model: ModelRef::new("lade-fixture", "scripted"),
            thinking: ThinkingLevel::Off,
            association_revision: Some(1),
            pi_session_id: None,
            leaf_id: None,
        };
        state.apply(AgentSessionUpdate::Session {
            objective_id: objective.clone(),
            session: session(3),
        });
        assert!(
            state
                .apply(AgentSessionUpdate::Session {
                    objective_id: objective.clone(),
                    session: session(2),
                })
                .is_empty()
        );
        assert_eq!(state.sessions[&objective].generation, 3);
        let dialog = UiDialog {
            objective_id: objective,
            ui_id: "u".into(),
            generation: 3,
            prompt: UiPrompt::Confirm {
                title: "t".into(),
                message: "m".into(),
            },
        };
        assert_eq!(
            state.apply(AgentSessionUpdate::DialogOpened(dialog.clone())),
            [AgentSessionChange::Dialogs]
        );
        assert!(
            state
                .apply(AgentSessionUpdate::DialogOpened(dialog))
                .is_empty()
        );
        assert_eq!(
            state.apply(AgentSessionUpdate::DialogClosed { ui_id: "u".into() }),
            [AgentSessionChange::Dialogs]
        );
        assert!(state.dialogs.is_empty());
    }

    fn node(id: &str, parent: Option<&str>) -> TreeNode {
        TreeNode {
            entry_id: id.into(),
            parent_id: parent.map(str::to_string),
            kind: TreeEntryKind::User,
            label: id.into(),
            bookmark: None,
            depth: 9,
        }
    }

    #[test]
    fn tree_orders_depth_first_and_promotes_orphans() {
        let tree = SessionTree::ordered(
            vec![
                node("a", None),
                node("b", Some("a")),
                node("c", Some("a")),
                node("d", Some("b")),
                node("e", Some("missing")),
            ],
            Some("d".into()),
            true,
        );
        let order: Vec<(&str, usize)> = tree
            .entries
            .iter()
            .map(|n| (n.entry_id.as_str(), n.depth))
            .collect();
        assert_eq!(order, [("a", 0), ("b", 1), ("d", 2), ("c", 1), ("e", 0)]);
    }

    #[test]
    fn labels_never_claim_more_than_the_service_said() {
        let provider = ProviderInfo {
            id: "openai-codex".into(),
            name: "OpenAI Codex".into(),
            auth: vec!["OAuth login".into()],
            status: ProviderStatus::Restricted,
            reason: Some("interactive OAuth login is restricted".into()),
            variables: Vec::new(),
            models: 8,
        };
        assert_eq!(provider.picker_label(), "OpenAI Codex");
        assert_eq!(
            provider.subtitle(),
            "openai-codex · restricted · OAuth login · 8 models"
        );
        assert_eq!(ThinkingLevel::default(), ThinkingLevel::Off);
        assert_eq!(
            provider.credential_help(),
            None,
            "only for missing credentials"
        );
    }

    fn provider(id: &str, status: ProviderStatus) -> ProviderInfo {
        ProviderInfo {
            id: id.into(),
            name: id.to_uppercase(),
            auth: Vec::new(),
            status,
            reason: None,
            variables: Vec::new(),
            models: 1,
        }
    }

    fn model(id: &str, reasoning: bool, thinking: Vec<ThinkingLevel>) -> ModelInfo {
        ModelInfo {
            provider: "a".into(),
            id: id.into(),
            name: id.to_uppercase(),
            reasoning,
            thinking,
        }
    }

    fn with_catalog(providers: Vec<ProviderInfo>, error: Option<&str>) -> AgentSessionState {
        AgentSessionState {
            catalog: Some(Catalog {
                sdk_version: "1".into(),
                providers,
                error: error.map(str::to_owned),
            }),
            ..AgentSessionState::default()
        }
    }

    #[test]
    fn the_model_picker_waits_for_the_catalogue_and_explains_an_empty_one() {
        assert_eq!(
            AgentSessionState::default().model_picker(),
            ModelPicker::Loading
        );
        assert_eq!(
            with_catalog(Vec::new(), Some("sandbox_unavailable")).model_picker(),
            ModelPicker::NoProviders {
                error: Some("sandbox_unavailable".into())
            }
        );
    }

    #[test]
    fn the_model_picker_offers_models_and_their_thinking_levels_once_ready() {
        let mut state = with_catalog(
            vec![
                provider("b", ProviderStatus::Available),
                provider("a", ProviderStatus::Available),
            ],
            Some("one provider failed"),
        );
        state.choice = ModelChoice {
            provider: Some("a".into()),
            model: Some("y".into()),
            thinking: ThinkingLevel::High,
        };
        let ModelPicker::Choices(picker) = state.model_picker() else {
            panic!("choices");
        };
        assert_eq!(picker.catalog_error.as_deref(), Some("one provider failed"));
        assert_eq!(picker.selected_provider, Some(1));
        assert_eq!(picker.provider().map(|p| p.id.as_str()), Some("a"));
        assert_eq!(
            picker.model_row,
            ModelRow::Problem("Loading A's models…".into())
        );
        state.models.insert(
            "a".into(),
            vec![
                model("x", false, vec![ThinkingLevel::Off]),
                model("y", true, Vec::new()),
            ],
        );
        let ModelPicker::Choices(picker) = state.model_picker() else {
            panic!("choices");
        };
        assert_eq!(
            picker.model_row,
            ModelRow::Ready {
                selected_model: Some(1),
                thinking: ThinkingLevel::ALL.to_vec(),
                selected_thinking: Some(4),
            },
            "a model that lists no levels offers them all"
        );
        assert_eq!(picker.models[1].picker_label(), "Y (reasoning)");
        assert_eq!(picker.models[0].picker_label(), "X");
    }

    #[test]
    fn the_model_picker_leaves_an_unusable_provider_to_the_provider_row() {
        let mut state = with_catalog(vec![provider("a", ProviderStatus::Restricted)], None);
        state.choice.provider = Some("a".into());
        let ModelPicker::Choices(picker) = state.model_picker() else {
            panic!("choices");
        };
        assert_eq!(picker.model_row, ModelRow::Hidden);
        state.choice.provider = None;
        let ModelPicker::Choices(picker) = state.model_picker() else {
            panic!("choices");
        };
        assert_eq!(picker.selected_provider, None);
        assert_eq!(
            picker.model_row,
            ModelRow::Problem("Choose a model provider.".into())
        );
    }

    #[test]
    fn credential_help_names_only_the_published_variables() {
        let mut provider = ProviderInfo {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            auth: vec!["API key".into(), "subscription token".into()],
            status: ProviderStatus::MissingCredentials,
            reason: Some("Add an API key or subscription token for Anthropic to use it.".into()),
            variables: vec!["ANTHROPIC_API_KEY".into(), "ANTHROPIC_OAUTH_TOKEN".into()],
            models: 23,
        };
        let help = provider.credential_help().unwrap();
        assert!(help.starts_with("Needs ANTHROPIC_API_KEY or ANTHROPIC_OAUTH_TOKEN."));
        assert!(help.contains("laded credentials set anthropic\n"));
        provider.auth = vec!["custom provider".into()];
        provider.variables = vec!["LADE_PROVIDER_A".into(), "LADE_PROVIDER_B".into()];
        let help = provider.credential_help().unwrap();
        assert!(help.starts_with("Needs LADE_PROVIDER_A and LADE_PROVIDER_B."));
        assert!(help.contains("--variable NAME"));
        provider.variables.clear();
        assert_eq!(provider.credential_help(), None);
    }
}
