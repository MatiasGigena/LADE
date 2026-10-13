//! IMP-011 presentation types: the service's model catalogue with provider access, the Agent
//! Session's pi state and tree, LADE-profile resources and semantic extension dialogs. Pure data
//! and labels; the service decides availability, approval and routing.

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
pub struct ProviderView {
    pub id: String,
    pub name: String,
    /// Authentication kinds as labels ("API key", "subscription token", ...).
    pub auth: Vec<String>,
    pub status: ProviderStatus,
    /// Why it is unavailable or restricted, or which endpoint a custom provider uses.
    pub reason: Option<String>,
    /// Credential variable names (never values).
    pub variables: Vec<String>,
    pub models: u64,
}

impl ProviderView {
    /// Picker entry: the provider's name only (the id when it has none). Its id, access status
    /// and authentication are the [`ProviderView::subtitle`].
    pub fn picker_label(&self) -> String {
        if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        }
    }

    /// The secondary line under the chosen provider: id, access status, authentication and how
    /// many models it lists.
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

    /// For a provider that needs credentials: where its key can go, for a collapsed *Details*
    /// under the service's one-sentence reason. Built only from what the service published (the
    /// provider id and its credential variable names) and the credential sources of the user
    /// guide (§5); `None` for any other status or without variable names.
    pub fn credential_help(&self) -> Option<String> {
        if self.status != ProviderStatus::MissingCredentials || self.variables.is_empty() {
            return None;
        }
        let id = &self.id;
        // A custom provider needs every one of its variables, each stored by name.
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
pub struct ModelView {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub thinking: Vec<ThinkingLevel>,
}

impl ModelView {
    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(self.provider.clone(), self.id.clone())
    }
}

/// The pinned SDK catalogue as the service reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub sdk_version: String,
    pub providers: Vec<ProviderView>,
    /// Why the catalogue is partial (for example: the sandbox is unavailable).
    pub error: Option<String>,
}

impl Catalog {
    pub fn provider(&self, id: &str) -> Option<&ProviderView> {
        self.providers.iter().find(|p| p.id == id)
    }
}

/// What the developer picked before starting or reconfiguring the agent (transient UI state).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelChoice {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: ThinkingLevel,
}

/// The live worker's session runtime as last reported (`session.changed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionView {
    pub generation: u64,
    pub reason: String,
    pub model: ModelRef,
    pub thinking: ThinkingLevel,
    pub association_revision: Option<u64>,
    pub pi_session_id: Option<String>,
    pub leaf_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub entry_id: String,
    pub parent_id: Option<String>,
    /// Entry kind as a label ("You", "Agent", "Tool result", ...).
    pub kind: String,
    pub user: bool,
    pub label: String,
    pub bookmark: Option<String>,
    /// Depth in the tree (0 for roots), for indentation.
    pub depth: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionTree {
    pub entries: Vec<TreeNode>,
    pub leaf_id: Option<String>,
    pub truncated: bool,
}

impl SessionTree {
    /// Orders `entries` depth-first from their roots (siblings in file order) and sets depths;
    /// entries whose parent is missing (truncated history) become roots.
    pub fn ordered(entries: Vec<TreeNode>, leaf_id: Option<String>, truncated: bool) -> Self {
        let known: std::collections::HashSet<String> =
            entries.iter().map(|e| e.entry_id.clone()).collect();
        let mut children: std::collections::HashMap<Option<String>, Vec<usize>> =
            std::collections::HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let parent = entry.parent_id.clone().filter(|p| known.contains(p));
            children.entry(parent).or_default().push(index);
        }
        let mut ordered = Vec::with_capacity(entries.len());
        let mut stack: Vec<(usize, usize)> = children
            .get(&None)
            .map(|roots| roots.iter().rev().map(|&i| (i, 0)).collect())
            .unwrap_or_default();
        while let Some((index, depth)) = stack.pop() {
            let mut node = entries[index].clone();
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
    /// A new session from the start up to (not including) the selected user message.
    Before,
    /// A copy of the session up to and including the selected entry (clone).
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
pub struct ResourceView {
    pub kind: ResourceKind,
    pub name: String,
    pub state: ResourceState,
    pub sha256: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Inventory {
    pub profile_dir: String,
    pub entries: Vec<ResourceView>,
    pub restrictions: Vec<String>,
}

impl Inventory {
    pub fn pending(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.state == ResourceState::PendingApproval)
            .count()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiPromptView {
    Confirm { title: String, message: String },
    Select { title: String, options: Vec<String> },
    Input { title: String, placeholder: String },
    Editor { title: String, prefill: String },
}

impl UiPromptView {
    pub fn title(&self) -> &str {
        match self {
            Self::Confirm { title, .. }
            | Self::Select { title, .. }
            | Self::Input { title, .. }
            | Self::Editor { title, .. } => title,
        }
    }
}

/// An open extension dialog of one session generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiDialog {
    pub objective_id: ObjectiveId,
    pub ui_id: String,
    pub generation: u64,
    pub prompt: UiPromptView,
}

/// The developer's explicit answer. There is no default answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAnswerView {
    Confirm(bool),
    Value(String),
    Cancel,
}

/// How far the model picker is from a model that Start agent (or Apply model and thinking) can
/// use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The service has not reported its catalogue yet.
    CatalogLoading,
    /// The catalogue lists no provider; `error` is the service's reason, when it gave one.
    NoProviders { error: Option<String> },
    /// Providers are listed but none is chosen.
    NoProviderChosen,
    /// The chosen provider's models (named by the provider's name) are being loaded.
    ModelsLoading { provider: String },
    /// The chosen provider lists no models.
    NoModels { provider: String },
    /// A model is chosen.
    Ready {
        model: ModelView,
        thinking: ThinkingLevel,
    },
}

impl Readiness {
    /// Why there is no usable model yet, as one short sentence; `None` when ready.
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
            Self::Ready { .. } => return None,
        })
    }
}

