//! Agent activity: one line of a harness transcript becomes a few small events, and each tool
//! call is sorted into one of nine stations with a short label.
//!
//! This slice is pure: no files, no clock, no threads. Later slices tail the transcripts, keep a
//! per-run summary and send it to the UI. See `notes/agent-activity-map-plan.md`.
//!
//! **Labels carry names, never content.** A label may be a file's last two path parts, a web
//! host, a command name, a subcommand or Claude's own short description. It never holds a command
//! line, a prompt, a query, a URL path or file contents, and nothing here logs any input.
//!
//! Only Claude Code lines are understood so far; Copilot and Codex return no events.

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Serialize, Serializer};
use serde_json::Value;

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
#[derive(Debug, Default)]
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

// ---- Claude lines ----------------------------------------------------------------------------

fn parse_claude_line(line: &str) -> Vec<Parsed> {
    let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    let Some(at) = entry.get("timestamp").and_then(Value::as_str).and_then(parse_time) else { return Vec::new() };
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
    // Like the usage collector: no usage, no model or a synthetic message is not a step.
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
