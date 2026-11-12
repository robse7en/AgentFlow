//! Live activity for every running agent session: one background task per session tails its
//! transcript into a [`RunActivity`], and sends the UI small `activity` updates over the websocket.
//!
//! This follows the life cycle of the screen trackers (`engine/screen.rs`): a map from session id to
//! a task, an idempotent `start`, a `cancel` and a `stop_all`. Unlike them it also keeps each run's
//! summary after the task ends, so `GET /api/tasks/{key}/activity` can serve the whole task. Nothing
//! here is stored in the database: a restart rebuilds the running runs from their launch offset.
//!
//! Nothing here logs or keeps tool input; see `activity.rs` for what a label may hold.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{Map, Value};

use super::activity::{
    RunActivity, Tail, TranscriptStart, parse_line, parses, subagent_files, subagent_key, subagents_dir,
    transcript_start,
};
use crate::collectors::{CollectorEnv, pricing};
use crate::events::Broadcaster;

/// How often each transcript is checked for new lines.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// The shortest time between two updates for one run.
pub const MIN_SEND_GAP: Duration = Duration::from_millis(250);
/// How long to look for a transcript that is not there yet before saying there is no activity data.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(60);

/// Where the price catalog comes from. It can need a download, so it is asked for once per run, off
/// the async threads, and only when a transcript has appeared.
pub type CatalogSource = Arc<dyn Fn() -> Option<Arc<Map<String, Value>>> + Send + Sync>;

/// The real catalog (the Copilot proxy's copy, our cache, or a download), kept once found.
pub fn real_catalog() -> CatalogSource {
    static FOUND: OnceLock<Arc<Map<String, Value>>> = OnceLock::new();
    Arc::new(|| {
        if let Some(found) = FOUND.get() {
            return Some(found.clone());
        }
        let catalog = pricing::load_catalog(&CollectorEnv::from_process()).map(Arc::new)?;
        Some(FOUND.get_or_init(|| catalog).clone())
    })
}

/// Everything needed to follow one run.
#[derive(Clone, Debug)]
pub struct TrackerSpec {
    pub jira_key: String,
    /// The canvas node (a worker run has its `worker_<uid>` key).
    pub node_key: String,
    pub session_id: i64,
    pub run_id: i64,
    pub run_number: u32,
    /// `claude`, `copilot` or `codex`.
    pub harness: String,
    pub harness_session_id: Option<String>,
    /// The harness's home directory, where its transcripts live.
    pub home: PathBuf,
    /// Where `activity-offset.json` was written at launch.
    pub run_dir: Option<PathBuf>,
}

/// The most subagent transcripts followed for one run; any more are left unread.
const MAX_SUBAGENT_FILES: usize = 256;

/// One Claude subagent's transcript being followed.
struct Subagent {
    /// The file's name without its extension, `agent-<id>`.
    key: String,
    tail: Tail,
    /// `agent-<id>.meta.json`, which names the `Agent` call that started it (and may come later).
    meta: PathBuf,
    /// That `Agent` call's id, once the meta file said.
    parent: Option<String>,
    seen: Instant,
    /// The meta file is no longer waited for: its calls stay without a parent.
    settled: bool,
}

/// One run being followed.
struct RunTracker {
    run: RunActivity,
    tail: Option<Tail>,
    /// Claude subagent transcripts, by file name.
    subagents: BTreeMap<String, Subagent>,
    spec: TrackerSpec,
    /// Where the transcript stood when the run launched.
    recorded: TranscriptStart,
    started: Instant,
    last_sent: Option<Instant>,
}

impl RunTracker {
    fn new(spec: TrackerSpec) -> RunTracker {
        let mut run = RunActivity::new(
            &spec.jira_key,
            &spec.node_key,
            spec.run_id,
            spec.session_id,
            spec.run_number,
            &spec.harness,
            None,
        );
        if !parses(&spec.harness) {
            run.mark_unavailable();
        }
        let recorded = read_recorded(spec.run_dir.as_deref());
        RunTracker {
            run,
            tail: None,
            subagents: BTreeMap::new(),
            spec,
            recorded,
            started: Instant::now(),
            last_sent: None,
        }
    }

    /// The transcript file, once it exists.
    fn find_transcript(&self) -> Option<PathBuf> {
        let id = self.spec.harness_session_id.as_deref()?;
        transcript_start(&self.spec.home, &self.spec.harness, Some(id)).path.filter(|path| path.is_file())
    }

