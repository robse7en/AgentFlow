//! The bundled usage collectors, ported from `scripts/usage_claude.py`, `usage_codex.py`,
//! `usage_copilot.py` and `usage_pricing.py`. Each reads one harness's own session log for a
//! conversation's totals so far.
//!
//! The engine runs these in-process when a harness's `usageCommand` names one of the bundled
//! scripts (as the example config and the setup wizard write it), so config.json stays valid for
//! the Python backend too. Any other command still runs as a process.
//!
//! Each collector returns what the script would have exited with: a report (exit 0), a report or
//! nothing while the provider is still writing (exit 75), or nothing usable (exit 1). Where the
//! Python script would have raised on malformed data, the collector returns [`Outcome::Unavailable`]
//! too. Checked against the scripts across many fixtures (contract/golden/usage).

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod pricing;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};

/// What a collector found.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The session's totals (the script printed them and exited 0).
    Report(Value),
    /// The latest turn is not fully written yet (exit 75), with the totals so far when known.
    Pending(Option<Value>),
    /// No usable usage (exit 1).
    Unavailable,
}

/// A bundled collector, by the harness type it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Collector {
    Claude,
    Codex,
    Copilot,
}

impl Collector {
    pub fn script(self) -> &'static str {
        match self {
            Collector::Claude => "usage_claude.py",
            Collector::Codex => "usage_codex.py",
            Collector::Copilot => "usage_copilot.py",
        }
    }
}

/// The bundled collector a usage command runs, if any: an argument whose file name is one of the
/// bundled scripts (`["python", "scripts/usage_claude.py"]`, or the wizard's absolute paths).
pub fn bundled(argv: &[String]) -> Option<Collector> {
    argv.iter().find_map(|arg| {
        let name = arg.rsplit(['/', '\\']).next().unwrap_or_default().to_lowercase();
        [Collector::Claude, Collector::Codex, Collector::Copilot].into_iter().find(|c| c.script() == name)
    })
}

/// Fetches the models.dev price catalog's bytes within its deadline; None on any failure.
pub type Downloader = Arc<dyn Fn() -> Option<Vec<u8>> + Send + Sync>;

/// Where the collectors look, injectable so tests never read the user's own logs or the network.
#[derive(Clone)]
pub struct CollectorEnv {
    /// Python's `Path.home()`.
    pub home: PathBuf,
    /// `$COPILOT_API_HOME`, else `~/.local/share/copilot-api`: the Copilot proxy's data.
    pub copilot_api_home: PathBuf,
    /// Fetches the models.dev price catalog's bytes within its deadline; None on any failure.
    pub download: Downloader,
}

impl CollectorEnv {
    /// The real environment: this user's home, and the network for the price catalog.
    pub fn from_process() -> CollectorEnv {
        let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("."));
        CollectorEnv::at(home, std::env::var("COPILOT_API_HOME").ok(), Arc::new(pricing::download_catalog))
    }

    /// A home directory, an optional `COPILOT_API_HOME` (empty counts as unset), and a downloader.
    pub fn at(home: PathBuf, copilot_api_home: Option<String>, download: Downloader) -> CollectorEnv {
        let copilot_api_home = match copilot_api_home.filter(|v| !v.is_empty()) {
            Some(configured) => PathBuf::from(configured),
            None => home.join(".local").join("share").join("copilot-api"),
        };
        CollectorEnv { home, copilot_api_home, download }
    }
}

/// Run a collector for a harness session (`AGENTFLOW_SESSION_ID`). Blocking file and network work.
pub fn run(collector: Collector, env: &CollectorEnv, session_id: &str) -> Outcome {
    let outcome = match collector {
        Collector::Claude => claude::main(env, session_id),
        Collector::Codex => codex::main(env, session_id),
        Collector::Copilot => copilot::main(env, session_id),
    };
    outcome.unwrap_or(Outcome::Unavailable)
}

// ------------------------------------------------------------------------------ Python semantics

/// The script raised: it exits 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crash;

pub type Py<T> = Result<T, Crash>;

pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `value.get(key)`: raises unless `value` is a dict.
pub fn get<'a>(value: &'a Value, key: &str) -> Py<Option<&'a Value>> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        _ => Err(Crash),
    }
}

/// `(value or {}).get(key)`.
pub fn get_or<'a>(value: Option<&'a Value>, key: &str) -> Py<Option<&'a Value>> {
    match value {
        Some(value) if truthy(Some(value)) => get(value, key),
        _ => Ok(None),
    }
}

