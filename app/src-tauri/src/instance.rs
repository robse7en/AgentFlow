//! Remember an explicitly selected existing instance without copying its database or secrets.
use agentflow_core::config::{Config, Locations, find_checkout, fresh_config, read_config};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, super::Error>;
const SELECTION: &str = "desktop-instance.json";

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    config_path: PathBuf,
    base_directory: PathBuf,
}

pub fn selected(base: &Locations) -> Result<Locations> {
    if base.config_override.is_some() || !base.home.join(SELECTION).exists() {
        return Ok(base.clone());
    }
    let selection: Selection = serde_json::from_slice(&std::fs::read(base.home.join(SELECTION))?)?;
    if !selection.config_path.is_absolute() || !selection.base_directory.is_absolute() {
        return Err("The saved instance must use absolute paths. Choose it again in setup.".into());
    }
    let mut at = base.clone();
    at.config_override = Some(selection.config_path);
    at.checkout = Some(selection.base_directory);
    Ok(at)
}

/// Use the source checkout for relative paths, or the explicit root for an installed/custom copy.
/// The old file, task folders and database are left in place, including its credentials.
pub fn existing(base: &Locations, config_path: &str, base_directory: &str) -> Result<Locations> {
    let source = PathBuf::from(config_path.trim());
    if !source.is_absolute() || !source.is_file() {
        return Err("Choose an existing config.json using its full path.".into());
    }
    read_config(&source)?;
    let root = if base_directory.trim().is_empty() {
        find_checkout(&source).unwrap_or_else(|| source.parent().unwrap().to_path_buf())
    } else {
        PathBuf::from(base_directory.trim())
    };
    if !root.is_absolute() || !root.is_dir() {
        return Err("The base directory must be an existing folder using its full path.".into());
    }
    let mut at = base.clone();
    at.config_override = Some(source.canonicalize()?);
    at.checkout = Some(root.canonicalize()?);
    Ok(at)
}

pub fn remember(base: &Locations, at: &Locations) -> Result<()> {
    // A process-level config override continues to take precedence on later launches.
    let selection =
        Selection { config_path: at.config_path(), base_directory: at.checkout.as_ref().unwrap_or(&at.home).clone() };
    atomic_write(&base.home.join(SELECTION), &serde_json::to_vec_pretty(&selection)?, false)
}

pub fn create(base: &Locations, form: &super::onboarding::SetupForm) -> Result<Locations> {
    let path = base.config_path();
    if path.exists() && form.repair != "yes" {
        return Err("A configuration already exists here. Use the existing instance option to keep it.".into());
    }
    let previous = if path.exists() { Some(std::fs::read(&path)?) } else { None };
    let mut config = if previous.is_some() { read_config(&path)? } else { fresh_config(base) };
    if previous.is_none() && base.home_overridden {
        let home = base.home.to_string_lossy();
        config.app.data_directory = home.to_string();
        config.app.database_path = base.home.join("agentflow.db").to_string_lossy().into_owned();
        config.workspace.task_directory = base.home.join("tasks").to_string_lossy().into_owned();
        config.workspace.worktree_directory = base.home.join("worktrees").to_string_lossy().into_owned();
    }
    let repository = PathBuf::from(form.repository.trim());
    if !form.repository.trim().is_empty() && (!repository.is_absolute() || !repository.is_dir()) {
        return Err("Choose your repository using the full path to an existing folder.".into());
    }
    let harness = config.harnesses.get_mut(&form.harness).ok_or("Choose one of the listed coding CLIs.")?;
    harness.billing_mode = match form.billing.as_str() {
        "subscription" => agentflow_core::config::BillingMode::Subscription,
        "metered" => agentflow_core::config::BillingMode::Metered,
        "unknown" => agentflow_core::config::BillingMode::Unknown,
        _ => return Err("Choose the billing source for your coding CLI.".into()),
    };
    if form.harness == "copilot" {
        harness.billing_mode = agentflow_core::config::BillingMode::Subscription;
    }
    let agent = agentflow_core::config::AgentConfig {
        harness: form.harness.clone(),
        model: None,
        reasoning: Some(agentflow_core::config::Reasoning::High),
    };
    config.agents = agentflow_core::config::AgentsConfig {
        architect: agent.clone(),
        orchestrator: None, // The Architect plans Parallel tasks itself.
        implementer: agent.clone(),
        code_reviewer: agent.clone(),
        security_reviewer: agent,
    };
    for (key, harness) in &mut config.harnesses {
        harness.enabled = key == &form.harness;
    }
    if !form.repository.trim().is_empty() {
        config.workspace.default_repository_directory = Some(repository.to_string_lossy().into_owned());
    }
    let jira_entered = [&form.jira_url, &form.jira_token, &form.jira_email].iter().any(|v| !v.trim().is_empty());
    if jira_entered {
        if form.jira_url.trim().is_empty() {
            return Err("Enter your Jira base URL, or leave all Jira fields blank to skip Jira.".into());
        }
        let url: tauri::Url = form.jira_url.trim().parse()?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Use an HTTPS Jira URL without credentials, query parameters or a fragment.".into());
        }
        if form.jira_token.trim().is_empty() {
            return Err("Enter your Jira API token or personal access token.".into());
        }
        if form.jira_kind == "cloud" && form.jira_email.trim().is_empty() {
            return Err("Jira Cloud also requires your account email.".into());
        }
        if !["cloud", "server"].contains(&form.jira_kind.as_str()) {
            return Err("Choose Jira Cloud or Server / Data Center.".into());
        }
        config.jira.enabled = true;
        config.jira.base_url = Some(url.as_str().trim_end_matches('/').to_string());
        config.jira.api_token = Some(form.jira_token.trim().to_string());
        config.jira.email = (form.jira_kind == "cloud").then(|| form.jira_email.trim().to_string());
    } else if config.jira.enabled && !config.jira.missing().is_empty() {
        // Jira is optional: left blank, an incomplete Jira section stops blocking startup.
        config.jira.enabled = false;
    }
    // Apply the same validation as a loaded Python or Rust configuration before saving.
    let config = Config::from_value(serde_json::to_value(config)?).map_err(|e| e.join("\n"))?;
    config.validate_references()?;
    if let Some(previous) = previous {
        let backup = path.with_extension(format!("json.{}.bak", agentflow_core::api::auth::token_hex(8)));
        atomic_write(&backup, &previous, true)?;
        // Reject changes made by the old process since we read the file.
        if std::fs::read(&path)? != previous {
            return Err("The configuration changed during setup. Try again.".into());
        }
        atomic_write(&path, (config.to_json() + "\n").as_bytes(), false)?;
    } else {
        atomic_write(&path, (config.to_json() + "\n").as_bytes(), true)?;
    }
    Ok(base.clone())
}

fn atomic_write(path: &Path, bytes: &[u8], new_only: bool) -> Result<()> {
    let parent = path.parent().ok_or("The destination needs a parent directory.")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if new_only {
        file.persist_noclobber(path)?;
    } else {
        file.persist(path)?;
    }
    Ok(())
}
