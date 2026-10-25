use super::*;
use agentflow_core::config::{Config, read_config};

fn base(root: &Path) -> Locations {
    Locations { home: root.join("home"), home_overridden: true, checkout: None, config_override: None }
}

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("instance-test-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
        .unwrap()
}

#[test]
fn selecting_existing_preserves_config_and_relative_data_in_place() {
    let temp = tempdir();
    let source = temp.path().join("original");
    std::fs::create_dir_all(&source).unwrap();
    let mut config = Config::default();
    config.app.data_directory = "data".into();
    config.app.database_path = "data/history.db".into();
    config.workspace.task_directory = "data/tasks".into();
    config.workspace.worktree_directory = "worktrees".into();
    let file = source.join("config.json");
    let original = config.to_json();
    std::fs::write(&file, &original).unwrap();
    let base = base(temp.path());
    let at = existing(&base, file.to_str().unwrap(), source.to_str().unwrap()).unwrap();
    remember(&base, &at).unwrap();
    let selected = selected(&base).unwrap();
    let loaded = read_config(&selected.config_path()).unwrap();
    assert_eq!(loaded.database_file(&selected), source.canonicalize().unwrap().join("data/history.db"));
    assert_eq!(loaded.task_dir(&selected), source.canonicalize().unwrap().join("data/tasks"));
    assert_eq!(loaded.worktree_dir(&selected), source.canonicalize().unwrap().join("worktrees"));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    assert!(!base.config_path().exists());
}

#[test]
fn explicit_config_override_takes_precedence_over_saved_selection() {
    let temp = tempdir();
    let base = base(temp.path());
    let mut selection = base.clone();
    selection.config_override = Some(temp.path().join("old.json"));
    remember(&base, &selection).unwrap();
    let mut explicit = base;
    explicit.config_override = Some(temp.path().join("explicit.json"));
    assert_eq!(selected(&explicit).unwrap(), explicit);
}

#[test]
fn a_broken_saved_selection_is_an_error_and_is_not_replaced() {
    let temp = tempdir();
    let base = base(temp.path());
    std::fs::create_dir_all(&base.home).unwrap();
    std::fs::write(base.home.join(SELECTION), "invalid").unwrap();
    assert!(selected(&base).is_err());
    assert_eq!(std::fs::read_to_string(base.home.join(SELECTION)).unwrap(), "invalid");
}

#[test]
fn new_setup_cannot_overwrite_an_existing_configuration_without_consent() {
    let temp = tempdir();
    let base = base(temp.path());
    std::fs::create_dir_all(&base.home).unwrap();
    std::fs::write(base.config_path(), "original settings").unwrap();
    let form = super::super::onboarding::SetupForm::default();
    assert!(create(&base, &form).is_err());
    assert_eq!(std::fs::read_to_string(base.config_path()).unwrap(), "original settings");
}

#[test]
fn existing_selection_rejects_missing_and_relative_paths() {
    let temp = tempdir();
    let base = base(temp.path());
    assert!(existing(&base, "config.json", "").is_err());
    assert!(existing(&base, temp.path().join("missing.json").to_str().unwrap(), "").is_err());
}

#[test]
fn fresh_setup_needs_no_repository_cli_or_jira() {
    let temp = tempdir();
    let base = base(temp.path());
    let form = super::super::onboarding::SetupForm {
        harness: "claude".into(),
        billing: "unknown".into(),
        ..Default::default()
    };
    let at = create(&base, &form).unwrap();
    let config = read_config(&at.config_path()).unwrap();
    assert!(!config.jira.enabled);
    assert!(config.workspace.default_repository_directory.is_none());
}

fn repair_with_jira(jira: serde_json::Value) -> Config {
    let temp = tempdir();
    let base = base(temp.path());
    std::fs::create_dir_all(&base.home).unwrap();
    std::fs::write(base.config_path(), serde_json::json!({"jira": jira}).to_string()).unwrap();
    let form = super::super::onboarding::SetupForm {
        harness: "claude".into(),
        billing: "unknown".into(),
        repair: "yes".into(),
        ..Default::default()
    };
    read_config(&create(&base, &form).unwrap().config_path()).unwrap()
}

#[test]
fn repairing_with_blank_jira_fields_turns_off_an_incomplete_jira_section() {
    let config = repair_with_jira(serde_json::json!({"enabled": true, "baseUrl": "https://x.atlassian.net"}));
    assert!(!config.jira.enabled);
    assert!(agentflow_core::config::jira_startup_problem(&config).is_none());
}

#[test]
fn repairing_with_blank_jira_fields_keeps_a_complete_jira_section() {
    let config = repair_with_jira(serde_json::json!({
        "enabled": true, "baseUrl": "https://x.atlassian.net", "email": "a@b.com", "apiToken": "t",
    }));
    assert!(config.jira.enabled && config.jira.api_token.as_deref() == Some("t"));
}

#[test]
fn a_jira_token_without_a_url_is_explained() {
    let temp = tempdir();
    let base = base(temp.path());
    let form = super::super::onboarding::SetupForm {
        harness: "claude".into(),
        billing: "unknown".into(),
        jira_token: "t".into(),
        ..Default::default()
    };
    assert!(create(&base, &form).unwrap_err().to_string().contains("Jira base URL"));
    assert!(!base.config_path().exists());
}