/// IMP-011 service projections and transient choices held by the presentation store.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CompatState {
    pub catalog: Option<Catalog>,
    /// Models per provider id, loaded on demand.
    pub models: std::collections::HashMap<String, Vec<ModelView>>,
    pub sessions: std::collections::HashMap<ObjectiveId, SessionView>,
    pub trees: std::collections::HashMap<ObjectiveId, SessionTree>,
    pub inventory: Option<Inventory>,
    /// Open extension dialogs, oldest first.
    pub dialogs: Vec<UiDialog>,
    pub exports: std::collections::HashMap<ObjectiveId, String>,
    /// The model picker's current choice (shared by start and configure).
    pub choice: ModelChoice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatUpdate {
    Catalog(Catalog),
    Models {
        provider: String,
        models: Vec<ModelView>,
    },
    Session {
        objective_id: ObjectiveId,
        session: SessionView,
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

/// What an IMP-011 update changed (mirrored by `store::Change`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompatChange {
    Catalog,
    Session(ObjectiveId),
    Resources,
    Dialogs,
}

impl CompatState {
    pub fn apply(&mut self, update: CompatUpdate) -> Vec<CompatChange> {
        match update {
            CompatUpdate::Catalog(catalog) => {
                self.catalog = Some(catalog);
                vec![CompatChange::Catalog]
            }
            CompatUpdate::Models { provider, models } => {
                self.models.insert(provider, models);
                vec![CompatChange::Catalog]
            }
            CompatUpdate::Session {
                objective_id,
                session,
            } => {
                // A late report of an older generation never replaces a newer one.
                if self
                    .sessions
                    .get(&objective_id)
                    .is_some_and(|s| s.generation > session.generation && session.generation > 0)
                {
                    return Vec::new();
                }
                self.sessions.insert(objective_id.clone(), session);
                vec![CompatChange::Session(objective_id)]
            }
            CompatUpdate::Tree { objective_id, tree } => {
                self.trees.insert(objective_id.clone(), tree);
                vec![CompatChange::Session(objective_id)]
            }
            CompatUpdate::Inventory(inventory) => {
                self.inventory = Some(inventory);
                vec![CompatChange::Resources]
            }
            CompatUpdate::DialogOpened(dialog) => {
                if self.dialogs.iter().any(|d| d.ui_id == dialog.ui_id) {
                    return Vec::new();
                }
                self.dialogs.push(dialog);
                vec![CompatChange::Dialogs]
            }
            CompatUpdate::DialogClosed { ui_id } => {
                let before = self.dialogs.len();
                self.dialogs.retain(|d| d.ui_id != ui_id);
                if self.dialogs.len() == before {
                    Vec::new()
                } else {
                    vec![CompatChange::Dialogs]
                }
            }
            CompatUpdate::Exported { objective_id, path } => {
                self.exports.insert(objective_id.clone(), path);
                vec![CompatChange::Session(objective_id)]
            }
            CompatUpdate::Choice(choice) => {
                if self.choice == choice {
                    return Vec::new();
                }
                self.choice = choice;
                vec![CompatChange::Catalog]
            }
        }
    }

    /// The selectable models for the chosen provider (empty until they are loaded).
    pub fn chosen_models(&self) -> &[ModelView] {
        self.choice
            .provider
            .as_ref()
            .and_then(|p| self.models.get(p))
            .map_or(&[], Vec::as_slice)
    }

    /// How far the picker is from a usable model.
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
        let provider = catalog
            .provider(id)
            .map_or_else(|| id.clone(), ProviderView::picker_label);
        match (self.models.get(id), self.chosen_model()) {
            (None, _) => Readiness::ModelsLoading { provider },
            (Some(_), None) => Readiness::NoModels { provider },
            (Some(_), Some(model)) => Readiness::Ready {
                model: model.clone(),
                thinking: self.choice.thinking,
            },
        }
    }

    /// The model the picker resolves to, if the provider's models are loaded: the chosen id, or
    /// the provider's first model when none (or one the catalogue no longer lists) was chosen.
    pub fn chosen_model(&self) -> Option<&ModelView> {
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
        let mut state = CompatState::default();
        let objective = ObjectiveId::new("o");
        let session = |generation| SessionView {
            generation,
            reason: "reload".into(),
            model: ModelRef::new("lade-fixture", "scripted"),
            thinking: ThinkingLevel::Off,
            association_revision: Some(1),
            pi_session_id: None,
            leaf_id: None,
        };
        state.apply(CompatUpdate::Session {
            objective_id: objective.clone(),
            session: session(3),
        });
        assert!(
            state
                .apply(CompatUpdate::Session {
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
            prompt: UiPromptView::Confirm {
                title: "t".into(),
                message: "m".into(),
            },
        };
        assert_eq!(
            state.apply(CompatUpdate::DialogOpened(dialog.clone())),
            [CompatChange::Dialogs]
        );
        assert!(state.apply(CompatUpdate::DialogOpened(dialog)).is_empty());
        assert_eq!(
            state.apply(CompatUpdate::DialogClosed { ui_id: "u".into() }),
            [CompatChange::Dialogs]
        );
        assert!(state.dialogs.is_empty());
    }

    fn node(id: &str, parent: Option<&str>) -> TreeNode {
        TreeNode {
            entry_id: id.into(),
            parent_id: parent.map(str::to_string),
            kind: "You".into(),
            user: true,
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
        let provider = ProviderView {
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

    #[test]
    fn credential_help_names_only_the_published_variables() {
        let mut provider = ProviderView {
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