/// `value or {}` as a map to look up in: None for the empty default.
pub fn object_or_empty(value: Option<&Value>) -> Py<Option<&Map<String, Value>>> {
    match value {
        Some(value) if truthy(Some(value)) => value.as_object().map(Some).ok_or(Crash),
        _ => Ok(None),
    }
}

/// `int(value or 0)`: truncates floats, accepts bools and numeric strings, raises on the rest.
pub fn py_int(value: Option<&Value>) -> Py<i64> {
    if !truthy(value) {
        return Ok(0);
    }
    match value {
        Some(Value::Bool(b)) => Ok(i64::from(*b)),
        Some(Value::Number(n)) => match n.as_i64() {
            Some(i) => Ok(i),
            None => n.as_f64().filter(|f| f.is_finite() && f.abs() < 9.2e18).map(|f| f.trunc() as i64).ok_or(Crash),
        },
        Some(Value::String(s)) => parse_py_int(s).ok_or(Crash),
        _ => Err(Crash),
    }
}

/// Python's `int(str)`: surrounding whitespace, a sign, and digits with single underscores.
fn parse_py_int(text: &str) -> Option<i64> {
    let text = crate::engine::prompts::py_strip(text);
    let (negative, digits) = match text.strip_prefix(['+', '-']) {
        Some(rest) => (text.starts_with('-'), rest),
        None => (false, text),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut value: i64 = 0;
    for c in digits.chars().filter(|c| *c != '_') {
        value = value.checked_mul(10)?.checked_add(i64::from(c.to_digit(10)?))?;
    }
    Some(if negative { -value } else { value })
}

/// `float(value or 0)`.
pub fn py_float(value: Option<&Value>) -> Py<f64> {
    if !truthy(value) {
        return Ok(0.0);
    }
    match value {
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64().ok_or(Crash),
        Some(Value::String(s)) => {
            crate::engine::prompts::py_strip(s).replace('_', "").parse::<f64>().map_err(|_| Crash)
        }
        _ => Err(Crash),
    }
}

/// A file's lines as a Python text-mode loop yields them (universal newlines), each stripped.
/// Invalid UTF-8 raises, as reading it in Python does.
pub fn stripped_lines(path: &Path) -> Py<Vec<String>> {
    let bytes = std::fs::read(path).map_err(|_| Crash)?;
    let text = String::from_utf8(bytes).map_err(|_| Crash)?;
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    Ok(text.split_inclusive('\n').map(|line| crate::engine::prompts::py_strip(line).to_string()).collect())
}

/// `json.loads(line)`, None where Python's would raise `JSONDecodeError`.
pub fn json_line(line: &str) -> Option<Value> {
    serde_json::from_str(line).ok()
}

/// Python's `re.fullmatch(r"[A-Za-z0-9_-]{1,128}", session_id)`.
pub fn safe_session_id(session_id: &str) -> bool {
    (1..=128).contains(&session_id.len())
        && session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bundled_scripts_are_recognised_by_file_name() {
        let argv = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(bundled(&argv(&["python", "scripts/usage_claude.py"])), Some(Collector::Claude));
        assert_eq!(
            bundled(&argv(&["C:/py/python.exe", "D:\\AgentFlow\\scripts\\usage_copilot.py"])),
            Some(Collector::Copilot)
        );
        assert_eq!(bundled(&argv(&["collector"])), None);
        assert_eq!(bundled(&argv(&["python", "my_usage_codex.py"])), None);
    }

    #[test]
    fn python_conversions() {
        assert_eq!(py_int(Some(&json!("  1_000 "))), Ok(1000));
        assert_eq!(py_int(Some(&json!(3.9))), Ok(3));
        assert_eq!(py_int(Some(&json!(-3.9))), Ok(-3));
        assert_eq!(py_int(Some(&json!(true))), Ok(1));
        assert_eq!(py_int(Some(&json!(""))), Ok(0));
        assert_eq!(py_int(Some(&json!("1.5"))), Err(Crash));
        assert_eq!(py_int(Some(&json!([1]))), Err(Crash));
        assert_eq!(py_float(Some(&json!("2.5"))), Ok(2.5));
        assert_eq!(py_float(None), Ok(0.0));
        assert!(safe_session_id("a-B_9") && !safe_session_id("a.b") && !safe_session_id(""));
    }

    #[test]
    fn universal_newlines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, "a\r\nb\rc\n d \x0b\n").unwrap();
        assert_eq!(stripped_lines(&path).unwrap(), ["a", "b", "c", "d"]);
        std::fs::write(&path, b"\xff\n").unwrap();
        assert_eq!(stripped_lines(&path), Err(Crash));
    }
}
