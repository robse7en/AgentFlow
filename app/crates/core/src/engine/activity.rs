//! Agent activity: one line of a harness transcript becomes a few small events, and each tool
//! call is sorted into one of nine stations with a short label.
//!
//! Parsing and classifying are pure: no files, no clock, no threads. [`Tail`] reads the new lines of
//! a transcript, and [`RunActivity`] keeps the per-run summary the UI shows. Trackers and the
//! transport come in later slices. See `notes/agent-activity-map-plan.md`.
//!
//! **Labels carry names, never content.** A label may be a file's last two path parts, a web
//! host, a command name, a subcommand or Claude's own short description. It never holds a command
//! line, a prompt, a query, a URL path or file contents, and nothing here logs any input.
//!
//! Only Claude Code lines are understood so far; Copilot and Codex return no events.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::collectors::pricing;

/// Times go to the UI as ISO strings with milliseconds, like the transcripts write them.
fn iso(at: &DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn time<S: Serializer>(at: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&iso(at))
}

fn time_opt<S: Serializer>(at: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error> {
    match at {
        Some(at) => serializer.serialize_str(&iso(at)),
        None => serializer.serialize_none(),
    }
}

/// The nine kinds of work a call can be sorted into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Station {
    You,
    Model,
    Agentflow,
    Files,
    Shell,
    Git,
    Network,
    Agents,
    Skills,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CallStatus {
    Run,
    Ok,
    Err,
}

/// One call as the UI lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEntry {
    /// The `tool_use` id, or a stable id built from the line when the harness has none.
    pub id: String,
    pub station: Station,
    /// A name, never content.
    pub label: String,
    #[serde(serialize_with = "time")]
    pub started_at: DateTime<Utc>,
    #[serde(serialize_with = "time_opt")]
    pub ended_at: Option<DateTime<Utc>>,
    pub status: CallStatus,
    pub by_subagent: bool,
    /// The Agents entry this subagent call belongs to (Claude only).
    pub parent_id: Option<String>,
    /// A prompt or answer arriving from the person (station You).
    pub inbound: bool,
}

/// A call without its label, for the "Run so far" strip.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub station: Station,
    #[serde(serialize_with = "time")]
    pub started_at: DateTime<Utc>,
    #[serde(serialize_with = "time_opt")]
    pub ended_at: Option<DateTime<Utc>>,
    pub status: CallStatus,
    pub by_subagent: bool,
    pub inbound: bool,
}

/// The context window's fill at one assistant step.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextStep {
    #[serde(serialize_with = "time")]
    pub at: DateTime<Utc>,
    pub percent: f64,
}

/// What one transcript line says. The tracker decides `by_subagent` / `parent_id` from which file
/// the line came from.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// A call began. An inbound start (a prompt from the person) has no matching `End`: it starts
    /// and ends at its own timestamp.
    Start { id: String, station: Station, label: String, at: DateTime<Utc>, inbound: bool },
    /// A call finished.
    End { id: String, at: DateTime<Utc>, is_error: bool },
    /// The agent's turn is over, so the model is no longer working on it: Claude Code wrote a
    /// `turn_duration` line, an interruption, or an error message in place of an answer.
    TurnEnd { at: DateTime<Utc> },
    /// One assistant line. A turn written as several lines repeats the same usage on each of them:
    /// count it once per `message_id` with [`StepDedup`].
    Step {
        /// The message id as a set key (see [`message_key`]); `None` when the line has no usable id.
        message_id: Option<String>,
        model: Option<String>,
        at: DateTime<Utc>,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_write: u64,
        /// Of `cache_write`, the part written to the one-hour cache (it is priced higher).
        cache_write_1h: u64,
    },
}

/// Never panics; an unknown or malformed line gives an empty list. `harness` is `claude`, `codex`
/// or `copilot`; only Claude is understood so far.
pub fn parse_line(harness: &str, line: &str) -> Vec<Parsed> {
    match harness {
        "claude" => parse_claude_line(line),
        _ => Vec::new(),
    }
}

/// Counts each assistant message's usage once, however many lines it was written as.
#[derive(Clone, Debug, Default)]
pub struct StepDedup {
    seen: HashSet<String>,
}

impl StepDedup {
    /// Whether a `Step` with this `message_id` should add its usage. A step without an id always
    /// counts, as in the usage collector.
    pub fn counts(&mut self, message_id: Option<&str>) -> bool {
        match message_id {
            None => true,
            Some(key) => self.seen.insert(key.to_string()),
        }
    }
}

// ---- tailing ---------------------------------------------------------------------------------

/// The longest line kept. A longer one (a huge tool result) is dropped, not buffered.
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
const READ_CHUNK: usize = 256 * 1024;

/// Reads the complete lines a growing transcript gains, from a byte offset on.
///
/// A partial last line (the writer is mid-line) is held back until its newline arrives. A missing
/// file reads as no lines, and a file that shrank is read again from the start.
#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    /// How far into the file has been read, whole lines or not.
    read: u64,
    /// The bytes after the last newline read so far.
    partial: Vec<u8>,
    /// The current line is over [`MAX_LINE_BYTES`] and is being thrown away up to its newline.
    skipping: bool,
}

/// What one [`Tail::poll`] saw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    /// New bytes were read (even if no line is complete yet).
    pub grew: bool,
    /// The file was shorter than the offset, so reading started over: the caller's summary is stale
    /// and should be rebuilt.
    pub restarted: bool,
}

