mod service_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use lade_protocol::client::{
    ActivityPageQuery, ErrorCode, Event, ExecutionStartCommand, ExecutionState,
    FeedbackSubmitCommand, Lifecycle, MessageSendCommand, ModelsListQuery, ObjectiveSelector,
    ProfileImportCommand, ProviderAuth, ProviderStatus, ReplyData, Request, ResourceDecideCommand,
    ResourceKind, ResourceState, SessionCompactCommand, SessionConfigureCommand,
    SessionForkCommand, SessionImportCommand, SessionNavigateCommand, UiRespondCommand,
};
use lade_protocol::types::{
    Counter, DeliveryMode, Digest, ForkPosition, Id, Model, PageLimit, ThinkingLevel,
    TreeEntryKind, UiAnswer, UiCloseReason, UiPrompt, WirePath,
};
use lade_system::service::{self, ServiceConfig};
use lade_system::supervision::credentials::FIXTURE_REFUSED;
use lade_system::supervision::secret_service::SecretService;
use service_support::*;

const EXTENSION: &str = r#"export default function (pi) {
  pi.registerCommand("ask", {
    description: "LADE fixture confirmation",
    handler: async (args, ctx) => {
      const ok = await ctx.ui.confirm("Proceed?", "Fixture confirmation " + args);
      pi.sendUserMessage("lade-fixture: echo confirmed=" + ok);
    },
  });
}
"#;

fn profile(env: &Env) -> PathBuf {
    env.root.path().join("state/profile")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn send(client: &mut Client, objective: &Id, text: &str) {
    match client.call(Request::MessageSend(MessageSendCommand {
        objective_id: objective.clone(),
        text: text.into(),
        mode: DeliveryMode::Prompt,
    })) {
        Ok(ReplyData::MessageSend(_)) => {}
        other => panic!("message.send: {other:?}"),
    }
}

fn activity(client: &mut Client, objective: &Id) -> Vec<(String, String)> {
    match client.call(Request::ActivityPage(ActivityPageQuery {
        objective_id: objective.clone(),
        limit: PageLimit::parse("200").unwrap(),
        before: None,
    })) {
        Ok(ReplyData::ActivityPage(page)) => page
            .items
            .into_iter()
            .map(|item| (item.title, item.text))
            .collect(),
        other => panic!("activity.page: {other:?}"),
    }
}

fn wait_activity(client: &mut Client, objective: &Id, needle: &str) -> String {
    eventually(&format!("activity containing {needle:?}"), || {
        activity(client, objective)
            .into_iter()
            .rev()
            .find(|(_, text)| text.contains(needle))
            .map(|(_, text)| text)
    })
}

fn idle_session(client: &mut Client, objective: &Id) {
    wait_state(client, objective, ExecutionState::Idle);
    wait_activity(client, objective, "Generation 1.");
}

fn snapshot_tree(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else {
                out.insert(
                    path.clone(),
                    (fs::read(&path).unwrap(), meta.modified().unwrap()),
                );
            }
        }
    }
    out
}

fn provider_ids(client: &mut Client, provider: Option<&str>) -> (Vec<String>, usize) {
    match client.call(Request::ModelsList(ModelsListQuery {
        provider: provider.map(str::to_owned),
    })) {
        Ok(ReplyData::ModelsList(catalog)) => (
            catalog.providers.into_iter().map(|p| p.provider).collect(),
            catalog.models.len(),
        ),
        other => panic!("models.list: {other:?}"),
    }
}

#[test]
fn the_fixture_provider_is_absent_and_refused_without_the_test_switch() {
    for (tag, launcher) in [
        ("svc-nofixture", None),
        ("svc-nofixture-fake", Some(FakeLauncher::shared())),
    ] {
        let env = Env::new(tag);
        let launcher = launcher.unwrap_or_else(|| best_launcher_with(&env, false).0);
        let config = ServiceConfig {
            test_fixture_provider: false,
            ..env.config()
        };
        let service = service::start(&config, launcher).expect("service starts");
        let mut client = env.connect();
        let (providers, _) = provider_ids(&mut client, None);
        assert!(!providers.iter().any(|p| p == "lade-fixture"), "{tag}");
        assert_eq!(
            provider_ids(&mut client, Some("lade-fixture")).1,
            0,
            "{tag}"
        );
        let ws = open_workspace(&mut client, &env.repo);
        let objective = create_objective(&mut client, &ws.workspace_id, "No fixture").objective_id;
        let refused = match client.call(Request::ExecutionStart(ExecutionStartCommand {
            objective_id: objective,
            model: Model {
                provider: "lade-fixture".into(),
                id: "scripted".into(),
            },
            thinking: None,
        })) {
            Err(rejection) => rejection,
            other => panic!("fixture start was not refused: {other:?}"),
        };
        assert_eq!(refused.code, ErrorCode::ExecutionUnavailable, "{tag}");
        assert_eq!(refused.detail, FIXTURE_REFUSED, "{tag}");
        drop(client);
        service.shutdown();
    }
}

