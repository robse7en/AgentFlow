//! List-price estimates for the bundled collectors (`scripts/usage_pricing.py`).
//!
//! Prices come from the public models.dev catalog: the copy the Copilot proxy (copilot-api)
//! already caches, else our own copy refreshed weekly. Estimates are reported with
//! `"costKind": "estimated"` so they are never added to billed amounts or counted against budgets.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

use super::{CollectorEnv, Crash, Py, object_or_empty, py_float, truthy};

pub const CATALOG_URL: &str = "https://models.dev/api.json";
const MAX_CATALOG_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// A hard overall deadline: the collector gets 15s in total, and a slow download must cost only
/// the estimate, never the token report.
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(6);
const DOWNLOAD_LIMIT: usize = 64 * 1024 * 1024;
const PREFERRED_PROVIDERS: [&str; 4] = ["anthropic", "openai", "google", "xai"];

fn read_json(path: &Path) -> Option<Map<String, Value>> {
    let text = String::from_utf8(std::fs::read(path).ok()?).ok()?;
    match serde_json::from_str(&text).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// The catalog's bytes from models.dev, or None on any failure or past the deadline.
pub fn download_catalog() -> Option<Vec<u8>> {
    let deadline = Instant::now() + DOWNLOAD_DEADLINE;
    let agent =
        ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(3)).timeout_read(Duration::from_secs(3)).build();
    // models.dev rejects requests without a User-Agent.
    let response = agent.get(CATALOG_URL).set("User-Agent", "agentflow-usage-collector").call().ok()?;
    let mut reader = response.into_reader();
    let mut data = Vec::new();
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut chunk).ok()?;
        if read == 0 {
            return Some(data);
        }
        data.extend_from_slice(&chunk[..read]);
        if Instant::now() > deadline || data.len() > DOWNLOAD_LIMIT {
            return None;
        }
    }
}

/// Download into our own cache (written through a temp file, so a reader never sees half of it).
fn download(env: &CollectorEnv, target: &Path) -> Option<Map<String, Value>> {
    let data = (env.download)()?;
    let catalog = match serde_json::from_slice(&data).ok()? {
        Value::Object(map) => map,
        _ => return None,
    };
    std::fs::create_dir_all(target.parent()?).ok()?;
    let partial = target.with_extension("tmp");
    std::fs::write(&partial, &data).ok()?;
    std::fs::rename(&partial, target).ok()?;
    Some(catalog)
}

/// The price catalog: the Copilot proxy's copy, else our own fresh copy, else a new download,
/// else our own stale copy.
pub fn load_catalog(env: &CollectorEnv) -> Option<Map<String, Value>> {
    if let Some(catalog) = read_json(&env.copilot_api_home.join("models-dev-api.json")).filter(|c| !c.is_empty()) {
        return Some(catalog);
    }
    let own = env.home.join(".cache").join("agentflow").join("models-dev-api.json");
    let fresh = std::fs::metadata(&own)
        .and_then(|m| m.modified())
        .map(|modified| SystemTime::now().duration_since(modified).map_or(true, |age| age < MAX_CATALOG_AGE))
        .unwrap_or(false);
    let cached = if fresh { read_json(&own) } else { None };
    cached
        .filter(|c| !c.is_empty())
        .or_else(|| download(env, &own).filter(|c| !c.is_empty()))
        .or_else(|| read_json(&own))
}

/// The catalog names to try for a model: lower case, without a `[1m]`-style suffix or a
/// `provider/` prefix, and with dots as dashes.
pub fn candidates(model: &str) -> Vec<String> {
    let mut name = crate::engine::prompts::py_strip(model).to_lowercase();
    // re.sub(r"\[[^\]]*\]$", "", name): the leftmost '[' after the last inner ']'.
    if name.ends_with(']') {
        let inner = &name[..name.len() - 1];
        let after_close = inner.rfind(']').map_or(0, |i| i + 1);
        if let Some(open) = inner[after_close..].find('[') {
            name.truncate(after_close + open);
        }
    }
    let name = name.rsplit('/').next().unwrap_or_default().to_string();
    let dashed = name.replace('.', "-");
    if dashed == name { vec![name] } else { vec![name, dashed] }
}