impl Tail {
    pub fn new(path: impl Into<PathBuf>, offset: u64) -> Tail {
        Tail { path: path.into(), read: offset, partial: Vec::new(), skipping: false }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where a new reader resumes to see the same lines from here on: the start of the first line
    /// not yet handed out.
    pub fn offset(&self) -> u64 {
        self.read.saturating_sub(self.partial.len() as u64)
    }

    /// Hands each new complete line to `each` (without its line ending; blank lines are skipped).
    /// Only bytes already in the file when the poll starts are read, so a busy writer cannot keep
    /// it looping.
    pub fn poll(&mut self, mut each: impl FnMut(&str)) -> io::Result<Poll> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Poll::default()),
            Err(error) => return Err(error),
        };
        let length = file.metadata()?.len();
        let mut poll = Poll::default();
        if length < self.read {
            (self.read, poll.restarted) = (0, true);
            self.partial.clear();
            self.skipping = false;
        }
        if length == self.read {
            return Ok(poll);
        }
        file.seek(SeekFrom::Start(self.read))?;
        let mut chunk = vec![0u8; READ_CHUNK];
        let mut remaining = length - self.read;
        while remaining > 0 {
            let wanted = remaining.min(READ_CHUNK as u64) as usize;
            let got = file.read(&mut chunk[..wanted])?;
            if got == 0 {
                break;
            }
            remaining -= got as u64;
            self.read += got as u64;
            poll.grew = true;
            self.consume(&chunk[..got], &mut each);
        }
        Ok(poll)
    }

    fn consume<F: FnMut(&str)>(&mut self, mut bytes: &[u8], each: &mut F) {
        while !bytes.is_empty() {
            let Some(end) = bytes.iter().position(|byte| *byte == b'\n') else {
                if !self.skipping {
                    if self.partial.len() + bytes.len() > MAX_LINE_BYTES {
                        self.partial.clear();
                        self.skipping = true;
                    } else {
                        self.partial.extend_from_slice(bytes);
                    }
                }
                return;
            };
            let (head, rest) = (&bytes[..end], &bytes[end + 1..]);
            if self.skipping {
                self.skipping = false;
            } else if self.partial.is_empty() {
                emit(head, each);
            } else if self.partial.len() + head.len() <= MAX_LINE_BYTES {
                self.partial.extend_from_slice(head);
                emit(&self.partial, each);
            }
            self.partial.clear();
            bytes = rest;
        }
    }
}

fn emit<F: FnMut(&str)>(line: &[u8], each: &mut F) {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if !line.iter().all(u8::is_ascii_whitespace) {
        each(&String::from_utf8_lossy(line));
    }
}

/// Where a run's transcript is, and how much of it was already written when the run launched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptStart {
    /// None while the file is not there (a fresh conversation) or cannot be named yet (Codex).
    pub path: Option<PathBuf>,
    pub offset: u64,
}

/// The transcript's place at launch. A resumed conversation appends to its old file, so the run
/// starts reading at that file's length; a fresh one has no file yet and starts at 0.
pub fn transcript_start(home: &Path, harness: &str, session_id: Option<&str>) -> TranscriptStart {
    let none = TranscriptStart { path: None, offset: 0 };
    let Some(session_id) = session_id.filter(|id| crate::collectors::safe_session_id(id)) else { return none };
    let path = match harness {
        "claude" => {
            let env = crate::collectors::CollectorEnv::at(home.to_path_buf(), None, std::sync::Arc::new(|| None));
            crate::collectors::claude::find_transcript(&env, session_id)
        }
        "copilot" => Some(home.join(".copilot").join("session-state").join(session_id).join("events.jsonl")),
        _ => None,
    };
    let Some(path) = path else { return none };
    let offset = std::fs::metadata(&path).map_or(0, |meta| meta.len());
    TranscriptStart { path: Some(path), offset }
}

/// The `activity-offset.json` body: `{"path": …, "offset": …}`.
pub fn launch_offset(start: &TranscriptStart) -> Value {
    let path = start.path.as_ref().map(|path| path.to_string_lossy().into_owned());
    serde_json::json!({ "path": path, "offset": start.offset })
}

// ---- the run summary -------------------------------------------------------------------------

/// The newest calls a run keeps with their labels.
pub const MAX_ENTRIES: usize = 80;
pub const MAX_SPANS: usize = 2000;
pub const MAX_CONTEXT_STEPS: usize = 500;

/// Totals for a run, small enough to send with every update.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySummary {
    /// Calls begun per station, subagents' and prompts included.
    pub counts: BTreeMap<Station, u32>,
    /// Milliseconds spent in finished calls per station: a plain sum, so parallel calls add up.
    /// Inbound prompts are left out.
    pub busy_ms: BTreeMap<Station, u64>,
    pub errors: u32,
    /// `Agent` calls of the run itself that have not ended.
    pub subagents_in_flight: u32,
    pub model: Option<String>,
    /// The run's own assistant messages (a subagent's are not counted).
    pub steps: u32,
    /// The last step's prompt size against the model's window, 0 to 100; None when the window is
    /// not known.
    pub context_percent: Option<f64>,
    /// Input, output and cache tokens of the run's own steps.
    pub tokens: u64,
    /// List-price estimate for the same steps; None once a step's model has no known prices.
    pub cost_usd: Option<f64>,
    #[serde(serialize_with = "time_opt")]
    pub last_write_at: Option<DateTime<Utc>>,
    /// False: no transcript data for this run.
    pub available: bool,
    pub ended: bool,
}

/// A call that has begun and not ended.
#[derive(Debug)]
struct OpenCall {
    station: Station,
    started_at: DateTime<Utc>,
    by_subagent: bool,
    source: Source,
    /// Its place in the run's span history, counted from the first span ever made.
    span: u64,
}

/// Which transcript a line came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Source {
    Main,
    /// A subagent, and the `Agent` call that started it when that is known.
    Subagent(Option<String>),
}

