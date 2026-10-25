CREATE TABLE tasks (id INTEGER NOT NULL PRIMARY KEY, jira_key VARCHAR(64) NOT NULL, repository_path TEXT NOT NULL,
  repository_root TEXT NOT NULL, project_relative_path TEXT, base_branch VARCHAR(255) NOT NULL,
  branch_name VARCHAR(255) NOT NULL, worktree_path TEXT NOT NULL, zellij_session_name VARCHAR(255) NOT NULL,
  task_dir TEXT NOT NULL, additional_context TEXT, status VARCHAR(32) NOT NULL, status_message TEXT,
  review_iteration INTEGER NOT NULL, max_review_iterations INTEGER NOT NULL, current_sha VARCHAR(64),
  agent_overrides JSON NOT NULL, created_at DATETIME NOT NULL, updated_at DATETIME NOT NULL);
CREATE UNIQUE INDEX ix_tasks_jira_key ON tasks (jira_key);
CREATE TABLE agent_questions (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER,
  role VARCHAR(64) NOT NULL, message TEXT NOT NULL, answered BOOLEAN NOT NULL, created_at DATETIME NOT NULL,
  answered_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE agent_sessions (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, role VARCHAR(64) NOT NULL,
  harness VARCHAR(64) NOT NULL, model VARCHAR(128), reasoning VARCHAR(32), zellij_tab_name VARCHAR(128) NOT NULL,
  zellij_tab_id INTEGER, zellij_pane_id VARCHAR(32), harness_session_id VARCHAR(128), status VARCHAR(32) NOT NULL,
  current_run_id INTEGER, started_at DATETIME NOT NULL, ended_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE review_findings (id INTEGER NOT NULL PRIMARY KEY, task_id INTEGER NOT NULL, node_run_id INTEGER,
  finding_id VARCHAR(32) NOT NULL, type VARCHAR(32) NOT NULL, iteration INTEGER NOT NULL, reviewed_sha VARCHAR(64),
  severity VARCHAR(16) NOT NULL, blocking BOOLEAN NOT NULL, file TEXT, line INTEGER, title TEXT NOT NULL,
  description TEXT, recommendation TEXT, status VARCHAR(16) NOT NULL, created_at DATETIME NOT NULL,
  resolved_at DATETIME, FOREIGN KEY(task_id) REFERENCES tasks (id));
CREATE TABLE run_evidence (uid VARCHAR(36) NOT NULL PRIMARY KEY, run_id INTEGER NOT NULL UNIQUE,
  requirements_digest VARCHAR(64), verification JSON, usage JSON, confirmed_model VARCHAR(128),
  FOREIGN KEY(run_id) REFERENCES node_runs (id));
INSERT INTO tasks VALUES (1, 'OLD-1', '/r', '/r', NULL, 'main', 'agent/OLD-1', '/w', 'agentflow-OLD-1', '/t', NULL,
  'ACTIVE', NULL, 1, 5, NULL, '{}', '2025-01-01 00:00:00.000000', '2025-01-01 00:00:00.000000');
INSERT INTO agent_questions VALUES (1, 1, NULL, 'architect', 'Old question?', 0, '2025-01-01 00:00:00.000000', NULL);
INSERT INTO agent_questions VALUES (2, 1, NULL, 'architect', 'Another?', 1, '2025-01-01 00:00:00', '2025-01-02 00:00:00');