#[test]
fn the_fixture_provider_is_listed_with_the_test_switch() {
    let env = Env::new("svc-fixture-fallback");
    let service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let (providers, _) = provider_ids(&mut client, None);
    assert_eq!(providers, ["lade-fixture"]);
    assert_eq!(provider_ids(&mut client, Some("lade-fixture")).1, 1);
    drop(client);
    service.shutdown();
}

#[test]
fn models_come_from_the_sdk_catalogue_with_explicit_provider_access() {
    let env = Env::new("svc-models");
    let (launcher, sandboxed) = best_launcher(&env);
    // The sandboxed launcher uses this host's keyring: a usable one may hold the key, which only
    // execution start reads, so the provider is then not reported missing.
    let keyring = SecretService::from_env();
    let keyring = sandboxed && keyring.is_enabled() && keyring.unavailable_reason().is_none();
    let _service = env.start(launcher);
    let mut client = env.connect();
    let catalog = match client.call(Request::ModelsList(ModelsListQuery { provider: None })) {
        Ok(ReplyData::ModelsList(catalog)) => catalog,
        other => panic!("models.list: {other:?}"),
    };
    assert_eq!(catalog.sdk_version, "1.5.0");
    assert_eq!(catalog.error, None, "the pinned SDK catalogue ran");
    assert!(catalog.models.is_empty(), "models are listed per provider");
    let by_id: BTreeMap<_, _> = catalog
        .providers
        .iter()
        .map(|p| (p.provider.as_str(), p))
        .collect();
    assert!(by_id.len() > 30, "the SDK registry, not a static table");
    assert_eq!(by_id["lade-fixture"].status, ProviderStatus::Available);
    let anthropic = by_id["anthropic"];
    if keyring {
        assert_eq!(anthropic.status, ProviderStatus::Available);
    } else {
        assert_eq!(anthropic.status, ProviderStatus::MissingCredentials);
        // One short sentence; where the key goes is the client's collapsed Details.
        assert_eq!(
            anthropic.reason.as_deref(),
            Some("Add an API key or subscription token for Anthropic to use it.")
        );
    }
    for auth in [
        ProviderAuth::ApiKey,
        ProviderAuth::SubscriptionToken,
        ProviderAuth::OauthLogin,
    ] {
        assert!(anthropic.auth.contains(&auth), "{auth:?}");
    }
    assert!(
        anthropic
            .variables
            .as_ref()
            .unwrap()
            .contains(&"ANTHROPIC_OAUTH_TOKEN".to_string())
    );
    let codex = by_id["openai-codex"];
    assert_eq!(codex.status, ProviderStatus::Restricted);
    assert!(codex.reason.as_deref().unwrap().contains("OAuth login"));
    assert_eq!(by_id["amazon-bedrock"].status, ProviderStatus::Restricted);
    assert!(
        by_id["amazon-bedrock"]
            .auth
            .contains(&ProviderAuth::CloudCredentials)
    );

    let fixture = match client.call(Request::ModelsList(ModelsListQuery {
        provider: Some("lade-fixture".into()),
    })) {
        Ok(ReplyData::ModelsList(catalog)) => catalog,
        other => panic!("models.list: {other:?}"),
    };
    assert_eq!(fixture.models.len(), 1);
    assert_eq!(fixture.models[0].id, "scripted");
    assert_eq!(fixture.models[0].thinking_levels, [ThinkingLevel::Off]);
    let anthropic_models = match client.call(Request::ModelsList(ModelsListQuery {
        provider: Some("anthropic".into()),
    })) {
        Ok(ReplyData::ModelsList(catalog)) => catalog.models,
        other => panic!("models.list: {other:?}"),
    };
    assert!(anthropic_models.iter().any(|m| m.reasoning));
}