/// Everything the activity map knows about one run, built by feeding it parsed transcript lines.
///
/// It reads no clock: times come from the transcript, and the caller supplies the rest
/// ([`note_write`](Self::note_write), [`finish`](Self::finish)). Feeding the same lines in the same
/// order always gives the same summary.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunActivity {
    pub jira_key: String,
    /// A worker run uses its `worker_<uid>` node key.
    pub node_key: String,
    pub run_id: i64,
    pub session_id: i64,
    pub run_number: u32,
    pub harness: String,
    /// The newest [`MAX_ENTRIES`] calls, oldest first.
    pub entries: VecDeque<ActivityEntry>,
    pub summary: ActivitySummary,
    /// Every call without its label, newest [`MAX_SPANS`] (GET only).
    #[serde(skip_serializing_if = "VecDeque::is_empty")]
    pub spans: VecDeque<Span>,
    /// The context fill at each of the newest [`MAX_CONTEXT_STEPS`] steps (GET only).
    #[serde(skip_serializing_if = "VecDeque::is_empty")]
    pub context_steps: VecDeque<ContextStep>,
    #[serde(skip)]
    open: HashMap<String, OpenCall>,
    #[serde(skip)]
    dedup: StepDedup,
    /// The open "model is working" entry of each transcript, by its id.
    #[serde(skip)]
    thinking: HashMap<Source, String>,
    /// Spans ever made, so an evicted span's place can be told from a held one's.
    #[serde(skip)]
    spans_made: u64,
    /// Model steps that came with no message id, to name them.
    #[serde(skip)]
    anonymous_steps: u32,
    #[serde(skip)]
    catalog: Option<Arc<Map<String, Value>>>,
    #[serde(skip)]
    cost_lost: bool,
}

impl RunActivity {
    /// `catalog` is the models.dev price catalog (never loaded here: it may need a download). Without
    /// one, or for a model it does not list, there is no cost, and no context percentage.
    pub fn new(
        jira_key: &str,
        node_key: &str,
        run_id: i64,
        session_id: i64,
        run_number: u32,
        harness: &str,
        catalog: Option<Arc<Map<String, Value>>>,
    ) -> RunActivity {
        let summary =
            ActivitySummary { available: true, cost_usd: catalog.as_ref().map(|_| 0.0), ..ActivitySummary::default() };
        RunActivity {
            jira_key: jira_key.to_string(),
            node_key: node_key.to_string(),
            run_id,
            session_id,
            run_number,
            harness: harness.to_string(),
            entries: VecDeque::new(),
            summary,
            spans: VecDeque::new(),
            context_steps: VecDeque::new(),
            open: HashMap::new(),
            dedup: StepDedup::default(),
            thinking: HashMap::new(),
            spans_made: 0,
            anonymous_steps: 0,
            catalog,
            cost_lost: false,
        }
    }

    /// Takes in one event from the run's own transcript.
    pub fn apply(&mut self, parsed: Parsed) {
        self.apply_from(Source::Main, parsed);
    }

    /// Takes in one event from a subagent's transcript. `parent_id` is the `Agent` call in the main
    /// transcript that started it, or None while that is not known. Its calls are marked
    /// `by_subagent`; its steps are not the run's own, so they leave steps, model, context, tokens
    /// and cost alone (the after-run usage reads only the main transcript).
    pub fn apply_subagent(&mut self, parent_id: Option<&str>, parsed: Parsed) {
        self.apply_from(Source::Subagent(parent_id.map(str::to_string)), parsed);
    }

    /// Records that the run's files grew at `at` (the latest time wins).
    pub fn note_write(&mut self, at: DateTime<Utc>) {
        if self.summary.last_write_at.is_none_or(|last| at > last) {
            self.summary.last_write_at = Some(at);
        }
    }

    /// The run is over: every call still open ends `ok` at `at`.
    pub fn finish(&mut self, at: DateTime<Utc>) {
        let open: Vec<(String, OpenCall)> = self.open.drain().collect();
        for (id, call) in open {
            self.close(&id, call, at, false);
        }
        self.thinking.clear();
        self.summary.subagents_in_flight = 0;
        self.summary.ended = true;
    }

    /// Marks that no transcript data can be had for this run.
    pub fn mark_unavailable(&mut self) {
        self.summary.available = false;
    }

    /// Calls that have begun and not ended.
    pub fn open_calls(&self) -> usize {
        self.open.len()
    }

    fn apply_from(&mut self, source: Source, parsed: Parsed) {
        match parsed {
            Parsed::Start { id, station, label, at, inbound } => {
                if self.open.contains_key(&id) || self.entries.iter().any(|entry| entry.id == id) {
                    return;
                }
                // The model has made its call, or the person has spoken: either way its turn is over.
                self.stop_thinking(&source, at, None);
                let entry = self.entry(&source, id, station, label, at);
                let entry = ActivityEntry {
                    ended_at: inbound.then_some(at),
                    status: if inbound { CallStatus::Ok } else { CallStatus::Run },
                    inbound,
                    ..entry
                };
                let id = entry.id.clone();
                self.add(entry, &source);
                if inbound {
                    self.start_thinking(&source, &id, at);
                }
            }
            Parsed::TurnEnd { at } => {
                self.stop_thinking(&source, at, None);
            }
            Parsed::End { id, at, is_error } => {
                if let Some(call) = self.open.remove(&id) {
                    self.close(&id, call, at, is_error);
                    // With nothing left running, the agent is back with the model.
                    if !self.open.values().any(|call| call.source == source) {
                        self.start_thinking(&source, &id, at);
                    }
                }
            }
            Parsed::Step {
                message_id,
                model,
                at,
                input_tokens,
                output_tokens,
                cache_read,
                cache_write,
                cache_write_1h,
            } => {
                if !self.dedup.counts(message_id.as_deref()) {
                    return;
                }
                let label = clean_label(model.as_deref().unwrap_or("Model"));
                // A line closes the model's work that began at the last call end or prompt. With
                // none open (a harness wake-up the parser skipped, say) the step takes no time:
                // reaching back to the previous turn would count the idle gap as work.
                if !self.stop_thinking(&source, at, Some(&label)) {
                    let id = match message_id.as_deref() {
                        Some(key) => format!("model-{key}"),
                        None => {
                            self.anonymous_steps += 1;
                            format!("model-{}", self.anonymous_steps)
                        }
                    };
                    let entry = self.entry(&source, id, Station::Model, label, at);
                    self.add(ActivityEntry { ended_at: Some(at), status: CallStatus::Ok, ..entry }, &source);
                }
                if source == Source::Main {
                    let tokens = StepTokens { input_tokens, output_tokens, cache_read, cache_write, cache_write_1h };
                    self.count_step(model.as_deref(), at, tokens);
                }
            }
        }
    }

