//! The SQLite schema of the service store, as ordered migrations.

pub(super) const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_V1), (2, SCHEMA_V2), (3, SCHEMA_V3)];

pub const SCHEMA_VERSION: i64 = latest(MIGRATIONS);

pub(super) const fn latest(migrations: &[(i64, &str)]) -> i64 {
    match migrations.last() {
        Some((version, _)) => *version,
        None => 0,
    }
}

pub(super) const SCHEMA_V1: &str = r"
CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    json TEXT NOT NULL
);
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    path BLOB NOT NULL UNIQUE,
    name TEXT NOT NULL,
    default_branch TEXT,
    opened_at INTEGER NOT NULL
);
CREATE TABLE objectives (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    title TEXT NOT NULL,
    outcome TEXT NOT NULL,
    constraints_text TEXT NOT NULL,
    acceptance TEXT NOT NULL,
    intent_revision INTEGER NOT NULL,
    integration_target TEXT NOT NULL,
    start_algorithm TEXT NOT NULL,
    start_commit TEXT NOT NULL,
    branch TEXT NOT NULL,
    lifecycle TEXT NOT NULL,
    exec_state TEXT NOT NULL,
    exec_interrupted INTEGER NOT NULL,
    exec_reason TEXT,
    exec_generation INTEGER NOT NULL,
    session_id TEXT,
    association_revision INTEGER,
    model_provider TEXT,
    model_id TEXT,
    cleaned_up INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    git_branch TEXT,
    worktree_path BLOB,
    integration_conflict INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    association_revision INTEGER NOT NULL,
    model_provider TEXT NOT NULL,
    model_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    pi_session_id TEXT,
    session_file TEXT,
    last_generation INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE captures (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    tree TEXT NOT NULL,
    target_commit TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    retention_ref TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX captures_by_objective ON captures(objective_id, created_at);
CREATE TABLE drafts (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    destination_session_id TEXT NOT NULL,
    reference TEXT NOT NULL,
    body TEXT NOT NULL,
    revision INTEGER NOT NULL,
    state TEXT NOT NULL,
    input_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE draft_revisions (
    draft_id TEXT NOT NULL REFERENCES drafts(id),
    revision INTEGER NOT NULL,
    reference TEXT NOT NULL,
    body TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY (draft_id, revision)
);
CREATE TABLE accepted_inputs (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    session_id TEXT NOT NULL,
    association_revision INTEGER NOT NULL,
    mode TEXT NOT NULL,
    items TEXT NOT NULL,
    note TEXT,
    rendered_text TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TRIGGER accepted_inputs_immutable BEFORE UPDATE ON accepted_inputs
BEGIN SELECT RAISE(ABORT, 'accepted input is immutable'); END;
CREATE TRIGGER accepted_inputs_undeletable BEFORE DELETE ON accepted_inputs
BEGIN SELECT RAISE(ABORT, 'accepted input is immutable'); END;
CREATE TABLE input_delivery (
    input_id TEXT PRIMARY KEY REFERENCES accepted_inputs(id),
    state TEXT NOT NULL,
    generation INTEGER,
    blocked_reason TEXT,
    updated_at INTEGER NOT NULL
);
CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    outcome TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE candidates (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    ordinal INTEGER NOT NULL,
    commit_id TEXT NOT NULL,
    tree TEXT NOT NULL,
    base_target_commit TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    files INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    UNIQUE (objective_id, ordinal)
);
CREATE TABLE verifications (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    candidate_id TEXT NOT NULL REFERENCES candidates(id),
    argv TEXT NOT NULL,
    cwd_label TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    exit_code INTEGER,
    outcome TEXT NOT NULL,
    output_artifact TEXT,
    output_tail TEXT NOT NULL DEFAULT '',
    truncated INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    candidate_id TEXT NOT NULL REFERENCES candidates(id),
    target_commit TEXT NOT NULL,
    override_reason TEXT,
    state TEXT NOT NULL,
    invalidation_reason TEXT,
    approved_at INTEGER NOT NULL,
    seq INTEGER NOT NULL
);
CREATE TABLE activity (
    item_id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    seq INTEGER NOT NULL,
    at INTEGER NOT NULL,
    role TEXT NOT NULL,
    title TEXT NOT NULL,
    text TEXT NOT NULL,
    tool_name TEXT,
    is_error INTEGER NOT NULL,
    truncated INTEGER NOT NULL,
    UNIQUE (objective_id, seq)
);
CREATE TABLE integration_journal (
    id TEXT PRIMARY KEY,
    objective_id TEXT NOT NULL REFERENCES objectives(id),
    approval_id TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    target_branch TEXT NOT NULL,
    expected_target TEXT NOT NULL,
    candidate_tree TEXT NOT NULL,
    message TEXT NOT NULL,
    commit_time INTEGER NOT NULL,
    state TEXT NOT NULL,
    result_commit TEXT,
    detail TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
";

/// Per-Objective session preferences and executable-extension decisions.
const SCHEMA_V2: &str = r"
CREATE TABLE session_preferences (
    objective_id TEXT PRIMARY KEY REFERENCES objectives(id),
    thinking TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE resource_approvals (
    name TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    decision TEXT NOT NULL CHECK (decision IN ('approved', 'rejected')),
    decided_at INTEGER NOT NULL,
    PRIMARY KEY (name, sha256)
);
";

const SCHEMA_V3: &str = r"
ALTER TABLE objectives ADD COLUMN history_deleted INTEGER NOT NULL DEFAULT 0;
";
