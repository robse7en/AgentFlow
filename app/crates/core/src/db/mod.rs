//! SQLite persistence, ported from `backend/agentflow/database/`.
//!
//! The database file is the Python backend's, unchanged: a fresh one gets exactly the schema
//! SQLAlchemy creates ([`schema::TABLES`]), and an existing one is brought up to date by the same
//! column-adding upgrades, so either backend can open a file the other wrote. Values use the
//! Python formats: datetimes as naive UTC text with microseconds ([`DbTime`]), JSON as text
//! ([`Json`]), booleans as 0/1.
//!
//! Defaults that SQLAlchemy applied in Python (`default=utcnow`, uuid4 ids, `"ACTIVE"`, `{}`, the
//! `updated_at` refresh) are not in the schema: code that inserts or updates rows supplies them.

mod rows;
pub mod schema;
pub mod table;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, Transaction};
use serde_json::Value;

pub use rows::*;
pub use table::{Row, Tracked};

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("cannot create {path}: {source}")]
    Directory { path: PathBuf, source: std::io::Error },
}

pub type Result<T> = std::result::Result<T, DbError>;

pub struct Database {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl Database {
    /// Open (creating if needed) the database file, with the pragmas the Python backend sets.
    pub fn open(path: &Path) -> Result<Database> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|source| DbError::Directory { path: parent.to_path_buf(), source })?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Database { path: path.to_path_buf(), connection: Mutex::new(connection) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Create missing tables, then add the columns and indexes newer versions introduced.
    pub fn create_all(&self) -> Result<()> {
        self.session(|tx| -> Result<()> {
            for (table, statements) in schema::TABLES {
                if !table_exists(tx, table)? {
                    for statement in statements.iter() {
                        tx.execute_batch(statement)?;
                    }
                }
            }
            Ok(())
        })?;
        self.session(upgrade_agent_questions)?;
        self.session(upgrade_tasks_pr_tracking)?;
        self.session(upgrade_agent_sessions_screen_state)?;
        self.session(upgrade_tasks_jira_title)?;
        self.session(upgrade_tasks_requirements_revisions)?;
        self.session(upgrade_usage_billing)?;
        self.session(upgrade_review_findings_lineage)?;
        self.session(upgrade_tasks_archived)?;
        self.session(upgrade_tasks_project)?;
        Ok(())
    }

    /// Run `work` in one transaction: committed when it returns Ok, rolled back when it returns Err
    /// (the Python `with db.session()`). A failure state the caller wants kept is returned as Ok.
    pub fn session<T, E>(
        &self,
        work: impl FnOnce(&Transaction) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        // A panic while holding the lock leaves no open transaction (it is rolled back on drop),
        // so a poisoned connection is still safe to use.
        let mut connection = self.connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = connection.transaction()?;
        let value = work(&tx)?;
        tx.commit()?;
        Ok(value)
    }
}

fn table_exists(tx: &Transaction, table: &str) -> rusqlite::Result<bool> {
    tx.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [table], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| count > 0)
}

fn columns(tx: &Transaction, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = tx.prepare(&format!("PRAGMA table_info('{table}')"))?;
    statement.query_map([], |row| row.get::<_, String>(1))?.collect()
}

/// `ALTER TABLE ... ADD COLUMN` for each (column, definition) the table lacks.
fn add_missing(tx: &Transaction, table: &str, additions: &[(&str, &str)]) -> rusqlite::Result<()> {
    let existing = columns(tx, table)?;
    for (column, definition) in additions {
        if !existing.iter().any(|c| c == column) {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"))?;
        }
    }
    Ok(())
}

// The upgrades, in the order and with the exact column definitions of database.py, so an upgraded
// file has the same schema whichever backend upgraded it.

