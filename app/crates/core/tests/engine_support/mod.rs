//! The Python engine tests' `engine` fixture (backend/tests/test_workflow_safety.py): an engine on a
//! temp database with task TEST-1 (every node but the orchestrator, PENDING, manual, checkpoint SHA,
//! approved requirements), fake git and fake terminals.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use agentflow_core::api::{self, App};
use agentflow_core::config::{Config, Locations};
use agentflow_core::db::table::{self, Tracked};
use agentflow_core::db::{
    AgentAccessRow, AgentSessionRow, Database, DbTime, Json, NodeRunRow, RequirementsApprovalRow, TaskRow,
    WorkflowNodeRow,
};
use agentflow_core::engine::store::TaskState;
use agentflow_core::engine::{Engine, EngineSetup, rules, spec};
use agentflow_core::events::Broadcaster;
use agentflow_core::harness::{HarnessAdapter, HarnessRegistry};
use agentflow_core::process::ProcessError;
use agentflow_core::process::worktree::{WorktreeHolder, WorktreeKiller, WorktreeProcess};
use agentflow_core::terminal::PaneInfo;
use agentflow_core::terminal::Terminals;
use agentflow_core::terminal::testing::FakeTerminals;
use agentflow_core::vcs::{FakeGit, Outcome};
use axum::body::Body;
use axum::http::Request;
use serde_json::{Value, json};
use tower::ServiceExt;

pub const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub struct Harness {
    pub engine: Arc<Engine>,
    pub git: Arc<FakeGit>,
    pub terminals: Arc<FakeTerminals>,
    pub killer: Arc<FakeKiller>,
    pub dir: tempfile::TempDir,
}

/// Python's monkeypatched `stop_processes_in`: records directories, reports `kills`.
#[derive(Default)]
pub struct FakeKiller {
    pub kills: std::sync::Mutex<Vec<WorktreeProcess>>,
    pub calls: std::sync::Mutex<Vec<PathBuf>>,
    /// What `holders` reports (an editor with the folder open).
    pub holders: std::sync::Mutex<Vec<WorktreeHolder>>,
}

#[async_trait::async_trait]
impl WorktreeKiller for FakeKiller {
    async fn stop_processes_in(&self, directory: &std::path::Path) -> Result<Vec<WorktreeProcess>, ProcessError> {
        self.calls.lock().unwrap().push(directory.to_path_buf());
        Ok(self.kills.lock().unwrap().clone())
    }

    async fn holders(&self, _directory: &std::path::Path) -> Result<Vec<WorktreeHolder>, ProcessError> {
        Ok(self.holders.lock().unwrap().clone())
    }
}

