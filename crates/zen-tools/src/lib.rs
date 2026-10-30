pub mod atomic;
pub mod builtin;
pub mod checkpoint;
pub mod edit_match;
pub mod gitignore;
pub mod jobs;
pub mod permissions;
pub mod syntax_guard;
pub mod tool_docs;

#[cfg(test)]
mod tests_atomic;
#[cfg(test)]
mod tests_checkpoint;
#[cfg(test)]
mod tests_p0;
#[cfg(test)]
mod tests_p0b;

pub use builtin::{
    builtin_tools, canonicalize_lenient, expand_tilde, html_to_text, render_todos, resolve_path,
    search_builtin, spill_output, truncate_middle, BashTool, EditTool, GlobTool, GrepTool,
    MultiEditTool, ReadTool, TodoWriteTool, WebFetchTool, WriteTool,
};
pub use jobs::{JobKillTool, JobOutputTool};
pub use tool_docs::tool_long_docs;

pub fn job_tools() -> Vec<std::sync::Arc<dyn zen_core::Tool>> {
    vec![
        std::sync::Arc::new(JobOutputTool),
        std::sync::Arc::new(JobKillTool),
    ]
}

pub use permissions::{
    dedup, persist_allow_rule, rule_matches, rule_removes_tool, split_compound, suggested_rule,
    suggested_rules, tool_is_read_only, RulePermissions,
};
