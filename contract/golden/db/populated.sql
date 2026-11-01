BEGIN TRANSACTION;
CREATE TABLE agent_access (
	id VARCHAR(36) NOT NULL, 
	task_id INTEGER NOT NULL, 
	run_id INTEGER NOT NULL, 
	role VARCHAR(64) NOT NULL, 
	token_hash VARCHAR(64) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id), 
	UNIQUE (run_id), 
	FOREIGN KEY(run_id) REFERENCES node_runs (id), 
	UNIQUE (token_hash)
);
INSERT INTO "agent_access" VALUES('18a26a6f-ccf9-414c-a5d3-8f8de8bcdc94',1,1,'architect','3b53eac09018d508259479447c14c5e9015896fe01da0f1e2fd467033c1d4de9');
INSERT INTO "agent_access" VALUES('b057a1ef-c99b-40ef-a095-59a75a084c22',1,2,'implementer','768925417b878cc528b50030fe27de6bfb457c7c43f4a0fb2a79988ee0fa3c42');
INSERT INTO "agent_access" VALUES('21ed8af1-120f-448e-8293-1ff586ba719c',1,3,'code_reviewer','28eb7834382ba217170a16cb4919d5aec5a82dcd9213c901a6ae0208ae1d3090');
INSERT INTO "agent_access" VALUES('e5d8090e-5565-4376-9628-4c50a9fd18e3',1,4,'security_reviewer','ead3a7d1c469c78c1313e71214565d66c4b21ea39072e9760af9f31144af6593');
INSERT INTO "agent_access" VALUES('ff9271a1-d9a4-4be9-ba22-83a459c34fe2',1,5,'implementer','a496d3580c512839b77bde9a3148fd6638e94e8a6801c8af0784b7e06d13a752');
INSERT INTO "agent_access" VALUES('78f9d376-dd8d-4127-a1a3-e43a381cb997',1,6,'code_reviewer','6276ea0c79e0601c1ff3d12516444c85da45cc0fed49bc430cbbf5b635f4c102');
INSERT INTO "agent_access" VALUES('b2974fae-70cb-4c5b-b1ef-4b28696cc785',1,7,'security_reviewer','d967ededdcd148e5ebfbbd31375deb595bee42b4b35cd886b1df9a0b1af679c0');
INSERT INTO "agent_access" VALUES('2e94f1ce-5855-41a8-bcd5-cda2fd2c87fb',1,8,'architect','e9d1a422cd77e9d3b792a5cacbd306835af3209d0f505f9f3f8773205488f75a');
CREATE TABLE agent_questions (
	id INTEGER NOT NULL, 
	uid VARCHAR(36) NOT NULL, 
	task_id INTEGER NOT NULL, 
	node_run_id INTEGER, 
	role VARCHAR(64) NOT NULL, 
	message TEXT NOT NULL, 
	choices JSON NOT NULL, 
	answered BOOLEAN NOT NULL, 
	answer TEXT, 
	created_at DATETIME NOT NULL, 
	answered_at DATETIME, decision JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "agent_questions" VALUES(1,'d06f9a97-00c5-4ddb-aa74-0772c704947a',1,1,'architect','Stub question: should this apply to all tenants?','[]',1,'Yes, all tenants','2026-10-01 22:50:29.857037','2026-10-01 22:50:30.321471',NULL);
INSERT INTO "agent_questions" VALUES(2,'00000000-0000-4000-8000-000000000002',1,NULL,'architect','Which cleanup — ünïcode?','["Soft-delete", "Hard-delete"]',0,NULL,'2026-10-25 09:00:00.000000',NULL,'{"context": "312 own invoices \u00b7 \u00fcn\u00efcode", "risk": "high", "choices": [{"label": "Soft-delete", "recommended": true, "followUp": {"message": "Purge when?", "choices": [{"label": "Later", "detail": "Nightly job"}, {"label": "Never", "note": "Legal asked.", "source": "docs/privacy.md"}]}}, {"label": "Hard-delete", "warning": "Breaks history"}]}');
CREATE TABLE agent_sessions (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	role VARCHAR(64) NOT NULL, 
	harness VARCHAR(64) NOT NULL, 
	model VARCHAR(128), 
	reasoning VARCHAR(32), 
	zellij_tab_name VARCHAR(128) NOT NULL, 
	zellij_tab_id INTEGER, 
	zellij_pane_id VARCHAR(32), 
	harness_session_id VARCHAR(128), 
	status VARCHAR(32) NOT NULL, 
	screen_state VARCHAR(16), 
	screen_state_at DATETIME, 
	current_run_id INTEGER, 
	started_at DATETIME NOT NULL, 
	ended_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "agent_sessions" VALUES(1,1,'architect','stub',NULL,'high','Architect',3,'terminal_3',NULL,'stopped','unknown','2026-10-01 22:50:29.386739',1,'2026-10-01 22:50:28.920798','2026-10-01 22:50:43.649051');
INSERT INTO "agent_sessions" VALUES(2,1,'implementer','stub',NULL,'high','Implementer',4,'terminal_4',NULL,'stopped','unknown','2026-10-01 22:50:31.727957',2,'2026-10-01 22:50:31.203167','2026-10-01 22:50:35.763182');
INSERT INTO "agent_sessions" VALUES(3,1,'code_reviewer','stub',NULL,'high','Code Review',5,'terminal_5',NULL,'stopped','unknown','2026-10-01 22:50:33.095910',3,'2026-10-01 22:50:32.384203','2026-10-01 22:50:38.567977');
INSERT INTO "agent_sessions" VALUES(4,1,'security_reviewer','stub',NULL,'high','Security',6,'terminal_6',NULL,'stopped','unknown','2026-10-01 22:50:33.770327',4,'2026-10-01 22:50:33.078073','2026-10-01 22:50:40.981475');
INSERT INTO "agent_sessions" VALUES(5,1,'implementer','stub',NULL,'high','Implementer',7,'terminal_7',NULL,'running','unknown','2026-10-01 22:50:36.468275',5,'2026-10-01 22:50:35.783853',NULL);
INSERT INTO "agent_sessions" VALUES(6,1,'code_reviewer','stub',NULL,'high','Code Review',8,'terminal_8',NULL,'running','unknown','2026-10-01 22:50:39.312518',6,'2026-10-01 22:50:38.584365',NULL);
INSERT INTO "agent_sessions" VALUES(7,1,'security_reviewer','stub',NULL,'high','Security',9,'terminal_9',NULL,'running','unknown','2026-10-01 22:50:41.675916',7,'2026-10-01 22:50:40.996897',NULL);
INSERT INTO "agent_sessions" VALUES(8,1,'architect','stub',NULL,'high','Architect',10,'terminal_10',NULL,'running','unknown','2026-10-01 22:50:44.379232',8,'2026-10-01 22:50:43.665048',NULL);
CREATE TABLE chat_threads (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	platform VARCHAR(32) NOT NULL, 
	channel VARCHAR(64), 
	thread_id VARCHAR(64), 
	title TEXT, 
	notified JSON NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	UNIQUE (task_id, platform), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "chat_threads" VALUES(1,1,'slack','D123','1727.0001','Ticket — ünïcode','{"WAITING_FOR_USER:architect": "1727.0002"}','2026-09-30 12:10:00.123456');
CREATE TABLE deleted_run_spend (
	id INTEGER NOT NULL, 
	jira_key VARCHAR(64) NOT NULL, 
	run_id INTEGER NOT NULL, 
	charged_at DATETIME NOT NULL, 
	billing_mode VARCHAR(16) NOT NULL, 
	cost_usd FLOAT, 
	PRIMARY KEY (id)
);
INSERT INTO "deleted_run_spend" VALUES(1,'GONE-1',99,'2026-09-30 12:00:00.000000','metered',0.42);
INSERT INTO "deleted_run_spend" VALUES(2,'GONE-1',100,'2026-09-30 12:05:00.000000','unknown',NULL);
CREATE TABLE implementation_work_items (
	uid VARCHAR(36) NOT NULL, 
	task_id INTEGER NOT NULL, 
	node_id INTEGER NOT NULL, 
	plan_key VARCHAR(64) NOT NULL, 
	brief TEXT NOT NULL, 
	owned_paths JSON NOT NULL, 
	acceptance_criteria JSON NOT NULL, 
	dependencies JSON NOT NULL, 
	worktree_path TEXT NOT NULL, 
	branch_name TEXT NOT NULL, 
	base_sha VARCHAR(64) NOT NULL, 
	prepared BOOLEAN NOT NULL, 
	PRIMARY KEY (uid), 
	FOREIGN KEY(task_id) REFERENCES tasks (id), 
	UNIQUE (node_id), 
	FOREIGN KEY(node_id) REFERENCES workflow_nodes (id)
);
INSERT INTO "implementation_work_items" VALUES('00000000-0000-4000-8000-000000000001',1,3,'api','Add the endpoint','["src/api/"]','["200 on GET"]','[]','/w/api','agent/X-api','aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',0);
CREATE TABLE node_runs (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	node_id INTEGER NOT NULL, 
	run_number INTEGER NOT NULL, 
	iteration INTEGER NOT NULL, 
	state VARCHAR(32) NOT NULL, 
	harness VARCHAR(64), 
	model VARCHAR(128), 
	reasoning VARCHAR(32), 
	run_dir TEXT, 
	prompt_path TEXT, 
	result_path TEXT, 
	expected_sha VARCHAR(64), 
	result JSON, 
	exit_code INTEGER, 
	error TEXT, 
	started_at DATETIME NOT NULL, 
	completed_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id), 
	FOREIGN KEY(node_id) REFERENCES workflow_nodes (id)
);
INSERT INTO "node_runs" VALUES(1,1,1,1,1,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\01','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\01\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\01\result.json',NULL,'{"status": "completed", "summary": "Stub discovery complete", "complexity": "Low", "affectedComponents": ["STUB_NOTES.md"], "assumptions": [], "risks": [], "openQuestions": []}',NULL,NULL,'2026-10-01 22:50:28.904164','2026-10-01 22:50:30.960497');
INSERT INTO "node_runs" VALUES(2,1,3,1,1,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\01','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\01\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\01\result.json',NULL,'{"status": "completed", "summary": "Stub implementation: appended to STUB_NOTES.md", "filesChanged": ["STUB_NOTES.md"], "tests": {"executed": 1, "passed": 1, "failed": 0, "notes": null}, "assumptions": [], "knownIssues": [], "findingResponses": [], "checkpointSha": null}',NULL,NULL,'2026-10-01 22:50:31.191215','2026-10-01 22:50:32.324613');
INSERT INTO "node_runs" VALUES(3,1,4,1,1,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\01','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\01\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\01\result.json','fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa','{"status": "completed", "reviewedSha": "fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa", "verdict": "changes_required", "summary": "Stub review", "findings": [{"id": "CODE-001", "severity": "medium", "blocking": true, "file": "STUB_NOTES.md", "line": 1, "title": "Stub finding iteration 1", "description": "Canned finding", "recommendation": "Fix it"}]}',NULL,NULL,'2026-10-01 22:50:32.370274','2026-10-01 22:50:33.754489');
INSERT INTO "node_runs" VALUES(4,1,5,1,1,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\01','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\01\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\01\result.json','fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa','{"status": "completed", "reviewedSha": "fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa", "verdict": "changes_required", "summary": "Stub review", "findings": [{"id": "SEC-001", "severity": "medium", "blocking": true, "file": "STUB_NOTES.md", "line": 1, "title": "Stub finding iteration 1", "description": "Canned finding", "recommendation": "Fix it"}]}',NULL,NULL,'2026-10-01 22:50:33.057374','2026-10-01 22:50:34.024564');
INSERT INTO "node_runs" VALUES(5,1,3,2,2,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\02','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\02\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\02\result.json','fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa','{"status": "completed", "summary": "Stub implementation: appended to STUB_NOTES.md", "filesChanged": ["STUB_NOTES.md"], "tests": {"executed": 1, "passed": 1, "failed": 0, "notes": null}, "assumptions": [], "knownIssues": [], "findingResponses": [{"id": "CODE-001", "action": "fixed", "rationale": "Stub: appended a new line to STUB_NOTES.md"}, {"id": "SEC-001", "action": "fixed", "rationale": "Stub: appended a new line to STUB_NOTES.md"}], "checkpointSha": null}',NULL,NULL,'2026-10-01 22:50:35.765096','2026-10-01 22:50:36.911848');
INSERT INTO "node_runs" VALUES(6,1,4,2,2,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\02','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\02\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\02\result.json','9122eb45e78b1a318d781749fc8ca56906ba4975','{"status": "completed", "reviewedSha": "9122eb45e78b1a318d781749fc8ca56906ba4975", "verdict": "approved", "summary": "Stub review", "findings": [], "priorFindings": [{"id": "CODE-001", "status": "fixed", "evidence": "STUB_NOTES.md:1"}]}',NULL,NULL,'2026-10-01 22:50:38.569744','2026-10-01 22:50:41.657218');
INSERT INTO "node_runs" VALUES(7,1,5,2,2,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\02','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\02\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\02\result.json','9122eb45e78b1a318d781749fc8ca56906ba4975','{"status": "completed", "reviewedSha": "9122eb45e78b1a318d781749fc8ca56906ba4975", "verdict": "approved", "summary": "Stub review", "findings": [], "priorFindings": [{"id": "SEC-001", "status": "fixed", "evidence": "STUB_NOTES.md:1"}]}',NULL,NULL,'2026-10-01 22:50:40.983072','2026-10-01 22:50:41.942558');
INSERT INTO "node_runs" VALUES(8,1,7,1,2,'COMPLETED','stub',NULL,'high','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\02','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\02\prompt.md','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\02\result.json','9122eb45e78b1a318d781749fc8ca56906ba4975','{"status": "completed", "taskSummary": "Stub task", "complexity": "Low", "complexityScore": 2, "originalScope": null, "implementationPerformed": null, "filesAffected": [], "keyFindings": [], "reviewFindings": null, "fixesMade": null, "testsPerformed": "One stub assertion passed; no live application tests.", "knownLimitations": [], "outstandingUncertainty": [], "regressionRisk": "LOW", "securityRisk": "LOW", "overallRisk": "LOW", "riskFactors": [], "humanReviewRecommendation": "LOW: normal PR review is sufficient", "requirementsVerified": true, "finalSha": "9122eb45e78b1a318d781749fc8ca56906ba4975", "reviewIterations": 2}',NULL,NULL,'2026-10-01 22:50:43.651025','2026-10-01 22:50:44.577015');
CREATE TABLE process_leases (
	id VARCHAR(36) NOT NULL, 
	session_id INTEGER NOT NULL, 
	launch_run_id INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	UNIQUE (session_id), 
	FOREIGN KEY(session_id) REFERENCES agent_sessions (id), 
	UNIQUE (launch_run_id), 
	FOREIGN KEY(launch_run_id) REFERENCES node_runs (id)
);
INSERT INTO "process_leases" VALUES('9836a131-2071-4c92-a686-29db83b37842',1,1);
INSERT INTO "process_leases" VALUES('5d6787ce-50f5-41bd-af22-99fc79548796',2,2);
INSERT INTO "process_leases" VALUES('81452e13-8388-4a91-aaf0-138a360adc91',3,3);
INSERT INTO "process_leases" VALUES('a942d360-1c54-4082-b68f-94bb2eb58788',4,4);
INSERT INTO "process_leases" VALUES('65b16718-ee46-4bbd-be9a-bdaf5efbde9f',5,5);
INSERT INTO "process_leases" VALUES('8da4737c-7884-42f5-ba1f-33b962290d8c',6,6);
INSERT INTO "process_leases" VALUES('c2e5c78b-fd6c-429f-9bee-6791b990aee0',7,7);
INSERT INTO "process_leases" VALUES('117a48d5-b240-45b5-b579-e8567135aa8f',8,8);
CREATE TABLE projects (
	id INTEGER NOT NULL, 
	slug VARCHAR(64) NOT NULL, 
	name VARCHAR(80) NOT NULL, 
	color VARCHAR(16) NOT NULL, 
	description TEXT, 
	default_repository_path TEXT, 
	default_project_relative_path TEXT, 
	default_base_branch VARCHAR(255), 
	agent_overrides JSON NOT NULL, 
	archived_at DATETIME, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	UNIQUE (name)
);
INSERT INTO "projects" VALUES(1,'billing-app','Billing App — ünïcode','teal','Invoice pages','/r','src','main','{"implementer": {"harness": "codex"}}',NULL,'2026-10-22 09:00:00.000000','2026-10-22 09:00:00.654321');
CREATE TABLE requirements_approvals (
	uid VARCHAR(36) NOT NULL, 
	task_id INTEGER NOT NULL, 
	digest VARCHAR(64) NOT NULL, 
	documents JSON NOT NULL, 
	approved_at DATETIME NOT NULL, 
	PRIMARY KEY (uid), 
	UNIQUE (task_id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "requirements_approvals" VALUES('9bff7d54-6672-46f3-919c-6183158abef5',1,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08','{"requirements": "# Requirements\n\n- Add STUB_NOTES.md describing the change.\n", "acceptanceCriteria": "# Acceptance criteria\n\n- STUB_NOTES.md exists.\n", "assumptions": "# Assumptions\n\n- None.\n", "implementationPlan": "# Plan\n\n1. Create STUB_NOTES.md.\n", "risks": "# Risks\n\n- None.\n"}','2026-10-01 22:50:31.144207');
CREATE TABLE review_findings (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	node_run_id INTEGER, 
	finding_id VARCHAR(32) NOT NULL, 
	type VARCHAR(32) NOT NULL, 
	iteration INTEGER NOT NULL, 
	reviewed_sha VARCHAR(64), 
	severity VARCHAR(16) NOT NULL, 
	blocking BOOLEAN NOT NULL, 
	file TEXT, 
	line INTEGER, 
	title TEXT NOT NULL, 
	description TEXT, 
	recommendation TEXT, 
	status VARCHAR(16) NOT NULL, 
	prior_finding_id INTEGER, 
	occurrence INTEGER NOT NULL, 
	resolution TEXT, 
	response VARCHAR(16), 
	response_note TEXT, 
	created_at DATETIME NOT NULL, 
	resolved_at DATETIME, end_line INTEGER, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "review_findings" VALUES(1,1,3,'CODE-001','code-review',1,'fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa','medium',1,'STUB_NOTES.md',1,'Stub finding iteration 1','Canned finding','Fix it','resolved',NULL,1,'STUB_NOTES.md:1','fixed','Stub: appended a new line to STUB_NOTES.md','2026-10-01 22:50:33.756382','2026-10-01 22:50:41.656508',NULL);
INSERT INTO "review_findings" VALUES(2,1,4,'SEC-001','security-review',1,'fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa','medium',1,'STUB_NOTES.md',1,'Stub finding iteration 1','Canned finding','Fix it','resolved',NULL,1,'STUB_NOTES.md:1','fixed','Stub: appended a new line to STUB_NOTES.md','2026-10-01 22:50:34.026028','2026-10-01 22:50:41.941844',NULL);
CREATE TABLE run_evidence (
	uid VARCHAR(36) NOT NULL, 
	run_id INTEGER NOT NULL, 
	requirements_digest VARCHAR(64), 
	verification JSON, 
	usage JSON, 
	confirmed_model VARCHAR(128), 
	billing_mode VARCHAR(16) DEFAULT 'unknown' NOT NULL, 
	PRIMARY KEY (uid), 
	UNIQUE (run_id), 
	FOREIGN KEY(run_id) REFERENCES node_runs (id)
);
INSERT INTO "run_evidence" VALUES('9741ccb7-b271-4c54-b012-1e1d87626aaa',1,NULL,NULL,NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('1282ff94-18f9-4ef2-b66b-c9a78da62658',2,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08','{"sha": "fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa", "commands": [{"command": ["D:\\Dev\\AgentFlow\\backend\\.venv\\Scripts\\python.exe", "-c", "import subprocess, pathlib; root = subprocess.check_output([''git'', ''rev-parse'', ''--show-toplevel''], text=True).strip(); assert pathlib.Path(root, ''STUB_NOTES.md'').exists(), ''STUB_NOTES.md missing''"], "exitCode": 0, "output": ""}], "passed": true}',NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('e3b34327-21c0-4b80-a2e6-826802535b48',3,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08',NULL,NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('7fd05d91-3df0-41a6-9109-09958e80f5e1',4,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08',NULL,NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('9eb63bb4-5941-4eb5-8d5d-1e79cdd22a65',5,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08','{"sha": "9122eb45e78b1a318d781749fc8ca56906ba4975", "commands": [{"command": ["D:\\Dev\\AgentFlow\\backend\\.venv\\Scripts\\python.exe", "-c", "import subprocess, pathlib; root = subprocess.check_output([''git'', ''rev-parse'', ''--show-toplevel''], text=True).strip(); assert pathlib.Path(root, ''STUB_NOTES.md'').exists(), ''STUB_NOTES.md missing''"], "exitCode": 0, "output": ""}], "passed": true}',NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('a1086dfe-763f-4018-b7e3-21face77a360',6,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08',NULL,NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('614cc71c-3edd-47ae-ac22-e9c78d6f2f63',7,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08',NULL,NULL,NULL,'unknown');
INSERT INTO "run_evidence" VALUES('0e5f4a0c-dbe3-41c4-a1fc-402e902326ed',8,'a37ef96d5a34327242b00d7dc0432737b6c0100b6aaef03f28b2c52bf8ee6b08',NULL,NULL,NULL,'unknown');
CREATE TABLE run_heartbeats (
	uid VARCHAR(36) NOT NULL, 
	run_id INTEGER NOT NULL, 
	received_at DATETIME NOT NULL, 
	phase VARCHAR(32) NOT NULL, 
	PRIMARY KEY (uid), 
	UNIQUE (run_id), 
	FOREIGN KEY(run_id) REFERENCES node_runs (id) ON DELETE CASCADE
);
CREATE TABLE task_artifacts (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	node_run_id INTEGER, 
	kind VARCHAR(64) NOT NULL, 
	path TEXT NOT NULL, 
	created_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "task_artifacts" VALUES(1,1,1,'architect-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\01\prompt.md','2026-10-01 22:50:28.910270');
INSERT INTO "task_artifacts" VALUES(2,1,1,'architect-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\01\result.json','2026-10-01 22:50:30.961031');
INSERT INTO "task_artifacts" VALUES(3,1,2,'implementer-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\01\prompt.md','2026-10-01 22:50:31.195593');
INSERT INTO "task_artifacts" VALUES(4,1,2,'implementer-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\01\result.json','2026-10-01 22:50:32.115935');
INSERT INTO "task_artifacts" VALUES(5,1,3,'code_review-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\01\prompt.md','2026-10-01 22:50:32.376688');
INSERT INTO "task_artifacts" VALUES(6,1,4,'security_review-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\01\prompt.md','2026-10-01 22:50:33.066901');
INSERT INTO "task_artifacts" VALUES(7,1,3,'code_review-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\01\result.json','2026-10-01 22:50:33.750535');
INSERT INTO "task_artifacts" VALUES(8,1,3,'code-review','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\review-01.json','2026-10-01 22:50:33.756881');
INSERT INTO "task_artifacts" VALUES(9,1,4,'security_review-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\01\result.json','2026-10-01 22:50:34.021620');
INSERT INTO "task_artifacts" VALUES(10,1,4,'security-review','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\security-01.json','2026-10-01 22:50:34.026295');
INSERT INTO "task_artifacts" VALUES(11,1,NULL,'consolidated','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\consolidated-01.json','2026-10-01 22:50:34.036644');
INSERT INTO "task_artifacts" VALUES(12,1,5,'implementer-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\02\prompt.md','2026-10-01 22:50:35.775823');
INSERT INTO "task_artifacts" VALUES(13,1,5,'implementer-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\implementer\02\result.json','2026-10-01 22:50:36.707045');
INSERT INTO "task_artifacts" VALUES(14,1,6,'code_review-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\02\prompt.md','2026-10-01 22:50:38.576467');
INSERT INTO "task_artifacts" VALUES(15,1,7,'security_review-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\02\prompt.md','2026-10-01 22:50:40.988945');
INSERT INTO "task_artifacts" VALUES(16,1,6,'code_review-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\code-review\02\result.json','2026-10-01 22:50:41.655258');
INSERT INTO "task_artifacts" VALUES(17,1,6,'code-review','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\review-02.json','2026-10-01 22:50:41.658198');
INSERT INTO "task_artifacts" VALUES(18,1,7,'security_review-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\security-review\02\result.json','2026-10-01 22:50:41.940626');
INSERT INTO "task_artifacts" VALUES(19,1,7,'security-review','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\security-02.json','2026-10-01 22:50:41.943385');
INSERT INTO "task_artifacts" VALUES(20,1,NULL,'consolidated','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\findings\consolidated-02.json','2026-10-01 22:50:41.945610');
INSERT INTO "task_artifacts" VALUES(21,1,8,'final_architect-prompt','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\02\prompt.md','2026-10-01 22:50:43.657727');
INSERT INTO "task_artifacts" VALUES(22,1,8,'final_architect-result','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\runs\architect\02\result.json','2026-10-01 22:50:44.578153');
INSERT INTO "task_artifacts" VALUES(23,1,8,'final-assessment','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B\final\assessment.json','2026-10-01 22:50:44.578155');
CREATE TABLE tasks (
	id INTEGER NOT NULL, 
	jira_key VARCHAR(64) NOT NULL, 
	api_budget_usd FLOAT, 
	jira_title TEXT, 
	repository_path TEXT NOT NULL, 
	repository_root TEXT NOT NULL, 
	project_relative_path TEXT, 
	base_branch VARCHAR(255) NOT NULL, 
	branch_name VARCHAR(255) NOT NULL, 
	worktree_path TEXT NOT NULL, 
	zellij_session_name VARCHAR(255) NOT NULL, 
	task_dir TEXT NOT NULL, 
	additional_context TEXT, 
	status VARCHAR(32) NOT NULL, 
	status_message TEXT, 
	review_iteration INTEGER NOT NULL, 
	max_review_iterations INTEGER NOT NULL, 
	max_requirements_revisions INTEGER NOT NULL, 
	current_sha VARCHAR(64), 
	pr_url TEXT, 
	pr_created_at DATETIME, 
	merged_at DATETIME, 
	pr_sync_paused BOOLEAN DEFAULT '0' NOT NULL, 
	merged_head_sha VARCHAR(64), 
	archived_at DATETIME, 
	agent_overrides JSON NOT NULL, 
	created_at DATETIME NOT NULL, 
	updated_at DATETIME NOT NULL, pr_base_branch VARCHAR(255), project_id INTEGER REFERENCES projects (id), 
	PRIMARY KEY (id)
);
INSERT INTO "tasks" VALUES(1,'FLOW-92B91B',NULL,NULL,'C:\Users\Rob Rodolfo\AppData\Local\Temp\pytest-of-Rob Rodolfo\pytest-856\repo0\repo','C:\Users\Rob Rodolfo\AppData\Local\Temp\pytest-of-Rob Rodolfo\pytest-856\repo0\repo','src','main','agent/FLOW-92B91B','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B','agentflow-FLOW-92B91B','C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\home\tasks\FLOW-92B91B','Contract test context','READY_FOR_PR','Ready for PR. Create the pull request manually from branch agent/FLOW-92B91B',2,3,3,'9122eb45e78b1a318d781749fc8ca56906ba4975',NULL,NULL,NULL,0,NULL,NULL,'{}','2026-10-01 22:50:27.462154','2026-10-01 22:50:44.799001',NULL,1);
CREATE TABLE workflow_events (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	timestamp DATETIME NOT NULL, 
	level VARCHAR(16) NOT NULL, 
	node_key VARCHAR(64), 
	message TEXT NOT NULL, 
	data JSON, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "workflow_events" VALUES(1,1,'2026-10-01 22:50:28.832096','info',NULL,'Task created','{"repository": "C:\\Users\\Rob Rodolfo\\AppData\\Local\\Temp\\pytest-of-Rob Rodolfo\\pytest-856\\repo0\\repo", "baseBranch": "main"}');
INSERT INTO "workflow_events" VALUES(2,1,'2026-10-01 22:50:28.832098','info',NULL,'Worktree created: C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B on branch agent/FLOW-92B91B','null');
INSERT INTO "workflow_events" VALUES(3,1,'2026-10-01 22:50:28.832099','info',NULL,'Zellij session created: agentflow-FLOW-92B91B','null');
INSERT INTO "workflow_events" VALUES(4,1,'2026-10-01 22:50:28.922094','info','architect','Architect → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(5,1,'2026-10-01 22:50:29.337788','info','architect','Architect started: stub in zellij tab ''Architect'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(6,1,'2026-10-01 22:50:29.337790','info','architect','Architect → RUNNING: Run #1 (stub)','null');
INSERT INTO "workflow_events" VALUES(7,1,'2026-10-01 22:50:29.857345','info','architect','Architect → WAITING_FOR_USER: Input required','null');
INSERT INTO "workflow_events" VALUES(8,1,'2026-10-01 22:50:29.857347','warning','architect','Architect asked: Stub question: should this apply to all tenants?','null');
INSERT INTO "workflow_events" VALUES(9,1,'2026-10-01 22:50:30.324180','info','architect','Architect → RUNNING: All replies sent; agent resumed','null');
INSERT INTO "workflow_events" VALUES(10,1,'2026-10-01 22:50:30.324182','info','architect','Human replied to question d06f9a97-00c5-4ddb-aa74-0772c704947a','null');
INSERT INTO "workflow_events" VALUES(11,1,'2026-10-01 22:50:30.961228','info','architect','Architect → COMPLETED: Stub discovery complete','null');
INSERT INTO "workflow_events" VALUES(12,1,'2026-10-01 22:50:30.961230','info','requirements_gate','Requirements Approval → WAITING_FOR_APPROVAL: Review requirements and approve','null');
INSERT INTO "workflow_events" VALUES(13,1,'2026-10-01 22:50:31.145127','info','requirements_gate','Requirements Approval → COMPLETED: Requirements approved','null');
INSERT INTO "workflow_events" VALUES(14,1,'2026-10-01 22:50:31.145129','info',NULL,'Requirements approved','null');
INSERT INTO "workflow_events" VALUES(15,1,'2026-10-01 22:50:31.145130','info','implementer','Implementer → READY','null');
INSERT INTO "workflow_events" VALUES(16,1,'2026-10-01 22:50:31.204256','info','implementer','Implementer → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(17,1,'2026-10-01 22:50:31.682837','info','implementer','Implementer started: stub in zellij tab ''Implementer'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(18,1,'2026-10-01 22:50:31.682840','info','implementer','Implementer → RUNNING: Run #1 (stub)','null');
INSERT INTO "workflow_events" VALUES(19,1,'2026-10-01 22:50:32.326016','info','implementer','Implementer → COMPLETED: Stub implementation: appended to STUB_NOTES.md','null');
INSERT INTO "workflow_events" VALUES(20,1,'2026-10-01 22:50:32.326018','info','implementer','Checkpoint SHA fe1a3dbcc2 (committed)','{"sha": "fe1a3dbcc2e3ef0c998fb88d0476127bf8bdfaaa"}');
INSERT INTO "workflow_events" VALUES(21,1,'2026-10-01 22:50:32.326019','info','code_review','Code Review → READY','null');
INSERT INTO "workflow_events" VALUES(22,1,'2026-10-01 22:50:32.326020','info','security_review','Security Review → READY','null');
INSERT INTO "workflow_events" VALUES(23,1,'2026-10-01 22:50:32.384805','info','code_review','Code Review → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(24,1,'2026-10-01 22:50:33.007036','info','code_review','Code Review started: stub in zellij tab ''Code Review'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(25,1,'2026-10-01 22:50:33.007038','info','code_review','Code Review → RUNNING: Run #1 (stub)','null');
INSERT INTO "workflow_events" VALUES(26,1,'2026-10-01 22:50:33.078699','info','security_review','Security Review → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(27,1,'2026-10-01 22:50:33.715896','info','security_review','Security Review started: stub in zellij tab ''Security'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(28,1,'2026-10-01 22:50:33.715899','info','security_review','Security Review → RUNNING: Run #1 (stub)','null');
INSERT INTO "workflow_events" VALUES(29,1,'2026-10-01 22:50:33.757065','info','code_review','Code Review → COMPLETED: 1 finding(s), 1 blocking — changes_required','null');
INSERT INTO "workflow_events" VALUES(30,1,'2026-10-01 22:50:34.026634','info','security_review','Security Review → COMPLETED: 1 finding(s), 1 blocking — changes_required','null');
INSERT INTO "workflow_events" VALUES(31,1,'2026-10-01 22:50:34.026637','info','consolidate','Consolidate Findings → READY','null');
INSERT INTO "workflow_events" VALUES(32,1,'2026-10-01 22:50:34.026638','info','consolidate','Consolidate Findings → RUNNING','null');
INSERT INTO "workflow_events" VALUES(33,1,'2026-10-01 22:50:34.037027','info','consolidate','Consolidate Findings → COMPLETED: Iteration 1: 2 blocking finding(s) → Implementer fixes','null');
INSERT INTO "workflow_events" VALUES(34,1,'2026-10-01 22:50:34.037030','info','implementer','Implementer → READY: Fix 2 finding(s) from iteration 1','null');
INSERT INTO "workflow_events" VALUES(35,1,'2026-10-01 22:50:35.784508','info','implementer','Implementer → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(36,1,'2026-10-01 22:50:36.418100','info','implementer','Implementer started: stub in zellij tab ''Implementer'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(37,1,'2026-10-01 22:50:36.418103','info','implementer','Implementer → RUNNING: Run #2 (stub)','null');
INSERT INTO "workflow_events" VALUES(38,1,'2026-10-01 22:50:36.915862','info','implementer','Implementer → COMPLETED: Stub implementation: appended to STUB_NOTES.md','null');
INSERT INTO "workflow_events" VALUES(39,1,'2026-10-01 22:50:36.915864','info','implementer','Checkpoint SHA 9122eb45e7 (committed)','{"sha": "9122eb45e78b1a318d781749fc8ca56906ba4975"}');
INSERT INTO "workflow_events" VALUES(40,1,'2026-10-01 22:50:36.915865','info','code_review','Code Review → READY','null');
INSERT INTO "workflow_events" VALUES(41,1,'2026-10-01 22:50:36.915865','info','security_review','Security Review → READY','null');
INSERT INTO "workflow_events" VALUES(42,1,'2026-10-01 22:50:36.915866','info','consolidate','Consolidate Findings → PENDING','null');
INSERT INTO "workflow_events" VALUES(43,1,'2026-10-01 22:50:38.584549','info','code_review','Starting Code Review in a fresh conversation for review round 2','null');
INSERT INTO "workflow_events" VALUES(44,1,'2026-10-01 22:50:38.585270','info','code_review','Code Review → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(45,1,'2026-10-01 22:50:39.257346','info','code_review','Code Review started: stub in zellij tab ''Code Review'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(46,1,'2026-10-01 22:50:39.257351','info','code_review','Code Review → RUNNING: Run #2 (stub)','null');
INSERT INTO "workflow_events" VALUES(47,1,'2026-10-01 22:50:40.997045','info','security_review','Starting Security in a fresh conversation for review round 2','null');
INSERT INTO "workflow_events" VALUES(48,1,'2026-10-01 22:50:40.997683','info','security_review','Security Review → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(49,1,'2026-10-01 22:50:41.619499','info','security_review','Security Review started: stub in zellij tab ''Security'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(50,1,'2026-10-01 22:50:41.619502','info','security_review','Security Review → RUNNING: Run #2 (stub)','null');
INSERT INTO "workflow_events" VALUES(51,1,'2026-10-01 22:50:41.658335','info','code_review','Code Review → COMPLETED: 0 finding(s), 0 blocking — approved','null');
INSERT INTO "workflow_events" VALUES(52,1,'2026-10-01 22:50:41.943569','info','security_review','Security Review → COMPLETED: 0 finding(s), 0 blocking — approved','null');
INSERT INTO "workflow_events" VALUES(53,1,'2026-10-01 22:50:41.943570','info','consolidate','Consolidate Findings → READY','null');
INSERT INTO "workflow_events" VALUES(54,1,'2026-10-01 22:50:41.943571','info','consolidate','Consolidate Findings → RUNNING','null');
INSERT INTO "workflow_events" VALUES(55,1,'2026-10-01 22:50:41.945779','info','consolidate','Consolidate Findings → COMPLETED: Iteration 2: no blocking findings — reviews passed','null');
INSERT INTO "workflow_events" VALUES(56,1,'2026-10-01 22:50:41.945780','info',NULL,'Code Review passed and Security Review passed','null');
INSERT INTO "workflow_events" VALUES(57,1,'2026-10-01 22:50:41.945781','info','final_architect','Final Architect Verification → READY','null');
INSERT INTO "workflow_events" VALUES(58,1,'2026-10-01 22:50:43.665625','info','final_architect','Final Architect Verification → RUNNING: Launching agent','null');
INSERT INTO "workflow_events" VALUES(59,1,'2026-10-01 22:50:44.328872','info','final_architect','Final Architect Verification started: stub in zellij tab ''Architect'' (cwd C:\Users\Rob Rodolfo\AppData\Local\Temp\agentflow-contract-python-gc47jxyk\worktrees\FLOW-92B91B\src)','{"harness": "stub", "requestedModel": null}');
INSERT INTO "workflow_events" VALUES(60,1,'2026-10-01 22:50:44.328875','info','final_architect','Final Architect Verification → RUNNING: Run #1 (stub)','null');
INSERT INTO "workflow_events" VALUES(61,1,'2026-10-01 22:50:44.578525','info','final_architect','Final Architect Verification → COMPLETED: Assessment received. Risk LOW','null');
INSERT INTO "workflow_events" VALUES(62,1,'2026-10-01 22:50:44.578527','info',NULL,'Final assessment received. Risk: LOW','null');
INSERT INTO "workflow_events" VALUES(63,1,'2026-10-01 22:50:44.581777','info','ready_for_pr','Ready for PR → WAITING_FOR_APPROVAL: Confirm the assessment to mark Ready for PR','null');
INSERT INTO "workflow_events" VALUES(64,1,'2026-10-01 22:50:44.799254','info','ready_for_pr','Ready for PR → COMPLETED: Ready for PR','null');
INSERT INTO "workflow_events" VALUES(65,1,'2026-10-01 22:50:44.799255','info',NULL,'Task is READY_FOR_PR','null');
CREATE TABLE workflow_nodes (
	id INTEGER NOT NULL, 
	task_id INTEGER NOT NULL, 
	"key" VARCHAR(64) NOT NULL, 
	kind VARCHAR(32) NOT NULL, 
	label VARCHAR(128) NOT NULL, 
	role VARCHAR(64), 
	state VARCHAR(32) NOT NULL, 
	execution_mode VARCHAR(32) NOT NULL, 
	iteration INTEGER NOT NULL, 
	checkpoint_sha VARCHAR(64), 
	message TEXT, 
	error TEXT, 
	config_override JSON NOT NULL, 
	updated_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(task_id) REFERENCES tasks (id)
);
INSERT INTO "workflow_nodes" VALUES(1,1,'architect','agent','Architect','architect','COMPLETED','manual',0,NULL,'Stub discovery complete',NULL,'{}','2026-10-01 22:50:30.961685');
INSERT INTO "workflow_nodes" VALUES(2,1,'requirements_gate','approval','Requirements Approval',NULL,'COMPLETED','approval_required',0,NULL,'Requirements approved',NULL,'{}','2026-10-01 22:50:31.145385');
INSERT INTO "workflow_nodes" VALUES(3,1,'implementer','agent','Implementer','implementer','COMPLETED','automatic',2,'9122eb45e78b1a318d781749fc8ca56906ba4975','Stub implementation: appended to STUB_NOTES.md',NULL,'{}','2026-10-01 22:50:36.916127');
INSERT INTO "workflow_nodes" VALUES(4,1,'code_review','agent','Code Review','code_reviewer','COMPLETED','automatic',2,'9122eb45e78b1a318d781749fc8ca56906ba4975','0 finding(s), 0 blocking — approved',NULL,'{}','2026-10-01 22:50:41.658461');
INSERT INTO "workflow_nodes" VALUES(5,1,'security_review','agent','Security Review','security_reviewer','COMPLETED','automatic',2,'9122eb45e78b1a318d781749fc8ca56906ba4975','0 finding(s), 0 blocking — approved',NULL,'{}','2026-10-01 22:50:41.943810');
INSERT INTO "workflow_nodes" VALUES(6,1,'consolidate','system','Consolidate Findings',NULL,'COMPLETED','automatic',2,NULL,'Iteration 2: no blocking findings — reviews passed',NULL,'{}','2026-10-01 22:50:41.945984');
INSERT INTO "workflow_nodes" VALUES(7,1,'final_architect','agent','Final Architect Verification','architect','COMPLETED','automatic',2,'9122eb45e78b1a318d781749fc8ca56906ba4975','Assessment received. Risk LOW',NULL,'{}','2026-10-01 22:50:44.578726');
INSERT INTO "workflow_nodes" VALUES(8,1,'ready_for_pr','terminal','Ready for PR',NULL,'COMPLETED','approval_required',0,NULL,'Ready for PR',NULL,'{}','2026-10-01 22:50:44.799446');
CREATE INDEX ix_deleted_run_spend_charged_at ON deleted_run_spend (charged_at);
CREATE UNIQUE INDEX ix_tasks_jira_key ON tasks (jira_key);
CREATE INDEX ix_workflow_nodes_task_id ON workflow_nodes (task_id);
CREATE INDEX ix_agent_sessions_task_id ON agent_sessions (task_id);
CREATE INDEX ix_review_findings_task_id ON review_findings (task_id);
CREATE INDEX ix_workflow_events_task_id ON workflow_events (task_id);
CREATE INDEX ix_task_artifacts_task_id ON task_artifacts (task_id);
CREATE INDEX ix_agent_questions_task_id ON agent_questions (task_id);
CREATE UNIQUE INDEX ix_agent_questions_uid ON agent_questions (uid);
CREATE INDEX ix_chat_threads_task_id ON chat_threads (task_id);
CREATE INDEX ix_node_runs_task_id ON node_runs (task_id);
CREATE INDEX ix_node_runs_node_id ON node_runs (node_id);
CREATE INDEX ix_implementation_work_items_task_id ON implementation_work_items (task_id);
CREATE INDEX ix_agent_access_task_id ON agent_access (task_id);
CREATE UNIQUE INDEX ix_projects_slug ON projects (slug);
CREATE INDEX ix_tasks_project_id ON tasks (project_id);
COMMIT;