impl Harness {
    pub fn tmp(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    pub fn config(&self) -> Arc<Config> {
        self.engine.config()
    }

    /// Change the live configuration, as the Python tests assigned engine.config fields.
    pub fn configure(&self, change: impl FnOnce(&mut Config)) {
        let mut config = (*self.engine.config()).clone();
        change(&mut config);
        self.engine.set_config(Arc::new(config));
    }

    pub fn worktree_dir(&self) -> PathBuf {
        self.config().worktree_dir(&self.engine.locations)
    }

    pub fn task_dir(&self) -> PathBuf {
        self.config().task_dir(&self.engine.locations)
    }

    pub fn load(&self) -> TaskState {
        self.engine.db.session(|tx| TaskState::load(tx, "TEST-1")).unwrap()
    }

    /// Change and flush TEST-1 (Python: `with engine.db.session(): task = _load_task(...)`).
    pub fn with_task<T>(&self, change: impl FnOnce(&mut TaskState) -> T) -> T {
        let mut state = self.load();
        let result = change(&mut state);
        self.engine.db.session(|tx| state.flush(tx)).unwrap();
        result
    }

    pub fn set_status(&self, status: &str) {
        self.with_task(|state| state.task.status = status.into());
    }

    /// Python's `add_run`: the node takes `state` and gets a new run in that state.
    pub fn add_run(&self, key: &str, state: &str, result: Option<Value>) -> i64 {
        self.with_task(|task| {
            let index = task.node_index(key).unwrap();
            task.nodes[index].state = state.into();
            let node_id = task.nodes[index].id;
            let count = task.runs.iter().filter(|r| r.node_id == node_id).count() as i64 + 1;
            let mut run = Tracked::new(NodeRunRow {
                id: 0,
                task_id: task.task.id,
                node_id,
                run_number: count,
                iteration: task.task.review_iteration,
                state: state.into(),
                harness: None,
                model: None,
                reasoning: None,
                run_dir: None,
                prompt_path: None,
                result_path: None,
                expected_sha: Some(SHA.into()),
                result: Json(result.unwrap_or(Value::Null)),
                exit_code: None,
                error: None,
                started_at: DbTime::now(),
                completed_at: None,
            });
            self.engine.db.session(|tx| run.flush(tx)).unwrap();
            let id = run.id;
            task.runs.push(run);
            id
        })
    }

    /// Every configured harness usable, its CLI state under a temp home (Python's tests replaced
    /// the adapters with stubs; these build argv only, and FakeTerminals runs nothing).
    pub fn usable_harnesses(&self) -> PathBuf {
        let config = self.config();
        let home = self.tmp().join("home");
        let mut registry = HarnessRegistry::new(&config);
        registry.adapters = config
            .harnesses
            .iter()
            .map(|(key, cfg)| (key.clone(), HarnessAdapter::with_home(key, cfg.clone(), home.clone())))
            .collect();
        registry.assume_capabilities();
        for caps in registry.capabilities.values_mut() {
            caps.enabled = true;
            caps.available = true;
        }
        self.engine.set_harnesses(Arc::new(registry));
        home
    }

    /// Python's `launchable` fixture: harnesses usable, the worktree present, the Architect READY.
    /// Every role runs on Claude, which names its session upfront (Codex would be polled for its).
    pub fn launchable(&self) -> PathBuf {
        self.configure(|config| {
            for agent in [&mut config.agents.implementer, &mut config.agents.code_reviewer] {
                agent.harness = "claude".into();
            }
        });
        let home = self.usable_harnesses();
        std::fs::create_dir_all(&self.load().task.worktree_path).unwrap();
        self.with_task(|state| {
            let index = state.node_index(spec::ARCHITECT).unwrap();
            state.nodes[index].state = "READY".into();
        });
        home
    }

    /// A second engine on the same database and terminals, as after a backend restart.
    pub fn restarted(&self) -> Arc<Engine> {
        self.restarted_with_flowctl(PathBuf::from("flowctl"))
    }

    pub fn restarted_with_flowctl(&self, flowctl: PathBuf) -> Arc<Engine> {
        self.rebuilt(flowctl, None)
    }

    /// A second engine whose tasks may have left sessions in `zellij` (agents now run embedded).
    pub fn restarted_with_leftover_zellij(&self, zellij: Arc<FakeTerminals>) -> Arc<Engine> {
        self.rebuilt(PathBuf::from("flowctl"), Some(zellij))
    }

    fn rebuilt(&self, flowctl: PathBuf, leftover_zellij: Option<Arc<FakeTerminals>>) -> Arc<Engine> {
        let config = (*self.engine.config()).clone();
        let db = Arc::new(Database::open(&config.database_file(&self.engine.locations)).unwrap());
        db.create_all().unwrap();
        Engine::new(EngineSetup {
            locations: self.engine.locations.clone(),
            config: Arc::new(config),
            harnesses: self.engine.harnesses(),
            db,
            git: self.git.clone(),
            terminals: self.terminals.clone(),
            leftover_zellij: leftover_zellij.map(|z| z as Arc<dyn Terminals>),
            worktree_killer: self.killer.clone(),
            broadcaster: Arc::new(Broadcaster::default()),
            api_base_url: self.engine.api_base_url.clone(),
            flowctl,
        })
        .unwrap()
    }

    /// The text of the newest run's launch script.
    pub fn launch_script(&self) -> String {
        let run = self.load().runs.last().unwrap().row.clone();
        let dir = PathBuf::from(run.run_dir.unwrap());
        let script = if cfg!(windows) { dir.join("launch.ps1") } else { dir.join("launch.sh") };
        String::from_utf8(std::fs::read(script).unwrap()).unwrap()
    }

    pub fn count(&self, table_name: &str) -> i64 {
        self.engine
            .db
            .session(|tx| tx.query_row(&format!("SELECT count(*) FROM {table_name}"), [], |r| r.get(0)))
            .unwrap()
    }

    /// A paired router over this engine; returns (status, body) of one call.
    pub async fn route(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let app = App::around(self.engine.clone(), self.tmp().join("config.json"));
        let code = app.human.lock().unwrap().pairing_code.clone();
        let pair = Request::builder()
            .method("POST")
            .uri("/api/auth/pair")
            .header("host", "127.0.0.1")
            .header("origin", "http://127.0.0.1")
            .header("content-type", "application/json")
            .body(Body::from(json!({"code": code}).to_string()))
            .unwrap();
        let paired = api::router(app.clone()).oneshot(pair).await.unwrap();
        let cookie = paired.headers()["set-cookie"].to_str().unwrap().split(';').next().unwrap().to_string();
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1")
            .header("origin", "http://127.0.0.1")
            .header("cookie", cookie);
        let body = match body {
            Some(value) => {
                request = request.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = api::router(app).oneshot(request.body(body).unwrap()).await.unwrap();
        let status = response.status().as_u16();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

pub async fn engine() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path();
    let config = Config::from_value(json!({
        "app": {"dataDirectory": tmp, "databasePath": tmp.join("test.db")},
        "workspace": {"taskDirectory": tmp.join("tasks"), "worktreeDirectory": tmp.join("trees")},
        "github": {"prSync": false},
    }))
    .unwrap();
    let locations = Locations::from_env_with_checkout(None);
    let db = Arc::new(Database::open(&config.database_file(&locations)).unwrap());
    db.create_all().unwrap();
    let git = Arc::new(FakeGit::default());
    git.returns("head_sha", Outcome::Ok(json!(SHA)));
    git.returns("is_dirty", Outcome::Ok(json!(false)));
    let terminals = Arc::new(FakeTerminals::default());
    terminals.ensure_session("agentflow-TEST-1").await.unwrap();
    let mut harnesses = HarnessRegistry::new(&config);
    harnesses.assume_capabilities();
    let killer = Arc::new(FakeKiller::default());
    let engine = Engine::new(EngineSetup {
        locations: locations.clone(),
        config: Arc::new(config.clone()),
        harnesses: Arc::new(harnesses),
        db: db.clone(),
        git: git.clone(),
        terminals: terminals.clone(),
        leftover_zellij: None,
        worktree_killer: killer.clone(),
        broadcaster: Arc::new(Broadcaster::default()),
        api_base_url: "http://127.0.0.1:8765".into(),
        flowctl: PathBuf::from("flowctl"),
    })
    .unwrap();

    let now = DbTime::now();
    let mut state = TaskState::new(TaskRow {
        id: 0,
        jira_key: "TEST-1".into(),
        api_budget_usd: None,
        jira_title: None,
        repository_path: tmp.to_string_lossy().into_owned(),
        repository_root: tmp.to_string_lossy().into_owned(),
        project_relative_path: None,
        base_branch: "main".into(),
        branch_name: "agent/TEST-1".into(),
        worktree_path: tmp.join("tree").to_string_lossy().into_owned(),
        zellij_session_name: "agentflow-TEST-1".into(),
        task_dir: tmp.join("tasks").join("TEST-1").to_string_lossy().into_owned(),
        additional_context: None,
        status: "ACTIVE".into(),
        status_message: None,
        review_iteration: 1,
        max_review_iterations: 1,
        max_requirements_revisions: 3,
        current_sha: Some(SHA.into()),
        pr_url: None,
        pr_created_at: None,
        merged_at: None,
        pr_sync_paused: false,
        merged_head_sha: None,
        pr_base_branch: None,
        archived_at: None,
        project_id: None,
        agent_overrides: Json(json!({})),
        created_at: now,
        updated_at: now,
    });
    // This fixture represents an existing task created before worker orchestration.
    for node in spec::nodes(&config).into_iter().filter(|n| n.key != spec::ORCHESTRATOR) {
        state.add_node(WorkflowNodeRow {
            id: 0,
            task_id: 0,
            key: node.key.into(),
            kind: node.kind.as_str().into(),
            label: node.label.into(),
            role: node.role.map(|r| r.as_str().into()),
            state: "PENDING".into(),
            execution_mode: "manual".into(),
            iteration: 1,
            checkpoint_sha: Some(SHA.into()),
            message: None,
            error: None,
            config_override: Json(json!({})),
            updated_at: now,
        });
    }
    db.session(|tx| state.flush(tx)).unwrap();
    let workspace = agentflow_core::engine::snapshot::workspace(&state.task);
    workspace.create(&json!({}), None).unwrap();
    for name in ["requirements.md", "acceptance-criteria.md", "implementation-plan.md"] {
        std::fs::write(workspace.requirements_dir.join(name), "Approved scope with required behavior and tests")
            .unwrap();
    }
    let documents = workspace.requirement_files().unwrap();
    let mut approval = RequirementsApprovalRow {
        uid: agentflow_core::db::new_uid(),
        task_id: state.task.id,
        digest: rules::documents_digest(&documents),
        documents: Json(serde_json::to_value(&documents).unwrap()),
        approved_at: now,
    };
    db.session(|tx| table::insert(tx, &mut approval)).unwrap();
    Harness { engine, git, terminals, killer, dir }
}

/// Python's `add_session`: a running implementer session on tab 7 with a scoped credential.
pub fn add_session(harness: &Harness, run_id: i64) {
    harness.with_task(|state| {
        state.agents.push(Tracked::new(AgentSessionRow {
            id: 0,
            task_id: state.task.id,
            role: "implementer".into(),
            harness: "codex".into(),
            model: None,
            reasoning: None,
            zellij_tab_name: "Implementer".into(),
            zellij_tab_id: Some(7),
            zellij_pane_id: Some("terminal_7".into()),
            harness_session_id: None,
            status: "running".into(),
            screen_state: None,
            screen_state_at: None,
            current_run_id: Some(run_id),
            started_at: DbTime::now(),
            ended_at: None,
        }));
    });
    let task_id = harness.load().task.id;
    let mut access = AgentAccessRow {
        id: agentflow_core::db::new_uid(),
        task_id,
        run_id,
        role: "implementer".into(),
        token_hash: agentflow_core::api::auth::digest("test-token"),
    };
    harness.engine.db.session(|tx| table::insert(tx, &mut access)).unwrap();
}

pub fn tab_seven(harness: &Harness) {
    harness.terminals.add_pane(
        "agentflow-TEST-1",
        PaneInfo {
            pane_id: "terminal_7".into(),
            tab_id: 7,
            tab_name: "Implementer".into(),
            exited: false,
            exit_status: None,
            command: None,
        },
    );
}