    /// Finds the transcript if it is not being followed yet. True when it was just found and the
    /// price catalog should be set before [`read`](Self::read) (that may need a download, which
    /// must not happen while this tracker is locked).
    fn locate(&mut self, give_up: Duration) -> bool {
        if !parses(&self.spec.harness) || self.tail.is_some() {
            return false;
        }
        // The file named at launch, from the length it had then; else look for it by session id,
        // and read it whole. (The two spellings of one path may differ, so the recorded one is used
        // as written.)
        let recorded = self.recorded.path.clone().filter(|path| path.is_file());
        let (path, offset) = match recorded {
            Some(path) => (path, self.recorded.offset),
            None => match self.find_transcript() {
                Some(path) => (path, 0),
                None => {
                    if self.started.elapsed() >= give_up {
                        self.run.mark_unavailable();
                    }
                    return false;
                }
            },
        };
        self.tail = Some(Tail::new(path, offset));
        self.run.mark_available();
        true
    }

    /// Reads what the transcript and its subagents' transcripts gained since the last read. A
    /// subagent whose meta file is still missing after `give_up` goes without a parent for good.
    fn read(&mut self, give_up: Duration) {
        self.read_main();
        self.read_subagents(give_up);
    }

    fn read_main(&mut self) {
        let harness = self.spec.harness.as_str();
        let run = RefCell::new(&mut self.run);
        let Some(tail) = self.tail.as_mut() else { return };
        let _ = tail.poll_with(
            || run.borrow_mut().reset(),
            |line| {
                let mut run = run.borrow_mut();
                for event in parse_line(harness, line) {
                    run.apply(event);
                }
            },
        );
        // When the file was last written, not when it was read: a rebuilt run reads a backlog.
        if let Ok(modified) = std::fs::metadata(tail.path()).and_then(|meta| meta.modified()) {
            run.borrow_mut().note_write(modified.into());
        }
    }

    /// Lists the session's `subagents/` folder, follows any transcript not followed yet, and reads
    /// what each gained. A file that was there at launch is read from its length then; one that
    /// appeared later, from its start.
    fn read_subagents(&mut self, give_up: Duration) {
        if self.spec.harness != "claude" {
            return;
        }
        let Some(dir) = self.tail.as_ref().and_then(|tail| subagents_dir(tail.path())) else { return };
        for (name, _) in subagent_files(&dir) {
            if self.subagents.len() >= MAX_SUBAGENT_FILES {
                break;
            }
            let Some(key) = subagent_key(&name).map(str::to_string) else { continue };
            if self.subagents.contains_key(&name) {
                continue;
            }
            // Without a record (an older launch, or a fresh conversation) every file is this run's.
            let offset = self.recorded.subagents.as_ref().and_then(|known| known.get(&name)).copied().unwrap_or(0);
            self.subagents.insert(
                name.clone(),
                Subagent {
                    meta: dir.join(format!("{key}.meta.json")),
                    tail: Tail::new(dir.join(&name), offset),
                    key,
                    parent: None,
                    seen: Instant::now(),
                    settled: false,
                },
            );
        }
        let harness = self.spec.harness.as_str();
        let run = &mut self.run;
        for subagent in self.subagents.values_mut() {
            // The meta file can come after the transcript: keep looking, and give the calls read so
            // far their parent when it appears.
            if subagent.parent.is_none() && !subagent.settled {
                match read_tool_use_id(&subagent.meta) {
                    Some(parent) => {
                        run.set_subagent_parent(&subagent.key, &parent);
                        subagent.parent = Some(parent);
                    }
                    None => subagent.settled = subagent.seen.elapsed() >= give_up,
                }
            }
            let (key, parent) = (subagent.key.clone(), subagent.parent.clone());
            let poll = subagent.tail.poll(|line| {
                for event in parse_line(harness, line) {
                    run.apply_subagent(&key, parent.as_deref(), event);
                }
            });
            if poll.is_ok_and(|poll| poll.grew)
                && let Ok(modified) = std::fs::metadata(subagent.tail.path()).and_then(|meta| meta.modified())
            {
                run.note_write(modified.into());
            }
        }
    }