/// The first catalog entry with token prices, preferring the model's own vendor.
pub fn rates_for<'a>(catalog: Option<&'a Map<String, Value>>, model: &Value) -> Py<Option<&'a Map<String, Value>>> {
    let Some(catalog) = catalog.filter(|c| !c.is_empty()) else { return Ok(None) };
    if !truthy(Some(model)) {
        return Ok(None);
    }
    let Some(model) = model.as_str() else { return Err(Crash) };
    let mut providers: Vec<&str> = PREFERRED_PROVIDERS.iter().copied().filter(|p| catalog.contains_key(*p)).collect();
    providers.extend(catalog.keys().map(String::as_str).filter(|p| !PREFERRED_PROVIDERS.contains(p)));
    for name in candidates(model) {
        for provider in &providers {
            let Some(provider) = object_or_empty(catalog.get(*provider))? else { continue };
            let Some(models) = object_or_empty(provider.get("models"))? else { continue };
            let Some(entry) = object_or_empty(models.get(&name))? else { continue };
            if let Some(Value::Object(cost)) = entry.get("cost")
                && (truthy(cost.get("input")) || truthy(cost.get("output")))
            {
                return Ok(Some(cost));
            }
        }
    }
    Ok(None)
}

fn number(value: Option<&Value>) -> Py<f64> {
    match value {
        None => Ok(0.0),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64().ok_or(Crash),
        // Python compares the size with ints, which raises for anything else.
        Some(_) => Err(Crash),
    }
}

/// The long-context tier a single request of `context_tokens` falls in, else the base rates.
fn tier(rates: &Map<String, Value>, context_tokens: Option<i64>) -> Py<&Map<String, Value>> {
    let Some(context) = context_tokens else { return Ok(rates) };
    let mut selected = rates;
    let mut selected_size = 0.0;
    let tiers = match rates.get("tiers") {
        Some(value) if truthy(Some(value)) => value.as_array().ok_or(Crash)?.as_slice(),
        _ => &[],
    };
    for tier in tiers {
        let tier_map = tier.as_object().ok_or(Crash)?;
        let spec = object_or_empty(tier_map.get("tier"))?;
        let size_value = spec.and_then(|s| s.get("size")).filter(|v| truthy(Some(v)));
        let is_context = spec.and_then(|s| s.get("type")) == Some(&Value::String("context".into()));
        if !is_context {
            continue;
        }
        let size = number(size_value)?;
        if selected_size < size && size < context as f64 {
            selected = tier_map;
            selected_size = size;
        }
    }
    Ok(selected)
}

/// One input to [`price`]: token counts by kind.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokens {
    pub uncached: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cache_write_1h: i64,
    pub output: i64,
}

/// USD at per-million list prices. Only a single request's context selects a long-context tier.
pub fn price(rates: &Map<String, Value>, tokens: Tokens, per_request: bool) -> Py<f64> {
    let context = per_request.then(|| tokens.uncached + tokens.cache_read + tokens.cache_write + tokens.cache_write_1h);
    let rate = tier(rates, context)?;
    let input_rate = py_float(rate.get("input"))?;
    // rate.get(key, input_rate) or 0: a missing key falls back to the input price, a null one is 0.
    let or_input = |key: &str| match rate.get(key) {
        None => Ok(input_rate),
        Some(value) => py_float(Some(value)),
    };
    let total = tokens.uncached.max(0) as f64 * input_rate
        + tokens.cache_read as f64 * or_input("cache_read")?
        + tokens.cache_write as f64 * or_input("cache_write")?
        // Anthropic bills one-hour cache writes at twice the input price.
        + tokens.cache_write_1h as f64 * input_rate * 2.0
        + tokens.output as f64 * py_float(rate.get("output"))?;
    Ok(total / 1_000_000.0)
}

/// `rates_for` on a model given as text (the collectors' common case).
pub fn rates_for_name<'a>(catalog: Option<&'a Map<String, Value>>, model: &str) -> Py<Option<&'a Map<String, Value>>> {
    rates_for(catalog, &Value::String(model.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_follow_the_regex() {
        assert_eq!(candidates(" GPT-Test[1m] "), ["gpt-test"]);
        assert_eq!(candidates("openai/gpt-5.5"), ["gpt-5.5", "gpt-5-5"]);
        assert_eq!(candidates("a[b[c]"), ["a"]);
        assert_eq!(candidates("a[x]y[z]"), ["a[x]y"]);
        assert_eq!(candidates("a]b]"), ["a]b]"]);
    }
}
