//! zen-tools — OWNER: worker B. Built-in tools (SPEC §4) + permission rules/modes (SPEC §5).
//!
//! This crate provides the eight built-in tools and the permission engine that enforces
//! the allow/ask/deny rules and five permission modes. It is a pure library with no
//! LLM dependencies — summarization, prompting, and the agent loop live in zen-agent.
//!
//! Architecture:
//! - `builtin.rs` implements the eight SPEC §4 tools (Read, Write, Edit, Bash, Glob,
//!   Grep, WebFetch, TodoWrite) plus shared helpers like path resolution, HTML→text
//!   conversion, and output truncation. Each tool is a standalone struct implementing
//!   the `zen_core::Tool` trait.
//! - `permissions.rs` implements the SPEC §5 rule language and evaluation order:
//!   deny > ask > allow > mode default. The `RulePermissions` struct holds the
//!   compiled rules and implements `PermissionChecker` for the agent loop.
//! - `gitignore.rs` provides a minimal `.gitignore` engine used by Glob and Grep.
//!
//! PUBLIC API CONTRACT (zen-cli codes against these exact signatures; keep them, add freely):
//!   builtin_tools() -> Vec<Arc<dyn Tool>>   (Read, Write, Edit, MultiEdit, Bash, Glob, Grep, WebFetch, TodoWrite)
//!   RulePermissions::new(...) + impl PermissionChecker
//!   rule_matches(rule, tool_name, input, cwd) -> bool
//!
//! Extra helpers beyond the contract: `html_to_text`, `truncate_middle`, `resolve_path`,
//! `render_todos`, `persist_allow_rule`, `search_builtin`, `tool_is_read_only`,
//! `suggested_rule`, `expand_tilde`, `canonicalize_lenient`.
//! Everything is clean-room: descriptions and prompts are written from scratch.

pub mod builtin;
pub mod edit_match;
pub mod gitignore;
pub mod jobs;
pub mod permissions;
pub mod syntax_guard;

#[cfg(test)]
mod tests_p0;
#[cfg(test)]
mod tests_p0b;

pub use builtin::{
    builtin_tools, canonicalize_lenient, expand_tilde, html_to_text, render_todos, resolve_path,
    search_builtin, spill_output, truncate_middle, BashTool, EditTool, GlobTool, GrepTool, MultiEditTool, ReadTool,
    TodoWriteTool, WebFetchTool, WriteTool,
};
pub use jobs::{JobKillTool, JobOutputTool};

/// The background-job tools, registered next to Bash (p0b).
pub fn job_tools() -> Vec<std::sync::Arc<dyn zen_core::Tool>> {
    vec![std::sync::Arc::new(JobOutputTool), std::sync::Arc::new(JobKillTool)]
}

pub use permissions::{
    dedup, persist_allow_rule, rule_matches, rule_removes_tool, split_compound, suggested_rule,
    suggested_rules, tool_is_read_only, RulePermissions,
};