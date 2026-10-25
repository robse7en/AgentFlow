//! A task and its nodes as the engine works on them: loaded in one short transaction, changed in
//! memory under the task's lock, and flushed (changed columns only, plus new event rows) where the
//! Python engine called `session.commit()`. See [`crate::db::table`].

use rusqlite::Transaction;
use serde_json::Value;

use super::WorkflowError;
use super::states::{NodeState, TaskStatus};
use crate::db::table::{self, Tracked};
use crate::db::{AgentSessionRow, DbTime, Json, NodeRunRow, TaskRow, WorkflowEventRow, WorkflowNodeRow};

pub struct TaskState {
    pub task: Tracked<TaskRow>,
    /// In id order (Python's `task.nodes`).
    pub nodes: Vec<Tracked<WorkflowNodeRow>>,
    /// The task's runs, in id order.
    pub runs: Vec<Tracked<NodeRunRow>>,
    /// The task's agent sessions, in id order.
    pub agents: Vec<Tracked<AgentSessionRow>>,
    /// Log lines written on the next flush.
    events: Vec<WorkflowEventRow>,
}

impl TaskState {
    /// The task by key (case-insensitive) with its nodes, runs and agent sessions.
    pub fn load(tx: &Transaction, jira_key: &str) -> Result<TaskState, WorkflowError> {
        let task = table::select_one::<TaskRow>(tx, "WHERE jira_key = ?1", [jira_key.to_uppercase()])?
            .ok_or_else(|| WorkflowError::not_found(format!("Task {jira_key} not found")))?;
        let nodes = table::load(tx, "WHERE task_id = ?1 ORDER BY id", [task.id])?;
        let runs = table::load(tx, "WHERE task_id = ?1 ORDER BY id", [task.id])?;
        let agents = table::load(tx, "WHERE task_id = ?1 ORDER BY id", [task.id])?;
        Ok(TaskState { task: Tracked::loaded(task), nodes, runs, agents, events: Vec::new() })
    }

    /// A task about to be inserted (its nodes are added with [`TaskState::add_node`]).
    pub fn new(task: TaskRow) -> TaskState {
        TaskState {
            task: Tracked::new(task),
            nodes: Vec::new(),
            runs: Vec::new(),
            agents: Vec::new(),
            events: Vec::new(),
        }
    }

    /// The key of the node a run belongs to.
    pub fn run_node_key(&self, run: &NodeRunRow) -> Option<&str> {
        self.nodes.iter().find(|n| n.id == run.node_id).map(|n| n.key.as_str())
    }

    /// Python's `_finish_run`.
    pub fn finish_run(run: &mut NodeRunRow, state: NodeState, error: Option<String>) {
        run.state = state.as_str().into();
        run.error = error;
        run.completed_at = Some(DbTime::now());
    }

    pub fn add_node(&mut self, node: WorkflowNodeRow) {
        self.nodes.push(Tracked::new(node));
    }

    pub fn key(&self) -> &str {
        &self.task.jira_key
    }

    pub fn status(&self) -> Option<TaskStatus> {
        TaskStatus::parse(&self.task.status)
    }

    pub fn set_status(&mut self, status: TaskStatus) {
        self.task.status = status.as_str().into();
    }

    pub fn node_index(&self, key: &str) -> Result<usize, WorkflowError> {
        self.nodes
            .iter()
            .position(|n| n.key == key)
            .ok_or_else(|| WorkflowError::not_found(format!("Node {key} not found")))
    }

    pub fn node(&self, key: &str) -> Result<&WorkflowNodeRow, WorkflowError> {
        Ok(&self.nodes[self.node_index(key)?].row)
    }

    pub fn node_state(&self, index: usize) -> Option<NodeState> {
        NodeState::parse(&self.nodes[index].state)
    }

    /// Record a workflow event (Python's `_log`).
    pub fn log(&mut self, message: impl Into<String>, node_key: Option<&str>, level: &str, data: Option<Value>) {
        let message = message.into();
        tracing::info!("[{}] {message}", self.task.jira_key);
        self.events.push(WorkflowEventRow {
            id: 0,
            task_id: self.task.id,
            timestamp: DbTime::now(),
            level: level.into(),
            node_key: node_key.map(String::from),
            message,
            data: Json(data.unwrap_or(Value::Null)),
        });
    }

    pub fn info(&mut self, message: impl Into<String>) {
        self.log(message, None, "info", None);
    }

    /// Python's `_set_state`: change a node's state (and message), logging the transition.
    pub fn set_state(&mut self, index: usize, state: NodeState, message: Option<&str>, error: Option<String>) {
        let node = &mut self.nodes[index];
        if node.state == state.as_str() && message.is_none() {
            return;
        }
        node.state = state.as_str().into();
        if let Some(message) = message {
            node.message = Some(message.into());
        }
        node.error = error;
        let (label, key) = (node.label.clone(), node.key.clone());
        let text = match message {
            Some(message) if !message.is_empty() => format!("{label} → {state}: {message}"),
            _ => format!("{label} → {state}"),
        };
        let level = if state == NodeState::Failed { "error" } else { "info" };
        self.log(text, Some(&key), level, None);
    }

    /// Write every change since the last flush: the task row, its nodes and new events.
    pub fn flush(&mut self, tx: &Transaction) -> rusqlite::Result<()> {
        self.task.flush(tx)?;
        let task_id = self.task.id;
        for node in &mut self.nodes {
            node.task_id = task_id;
            node.flush(tx)?;
        }
        for run in &mut self.runs {
            run.flush(tx)?;
        }
        for agent in &mut self.agents {
            agent.flush(tx)?;
        }
        for mut event in self.events.drain(..) {
            event.task_id = task_id;
            table::insert(tx, &mut event)?;
        }
        Ok(())
    }
}
