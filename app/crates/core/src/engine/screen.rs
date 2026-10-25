//! Background classification of each active agent's live terminal screen, ported from
//! `backend/agentflow/workflow/screen_tracking.py`.
//!
//! One pane stream per running agent session, feeding each rendered frame through the harness
//! adapter's `classify_screen`. The state is written to `agent_sessions.screen_state`, and the task
//! republished, only on a transition, so an animating spinner causes neither a write nor a
//! broadcast.

use std::collections::HashMap;
use std::sync::{Mutex, Weak};

use super::Engine;
use crate::db::table;
use crate::db::{AgentSessionRow, DbTime};
use crate::harness::HarnessAdapter;
use crate::terminal::PaneEvent;

#[derive(Default)]
pub struct ScreenTrackers {
    /// Session id -> (generation, task). A finished task removes only its own generation.
    tasks: Mutex<HashMap<i64, (u64, tokio::task::JoinHandle<()>)>>,
    generation: std::sync::atomic::AtomicU64,
}

impl ScreenTrackers {
    /// Idempotent: a session already being tracked is left alone.
    pub fn start(
        &self,
        engine: Weak<Engine>,
        jira_key: String,
        session_id: i64,
        zellij_session: String,
        pane_id: String,
        adapter: HarnessAdapter,
    ) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if tasks.get(&session_id).is_some_and(|(_, task)| !task.is_finished()) {
            return;
        }
        let generation = self.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let task = tokio::spawn(async move {
            track(engine.clone(), &jira_key, session_id, &zellij_session, &pane_id, &adapter).await;
            if let Some(engine) = engine.upgrade() {
                let mut tasks = engine.screen_trackers.tasks.lock().unwrap_or_else(|e| e.into_inner());
                if tasks.get(&session_id).is_some_and(|(g, _)| *g == generation) {
                    tasks.remove(&session_id);
                }
            }
        });
        tasks.insert(session_id, (generation, task));
    }

    /// Stop a session's tracker without waiting for it (safe under the task lock).
    pub fn cancel(&self, session_id: i64) {
        if let Some((_, task)) = self.tasks.lock().unwrap_or_else(|e| e.into_inner()).remove(&session_id) {
            task.abort();
        }
    }

    pub fn stop_all(&self) {
        for (_, (_, task)) in self.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            task.abort();
        }
    }

    pub fn is_tracking(&self, session_id: i64) -> bool {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).get(&session_id).is_some_and(|(_, t)| !t.is_finished())
    }
}

async fn track(
    engine: Weak<Engine>,
    jira_key: &str,
    session_id: i64,
    zellij_session: &str,
    pane_id: &str,
    adapter: &HarnessAdapter,
) {
    let Some(subscribed) = engine.upgrade().map(|e| e.terminals.subscribe(zellij_session, pane_id)) else { return };
    let mut events = match subscribed {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!(session_id, %error, "screen tracker could not subscribe");
            return;
        }
    };
    let mut last: Option<&'static str> = None;
    while let Some(event) = events.recv().await {
        let PaneEvent::Update { viewport, .. } = event else { continue };
        let state = adapter.classify_screen(&viewport).as_str();
        if last == Some(state) {
            continue;
        }
        let Some(engine) = engine.upgrade() else { return };
        match record(&engine, session_id, state) {
            // The row isn't visible yet (a launch still mid-transaction): retry on the next frame
            // instead of caching this classification as already seen.
            None => continue,
            Some(changed) => {
                last = Some(state);
                if changed && let Err(error) = engine.publish(jira_key) {
                    tracing::warn!(session_id, %error, "screen tracker could not publish");
                }
            }
        }
    }
}

/// Some(true): changed and stored (broadcast). Some(false): the database already agrees, or the
/// session is no longer running. None: no such row (yet).
fn record(engine: &Engine, session_id: i64, state: &str) -> Option<bool> {
    let result = engine.db.session(|tx| -> rusqlite::Result<Option<bool>> {
        let Some(original) = table::select_one::<AgentSessionRow>(tx, "WHERE id = ?1", [session_id])? else {
            return Ok(None);
        };
        if original.status != "running" || original.screen_state.as_deref() == Some(state) {
            return Ok(Some(false));
        }
        let mut changed = original.clone();
        changed.screen_state = Some(state.into());
        changed.screen_state_at = Some(DbTime::now());
        table::update(tx, &original, &changed)?;
        Ok(Some(true))
    });
    result.unwrap_or_else(|error| {
        tracing::warn!(session_id, %error, "screen tracker could not record");
        Some(false)
    })
}