    /// A new entry for a call of `source`, running.
    fn entry(&self, source: &Source, id: String, station: Station, label: String, at: DateTime<Utc>) -> ActivityEntry {
        let (by_subagent, parent_id) = match source {
            Source::Main => (false, None),
            Source::Subagent(parent) => (true, parent.clone()),
        };
        ActivityEntry {
            id,
            station,
            label,
            started_at: at,
            ended_at: None,
            status: CallStatus::Run,
            by_subagent,
            parent_id,
            inbound: false,
        }
    }

    /// The model starts working on what just happened (a prompt, or the end of its last call).
    fn start_thinking(&mut self, source: &Source, trigger: &str, at: DateTime<Utc>) {
        let id = format!("think-{trigger}");
        let entry = self.entry(source, id.clone(), Station::Model, "Model".to_string(), at);
        self.add(entry, source);
        self.thinking.insert(source.clone(), id);
    }

    /// Ends the model's open work in `source`, if any (naming its model when known).
    fn stop_thinking(&mut self, source: &Source, at: DateTime<Utc>, label: Option<&str>) -> bool {
        let Some(id) = self.thinking.remove(source) else { return false };
        let Some(call) = self.open.remove(&id) else { return false };
        if let Some(label) = label
            && let Some(entry) = self.entries.iter_mut().rev().find(|entry| entry.id == id)
        {
            entry.label = label.to_string();
        }
        self.close(&id, call, at, false);
        true
    }

