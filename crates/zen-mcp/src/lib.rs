






mod agents;
mod config;
mod lsp;
mod lsp_tools;
mod manager;
mod task;
mod transport;

pub use agents::{load_agents, load_cli_agents};
pub use config::{
    load_mcp_configs, load_mcp_configs_ex, load_mcp_configs_scoped, mcp_add, mcp_add_json, mcp_get,
    mcp_list, mcp_remove, parse_env_kv, parse_headers, scope_path, McpServerConfig,
};
pub use lsp::{LspLanguage, LspManager};
pub use lsp_tools::{FindReferencesTool, GotoDefinitionTool, HoverTool};
pub use manager::{McpManager, SkipReason};
pub use task::TaskTool;

#[cfg(test)]
mod lsp_tests;
#[cfg(test)]
mod tests;