fn upgrade_agent_questions(tx: &Transaction) -> Result<()> {
    add_missing(
        tx,
        "agent_questions",
        &[("uid", "VARCHAR(36)"), ("choices", "JSON NOT NULL DEFAULT '[]'"), ("answer", "TEXT"), ("decision", "JSON")],
    )?;
    let missing: Vec<i64> = {
        let mut statement = tx.prepare("SELECT id FROM agent_questions WHERE uid IS NULL")?;
        statement.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for id in missing {
        tx.execute("UPDATE agent_questions SET uid = ?1 WHERE id = ?2", (new_uid(), id))?;
    }
    tx.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS ix_agent_questions_uid ON agent_questions(uid)")?;
    Ok(())
}

fn upgrade_tasks_pr_tracking(tx: &Transaction) -> Result<()> {
    add_missing(
        tx,
        "tasks",
        &[
            ("pr_url", "TEXT"),
            ("pr_created_at", "DATETIME"),
            ("merged_at", "DATETIME"),
            ("pr_sync_paused", "BOOLEAN NOT NULL DEFAULT 0"),
            ("merged_head_sha", "VARCHAR(64)"),
            ("pr_base_branch", "VARCHAR(255)"),
        ],
    )?;
    Ok(())
}

fn upgrade_agent_sessions_screen_state(tx: &Transaction) -> Result<()> {
    add_missing(tx, "agent_sessions", &[("screen_state", "VARCHAR(16)"), ("screen_state_at", "DATETIME")])?;
    Ok(())
}

fn upgrade_tasks_jira_title(tx: &Transaction) -> Result<()> {
    add_missing(tx, "tasks", &[("jira_title", "TEXT")])?;
    Ok(())
}

fn upgrade_tasks_requirements_revisions(tx: &Transaction) -> Result<()> {
    add_missing(tx, "tasks", &[("max_requirements_revisions", "INTEGER NOT NULL DEFAULT 3")])?;
    Ok(())
}

/// Preserve historical reports without guessing how historical runs were billed.
fn upgrade_usage_billing(tx: &Transaction) -> Result<()> {
    add_missing(tx, "tasks", &[("api_budget_usd", "FLOAT")])?;
    add_missing(tx, "run_evidence", &[("billing_mode", "VARCHAR(16) NOT NULL DEFAULT 'unknown'")])?;
    Ok(())
}

fn upgrade_review_findings_lineage(tx: &Transaction) -> Result<()> {
    add_missing(
        tx,
        "review_findings",
        &[
            ("prior_finding_id", "INTEGER"),
            ("occurrence", "INTEGER NOT NULL DEFAULT 1"),
            ("resolution", "TEXT"),
            ("response", "VARCHAR(16)"),
            ("response_note", "TEXT"),
        ],
    )?;
    Ok(())
}

fn upgrade_tasks_archived(tx: &Transaction) -> Result<()> {
    add_missing(tx, "tasks", &[("archived_at", "DATETIME")])?;
    Ok(())
}

/// The project assignment (create_all made the projects table).
fn upgrade_tasks_project(tx: &Transaction) -> Result<()> {
    add_missing(tx, "tasks", &[("project_id", "INTEGER REFERENCES projects (id)")])?;
    tx.execute_batch("CREATE INDEX IF NOT EXISTS ix_tasks_project_id ON tasks (project_id)")?;
    Ok(())
}

/// A new random identifier in the form the Python backend uses for uid/id text keys.
pub fn new_uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ------------------------------------------------------------------------------------------
// Value types
// ------------------------------------------------------------------------------------------

/// A UTC instant stored the way SQLAlchemy stores `DateTime(timezone=True)` in SQLite: naive text
/// `YYYY-MM-DD HH:MM:SS.ffffff`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DbTime(pub NaiveDateTime);

impl DbTime {
    const STORED: &'static str = "%Y-%m-%d %H:%M:%S%.6f";

    /// Now, to the microsecond (Python's datetime precision, and what the column stores).
    pub fn now() -> DbTime {
        use chrono::{DurationRound, TimeDelta};
        let now = Utc::now().naive_utc();
        DbTime(now.duration_trunc(TimeDelta::microseconds(1)).unwrap_or(now))
    }

    /// Accepts the stored form with or without a fractional part, and with a `T` separator.
    pub fn parse(text: &str) -> Option<DbTime> {
        let text = text.trim();
        ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"]
            .iter()
            .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
            .or_else(|| DateTime::parse_from_rfc3339(text).ok().map(|t| t.naive_utc()))
            .map(DbTime)
    }

    pub fn stored(&self) -> String {
        self.0.format(Self::STORED).to_string()
    }

    /// Python's `datetime.isoformat()` of the UTC value: `2026-10-01T23:17:22.331150+00:00`,
    /// without the fraction when the microseconds are zero.
    pub fn isoformat(&self) -> String {
        let micros = self.0.and_utc().timestamp_subsec_micros();
        if micros == 0 {
            self.0.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
        } else {
            self.0.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
        }
    }
}

impl ToSql for DbTime {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.stored()))
    }
}

impl FromSql for DbTime {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        DbTime::parse(text).ok_or_else(|| FromSqlError::Other(format!("not a stored datetime: {text:?}").into()))
    }
}

/// A JSON column. SQL NULL and the JSON text `null` (what SQLAlchemy writes for an explicit None)
/// both read as `Value::Null`; writes use Python's `json.dumps` format.
#[derive(Debug, Clone, PartialEq)]
pub struct Json(pub Value);

impl Json {
    /// None for SQL NULL or JSON null, for nullable JSON columns.
    pub fn into_option(self) -> Option<Value> {
        match self.0 {
            Value::Null => None,
            value => Some(value),
        }
    }
}

impl ToSql for Json {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(pyjson::dumps_compact(&self.0)))
    }
}

impl FromSql for Json {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        match value {
            ValueRef::Null => Ok(Json(Value::Null)),
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => {
                serde_json::from_slice(bytes).map(Json).map_err(|e| FromSqlError::Other(Box::new(e)))
            }
            ValueRef::Integer(_) | ValueRef::Real(_) => Err(FromSqlError::InvalidType),
        }
    }
}