    /// Lists a call that has begun. An entry with an end time (a prompt, a model step) is already
    /// finished; any other stays open until its `End`.
    fn add(&mut self, entry: ActivityEntry, source: &Source) {
        *self.summary.counts.entry(entry.station).or_insert(0) += 1;
        if entry.ended_at.is_none() {
            let call = OpenCall {
                station: entry.station,
                started_at: entry.started_at,
                by_subagent: entry.by_subagent,
                source: source.clone(),
                span: self.spans_made,
            };
            self.open.insert(entry.id.clone(), call);
            if entry.station == Station::Agents && !entry.by_subagent {
                self.summary.subagents_in_flight += 1;
            }
        }
        self.spans.push_back(Span {
            station: entry.station,
            started_at: entry.started_at,
            ended_at: entry.ended_at,
            status: entry.status,
            by_subagent: entry.by_subagent,
            inbound: entry.inbound,
        });
        self.spans_made += 1;
        if self.spans.len() > MAX_SPANS {
            self.spans.pop_front();
        }
        self.entries.push_back(entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    /// Ends an open call. Its entry may have left the newest 80 and its span the newest 2000; the
    /// totals still count it.
    fn close(&mut self, id: &str, call: OpenCall, at: DateTime<Utc>, is_error: bool) {
        let ended_at = at.max(call.started_at);
        let status = if is_error { CallStatus::Err } else { CallStatus::Ok };
        if let Some(entry) = self.entries.iter_mut().rev().find(|entry| entry.id == id) {
            entry.ended_at = Some(ended_at);
            entry.status = status;
        }
        let first_held = self.spans_made - self.spans.len() as u64;
        if let Some(span) = call.span.checked_sub(first_held).and_then(|index| self.spans.get_mut(index as usize)) {
            span.ended_at = Some(ended_at);
            span.status = status;
        }
        self.add_busy(call.station, call.started_at, ended_at);
        if is_error {
            self.summary.errors += 1;
        }
        if call.station == Station::Agents && !call.by_subagent {
            self.summary.subagents_in_flight = self.summary.subagents_in_flight.saturating_sub(1);
        }
    }

    fn add_busy(&mut self, station: Station, from: DateTime<Utc>, to: DateTime<Utc>) {
        let millis = (to - from).num_milliseconds().max(0) as u64;
        *self.summary.busy_ms.entry(station).or_insert(0) += millis;
    }

    /// One of the run's own assistant messages: steps, tokens, cost and context.
    fn count_step(&mut self, model: Option<&str>, at: DateTime<Utc>, tokens: StepTokens) {
        self.summary.steps += 1;
        self.summary.model = model.map(str::to_string).or_else(|| self.summary.model.take());
        self.summary.tokens += tokens.input_tokens + tokens.output_tokens + tokens.cache_read + tokens.cache_write;
        let catalog = self.catalog.clone();
        let catalog = catalog.as_deref();
        let Some(model) = model else { return };
        self.price(catalog, model, &tokens);
        let window = pricing::context_window_for_name(catalog, model);
        self.summary.context_percent = window.map(|window| {
            let used = (tokens.input_tokens + tokens.cache_read + tokens.cache_write) as f64;
            ((used / window as f64 * 100.0).min(100.0) * 10.0).round() / 10.0
        });
        if let Some(percent) = self.summary.context_percent {
            self.context_steps.push_back(ContextStep { at, percent });
            if self.context_steps.len() > MAX_CONTEXT_STEPS {
                self.context_steps.pop_front();
            }
        }
    }

    /// Adds the step to the cost, priced like the usage collector prices a request. A model without
    /// prices leaves the cost unknown for the rest of the run.
    fn price(&mut self, catalog: Option<&Map<String, Value>>, model: &str, tokens: &StepTokens) {
        if self.cost_lost {
            return;
        }
        let priced = pricing::rates_for_name(catalog, model).ok().flatten().and_then(|rates| {
            let tokens = pricing::Tokens {
                uncached: tokens.input_tokens as i64,
                cache_read: tokens.cache_read as i64,
                cache_write: tokens.cache_write.saturating_sub(tokens.cache_write_1h) as i64,
                cache_write_1h: tokens.cache_write_1h as i64,
                output: tokens.output_tokens as i64,
            };
            pricing::price(rates, tokens, true).ok()
        });
        match (priced, self.summary.cost_usd) {
            (Some(cost), Some(total)) => self.summary.cost_usd = Some(total + cost),
            _ => {
                self.cost_lost = true;
                self.summary.cost_usd = None;
            }
        }
    }
}

struct StepTokens {
    input_tokens: u64,
    output_tokens: u64,
    cache_read: u64,
    cache_write: u64,
    cache_write_1h: u64,
}

// ---- Claude lines ----------------------------------------------------------------------------

fn parse_claude_line(line: &str) -> Vec<Parsed> {
    let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let Some(at) = entry.get("timestamp").and_then(Value::as_str).and_then(parse_time) else { return Vec::new() };
    if entry.get("type").and_then(Value::as_str) == Some("system") {
        let ended = entry.get("subtype").and_then(Value::as_str) == Some("turn_duration");
        return if ended { vec![Parsed::TurnEnd { at }] } else { Vec::new() };
    }
    let Some(Value::Object(message)) = entry.get("message") else { return Vec::new() };
    match entry.get("type").and_then(Value::as_str) {
        Some("assistant") => claude_assistant(message, at),
        Some("user") => claude_user(&entry, message, at),
        _ => Vec::new(),
    }
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text).ok().map(|time| time.with_timezone(&Utc))
}

type Object = serde_json::Map<String, Value>;

/// An assistant line: one `Step` (when it carries usage), then one `Start` per `tool_use` block.
fn claude_assistant(message: &Object, at: DateTime<Utc>) -> Vec<Parsed> {
    let mut events = Vec::new();
    let model = message.get("model").and_then(Value::as_str).filter(|model| !model.is_empty());
    let usage = message.get("usage").and_then(Value::as_object).filter(|usage| !usage.is_empty());
    // Like the usage collector: no usage, no model or a synthetic message is not a step. A synthetic
    // message is Claude Code's own text in place of an answer (an API error, a limit): the turn is over.
    if model == Some("<synthetic>") {
        events.push(Parsed::TurnEnd { at });
    }
    if let (Some(usage), Some(model)) = (usage, model)
        && model != "<synthetic>"
    {
        let cache_write = count(usage.get("cache_creation_input_tokens"));
        let one_hour = count(
            usage.get("cache_creation").and_then(Value::as_object).and_then(|c| c.get("ephemeral_1h_input_tokens")),
        );
        events.push(Parsed::Step {
            message_id: message.get("id").and_then(message_key),
            model: Some(model.to_string()),
            at,
            input_tokens: count(usage.get("input_tokens")),
            output_tokens: count(usage.get("output_tokens")),
            cache_read: count(usage.get("cache_read_input_tokens")),
            cache_write,
            cache_write_1h: one_hour.min(cache_write),
        });
    }
    if let Some(Value::Array(blocks)) = message.get("content") {
        for block in blocks.iter().filter_map(Value::as_object) {
            // `server_tool_use` and `advisor_tool_result` never get a `tool_result`, so they would
            // stay open for good: only plain `tool_use` blocks are calls.
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let Some(id) = block.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else { continue };
            let Some(name) = block.get("name").and_then(Value::as_str) else { continue };
            let (station, label) = classify_tool(name, block.get("input").unwrap_or(&Value::Null));
            events.push(Parsed::Start { id: id.to_string(), station, label, at, inbound: false });
        }
    }
    events
}

/// A user line: the results of tool calls, or (rarely) the person talking.
fn claude_user(entry: &Object, message: &Object, at: DateTime<Utc>) -> Vec<Parsed> {
    let content = message.get("content");
    if let Some(Value::Array(blocks)) = content {
        let ends: Vec<Parsed> = blocks
            .iter()
            .filter_map(Value::as_object)
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|block| {
                let id = block.get("tool_use_id").and_then(Value::as_str).filter(|id| !id.is_empty())?;
                let is_error = block.get("is_error").and_then(Value::as_bool) == Some(true);
                Some(Parsed::End { id: id.to_string(), at, is_error })
            })
            .collect();
        if !ends.is_empty() {
            return ends;
        }
        // A list of blocks with a tool result that has no id is not the person either.
        if blocks
            .iter()
            .filter_map(Value::as_object)
            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        {
            return Vec::new();
        }
    }
    let text = match content {
        Some(Value::String(text)) => Some(text.as_str()),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(Value::as_object)
            .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|block| block.get("text").and_then(Value::as_str)),
        _ => None,
    };
    let Some(text) = text else { return Vec::new() };
    if text.trim_start().starts_with("[Request interrupted") {
        return vec![Parsed::TurnEnd { at }];
    }
    if !is_from_the_person(entry, text) {
        return Vec::new();
    }
    // The line's own uuid keeps the id the same when the file is read again; the timestamp stands
    // in for it on a line without one.
    let stamp = entry.get("uuid").and_then(Value::as_str).filter(|id| !id.is_empty());
    let id = match stamp {
        Some(uuid) => format!("prompt-{uuid}"),
        None => format!("prompt-{}", at.timestamp_millis()),
    };
    vec![Parsed::Start { id, station: Station::You, label: "Prompt".to_string(), at, inbound: true }]
}

/// Many user-role lines are not the person: harness notices, task notifications, subagent
/// hand-backs, compaction summaries and automatic continuations. A line counts only when all of
/// these hold (checked against real transcripts on 2026-11-12).
fn is_from_the_person(entry: &Object, text: &str) -> bool {
    // `origin.kind` is "human", or `origin` is absent (older CLIs).
    match entry.get("origin") {
        None | Some(Value::Null) => {}
        Some(Value::Object(origin)) if origin.get("kind").and_then(Value::as_str) == Some("human") => {}
        Some(_) => return false,
    }
    let flag = |key: &str| entry.get(key).and_then(Value::as_bool) == Some(true);
    // A subagent's first line is the prompt its parent wrote, not the person.
    if flag("isMeta") || flag("isCompactSummary") || flag("isVisibleInTranscriptOnly") || flag("isSidechain") {
        return false;
    }
    // `<system-reminder>`, `<task-notification>`, `<command-name>`, `<local-command-…>`, and
    // bracketed harness notices such as `[Request interrupted by user]`.
    let text = text.trim_start();
    !text.is_empty() && !text.starts_with('<') && !text.starts_with('[')
}

