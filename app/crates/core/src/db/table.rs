//! Reading and writing typed rows the way the Python backend's SQLAlchemy session did.
//!
//! The engine loads rows, changes them in memory (across awaits, under the task's lock) and
//! flushes at the points where Python called `commit()`. A [`Tracked`] row remembers what was
//! loaded, so a flush writes **only the columns that changed**: a writer outside the task lock
//! (the screen tracker, usage retries, Jira title fetches) never has its columns overwritten with
//! stale values. Like SQLAlchemy's `onupdate=utcnow`, an `updated_at` column is refreshed whenever
//! a row with one is updated.

use rusqlite::types::{ToSql, ToSqlOutput, Value};
use rusqlite::{Params, Transaction};

use super::DbTime;

pub trait Row: Clone + PartialEq + Sized {
    const TABLE: &'static str;
    /// The primary key first, then every other column, in declaration order.
    const COLUMNS: &'static [&'static str];
    /// Whether the key is an INTEGER the database assigns on insert.
    const AUTO_KEY: bool;
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;
    /// Column values in [`Row::COLUMNS`] order.
    fn values(&self) -> rusqlite::Result<Vec<Value>>;
    /// Record the key the database assigned (auto keys only).
    fn set_key(&mut self, id: i64);
}

/// What kind of primary key a row has.
pub trait KeyKind {
    const AUTO: bool;
    fn assign(slot: &mut Self, id: i64);
}

impl KeyKind for i64 {
    const AUTO: bool = true;
    fn assign(slot: &mut Self, id: i64) {
        *slot = id;
    }
}

impl KeyKind for String {
    const AUTO: bool = false;
    fn assign(_: &mut Self, _: i64) {}
}

/// A field's SQL value, owned.
pub fn owned(value: &dyn ToSql) -> rusqlite::Result<Value> {
    Ok(match value.to_sql()? {
        ToSqlOutput::Borrowed(value) => value.into(),
        ToSqlOutput::Owned(value) => value,
        _ => return Err(rusqlite::Error::ToSqlConversionFailure("unsupported column value".into())),
    })
}

/// `SELECT <columns> FROM <table> <rest>`, e.g. `rest = "WHERE task_id = ?1 ORDER BY id"`.
pub fn select<R: Row>(tx: &Transaction, rest: &str, params: impl Params) -> rusqlite::Result<Vec<R>> {
    let sql = format!("SELECT {} FROM {} {rest}", quoted(R::COLUMNS).join(", "), R::TABLE);
    let mut statement = tx.prepare(&sql)?;
    statement.query_map(params, R::from_row)?.collect()
}

/// The first row `rest` selects, if any.
pub fn select_one<R: Row>(tx: &Transaction, rest: &str, params: impl Params) -> rusqlite::Result<Option<R>> {
    Ok(select(tx, &format!("{rest} LIMIT 1"), params)?.into_iter().next())
}

/// Insert `row`; an auto key is left to the database and recorded on the row.
pub fn insert<R: Row>(tx: &Transaction, row: &mut R) -> rusqlite::Result<()> {
    let skip = usize::from(R::AUTO_KEY);
    let columns = &quoted(R::COLUMNS)[skip..];
    let values = &row.values()?[skip..];
    let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", R::TABLE, columns.join(", "), placeholders.join(", "));
    tx.execute(&sql, rusqlite::params_from_iter(values))?;
    if R::AUTO_KEY {
        row.set_key(tx.last_insert_rowid());
    }
    Ok(())
}

/// Write the columns of `row` that differ from `original` (refreshing `updated_at` when the table
/// has one). Returns whether anything was written.
pub fn update<R: Row>(tx: &Transaction, original: &R, row: &R) -> rusqlite::Result<bool> {
    let (old, new) = (original.values()?, row.values()?);
    let mut sets: Vec<String> = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    for (index, column) in R::COLUMNS.iter().enumerate().skip(1) {
        if old[index] != new[index] && *column != "updated_at" {
            values.push(new[index].clone());
            sets.push(format!("\"{column}\" = ?{}", values.len()));
        }
    }
    if sets.is_empty() {
        return Ok(false);
    }
    if R::COLUMNS.contains(&"updated_at") {
        values.push(owned(&DbTime::now())?);
        sets.push(format!("\"updated_at\" = ?{}", values.len()));
    }
    values.push(new[0].clone());
    let sql = format!("UPDATE {} SET {} WHERE \"{}\" = ?{}", R::TABLE, sets.join(", "), R::COLUMNS[0], values.len());
    tx.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(true)
}

/// Delete one row by its key.
pub fn delete<R: Row>(tx: &Transaction, row: &R) -> rusqlite::Result<()> {
    let key = row.values()?.swap_remove(0);
    tx.execute(&format!("DELETE FROM {} WHERE \"{}\" = ?1", R::TABLE, R::COLUMNS[0]), [key])?;
    Ok(())
}

fn quoted(columns: &[&str]) -> Vec<String> {
    columns.iter().map(|c| format!("\"{c}\"")).collect()
}

