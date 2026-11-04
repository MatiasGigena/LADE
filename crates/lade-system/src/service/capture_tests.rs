use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::sync::{Arc, OnceLock, mpsc};

use lade_protocol::client::{Intent, ObjectiveCreateCommand, RequestName, Settings};
use lade_protocol::types::WirePath;

use super::*;
use crate::adapters::artifacts::ArtifactStore;
use crate::service::{Paths, ResourceLimits, execution, inspect::adapters_support, limits};
use crate::storage::Store;
use crate::supervision::{
    LaunchError, LaunchRequest, SupervisorEvent, WorkerControl, WorkerLauncher,
};

struct NoLaunch;

impl WorkerLauncher for NoLaunch {
    fn launch(
        &self,
        _request: LaunchRequest,
        _events: mpsc::SyncSender<SupervisorEvent>,
    ) -> Result<Box<dyn WorkerControl>, LaunchError> {
        panic!("capture registration does not launch workers")
    }
}

#[test]
fn reused_capture_registration_keeps_its_original_identity_after_a_newer_capture() {
    let root = adapters_support::TempDir::new("capture-completion-order");
    let repo = adapters_support::init_repo(root.path(), "sha1");
    let (queue_tx, _) = mpsc::sync_channel(16);
    let (supervisor_tx, _) = mpsc::sync_channel(16);
    let paths = Paths {
        state: root.path().to_path_buf(),
        worktrees: root.path().join("worktrees"),
        sessions: root.path().join("sessions"),
        verify: root.path().join("verify"),
        tmp: root.path().join("tmp"),
        run: root.path().join("run"),
        sockets: root.path().join("sockets"),
    };
    crate::support::fs::ensure_private_dir(&paths.tmp).unwrap();
    let mut service = Service {
        store: Store::open(&root.path().join("lade.db")).unwrap(),
        artifacts: Arc::new(
            ArtifactStore::open(&root.path().join("artifacts"), &paths.tmp).unwrap(),
        ),
        paths,
        large_blobs: Arc::default(),
        launcher: Arc::new(NoLaunch),
        supervisor_tx,
        queue_tx,
        pool: None,
        long: None,
        reaper: None,
        epoch: wire_id(new_uuid()),
        sequence: 0,
        controller: None,
        workspaces: BTreeMap::new(),
        objectives: BTreeMap::new(),
        runtime: HashMap::new(),
        settings: Settings {
            verification_commands: Vec::new(),
            default_model: None,
        },
        target_cache: HashMap::new(),
        pending: HashMap::new(),
        next_token: 1,
        activity_seq: HashMap::new(),
        input_seq: 0,
        live: execution::LiveActivity::default(),
        ticks: 0,
        sandbox: OnceLock::new(),
        catalog: super::super::models::CatalogCache::default(),
        extension_approvals: HashMap::new(),
        agent_sessions: super::super::agent_session::AgentSessions::default(),
        terminals: BTreeMap::new(),
        terminals_opening: 0,
        stopping: false,
        limits: ResourceLimits::default(),
        free_space: limits::Storage::default(),
        recovery_notes: Vec::new(),
        integrating: HashMap::new(),
        held: None,
        storage_limit: crate::adapters::sandbox::WRITABLE_STORAGE_BYTES,
        storage_check: false,
        cgroups: None,
        test_fixture_provider: false,
    };
    let ReplyData::WorkspaceOpen(workspace) = service
        .workspace_open(&WorkspaceOpenCommand {
            path: WirePath::from_os_str(repo.as_os_str()).unwrap(),
        })
        .unwrap()
    else {
        panic!("expected workspace")
    };
    let mut ctx = ReqCtx {
        conn: 1,
        request_id: wire_id(new_uuid()),
        name: RequestName::ObjectiveCreate,
        op: None,
        recorded: false,
    };
    let ReplyData::ObjectiveCreate(objective) = service
        .objective_create(
            &mut ctx,
            &ObjectiveCreateCommand {
                workspace_id: workspace.workspace_id,
                title: "Capture completion order".into(),
                intent: Intent {
                    outcome: "Capture identifiers resolve the bytes they name".into(),
                    constraints: String::new(),
                    acceptance: String::new(),
                },
                integration_target: "main".into(),
                start: Start::Target,
            },
        )
        .unwrap()
    else {
        panic!("expected objective")
    };
    let oid = service.objective_id(&objective.objective_id).unwrap();
    let input = service
        .capture_input(service.obj(oid).unwrap(), repo.clone(), Vec::new())
        .unwrap();
    let original = jobs::capture(&input).unwrap();
    service.register_capture(oid, &original).unwrap();

    // Two independent capture requests can run on the general pool. Hold the
    // reused result while another request captures and registers changed bytes.
    let reused_input = service
        .capture_input(service.obj(oid).unwrap(), repo.clone(), Vec::new())
        .unwrap();
    let reused = jobs::capture(&reused_input).unwrap();
    assert!(reused.reused);
    assert_eq!(reused.capture_id, original.capture_id);
    fs::write(
        repo.join("README"),
        "changed while the first result waits\n",
    )
    .unwrap();
    let changed_input = service
        .capture_input(service.obj(oid).unwrap(), repo, Vec::new())
        .unwrap();
    let changed = jobs::capture(&changed_input).unwrap();
    assert!(!changed.reused);
    let changed_row = service.register_capture(oid, &changed).unwrap();
    assert_ne!(changed_row.tree, reused.tree);

    let reused_row = service.register_capture(oid, &reused).unwrap();
    let ReplyData::ChangesList(list) = service
        .working_change_list(oid, &reused, &reused_row)
        .unwrap()
    else {
        panic!("expected changes")
    };
    let returned_id = CaptureId::new(parse_uuid(list.capture_id.as_ref().unwrap()).unwrap());
    let retained = storage::load_capture(service.store.conn(), returned_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.tree, reused.tree,
        "the capture ID in the reply must resolve to the tree used for its files"
    );
    assert_eq!(retained.target_commit, reused.target);
    assert_eq!(retained.id, original.capture_id);
    assert_eq!(retained.counts, jobs::counts(&reused.files));
}