/// A message id as a set key, by the same rule as `id_key` in `collectors/claude.rs`: a string is
/// `s:…`, a number or boolean is `n:…`, and a falsy or non-scalar id (which the collector does not
/// dedup on either) gives `None`.
pub fn message_key(id: &Value) -> Option<String> {
    match id {
        Value::String(text) if !text.is_empty() => Some(format!("s:{text}")),
        Value::Bool(true) => Some("n:1".to_string()),
        Value::Number(number) => {
            if number.as_f64() == Some(0.0) {
                return None;
            }
            Some(match (number.as_i64(), number.as_f64()) {
                (Some(whole), _) => format!("n:{whole}"),
                (None, Some(float)) if float.fract() == 0.0 && float.abs() < 9.2e18 => format!("n:{}", float as i64),
                _ => format!("n:{number}"),
            })
        }
        _ => None,
    }
}

/// A token count; anything that is not a non-negative number counts as 0.
fn count(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => match (number.as_u64(), number.as_f64()) {
            (Some(whole), _) => whole,
            (None, Some(float)) if float.is_finite() && float >= 0.0 => float as u64,
            _ => 0,
        },
        _ => 0,
    }
}

// ---- classification --------------------------------------------------------------------------

// Adapted from lot-map by alexeyw (https://github.com/alexeyw/lot-map), MIT licence. See THIRD-PARTY-NOTICES.md.

// Everything below, down to the end of the shell-command code, is adapted from lot-map's `classify`,
// `classifyBash`, `hostOf`, `clean` and `lastTwo` (hooks/map.ts).

/// Sorts one Claude tool call into a station and a label. Unknown tools land on Shell, labelled
/// with the tool's name (as lot-map does).
pub fn classify_tool(name: &str, input: &Value) -> (Station, String) {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or("");
    let (station, label) = match name {
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let path = if text("file_path").is_empty() { text("notebook_path") } else { text("file_path") };
            (Station::Files, last_two(path))
        }
        "Grep" | "Glob" => (Station::Files, name.to_string()),
        "Bash" | "PowerShell" => classify_shell(text("command"), text("description")),
        "WebFetch" => (Station::Network, host_of(text("url"))),
        "WebSearch" => (Station::Network, "Web search".to_string()),
        "Artifact" => (Station::Network, "Artifact".to_string()),
        "Agent" | "Task" => (Station::Agents, text("description").to_string()),
        "Workflow" => (Station::Agents, "Workflow".to_string()),
        "SendMessage" | "TaskStop" => (Station::Agents, name.to_string()),
        "Skill" => (Station::Skills, text("skill").to_string()),
        "ToolSearch" => (Station::Skills, "Loading tools".to_string()),
        "AskUserQuestion" => (Station::You, "Question for you".to_string()),
        "SendUserFile" => (Station::You, "File for you".to_string()),
        _ => match name.strip_prefix("mcp__") {
            Some(rest) => {
                let (server, tool) = rest.split_once("__").unwrap_or((rest, ""));
                let tool = tool.replace('_', " ");
                (Station::Network, if tool.is_empty() { server.to_string() } else { format!("{server}: {tool}") })
            }
            None => (Station::Shell, name.to_string()),
        },
    };
    let label = clean_label(&label);
    let label = if label.is_empty() { fallback_label(station, name) } else { label };
    (station, label)
}

/// What a call is called when its input gave no name to show.
fn fallback_label(station: Station, tool: &str) -> String {
    match (station, tool) {
        (Station::Agents, "Agent" | "Task") => "Subagent".to_string(),
        (Station::Skills, "Skill") => "Skill".to_string(),
        (Station::Network, "WebFetch") => "Web page".to_string(),
        (Station::Shell, "Bash" | "PowerShell") => "shell".to_string(),
        _ => clean_label(tool),
    }
}

/// The longest label that is sent.
const LABEL_MAX: usize = 120;

/// A label as it may be shown: no control characters (a terminal would act on them), no
/// direction marks, runs of spaces folded, and at most 120 characters.
fn clean_label(label: &str) -> String {
    let blocked = |c: char| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let mut text = String::with_capacity(label.len());
    for c in label.chars() {
        let c = if blocked(c) { ' ' } else { c };
        if c == ' ' && text.ends_with(' ') {
            continue;
        }
        text.push(c);
    }
    let text = text.trim();
    if text.chars().count() > LABEL_MAX {
        let mut cut: String = text.chars().take(LABEL_MAX - 1).collect();
        cut.push('…');
        cut
    } else {
        text.to_string()
    }
}

/// The last two parts of a path (either slash), so a file shows as `src/main.rs`.
fn last_two(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
    parts[parts.len().saturating_sub(2)..].join("/")
}

/// Only the host: a login, a password, a path or a query never reach a label. A password may hold
/// `/`, `?` or `#` unencoded, so the first of them is not always where the host ends:
/// `user:pa/ss@host` reads as the host `host`, and an address that can be read both ways gives its
/// scheme alone.
fn host_of(url: &str) -> String {
    static SCHEME: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?s)^(?:([A-Za-z][A-Za-z0-9+.-]*)://)?(.*)$").unwrap());
    let Some(parts) = SCHEME.captures(url) else { return String::new() };
    let scheme = parts.get(1).map_or("", |m| m.as_str());
    let rest = parts.get(2).map_or("", |m| m.as_str());
    let end = rest.find(['/', '?', '#']);
    let (first, beyond) = match end {
        Some(end) => (&rest[..end], &rest[end..]),
        None => (rest, ""),
    };
    let mut host = first.rsplit('@').next().unwrap_or("");
    if !first.contains('@') && first.contains(':') && beyond.contains('@') {
        // `name:word` before an `@` further on: a port if the word is a number, or else a login
        // and a password.
        host = if ends_with_port(first) {
            ""
        } else {
            let after = &rest[rest.rfind('@').map_or(0, |at| at + 1)..];
            after.split(['/', '?', '#']).next().unwrap_or("")
        };
    }
    if is_host(host) { host.chars().take(40).collect() } else { scheme.to_string() }
}

