//! db/ against databases the Python backend creates, upgrades and fills
//! (contract/golden/db, recorded by contract/record_python_goldens.py).

use std::path::{Path, PathBuf};

use agentflow_core::db::{
    AgentAccessRow, AgentQuestionRow, AgentSessionRow, ChatThreadRow, Database, DbError, DbTime, DeletedRunSpendRow,
    ImplementationWorkItemRow, Json, NodeRunRow, ProcessLeaseRow, ProjectRow, RequirementsApprovalRow,
    ReviewFindingRow, RunEvidenceRow, TaskArtifactRow, TaskRow, WorkflowEventRow, WorkflowNodeRow, table,
};

#[test]
fn test_lineage_migration_upgrades_an_existing_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("legacy.db")).unwrap();
    db.create_all().unwrap();
    db.session(|tx|tx.execute_batch("ALTER TABLE review_findings DROP COLUMN prior_finding_id; ALTER TABLE review_findings DROP COLUMN occurrence; ALTER TABLE review_findings DROP COLUMN resolution; ALTER TABLE review_findings DROP COLUMN response; ALTER TABLE review_findings DROP COLUMN response_note;")).unwrap();
    db.create_all().unwrap();
    db.create_all().unwrap();
    let columns: Vec<(String, bool)> = db
        .session(|tx| {
            let mut s = tx.prepare("PRAGMA table_info(review_findings)")?;
            s.query_map([], |r| Ok((r.get(1)?, r.get(3)?)))?.collect()
        })
        .unwrap();
    for name in ["prior_finding_id", "occurrence", "resolution", "response", "response_note"] {
        assert!(columns.iter().any(|(n, _)| n == name));
    }
    assert!(columns.iter().find(|(n, _)| n == "occurrence").unwrap().1);
}
use rusqlite::Connection;
use serde_json::{Map, Value, json};

fn golden(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contract/golden/db").join(name)
}