/// A row together with the state last written to (or read from) the database.
#[derive(Debug, Clone)]
pub struct Tracked<R: Row> {
    /// None until the row has been inserted.
    stored: Option<R>,
    pub row: R,
}

impl<R: Row> Tracked<R> {
    /// A row just read from the database.
    pub fn loaded(row: R) -> Self {
        Tracked { stored: Some(row.clone()), row }
    }

    /// A row to insert on the next flush.
    pub fn new(row: R) -> Self {
        Tracked { stored: None, row }
    }

    pub fn is_new(&self) -> bool {
        self.stored.is_none()
    }

    /// Insert or update as needed; afterwards the row counts as stored.
    pub fn flush(&mut self, tx: &Transaction) -> rusqlite::Result<()> {
        match &self.stored {
            None => insert(tx, &mut self.row)?,
            Some(stored) => {
                update(tx, stored, &self.row)?;
            }
        }
        self.stored = Some(self.row.clone());
        Ok(())
    }
}

impl<R: Row> std::ops::Deref for Tracked<R> {
    type Target = R;
    fn deref(&self) -> &R {
        &self.row
    }
}

impl<R: Row> std::ops::DerefMut for Tracked<R> {
    fn deref_mut(&mut self) -> &mut R {
        &mut self.row
    }
}

/// Load every row `rest` selects as tracked rows.
pub fn load<R: Row>(tx: &Transaction, rest: &str, params: impl Params) -> rusqlite::Result<Vec<Tracked<R>>> {
    Ok(select::<R>(tx, rest, params)?.into_iter().map(Tracked::loaded).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::db::{Database, Json, TaskRow, WorkflowNodeRow};

    fn task() -> TaskRow {
        let now = DbTime::now();
        TaskRow {
            id: 0,
            jira_key: "T-1".into(),
            api_budget_usd: None,
            jira_title: None,
            repository_path: "r".into(),
            repository_root: "r".into(),
            project_relative_path: None,
            base_branch: "main".into(),
            branch_name: "agent/T-1".into(),
            worktree_path: "w".into(),
            zellij_session_name: "agentflow-T-1".into(),
            task_dir: "t".into(),
            additional_context: None,
            status: "ACTIVE".into(),
            status_message: None,
            review_iteration: 1,
            max_review_iterations: 5,
            max_requirements_revisions: 3,
            current_sha: None,
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
        }
    }

    #[test]
    fn a_flush_writes_only_changed_columns_and_touches_updated_at() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        db.create_all().unwrap();
        let mut tracked = Tracked::new(task());
        db.session(|tx| tracked.flush(tx)).unwrap();
        assert!(tracked.id > 0 && !tracked.is_new());
        let before: TaskRow = db.session(|tx| select_one(tx, "WHERE id = ?1", [tracked.id])).unwrap().unwrap();

        // Someone else changes another column of the same row meanwhile.
        db.session(|tx| tx.execute("UPDATE tasks SET current_sha = 'abc' WHERE id = ?1", [tracked.id])).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        tracked.status = "CANCELLED".into();
        db.session(|tx| tracked.flush(tx)).unwrap();
        let after: TaskRow = db.session(|tx| select_one(tx, "WHERE id = ?1", [tracked.id])).unwrap().unwrap();
        assert_eq!(after.status, "CANCELLED");
        assert_eq!(after.current_sha.as_deref(), Some("abc"), "the other writer's column survives");
        assert!(after.updated_at > before.updated_at);

        // Nothing changed: nothing written, updated_at untouched.
        db.session(|tx| tracked.flush(tx)).unwrap();
        let again: TaskRow = db.session(|tx| select_one(tx, "WHERE id = ?1", [tracked.id])).unwrap().unwrap();
        assert_eq!(again.updated_at, after.updated_at);
    }

    #[test]
    fn rows_without_updated_at_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("t.db")).unwrap();
        db.create_all().unwrap();
        let mut task = task();
        db.session(|tx| insert(tx, &mut task)).unwrap();
        let mut node = Tracked::new(WorkflowNodeRow {
            id: 0,
            task_id: task.id,
            key: "architect".into(),
            kind: "agent".into(),
            label: "Architect".into(),
            role: Some("architect".into()),
            state: "READY".into(),
            execution_mode: "manual".into(),
            iteration: 0,
            checkpoint_sha: None,
            message: None,
            error: None,
            config_override: Json(json!({})),
            updated_at: DbTime::now(),
        });
        db.session(|tx| node.flush(tx)).unwrap();
        let loaded: Vec<Tracked<WorkflowNodeRow>> =
            db.session(|tx| load(tx, "WHERE task_id = ?1 ORDER BY id", [task.id])).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].row, node.row);
        db.session(|tx| {
            delete(tx, &node.row)?;
            delete(tx, &task)
        })
        .unwrap();
        let count: i64 = db.session(|tx| tx.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))).unwrap();
        assert_eq!(count, 0);
    }
}