fn ends_with_port(text: &str) -> bool {
    text.rsplit_once(':').is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

/// A host name, address or `[ipv6]`, with an optional `:port`.
fn is_host(text: &str) -> bool {
    let (name, port) = match text.rsplit_once(':') {
        Some((name, port)) if !text.starts_with('[') || name.ends_with(']') => (name, Some(port)),
        _ => (text, None),
    };
    if port.is_some_and(|port| port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit())) {
        return false;
    }
    if let Some(inner) = name.strip_prefix('[').and_then(|n| n.strip_suffix(']')) {
        return !inner.is_empty() && inner.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    }
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

// ---- shell commands --------------------------------------------------------------------------

/// Commands that work on files and nothing else: they read, search, move or edit them in place.
const FILE_WORDS: &[&str] = &[
    "cat", "head", "tail", "sed", "grep", "rg", "find", "ls", "wc", "cp", "mv", "mkdir", "touch", "diff", "cmp",
    "sort", "uniq", "cut", "tr", "awk", "tee", "stat", "file", "du", "tree", "ln", "basename", "dirname", "realpath",
    "jq",
];
/// Commands that say nothing either way: `cd` and `echo` do not make a line file work or shell work.
const NEUTRAL_WORDS: &[&str] = &["echo", "printf", "cd", "export", "true", "set", "pwd", "sleep", "which", "test", "["];
const NET_WORDS: &[&str] =
    &["curl", "wget", "gh", "ssh", "scp", "sftp", "ftp", "rsync", "nc", "ncat", "telnet", "ping", "dig", "nslookup"];
/// Commands that reach the network only with some of their subcommands. `git` is not here: all git
/// work, push and fetch included, lights Git.
const NET_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("npm", &["install", "i", "ci", "add", "update", "publish", "view", "audit"]),
    ("pnpm", &["install", "i", "add", "update", "publish"]),
    ("yarn", &["install", "add", "upgrade", "publish"]),
    ("bun", &["install", "add", "update", "publish"]),
    ("pip", &["install", "download"]),
    ("pip3", &["install", "download"]),
    ("docker", &["pull", "push", "login"]),
    ("brew", &["install", "update", "upgrade", "fetch"]),
    ("cargo", &["install", "fetch", "publish", "update"]),
];
/// Words that only run the command after them.
const WRAPPERS: &[&str] = &["rtk", "sudo", "env", "time", "exec", "nohup", "nice"];

/// Sorts a shell command into a station. The command name decides, also inside `$(…)`; `flowctl`
/// is checked first, then `git`, then the network. The label is the subcommand for flowctl and
/// git, else Claude's own `description`, else the command's name. Never the command line.
pub fn classify_shell(command: &str, description: &str) -> (Station, String) {
    let deep = segments(command, true);
    if let Some(words) = deep.iter().find(|words| command_name(&words[0]) == "flowctl") {
        return (Station::Agentflow, subcommand_label("flowctl", &words[1..]));
    }
    if let Some(words) = deep.iter().find(|words| command_name(&words[0]) == "git") {
        return (Station::Git, subcommand_label("git", &words[1..]));
    }

    let lines = segments(command, false);
    let description = description.trim();
    let label = if !description.is_empty() {
        description.to_string()
    } else {
        let first =
            lines.iter().find(|words| !NEUTRAL_WORDS.contains(&command_name(&words[0]).as_str())).or(lines.first());
        first.map_or_else(|| "shell".to_string(), |words| base(&words[0]))
    };
    let label = clean_label(&label);
    let label = if label.is_empty() { "shell".to_string() } else { label };

    let reaches_network = deep.iter().any(|words| {
        let head = command_name(&words[0]);
        NET_WORDS.contains(&head.as_str())
            || NET_SUBCOMMANDS
                .iter()
                .any(|(name, subs)| *name == head && words.get(1).is_some_and(|sub| subs.contains(&sub.as_str())))
    });
    if reaches_network {
        return (Station::Network, label);
    }
    let real: Vec<&Vec<String>> =
        lines.iter().filter(|words| !NEUTRAL_WORDS.contains(&command_name(&words[0]).as_str())).collect();
    if !real.is_empty() && real.iter().all(|words| FILE_WORDS.contains(&command_name(&words[0]).as_str())) {
        return (Station::Files, label);
    }
    (Station::Shell, label)
}

/// `flowctl ask`, `git commit`: the program and its subcommand, skipping options (and, for git,
/// the arguments of `-C` and `-c`). A subcommand that does not look like a name is left out, so
/// pasted text never becomes a label.
fn subcommand_label(program: &str, args: &[String]) -> String {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg.starts_with('-') {
            if program == "git" && matches!(arg.as_str(), "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
                args.next();
            }
            continue;
        }
        let valid = arg.len() <= 30
            && arg.starts_with(|c: char| c.is_ascii_lowercase())
            && arg.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        return if valid { format!("{program} {arg}") } else { program.to_string() };
    }
    program.to_string()
}

/// A command word without its folder or its Windows ending: `/mingw64/bin/git` and `git.exe` are
/// `git`.
fn base(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let lower = name.to_ascii_lowercase();
    for ending in [".exe", ".cmd", ".bat"] {
        if lower.ends_with(ending) && name.len() > ending.len() {
            return name[..name.len() - ending.len()].to_string();
        }
    }
    name.to_string()
}

/// The lower-case name to match a command word against.
fn command_name(word: &str) -> String {
    base(word).to_ascii_lowercase()
}