fn read_golden(name: &str) -> String {
    std::fs::read_to_string(golden(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// The same normalization as record_python_goldens.normalized_schema: what SQLite reports about
/// columns, indexes and foreign keys, independent of how the DDL was written.
fn normalized_schema(path: &Path) -> Value {
    let db = Connection::open(path).unwrap();
    let strings = |sql: &str| -> Vec<String> {
        let mut statement = db.prepare(sql).unwrap();
        statement.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    let mut schema = Map::new();
    for table in strings("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
    {
        let mut statement = db.prepare(&format!("PRAGMA table_info('{table}')")).unwrap();
        let columns: Vec<Value> = statement
            .query_map([], |r| {
                Ok(json!({
                    "name": r.get::<_, String>(1)?, "type": r.get::<_, String>(2)?, "notnull": r.get::<_, i64>(3)? != 0,
                    "default": r.get::<_, Option<String>>(4)?, "pk": r.get::<_, i64>(5)?,
                }))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();

        let mut statement = db.prepare(&format!("PRAGMA index_list('{table}')")).unwrap();
        let list: Vec<(String, bool, String)> = statement
            .query_map([], |r| Ok((r.get(1)?, r.get::<_, i64>(2)? != 0, r.get(3)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut indexes: Vec<(String, Value)> = list
            .into_iter()
            .map(|(name, unique, origin)| {
                let cols = strings(&format!("SELECT name FROM pragma_index_info('{name}')"));
                let key = if origin == "c" { name } else { format!("<{origin}:{}>", cols.join(",")) };
                (key, json!({"unique": unique, "origin": origin, "columns": cols}))
            })
            .collect();
        indexes.sort_by(|a, b| a.0.cmp(&b.0));

        let mut statement = db.prepare(&format!("PRAGMA foreign_key_list('{table}')")).unwrap();
        let mut foreign_keys: Vec<Vec<String>> = statement
            .query_map([], |r| Ok(vec![r.get(2)?, r.get(3)?, r.get(4)?]))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        foreign_keys.sort();

        schema.insert(
            table,
            json!({"columns": columns, "indexes": Map::from_iter(indexes), "foreignKeys": foreign_keys}),
        );
    }
    Value::Object(schema)
}

fn assert_schema(path: &Path, golden_name: &str) {
    let expected: Value = serde_json::from_str(&read_golden(golden_name)).unwrap();
    let actual = normalized_schema(path);
    let (Value::Object(expected), Value::Object(actual)) = (&expected, &actual) else { unreachable!() };
    let mut differences = Vec::new();
    for table in expected.keys().chain(actual.keys().filter(|t| !expected.contains_key(*t))) {
        if expected.get(table) != actual.get(table) {
            differences.push(format!(
                "{table}:\n  python: {}\n  rust:   {}",
                expected.get(table).map_or("missing".into(), Value::to_string),
                actual.get(table).map_or("missing".into(), Value::to_string)
            ));
        }
    }
    assert!(differences.is_empty(), "schema differs from {golden_name}:\n{}", differences.join("\n"));
}

#[test]
fn fresh_database_has_the_python_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("fresh.db");
    let db = Database::open(&path).unwrap();
    db.create_all().unwrap();
    db.create_all().unwrap(); // idempotent
    drop(db);
    assert_schema(&path, "schema-fresh.json");
    let mode: String = Connection::open(&path).unwrap().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn old_database_is_upgraded_like_python() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    Connection::open(&path).unwrap().execute_batch(&read_golden("old-shape.sql")).unwrap();
    let db = Database::open(&path).unwrap();
    db.create_all().unwrap();

    let questions: Vec<(i64, String, Value, Option<String>)> = db
        .session(|tx| {
            let mut statement = tx.prepare("SELECT id, uid, choices, answer FROM agent_questions ORDER BY id")?;
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Json>(2)?.0, r.get(3)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    drop(db);
    assert_schema(&path, "schema-upgraded.json");

    // Backfilled uids are random: distinct, uuid-shaped. The rest matches what Python left.
    assert!(questions.iter().all(|q| q.1.len() == 36));
    assert_ne!(questions[0].1, questions[1].1);
    let expected: Value = serde_json::from_str(&read_golden("old-shape-questions-after-upgrade.json")).unwrap();
    let actual: Vec<Value> = questions
        .iter()
        .map(|(id, _, choices, answer)| json!({"id": id, "choices": pyjson::dumps_compact(choices), "answer": answer}))
        .collect();
    assert_eq!(Value::Array(actual), expected);
}

/// Every table's rows read into their typed structs, with every datetime and JSON value written
/// back in exactly the text Python stored.
fn read_everything(db: &Database) -> Vec<(String, usize)> {
    fn read<T>(
        tx: &rusqlite::Transaction,
        table: &str,
        from_row: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> usize {
        let mut statement = tx.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let rows: Vec<T> = statement
            .query_map([], from_row)
            .unwrap()
            .map(|row| row.unwrap_or_else(|e| panic!("{table}: {e}")))
            .collect();
        rows.len()
    }
    db.session(|tx| -> rusqlite::Result<_> {
        Ok(vec![
            (TaskRow::TABLE.to_string(), read(tx, TaskRow::TABLE, TaskRow::from_row)),
            (WorkflowNodeRow::TABLE.into(), read(tx, WorkflowNodeRow::TABLE, WorkflowNodeRow::from_row)),
            (NodeRunRow::TABLE.into(), read(tx, NodeRunRow::TABLE, NodeRunRow::from_row)),
            (AgentSessionRow::TABLE.into(), read(tx, AgentSessionRow::TABLE, AgentSessionRow::from_row)),
            (ReviewFindingRow::TABLE.into(), read(tx, ReviewFindingRow::TABLE, ReviewFindingRow::from_row)),
            (WorkflowEventRow::TABLE.into(), read(tx, WorkflowEventRow::TABLE, WorkflowEventRow::from_row)),
            (TaskArtifactRow::TABLE.into(), read(tx, TaskArtifactRow::TABLE, TaskArtifactRow::from_row)),
            (AgentQuestionRow::TABLE.into(), read(tx, AgentQuestionRow::TABLE, AgentQuestionRow::from_row)),
            (AgentAccessRow::TABLE.into(), read(tx, AgentAccessRow::TABLE, AgentAccessRow::from_row)),
            (ProcessLeaseRow::TABLE.into(), read(tx, ProcessLeaseRow::TABLE, ProcessLeaseRow::from_row)),
            (RunEvidenceRow::TABLE.into(), read(tx, RunEvidenceRow::TABLE, RunEvidenceRow::from_row)),
            (
                RequirementsApprovalRow::TABLE.into(),
                read(tx, RequirementsApprovalRow::TABLE, RequirementsApprovalRow::from_row),
            ),
            (DeletedRunSpendRow::TABLE.into(), read(tx, DeletedRunSpendRow::TABLE, DeletedRunSpendRow::from_row)),
            (
                ImplementationWorkItemRow::TABLE.into(),
                read(tx, ImplementationWorkItemRow::TABLE, ImplementationWorkItemRow::from_row),
            ),
            (ChatThreadRow::TABLE.into(), read(tx, ChatThreadRow::TABLE, ChatThreadRow::from_row)),
            (ProjectRow::TABLE.into(), read(tx, ProjectRow::TABLE, ProjectRow::from_row)),
        ])
    })
    .unwrap()
}

/// Every DATETIME and JSON value in the file, as (table.column, stored text).
fn stored_values(path: &Path, declared: &str) -> Vec<(String, String)> {
    let db = Connection::open(path).unwrap();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut values = Vec::new();
    for table in tables {
        let columns: Vec<(String, String)> = db
            .prepare(&format!("PRAGMA table_info('{table}')"))
            .unwrap()
            .query_map([], |r| Ok((r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for (column, kind) in columns.into_iter().filter(|(_, kind)| kind == declared) {
            let _ = kind;
            let mut statement =
                db.prepare(&format!("SELECT \"{column}\" FROM {table} WHERE \"{column}\" IS NOT NULL")).unwrap();
            for text in statement.query_map([], |r| r.get::<_, String>(0)).unwrap() {
                values.push((format!("{table}.{column}"), text.unwrap()));
            }
        }
    }
    values
}

#[test]
fn reads_a_database_python_filled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("populated.db");
    // iterdump writes tables alphabetically, so references point forward; the bundled SQLite
    // enforces foreign keys by default, unlike Python's sqlite3, which restored this dump.
    let loader = Connection::open(&path).unwrap();
    loader.pragma_update(None, "foreign_keys", "OFF").unwrap();
    loader.execute_batch(&read_golden("populated.sql")).unwrap();
    drop(loader);
    let before = normalized_schema(&path);

    let db = Database::open(&path).unwrap();
    db.create_all().unwrap();
    let counts = read_everything(&db);
    drop(db);
    assert_eq!(normalized_schema(&path), before, "opening a current database must not change its schema");
    for (table, count) in &counts {
        assert!(*count > 0, "the fixture should have rows in {table}");
    }

    assert!(stored_values(&path, "DATETIME").len() > 100);
    assert!(stored_values(&path, "JSON").len() > 50);
    assert_round_trips(&path);
}

#[test]
fn session_commits_on_ok_and_rolls_back_on_err() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("s.db")).unwrap();
    db.create_all().unwrap();
    let insert = |tx: &rusqlite::Transaction, key: &str| {
        tx.execute(
            "INSERT INTO deleted_run_spend (jira_key, run_id, charged_at, billing_mode, cost_usd) VALUES (?1, 1, ?2, 'unknown', NULL)",
            (key, DbTime::now()),
        )
    };
    db.session(|tx| insert(tx, "KEPT-1")).unwrap();
    let failed: Result<(), DbError> = db.session(|tx| {
        insert(tx, "ROLLED-BACK-1")?;
        Err(DbError::Sqlite(rusqlite::Error::InvalidQuery))
    });
    assert!(failed.is_err());
    let keys: Vec<String> = db
        .session(|tx| {
            let mut statement = tx.prepare("SELECT jira_key FROM deleted_run_spend")?;
            statement.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(keys, ["KEPT-1"]);
}

#[test]
fn times_format_like_python() {
    let time = DbTime::parse("2026-10-01 23:17:22.331150").unwrap();
    assert_eq!(time.stored(), "2026-10-01 23:17:22.331150");
    assert_eq!(time.isoformat(), "2026-10-01T23:17:22.331150+00:00");
    let whole = DbTime::parse("2025-01-01 00:00:00").unwrap();
    assert_eq!(whole.stored(), "2025-01-01 00:00:00.000000");
    assert_eq!(whole.isoformat(), "2025-01-01T00:00:00+00:00");
}

/// Open, upgrade and read a copy of a real AgentFlow database. Never the live file: make a copy
/// with SQLite's backup API (the backend keeps it open in WAL mode) and point at it:
///     AGENTFLOW_DB_COPY=<copy> cargo test -p agentflow-core --test db -- --ignored
#[test]
#[ignore = "needs AGENTFLOW_DB_COPY"]
fn reads_a_copy_of_a_real_database() {
    let path = PathBuf::from(std::env::var("AGENTFLOW_DB_COPY").expect("AGENTFLOW_DB_COPY"));
    let before = normalized_schema(&path);
    let db = Database::open(&path).unwrap();
    db.create_all().unwrap();
    let counts = read_everything(&db);
    drop(db);
    assert_eq!(normalized_schema(&path), before, "opening an up-to-date database must not change its schema");
    assert_round_trips(&path);
    assert!(counts.iter().any(|(_, count)| *count > 0));
}

/// Every stored datetime and JSON value would be written back in exactly the text it has.
fn assert_round_trips(path: &Path) {
    for (column, text) in stored_values(path, "DATETIME") {
        let parsed = DbTime::parse(&text).unwrap_or_else(|| panic!("{column}: {text:?}"));
        assert_eq!(parsed.stored(), text, "{column} would not be written back as stored");
    }
    for (column, text) in stored_values(path, "JSON") {
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(pyjson::dumps_compact(&value), text, "{column} would not be written back as stored");
    }
}

/// Ported from backend/tests/test_task_archive_delete.py.
#[test]
fn test_upgrade_adds_the_archive_column() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("old.db")).unwrap();
    db.create_all().unwrap();
    db.session(|tx| tx.execute_batch("ALTER TABLE tasks DROP COLUMN archived_at")).unwrap();
    db.create_all().unwrap();
    let columns: Vec<String> = db
        .session(|tx| {
            let mut statement = tx.prepare("PRAGMA table_info(tasks)")?;
            statement.query_map([], |row| row.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert!(columns.iter().any(|c| c == "archived_at"));
}

/// Ported from backend/tests/test_projects.py: tasks as created before projects existed get the
/// project column, its index and its foreign key.
#[test]
fn test_upgrade_adds_the_project_column_and_its_index() {
    const OLD_TASKS: &str = "CREATE TABLE tasks (
        id INTEGER NOT NULL, jira_key VARCHAR(64) NOT NULL, api_budget_usd FLOAT, jira_title TEXT,
        repository_path TEXT NOT NULL, repository_root TEXT NOT NULL, project_relative_path TEXT,
        base_branch VARCHAR(255) NOT NULL, branch_name VARCHAR(255) NOT NULL, worktree_path TEXT NOT NULL,
        zellij_session_name VARCHAR(255) NOT NULL, task_dir TEXT NOT NULL, additional_context TEXT,
        status VARCHAR(32) NOT NULL, status_message TEXT, review_iteration INTEGER NOT NULL,
        max_review_iterations INTEGER NOT NULL, max_requirements_revisions INTEGER NOT NULL, current_sha VARCHAR(64),
        pr_url TEXT, pr_created_at DATETIME, merged_at DATETIME, pr_sync_paused BOOLEAN DEFAULT '0' NOT NULL,
        merged_head_sha VARCHAR(64), archived_at DATETIME, agent_overrides JSON NOT NULL,
        created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL, PRIMARY KEY (id));
    CREATE UNIQUE INDEX ix_tasks_jira_key ON tasks (jira_key);
    INSERT INTO tasks VALUES (1, 'OLD-1', NULL, NULL, '/r', '/r', NULL, 'main', 'agent/OLD-1', '/w', 'agentflow-OLD-1',
        '/t', NULL, 'MERGED', NULL, 1, 5, 3, NULL, NULL, NULL, NULL, 0, NULL, NULL, '{}',
        '2025-01-01 00:00:00.000000', '2025-01-01 00:00:00.000000');";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    Connection::open(&path).unwrap().execute_batch(OLD_TASKS).unwrap();
    let db = Database::open(&path).unwrap();
    db.create_all().unwrap();
    db.create_all().unwrap();
    db.session(|tx| {
        let task: TaskRow = table::select_one(tx, "", [])?.unwrap();
        assert_eq!(task.project_id, None);
        tx.execute_batch(
            "INSERT INTO projects (slug, name, color, agent_overrides, created_at, updated_at)
             VALUES ('billing', 'Billing', 'blue', '{}', '2025-01-01 00:00:00', '2025-01-01 00:00:00');
             UPDATE tasks SET project_id = 1;",
        )
    })
    .unwrap();
    drop(db);
    let connection = Connection::open(&path).unwrap();
    let indexed: Vec<String> = connection
        .prepare("PRAGMA index_info('ix_tasks_project_id')")
        .unwrap()
        .query_map([], |row| row.get(2))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(indexed, ["project_id"]);
    let keys: Vec<(String, String, String)> = connection
        .prepare("PRAGMA foreign_key_list('tasks')")
        .unwrap()
        .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(keys, [("projects".to_string(), "project_id".to_string(), "id".to_string())]);
}

/// Ported from backend/tests/test_questions_terminal.py: questions stored before uids, choices
/// and answers existed get them on upgrade.
#[test]
fn test_existing_question_rows_receive_uids_and_response_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(&dir.path().join("legacy.db")).unwrap();
    db.create_all().unwrap();
    db.session(|tx| {
        tx.execute_batch(
            "INSERT INTO tasks (jira_key, repository_path, repository_root, base_branch, branch_name, worktree_path,
                zellij_session_name, task_dir, status, review_iteration, max_review_iterations,
                max_requirements_revisions, pr_sync_paused, agent_overrides, created_at, updated_at)
             VALUES ('LEGACY-1', 'r', 'r', 'main', 'agent/LEGACY-1', 't', 'agentflow-LEGACY-1', 'd', 'ACTIVE', 1, 1, 3, 0,
                '{}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
             DROP INDEX IF EXISTS ix_agent_questions_uid;
             ALTER TABLE agent_questions RENAME TO agent_questions_new;
             CREATE TABLE agent_questions (id INTEGER PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER,
                role VARCHAR(64) NOT NULL, message TEXT NOT NULL, answered BOOLEAN NOT NULL DEFAULT 0,
                created_at DATETIME NOT NULL, answered_at DATETIME);
             INSERT INTO agent_questions (id, task_id, role, message, created_at)
                VALUES (1, (SELECT id FROM tasks WHERE jira_key = 'LEGACY-1'), 'architect', 'Legacy question', CURRENT_TIMESTAMP);
             DROP TABLE agent_questions_new;",
        )
    })
    .unwrap();
    db.create_all().unwrap();
    let (uid, choices, answer): (String, String, Option<String>) = db
        .session(|tx| {
            tx.query_row("SELECT uid, choices, answer FROM agent_questions WHERE id = 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        })
        .unwrap();
    assert!(uuid::Uuid::parse_str(&uid).is_ok(), "{uid}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&choices).unwrap(), serde_json::json!([]));
    assert_eq!(answer, None);
}
