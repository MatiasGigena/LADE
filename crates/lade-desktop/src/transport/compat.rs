use lade_protocol::client as wire;
use lade_protocol::types as wt;

use super::convert::{counter, model};
use crate::presentation::state::compat::{
    Catalog, ForkPosition, Inventory, ModelView, ProviderStatus, ProviderView, ResourceKind,
    ResourceState, ResourceView, SessionTree, SessionView, ThinkingLevel, TreeNode, UiAnswerView,
    UiDialog, UiPromptView,
};
use crate::presentation::state::model::ObjectiveId;

pub fn thinking(level: wt::ThinkingLevel) -> ThinkingLevel {
    match level {
        wt::ThinkingLevel::Off => ThinkingLevel::Off,
        wt::ThinkingLevel::Minimal => ThinkingLevel::Minimal,
        wt::ThinkingLevel::Low => ThinkingLevel::Low,
        wt::ThinkingLevel::Medium => ThinkingLevel::Medium,
        wt::ThinkingLevel::High => ThinkingLevel::High,
        wt::ThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        wt::ThinkingLevel::Max => ThinkingLevel::Max,
    }
}

pub fn to_thinking(level: ThinkingLevel) -> wt::ThinkingLevel {
    match level {
        ThinkingLevel::Off => wt::ThinkingLevel::Off,
        ThinkingLevel::Minimal => wt::ThinkingLevel::Minimal,
        ThinkingLevel::Low => wt::ThinkingLevel::Low,
        ThinkingLevel::Medium => wt::ThinkingLevel::Medium,
        ThinkingLevel::High => wt::ThinkingLevel::High,
        ThinkingLevel::Xhigh => wt::ThinkingLevel::Xhigh,
        ThinkingLevel::Max => wt::ThinkingLevel::Max,
    }
}

pub fn to_fork(position: ForkPosition) -> wt::ForkPosition {
    match position {
        ForkPosition::Before => wt::ForkPosition::Before,
        ForkPosition::At => wt::ForkPosition::At,
    }
}

pub fn to_answer(answer: &UiAnswerView) -> wt::UiAnswer {
    match answer {
        UiAnswerView::Confirm(confirmed) => wt::UiAnswer::Confirm {
            confirmed: *confirmed,
        },
        UiAnswerView::Value(value) => wt::UiAnswer::Value {
            value: value.clone(),
        },
        UiAnswerView::Cancel => wt::UiAnswer::Cancel,
    }
}

fn auth_label(auth: wire::ProviderAuth) -> &'static str {
    match auth {
        wire::ProviderAuth::None => "no credentials needed",
        wire::ProviderAuth::ApiKey => "API key",
        wire::ProviderAuth::SubscriptionToken => "subscription token",
        wire::ProviderAuth::OauthLogin => "OAuth login",
        wire::ProviderAuth::CloudCredentials => "cloud credentials",
        wire::ProviderAuth::Custom => "custom provider",
    }
}

pub fn catalog(c: &wire::ModelCatalog) -> Catalog {
    Catalog {
        sdk_version: c.sdk_version.clone(),
        providers: c
            .providers
            .iter()
            .map(|p| ProviderView {
                id: p.provider.clone(),
                name: p.name.clone(),
                auth: p.auth.iter().map(|a| auth_label(*a).to_owned()).collect(),
                status: match p.status {
                    wire::ProviderStatus::Available => ProviderStatus::Available,
                    wire::ProviderStatus::MissingCredentials => ProviderStatus::MissingCredentials,
                    wire::ProviderStatus::Restricted => ProviderStatus::Restricted,
                },
                reason: p.reason.clone(),
                variables: p.variables.clone().unwrap_or_default(),
                models: counter(&p.models),
            })
            .collect(),
        error: c.error.clone(),
    }
}

pub fn models(c: &wire::ModelCatalog) -> Vec<ModelView> {
    c.models
        .iter()
        .map(|m| ModelView {
            provider: m.provider.clone(),
            id: m.id.clone(),
            name: m.name.clone(),
            reasoning: m.reasoning,
            thinking: m.thinking_levels.iter().map(|l| thinking(*l)).collect(),
        })
        .collect()
}