/// The words of each command in a shell line, with leading assignments and wrapper words
/// (`rtk`, `sudo`, `env`…) taken off. Quotes are removed but keep their word. `deep` also cuts at
/// `$(…)`, backticks and brackets, to find a command run inside another.
fn segments(command: &str, deep: bool) -> Vec<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut scan = Scan::default();
    let mut quote: Option<char> = None;
    // The quote in force outside each open `$(` or `(`.
    let mut outer: Vec<Option<char>> = Vec::new();
    // Closing words of the heredocs begun on this line, and whether `<<-` lets tabs indent them.
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    scan.cur.push(c);
                }
            }
            Some(_) => {
                if c == '"' {
                    quote = None;
                } else if c == '\\' && next == Some('"') {
                    scan.cur.push('"');
                    i += 1;
                } else if deep && c == '$' && next == Some('(') {
                    scan.end_segment();
                    outer.push(quote);
                    quote = None;
                    i += 1;
                } else if deep && c == '`' {
                    scan.end_segment();
                } else {
                    scan.cur.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    scan.has_word = true;
                }
                // A backslash is a path separator on Windows, so it stays; only a line
                // continuation goes.
                '\\' if next == Some('\n') => i += 1,
                ' ' | '\t' | '\r' => scan.end_word(),
                ';' => scan.end_segment(),
                '\n' => {
                    scan.end_segment();
                    // The lines of a heredoc are text for the command, not commands.
                    i = skip_heredocs(&chars, i + 1, &mut heredocs) - 1;
                }
                // `<<<` is a here-string: its word is an argument, so it is no heredoc.
                '<' if next == Some('<') && chars.get(i + 2) == Some(&'<') => {
                    scan.cur.push_str("<<<");
                    scan.has_word = true;
                    i += 2;
                }
                '<' if next == Some('<') => match heredoc_delimiter(&chars, i) {
                    Some((word, tabs, end)) => {
                        scan.end_word();
                        heredocs.push((word, tabs));
                        i = end - 1;
                    }
                    None => {
                        scan.cur.push(c);
                        scan.has_word = true;
                    }
                },
                '&' if next == Some('&') => {
                    scan.end_segment();
                    i += 1;
                }
                '|' => {
                    scan.end_segment();
                    if next == Some('|') {
                        i += 1;
                    }
                }
                '`' if deep => scan.end_segment(),
                '$' if deep && next == Some('(') => {
                    scan.end_segment();
                    outer.push(quote);
                    i += 1;
                }
                '(' if deep => {
                    scan.end_segment();
                    outer.push(quote);
                }
                ')' if deep => {
                    scan.end_segment();
                    if let Some(before) = outer.pop() {
                        quote = before;
                    }
                }
                _ => {
                    scan.cur.push(c);
                    scan.has_word = true;
                }
            },
        }
        i += 1;
    }
    scan.end_segment();
    scan.out.into_iter().map(strip_prefixes).filter(|words| !words.is_empty()).collect()
}

/// The word after `<<` or `<<-` at `at`, which closes the heredoc, quotes removed: the word, whether
/// `-` was given, and the index after it. `None` when it is not a heredoc (`1 << 3`).
fn heredoc_delimiter(chars: &[char], at: usize) -> Option<(String, bool, usize)> {
    let mut j = at + 2;
    let tabs = chars.get(j) == Some(&'-');
    if tabs {
        j += 1;
    }
    while matches!(chars.get(j), Some(' ' | '\t')) {
        j += 1;
    }
    let first = *chars.get(j)?;
    let mut word = String::new();
    if first == '\'' || first == '"' {
        j += 1;
        while let Some(&c) = chars.get(j) {
            j += 1;
            if c == first {
                break;
            }
            word.push(c);
        }
    } else if first.is_alphabetic() || first == '_' || first == '\\' {
        while let Some(&c) = chars.get(j) {
            if c.is_whitespace() || ";&|()<>".contains(c) {
                break;
            }
            if !matches!(c, '\\' | '\'' | '"') {
                word.push(c);
            }
            j += 1;
        }
    } else {
        return None;
    }
    if word.is_empty() { None } else { Some((word, tabs, j)) }
}

/// Where scanning resumes after the bodies of the heredocs begun on the line that just ended: the
/// start of the line after each closing word (or the end, if one never closes).
fn skip_heredocs(chars: &[char], mut at: usize, heredocs: &mut Vec<(String, bool)>) -> usize {
    for (word, tabs) in heredocs.drain(..) {
        while at < chars.len() {
            let end = chars[at..].iter().position(|&c| c == '\n').map_or(chars.len(), |found| at + found);
            let line: String = chars[at..end].iter().collect();
            at = (end + 1).min(chars.len());
            let line = line.trim_end_matches('\r');
            if (if tabs { line.trim_start_matches('\t') } else { line }) == word {
                break;
            }
        }
    }
    at
}

#[derive(Default)]
struct Scan {
    out: Vec<Vec<String>>,
    words: Vec<String>,
    cur: String,
    has_word: bool,
}

impl Scan {
    fn end_word(&mut self) {
        if self.has_word || !self.cur.is_empty() {
            self.words.push(std::mem::take(&mut self.cur));
        }
        self.has_word = false;
    }

    fn end_segment(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            self.out.push(std::mem::take(&mut self.words));
        }
    }
}

/// Leading `NAME=value` words, and wrappers with their options, are not the command.
fn strip_prefixes(words: Vec<String>) -> Vec<String> {
    let mut start = 0;
    loop {
        while words.get(start).is_some_and(|word| is_assignment(word)) {
            start += 1;
        }
        let Some(word) = words.get(start) else { break };
        let wrapper = command_name(word);
        if start + 1 < words.len() && WRAPPERS.contains(&wrapper.as_str()) {
            start += 1;
            while let Some(flag) = words.get(start).filter(|flag| flag.starts_with('-') && start + 1 < words.len()) {
                start += if wrapper_takes_value(&wrapper, flag) { 2 } else { 1 };
            }
            continue;
        }
        break;
    }
    words.get(start..).unwrap_or_default().to_vec()
}

/// Options of a wrapper that take the next word as their value (`env -u NAME`, `nice -n 5`).
fn wrapper_takes_value(wrapper: &str, flag: &str) -> bool {
    matches!(
        (wrapper, flag),
        ("env", "-u" | "--unset" | "-C" | "--chdir") | ("sudo", "-u" | "-g") | ("nice", "-n") | ("time", "-f" | "-o")
    )
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else { return false };
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