    /// The update to send now, if anything changed and the last one is not too recent.
    fn message(&mut self, force: bool) -> Option<Value> {
        if !force && self.last_sent.is_some_and(|sent| sent.elapsed() < MIN_SEND_GAP) {
            return None;
        }
        let delta = self.run.take_delta()?;
        self.last_sent = Some(Instant::now());
        Some(self.run.message(&delta))
    }
}

/// The offset recorded at launch, or the start of an unnamed file.
fn read_recorded(run_dir: Option<&Path>) -> TranscriptStart {
    let none = TranscriptStart { path: None, offset: 0, subagents: None };
    let Some(text) = run_dir.and_then(|dir| std::fs::read_to_string(dir.join("activity-offset.json")).ok()) else {
        return none;
    };
    let Ok(Value::Object(recorded)) = serde_json::from_str::<Value>(&text) else { return none };
    TranscriptStart {
        path: recorded.get("path").and_then(Value::as_str).map(PathBuf::from),
        offset: recorded.get("offset").and_then(Value::as_u64).unwrap_or(0),
        subagents: recorded
            .get("subagents")
            .and_then(Value::as_object)
            .map(|known| known.iter().filter_map(|(name, length)| Some((name.clone(), length.as_u64()?))).collect()),
    }
}

/// The `Agent` call a subagent's meta file names (`toolUseId`), when the file is there and says.
/// Only the id is taken: the rest of the file holds the subagent's description and settings.
fn read_tool_use_id(meta: &Path) -> Option<String> {
    // A meta file is a few hundred bytes.
    if std::fs::metadata(meta).ok()?.len() > 64 * 1024 {
        return None;
    }
    let Value::Object(body) = serde_json::from_str::<Value>(&std::fs::read_to_string(meta).ok()?).ok()? else {
        return None;
    };
    let id = body.get("toolUseId")?.as_str()?;
    let plain = !id.is_empty()
        && id.len() <= 200
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'));
    plain.then(|| id.to_string())
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Settings {
    poll: Duration,
    give_up: Duration,
    catalog: CatalogSource,
}

/// The task following one session, and the run it follows.
struct Following {
    task: tokio::task::JoinHandle<()>,
    run_id: i64,
}

struct Inner {
    tasks: Mutex<HashMap<i64, Following>>,
    /// Every run seen since the process started, by run id, for the GET.
    runs: Mutex<HashMap<i64, Arc<Mutex<RunTracker>>>>,
    broadcaster: Arc<Broadcaster>,
    settings: RwLock<Settings>,
}

pub struct ActivityTrackers(Arc<Inner>);

impl ActivityTrackers {
    pub fn new(broadcaster: Arc<Broadcaster>) -> ActivityTrackers {
        ActivityTrackers(Arc::new(Inner {
            tasks: Mutex::new(HashMap::new()),
            runs: Mutex::new(HashMap::new()),
            broadcaster,
            settings: RwLock::new(Settings { poll: POLL_INTERVAL, give_up: GIVE_UP_AFTER, catalog: real_catalog() }),
        }))
    }

    /// Replace how often transcripts are polled and how long a missing one is waited for (tests).
    pub fn set_timing(&self, poll: Duration, give_up: Duration) {
        let mut settings = self.0.settings.write().unwrap_or_else(|p| p.into_inner());
        (settings.poll, settings.give_up) = (poll, give_up);
    }

    /// Replace where prices come from (tests, which must never download).
    pub fn set_catalog_source(&self, catalog: CatalogSource) {
        self.0.settings.write().unwrap_or_else(|p| p.into_inner()).catalog = catalog;
    }

    /// Follow a run's transcript. Idempotent: a run already being followed is left alone. A session
    /// that moves on to another run ends the earlier run first.
    pub fn start(&self, spec: TrackerSpec) {
        let mut tasks = locked(&self.0.tasks);
        let previous = tasks.get(&spec.session_id).map(|following| (following.task.is_finished(), following.run_id));
        match previous {
            Some((false, run)) if run == spec.run_id => return,
            Some((_, run)) => {
                if let Some(following) = tasks.remove(&spec.session_id) {
                    following.task.abort();
                }
                if run != spec.run_id {
                    self.0.finish_run(run);
                }
            }
            None => {}
        }
        let (session_id, run_id) = (spec.session_id, spec.run_id);
        let tracker = Arc::new(Mutex::new(RunTracker::new(spec)));
        locked(&self.0.runs).insert(run_id, tracker.clone());
        let task = tokio::spawn(track(self.0.clone(), tracker));
        tasks.insert(session_id, Following { task, run_id });
    }

    /// Stop following a session and close its run: what its transcript still holds is read, and
    /// calls left open end `ok`. Safe under the task lock, and a second call changes nothing.
    pub fn cancel(&self, session_id: i64) {
        let removed = locked(&self.0.tasks).remove(&session_id);
        if let Some(following) = removed {
            following.task.abort();
            self.0.finish_run(following.run_id);
        }
    }

    /// Stop every task, as the process shuts down.
    pub fn stop_all(&self) {
        for (_, following) in locked(&self.0.tasks).drain() {
            following.task.abort();
        }
    }

    pub fn is_tracking(&self, session_id: i64) -> bool {
        locked(&self.0.tasks).get(&session_id).is_some_and(|following| !following.task.is_finished())
    }

    /// Every run held for a task, oldest first, as the GET sends them (with spans and context steps).
    pub fn runs_for(&self, jira_key: &str) -> Vec<Value> {
        let trackers: Vec<Arc<Mutex<RunTracker>>> = locked(&self.0.runs).values().cloned().collect();
        let mut runs: Vec<(i64, Value)> = trackers
            .iter()
            .filter_map(|tracker| {
                let tracker = locked(tracker);
                let run = &tracker.run;
                run.jira_key.eq_ignore_ascii_case(jira_key).then(|| (run.run_id, serde_json::to_value(run).ok()))
            })
            .filter_map(|(id, value)| Some((id, value?)))
            .collect();
        runs.sort_by_key(|(id, _)| *id);
        runs.into_iter().map(|(_, value)| value).collect()
    }

    /// Drop everything held for a task that was deleted.
    pub fn forget_task(&self, jira_key: &str) {
        // The locks are taken one at a time: `start` takes them in the other order.
        let gone: Vec<i64> = {
            let mut runs = locked(&self.0.runs);
            let gone: Vec<i64> = runs
                .iter()
                .filter(|(_, tracker)| locked(tracker).run.jira_key.eq_ignore_ascii_case(jira_key))
                .map(|(id, _)| *id)
                .collect();
            for id in &gone {
                runs.remove(id);
            }
            gone
        };
        locked(&self.0.tasks).retain(|_, following| {
            let forgotten = gone.contains(&following.run_id);
            if forgotten {
                following.task.abort();
            }
            !forgotten
        });
    }
}

impl Inner {
    /// Read what is left of a run's transcript, close its open calls and send the last update.
    fn finish_run(&self, run_id: i64) {
        let Some(tracker) = locked(&self.runs).get(&run_id).cloned() else { return };
        let mut tracker = locked(&tracker);
        if tracker.run.summary.ended {
            return;
        }
        // Prices are not fetched here: this can run under the task lock.
        tracker.locate(Duration::MAX);
        let give_up = self.settings.read().unwrap_or_else(|p| p.into_inner()).give_up;
        tracker.read(give_up);
        tracker.run.finish(Utc::now());
        if let Some(message) = tracker.message(true) {
            self.broadcaster.activity(&message);
        }
    }
}

/// The task behind one session: poll, and send what changed.
async fn track(inner: Arc<Inner>, tracker: Arc<Mutex<RunTracker>>) {
    loop {
        let (poll, give_up, catalog) = {
            let settings = inner.settings.read().unwrap_or_else(|p| p.into_inner());
            (settings.poll, settings.give_up, settings.catalog.clone())
        };
        let work = tracker.clone();
        // File reads, and the catalog when it is first needed, stay off the async threads.
        let message = tokio::task::spawn_blocking(move || {
            let wanted = locked(&work).locate(give_up);
            // The catalog may need a download: fetch it without holding the tracker, which `cancel`
            // waits for under the task lock.
            let prices = wanted.then(|| catalog());
            let mut tracker = locked(&work);
            if let Some(prices) = prices {
                tracker.run.set_catalog(prices);
            }
            tracker.read(give_up);
            tracker.message(false)
        })
        .await;
        if let Ok(Some(message)) = message {
            inner.broadcaster.activity(&message);
        }
        tokio::time::sleep(poll).await;
    }
}
