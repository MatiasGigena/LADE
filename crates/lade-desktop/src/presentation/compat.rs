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
    pub auth: Vec<String>,
    pub status: ProviderStatus,
    pub reason: Option<String>,
    pub variables: Vec<String>,
    pub models: u64,
}

impl ProviderView {
    pub fn picker_label(&self) -> String {
        format!("{} ({}) — {}", self.name, self.id, self.status.label())
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub sdk_version: String,
    pub providers: Vec<ProviderView>,
    pub error: Option<String>,
}

impl Catalog {
    pub fn provider(&self, id: &str) -> Option<&ProviderView> {
        self.providers.iter().find(|p| p.id == id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelChoice {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub thinking: ThinkingLevel,
}

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
    pub kind: String,
    pub user: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiDialog {
    pub objective_id: ObjectiveId,
    pub ui_id: String,
    pub generation: u64,
    pub prompt: UiPromptView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiAnswerView {
    Confirm(bool),
    Value(String),
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CompatState {
    pub catalog: Option<Catalog>,
    pub models: std::collections::HashMap<String, Vec<ModelView>>,
    pub sessions: std::collections::HashMap<ObjectiveId, SessionView>,
    pub trees: std::collections::HashMap<ObjectiveId, SessionTree>,
    pub inventory: Option<Inventory>,
    pub dialogs: Vec<UiDialog>,
    pub exports: std::collections::HashMap<ObjectiveId, String>,
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

    pub fn chosen_models(&self) -> &[ModelView] {
        self.choice
            .provider
            .as_ref()
            .and_then(|p| self.models.get(p))
            .map_or(&[], Vec::as_slice)
    }

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
        assert_eq!(
            provider.picker_label(),
            "OpenAI Codex (openai-codex) — restricted"
        );
        assert_eq!(ThinkingLevel::default(), ThinkingLevel::Off);
    }
}