#[test]
fn profile_import_copies_only_and_never_touches_the_source() {
    let env = Env::new("svc-import");
    let _service = env.start(FakeLauncher::shared());
    let mut client = env.connect();
    let source = env.root.path().join("home/.pi/agent");
    write(&source.join("AGENTS.md"), "User context\n");
    write(
        &source.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: d\n---\nx\n",
    );
    write(&source.join("prompts/review.md"), "Review $1\n");
    write(&source.join("extensions/ask.js"), EXTENSION);
    write(
        &source.join("models.json"),
        r#"{"providers":{"p":{"baseUrl":"https://h.example","apiKey":"!op read secret"}}}"#,
    );
    let before = snapshot_tree(&source);
    let import = || {
        Request::ProfileImport(ProfileImportCommand {
            source: WirePath::from_os_str(source.as_os_str()).unwrap(),
        })
    };
    let result = match client.call(import()) {
        Ok(ReplyData::ProfileImport(result)) => result,
        other => panic!("profile.import: {other:?}"),
    };
    assert_eq!(
        result.copied,
        [
            "AGENTS.md",
            "skills/demo",
            "prompts/review.md",
            "extensions/ask.js"
        ]
    );
    assert!(
        result
            .skipped
            .iter()
            .any(|s| s.starts_with("models.json") && s.contains("shell command"))
    );
    assert_eq!(
        snapshot_tree(&source),
        before,
        "the source profile is untouched"
    );
    let extension = result
        .inventory
        .entries
        .iter()
        .find(|e| e.kind == ResourceKind::Extension)
        .unwrap();
    assert_eq!(extension.state, ResourceState::PendingApproval);
    assert!(fs::read_to_string(profile(&env).join("AGENTS.md")).is_ok());

    let again = match client.call(import()) {
        Ok(ReplyData::ProfileImport(result)) => result,
        other => panic!("profile.import: {other:?}"),
    };
    assert!(again.copied.is_empty(), "nothing is overwritten");
    assert_eq!(snapshot_tree(&source), before);
}

#[test]
fn executable_resources_load_only_by_approved_hash_and_dialogs_keep_generations() {
    let env = Env::new("svc-resources");
    let (launcher, _) = best_launcher(&env);
    let _service = env.start(launcher);
    let mut client = env.connect();
    write(
        &profile(&env).join("AGENTS.md"),
        "Profile LADE-MARKER:profile\n",
    );
    write(&profile(&env).join("extensions/ask.js"), EXTENSION);
    let inventory = match client.call(Request::ResourcesList(lade_protocol::client::Empty {})) {
        Ok(ReplyData::ResourcesList(inventory)) => inventory,
        other => panic!("resources.list: {other:?}"),
    };
    assert!(!inventory.restrictions.is_empty());
    let pending = inventory
        .entries
        .iter()
        .find(|e| e.kind == ResourceKind::Extension)
        .unwrap()
        .clone();
    assert_eq!(pending.state, ResourceState::PendingApproval);
    let sha = pending.sha256.clone().unwrap();

    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Resources").objective_id;
    start_execution(&mut client, &objective);
    idle_session(&mut client, &objective);
    let first = wait_activity(&mut client, &objective, "Generation 1.");
    assert!(
        first.contains("Extensions: none"),
        "unapproved code is not loaded: {first}"
    );
    assert!(first.contains("Context: AGENTS.md"));

    let decide = |approve: bool, sha: &Digest| {
        Request::ResourceDecide(ResourceDecideCommand {
            name: "ask.js".into(),
            sha256: sha.clone(),
            approve,
        })
    };
    match client.call(decide(true, &sha)) {
        Ok(ReplyData::ResourceDecide(inventory)) => assert!(
            inventory
                .entries
                .iter()
                .any(|e| e.kind == ResourceKind::Extension && e.state == ResourceState::Active)
        ),
        other => panic!("resource.decide: {other:?}"),
    }
    let reloaded = match client.call(Request::SessionReload(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::SessionReload(state)) => state,
        other => panic!("session.reload: {other:?}"),
    };
    assert_eq!(reloaded.generation, Counter::new(2));
    let second = wait_activity(&mut client, &objective, "Generation 2.");
    assert!(second.contains("Extensions: ask.js"), "{second}");

    send(&mut client, &objective, "/ask now");
    let request = match client.wait_event("ui.request", |e| matches!(e, Event::UiRequest(_))) {
        Event::UiRequest(request) => request,
        _ => unreachable!(),
    };
    assert_eq!(request.generation, Counter::new(2));
    assert!(matches!(request.prompt, UiPrompt::Confirm { .. }));
    let respond = |generation: u64| {
        Request::UiRespond(UiRespondCommand {
            objective_id: objective.clone(),
            ui_id: request.ui_id.clone(),
            generation: Counter::new(generation),
            answer: UiAnswer::Confirm { confirmed: true },
        })
    };
    let stale = client.call(respond(1)).unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleExecution);
    assert!(matches!(
        client.call(respond(2)),
        Ok(ReplyData::UiRespond(_))
    ));
    let closed = client.wait_event("ui.closed", |e| matches!(e, Event::UiClosed(_)));
    assert!(matches!(closed, Event::UiClosed(c) if c.reason == UiCloseReason::Answered));
    wait_activity(&mut client, &objective, "confirmed=true");
    assert_eq!(
        client.call(respond(2)).unwrap_err().code,
        ErrorCode::NotFound
    );

    write(
        &profile(&env).join("extensions/ask.js"),
        &format!("{EXTENSION}// edited\n"),
    );
    assert_eq!(
        client.call(decide(true, &sha)).unwrap_err().code,
        ErrorCode::StaleReference,
        "an approval names exact current content"
    );
    wait_state(&mut client, &objective, ExecutionState::Idle);
    match client.call(Request::SessionReload(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::SessionReload(state)) => assert_eq!(state.generation, Counter::new(3)),
        other => panic!("session.reload: {other:?}"),
    }
    let third = wait_activity(&mut client, &objective, "Generation 3.");
    assert!(third.contains("Extensions: none"), "{third}");
}