fn reason(reason: wt::SessionChangeReason) -> &'static str {
    match reason {
        wt::SessionChangeReason::Start => "started",
        wt::SessionChangeReason::Navigate => "moved in the tree",
        wt::SessionChangeReason::Fork => "forked",
        wt::SessionChangeReason::Import => "imported",
        wt::SessionChangeReason::Reload => "reloaded",
        wt::SessionChangeReason::Compact => "compacted",
        wt::SessionChangeReason::Configure => "reconfigured",
    }
}

pub fn session(s: &wire::SessionState) -> (ObjectiveId, SessionView) {
    (
        ObjectiveId::new(s.objective_id.as_str()),
        SessionView {
            generation: counter(&s.generation),
            reason: reason(s.reason).to_owned(),
            model: model(&s.model),
            thinking: thinking(s.thinking),
            association_revision: s.association_revision.as_ref().map(counter),
            pi_session_id: s.pi_session_id.clone(),
            leaf_id: s.leaf_id.clone(),
        },
    )
}

fn kind_label(kind: wt::TreeEntryKind) -> &'static str {
    match kind {
        wt::TreeEntryKind::User => "You",
        wt::TreeEntryKind::Assistant => "Agent",
        wt::TreeEntryKind::ToolResult => "Tool result",
        wt::TreeEntryKind::CustomMessage => "Extension message",
        wt::TreeEntryKind::Compaction => "Compaction",
        wt::TreeEntryKind::BranchSummary => "Branch summary",
        wt::TreeEntryKind::ModelChange => "Model change",
        wt::TreeEntryKind::ThinkingChange => "Thinking change",
        wt::TreeEntryKind::Label => "Bookmark",
        wt::TreeEntryKind::SessionInfo => "Session name",
        wt::TreeEntryKind::Custom => "Extension data",
        wt::TreeEntryKind::Other => "Entry",
    }
}

pub fn tree(t: &wire::SessionTree) -> SessionTree {
    SessionTree::ordered(
        t.entries
            .iter()
            .map(|e| TreeNode {
                entry_id: e.entry_id.clone(),
                parent_id: e.parent_id.clone(),
                kind: kind_label(e.kind).to_owned(),
                user: e.kind == wt::TreeEntryKind::User,
                label: e.label.clone(),
                bookmark: e.bookmark.clone(),
                depth: 0,
            })
            .collect(),
        t.leaf_id.clone(),
        t.truncated,
    )
}

pub fn inventory(i: &wire::ResourceInventory) -> Inventory {
    Inventory {
        profile_dir: i.profile_dir.display().to_owned(),
        entries: i
            .entries
            .iter()
            .map(|e| ResourceView {
                kind: match e.kind {
                    wire::ResourceKind::Skill => ResourceKind::Skill,
                    wire::ResourceKind::Prompt => ResourceKind::Prompt,
                    wire::ResourceKind::Context => ResourceKind::Context,
                    wire::ResourceKind::Extension => ResourceKind::Extension,
                    wire::ResourceKind::Models => ResourceKind::Models,
                },
                name: e.name.clone(),
                state: match e.state {
                    wire::ResourceState::Active => ResourceState::Active,
                    wire::ResourceState::PendingApproval => ResourceState::PendingApproval,
                    wire::ResourceState::Rejected => ResourceState::Rejected,
                    wire::ResourceState::Refused => ResourceState::Refused,
                },
                sha256: e.sha256.as_ref().map(|s| s.as_str().to_owned()),
                reason: e.reason.clone(),
            })
            .collect(),
        restrictions: i.restrictions.clone(),
    }
}

pub fn dialog(r: &wire::UiRequest) -> UiDialog {
    UiDialog {
        objective_id: ObjectiveId::new(r.objective_id.as_str()),
        ui_id: r.ui_id.as_str().to_owned(),
        generation: counter(&r.generation),
        prompt: match &r.prompt {
            wt::UiPrompt::Confirm { title, message } => UiPromptView::Confirm {
                title: title.clone(),
                message: message.clone(),
            },
            wt::UiPrompt::Select { title, options } => UiPromptView::Select {
                title: title.clone(),
                options: options.clone(),
            },
            wt::UiPrompt::Input { title, placeholder } => UiPromptView::Input {
                title: title.clone(),
                placeholder: placeholder.clone(),
            },
            wt::UiPrompt::Editor { title, prefill } => UiPromptView::Editor {
                title: title.clone(),
                prefill: prefill.clone(),
            },
        },
    }
}
