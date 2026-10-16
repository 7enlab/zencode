//! zen-mcp — OWNER: worker E. MCP client (SPEC §8) + sub-agent definitions and the Task tool (SPEC §9).
//! PUBLIC API CONTRACT (zen-cli codes against these exact signatures; keep them, add freely):
//!   McpServerConfig, load_mcp_configs, McpManager::{start, tools, status, shutdown},
//!   load_agents, TaskTool::new (impl Tool)

mod agents;
mod config;
mod manager;
mod task;
mod transport;

pub use agents::{load_agents, load_cli_agents};
pub use config::{
    load_mcp_configs, load_mcp_configs_ex, mcp_add, mcp_add_json, mcp_get, mcp_list, mcp_remove,
    parse_env_kv, parse_headers, scope_path, McpServerConfig,
};
pub use manager::{McpManager, SkipReason};
pub use task::TaskTool;

#[cfg(test)]
mod tests;