#[test]
fn tree_fork_export_import_and_routing_follow_the_session() {
    let env = Env::new("svc-tree");
    let (launcher, _) = best_launcher(&env);
    let _service = env.start(launcher);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Tree").objective_id;
    start_execution(&mut client, &objective);
    idle_session(&mut client, &objective);
    send(
        &mut client,
        &objective,
        "first\nlade-fixture: write kept.txt one",
    );
    wait_activity(&mut client, &objective, "1 of 1 tool call(s) succeeded");
    wait_state(&mut client, &objective, ExecutionState::Idle);
    send(&mut client, &objective, "second");
    wait_activity(&mut client, &objective, "holds 2 user message(s)");
    wait_state(&mut client, &objective, ExecutionState::Idle);
    let kept = worktree(&env, &objective).join("kept.txt");
    assert_eq!(fs::read_to_string(&kept).unwrap(), "one");

    let tree = match client.call(Request::SessionTree(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::SessionTree(tree)) => tree,
        other => panic!("session.tree: {other:?}"),
    };
    let users: Vec<_> = tree
        .entries
        .iter()
        .filter(|e| e.kind == TreeEntryKind::User)
        .collect();
    assert_eq!(users.len(), 2);
    match client.call(Request::SessionNavigate(SessionNavigateCommand {
        objective_id: objective.clone(),
        entry_id: users[0].entry_id.clone(),
    })) {
        Ok(ReplyData::SessionNavigate(state)) => assert_ne!(state.leaf_id, tree.leaf_id),
        other => panic!("session.navigate: {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(&kept).unwrap(),
        "one",
        "tree navigation never rewinds files"
    );

    let before = detail(&mut client, &objective);
    let old = before.session.clone().unwrap();
    let forked = match client.call(Request::SessionFork(SessionForkCommand {
        objective_id: objective.clone(),
        entry_id: users[1].entry_id.clone(),
        position: ForkPosition::Before,
    })) {
        Ok(ReplyData::SessionFork(state)) => state,
        other => panic!("session.fork: {other:?}"),
    };
    let after = detail(&mut client, &objective);
    let new = after.session.clone().unwrap();
    assert_ne!(new.session_id, old.session_id);
    assert_eq!(
        new.association_revision.value(),
        old.association_revision.value() + 1
    );
    assert_eq!(forked.session_id.as_ref(), Some(&new.session_id));
    let stale = client
        .call(Request::FeedbackSubmit(FeedbackSubmitCommand {
            objective_id: objective.clone(),
            destination_session_id: old.session_id.clone(),
            association_revision: old.association_revision.clone(),
            drafts: vec![lade_protocol::client::DraftRevision {
                draft_id: new_id(),
                revision: Counter::new(1),
            }],
            mode: DeliveryMode::Prompt,
            note: None,
        }))
        .unwrap_err();
    assert_eq!(stale.code, ErrorCode::StaleAssociation);

    let exported = match client.call(Request::SessionExport(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(ReplyData::SessionExport(export)) => PathBuf::from(export.path.to_os_string()),
        other => panic!("session.export: {other:?}"),
    };
    let header = fs::read_to_string(&exported).unwrap();
    assert!(header.starts_with("{\"type\":\"session\""));
    let original = fs::read(&exported).unwrap();

    let imported = match client.call(Request::SessionImport(SessionImportCommand {
        objective_id: objective.clone(),
        path: WirePath::from_os_str(exported.as_os_str()).unwrap(),
    })) {
        Ok(ReplyData::SessionImport(state)) => state,
        other => panic!("session.import: {other:?}"),
    };
    assert_eq!(
        fs::read(&exported).unwrap(),
        original,
        "import never modifies its source"
    );
    let imported_detail = detail(&mut client, &objective);
    assert_eq!(imported_detail.lifecycle, Lifecycle::Active);
    assert!(
        imported_detail.approval.is_none(),
        "an import never restores approvals"
    );
    assert_eq!(
        imported_detail.branch, before.branch,
        "an import never changes Git state"
    );
    assert_eq!(
        imported.session_id.as_ref(),
        Some(&imported_detail.session.unwrap().session_id)
    );
    let not_session = env.root.path().join("not-a-session.jsonl");
    fs::write(&not_session, "{\"type\":\"message\"}\n").unwrap();
    assert_eq!(
        client
            .call(Request::SessionImport(SessionImportCommand {
                objective_id: objective.clone(),
                path: WirePath::from_os_str(not_session.as_os_str()).unwrap(),
            }))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest
    );
    assert_eq!(fs::read_to_string(&kept).unwrap(), "one");
}

#[test]
fn model_and_thinking_selection_is_persisted_per_objective() {
    check_model_and_thinking_selection(false);
}

#[test]
fn model_and_thinking_selection_with_the_test_launcher() {
    check_model_and_thinking_selection(true);
}

fn check_model_and_thinking_selection(use_test_launcher: bool) {
    let env = Env::new("svc-configure");
    let launcher = if use_test_launcher {
        NodeLauncher::shared()
    } else {
        best_launcher(&env).0
    };
    let _service = env.start(launcher);
    let mut client = env.connect();
    let ws = open_workspace(&mut client, &env.repo);
    let objective = create_objective(&mut client, &ws.workspace_id, "Configure").objective_id;
    start_execution(&mut client, &objective);
    idle_session(&mut client, &objective);
    let fixture = Model {
        provider: "lade-fixture".into(),
        id: "scripted".into(),
    };
    let configure = |model: Model, thinking| {
        Request::SessionConfigure(SessionConfigureCommand {
            objective_id: objective.clone(),
            model,
            thinking,
        })
    };
    match client.call(configure(fixture.clone(), ThinkingLevel::High)) {
        Ok(ReplyData::SessionConfigure(state)) => {
            assert_eq!(state.thinking, ThinkingLevel::Off, "pi clamps to the model");
        }
        other => panic!("session.configure: {other:?}"),
    }
    assert_eq!(
        detail(&mut client, &objective).session.unwrap().thinking,
        Some(ThinkingLevel::Off)
    );
    let other_provider = Model {
        provider: "anthropic".into(),
        id: "claude-x".into(),
    };
    assert_eq!(
        client
            .call(configure(other_provider.clone(), ThinkingLevel::Low))
            .unwrap_err()
            .code,
        ErrorCode::InvalidRequest,
        "a live session keeps its provider (egress and credentials are per launch)"
    );
    match client.call(Request::SessionCompact(SessionCompactCommand {
        objective_id: objective.clone(),
        instructions: None,
    })) {
        Ok(ReplyData::SessionCompact(_)) => {
            wait_activity(&mut client, &objective, "Context compaction (manual)");
        }
        Err(rejection) => assert_eq!(rejection.code, ErrorCode::Conflict),
        other => panic!("session.compact: {other:?}"),
    }
    match client.call(Request::ExecutionCancel(ObjectiveSelector {
        objective_id: objective.clone(),
    })) {
        Ok(_) => {}
        other => panic!("execution.cancel: {other:?}"),
    }
    wait_state(&mut client, &objective, ExecutionState::Cancelled);
    match client.call(configure(fixture.clone(), ThinkingLevel::Minimal)) {
        Ok(ReplyData::SessionConfigure(state)) => {
            assert_eq!(state.thinking, ThinkingLevel::Minimal);
        }
        other => panic!("session.configure while stopped: {other:?}"),
    }
    assert_eq!(
        client
            .call(configure(other_provider, ThinkingLevel::Low))
            .unwrap_err()
            .code,
        ErrorCode::ExecutionUnavailable,
        "a provider without credentials is refused synchronously"
    );
    assert_eq!(
        detail(&mut client, &objective).session.unwrap().thinking,
        Some(ThinkingLevel::Minimal)
    );
}

#[test]
fn laded_warns_when_the_test_fixture_provider_is_enabled() {
    let env = Env::new("svc-fixture-warning");
    let daemon = env.spawn_laded(None, &[]);
    let log = daemon.log_text();
    assert!(
        log.contains("warning: offline test model lade-fixture enabled"),
        "{log}"
    );
}
