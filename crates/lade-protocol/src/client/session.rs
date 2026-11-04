//! Agent Session, model catalogue, resource and extension UI DTOs.

use crate::types::{
    Counter, Digest, ForkPosition, Id, Model, SessionChangeReason, ThinkingLevel, TreeEntry,
    UiAnswer, UiCloseReason, UiPrompt, WirePath,
};
use serde::{Deserialize, Serialize};

wire_enum!(ProviderAuth {
    None,
    ApiKey,
    SubscriptionToken,
    OauthLogin,
    CloudCredentials,
    Custom,
});
wire_enum!(ProviderStatus {
    Available,
    MissingCredentials,
    Restricted,
});
wire_struct!(ProviderInfo {
    provider: String,
    name: String,
    auth: Vec<ProviderAuth>,
    status: ProviderStatus,
    models: Counter,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    variables: Option<Vec<String>>,
});
wire_struct!(ModelInfo {
    provider: String,
    id: String,
    name: String,
    reasoning: bool,
    thinking_levels: Vec<ThinkingLevel>,
});
wire_struct!(ModelsListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
});
wire_struct!(ModelCatalog {
    sdk_version: String,
    providers: Vec<ProviderInfo>,
    models: Vec<ModelInfo>,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
});
wire_struct!(SessionTree {
    objective_id: Id,
    entries: Vec<TreeEntry>,
    truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leaf_id: Option<String>,
});
wire_struct!(SessionNavigateCommand {
    objective_id: Id,
    entry_id: String,
});
wire_struct!(SessionForkCommand {
    objective_id: Id,
    entry_id: String,
    position: ForkPosition,
});
wire_struct!(SessionImportCommand {
    objective_id: Id,
    path: WirePath,
});
wire_struct!(SessionCompactCommand {
    objective_id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
});
wire_struct!(SessionConfigureCommand {
    objective_id: Id,
    model: Model,
    thinking: ThinkingLevel,
});
wire_struct!(SessionState {
    objective_id: Id,
    generation: Counter,
    reason: SessionChangeReason,
    model: Model,
    thinking: ThinkingLevel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    association_revision: Option<Counter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pi_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    leaf_id: Option<String>,
});
wire_struct!(SessionExport {
    objective_id: Id,
    path: WirePath,
});
wire_enum!(ResourceKind {
    Skill,
    Prompt,
    Context,
    Extension,
    Models,
});
wire_enum!(ResourceState {
    Active,
    PendingApproval,
    Rejected,
    Refused,
});
wire_struct!(ResourceEntry {
    kind: ResourceKind,
    name: String,
    state: ResourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
});
wire_struct!(ResourceInventory {
    profile_dir: WirePath,
    entries: Vec<ResourceEntry>,
    restrictions: Vec<String>,
});
wire_struct!(ResourceDecideCommand {
    name: String,
    sha256: Digest,
    approve: bool,
});
wire_struct!(ProfileImportCommand { source: WirePath });
wire_struct!(ProfileImportResult {
    copied: Vec<String>,
    skipped: Vec<String>,
    inventory: ResourceInventory,
});
wire_struct!(UiRequest {
    objective_id: Id,
    ui_id: Id,
    generation: Counter,
    prompt: UiPrompt,
});
wire_struct!(UiClosed {
    objective_id: Id,
    ui_id: Id,
    reason: UiCloseReason,
});
wire_struct!(UiRespondCommand {
    objective_id: Id,
    ui_id: Id,
    generation: Counter,
    answer: UiAnswer,
});
