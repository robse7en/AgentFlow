//! AgentFlow backend, ported from `backend/agentflow` module by module.
//!
//! Every module must keep the HTTP contract that `contract/` checks against the Python backend;
//! modules the HTTP suite cannot reach yet are checked against Python-recorded goldens
//! (`contract/record_python_goldens.py`) by this crate's tests.

pub mod api;
pub mod chat;
pub mod collectors;
pub mod config;
pub mod db;
pub mod diagnostics;
pub mod engine;
pub mod events;
pub mod git;
pub mod github;
pub mod harness;
pub mod jira;
pub mod media;
pub mod paths;
pub mod process;
pub mod pyfile;
pub mod terminal;
pub mod vcs;
pub mod workspace;

/// Version reported by `/api/health`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod instructions;
