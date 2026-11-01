//! The task list and every snapshot built from a copy of a real database, written to a file for
//! comparison with the Python backend's. Prints nothing
//! from the data. Ignored; run with
//!
//!     AGENTFLOW_REAL_CONFIG=<config.json> AGENTFLOW_SNAPSHOT_DB_COPY=<copy.db> AGENTFLOW_SNAPSHOT_OUT=<file>
//!     AGENTFLOW_SNAPSHOT_DATA=<scratch dir> cargo test --test real_snapshot -- --ignored

use std::path::PathBuf;
use std::sync::Arc;

use agentflow_core::config::{Locations, read_config};
use agentflow_core::db::Database;
use agentflow_core::engine::{Engine, EngineSetup};
use agentflow_core::events::Broadcaster;
use agentflow_core::harness::HarnessRegistry;
use agentflow_core::terminal::zellij::ZellijService;
use agentflow_core::vcs::FakeGit;
use serde_json::{Map, Value, json};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name}"))
}

#[tokio::test]
#[ignore = "needs a copy of a real database; see the module docs"]
async fn snapshots_of_a_real_database() {
    let copy = PathBuf::from(var("AGENTFLOW_SNAPSHOT_DB_COPY"));
    let mut config = read_config(&PathBuf::from(var("AGENTFLOW_REAL_CONFIG"))).unwrap();
    config.app.database_path = copy.to_string_lossy().into_owned();
    // Engine::new writes flowctl shims into the data directory: keep them out of the real one.
    config.app.data_directory = var("AGENTFLOW_SNAPSHOT_DATA");
    let locations = Locations::from_env();
    let db = Arc::new(Database::open(&copy).unwrap());
    let engine = Engine::new(EngineSetup {
        locations,
        harnesses: Arc::new(HarnessRegistry::new(&config)), // no capabilities: nothing is run
        terminals: Arc::new(ZellijService::new(config.zellij.clone())), // asked only for attach text
        config: Arc::new(config),
        db,
        git: Arc::new(FakeGit::default()),
        leftover_zellij: None,
        worktree_killer: Arc::new(agentflow_core::process::worktree::SystemKiller),
        broadcaster: Arc::new(Broadcaster::default()),
        api_base_url: "http://127.0.0.1:8765".into(),
        flowctl: PathBuf::from("flowctl"),
    })
    .unwrap();
    let mut lists = Map::new();
    for archived in ["exclude", "include", "only"] {
        lists.insert(archived.into(), Value::Array(engine.list_tasks(archived).await.unwrap()));
    }
    let mut snapshots = Map::new();
    for task in lists["include"].as_array().unwrap() {
        let key = task["jiraKey"].as_str().unwrap();
        snapshots.insert(key.into(), engine.snapshot(key).unwrap());
    }
    println!("rust: {} snapshots", snapshots.len());
    std::fs::write(var("AGENTFLOW_SNAPSHOT_OUT"), json!({"list": lists, "snapshots": snapshots}).to_string()).unwrap();
}
