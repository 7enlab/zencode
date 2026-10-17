//! McpManager: owns live server connections and exposes Tools (SPEC §8).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use zen_core::{Tool, ToolContext, ToolOutput};

use crate::config::McpServerConfig;
use crate::transport::{handshake, RemoteTool, Transport};

/// MCP result cap: same as Bash. Longer text is spilled to a file and the model gets a preview.
pub const MCP_MAX_CHARS: usize = zen_tools::builtin::BASH_MAX_CHARS;

/// One Tool per remote tool, named `mcp__<server>__<tool>`; call → tools/call.
pub(crate) struct McpTool {
    #[allow(dead_code)]
    pub server: String,
    pub remote_name: String,
    pub full_name: String,
    pub description: String,
    pub schema: Value,
    pub transport: Arc<Transport>,
}

pub(crate) fn sanitize_component(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

pub(crate) fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize_component(server), sanitize_component(tool))
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let params = json!({"name": self.remote_name, "arguments": args});
        match self.transport.request("tools/call", params).await {
            Ok(result) => render_result(result, ctx),
            Err(e) => ToolOutput::err(format!("MCP call '{}' failed: {}", self.remote_name, e)),
        }
    }
}

/// Join text content parts; respect isError; render other content types as "[<type> content]".
/// Text longer than `MCP_MAX_CHARS` is spilled to the session scratch dir (`spill_output`).
pub(crate) fn render_result(result: Value, ctx: &ToolContext) -> ToolOutput {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = result.get("content").and_then(|v| v.as_array()) {
        for item in arr {
            let ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
            match ty {
                "text" => {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        parts.push(t.to_string());
                    }
                }
                other => {
                    parts.push(format!("[{} content]", other));
                }
            }
        }
    }
    if parts.is_empty() {
        if let Some(v) = result.get("structuredContent") {
            parts.push(v.to_string());
        }
    }
    let content = zen_tools::spill_output(&parts.join("\n"), MCP_MAX_CHARS, ctx);
    if is_error {
        ToolOutput::err(content)
    } else {
        ToolOutput::ok(content)
    }
}

/// Skip reason for MCP server configuration validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipReason {
    pub server_name: String,
    pub reason: String,
}

/// Owns live server connections. Failed servers are kept with status "failed" (never fatal).
pub struct McpManager {
    statuses: Vec<(String, String)>,   
    tools: Vec<Arc<dyn Tool>>,        
    transports: Vec<Arc<Transport>>,  
    skipped: Vec<SkipReason>,         
    failed: Vec<(String, String)>,    
}

impl McpManager {
    /// initialize → notifications/initialized → tools/list for every server, in parallel,
    /// each bounded by `timeout_ms` (MCP_TIMEOUT, default 30000).
    pub async fn start(configs: BTreeMap<String, McpServerConfig>, timeout_ms: u64) -> Self {
        let timeout_ms = if timeout_ms == 0 { 30_000 } else { timeout_ms };

        let mut join = Vec::new();
        let mut skipped = Vec::new();
        
        // Validate and filter configs before starting
        let mut valid_configs = BTreeMap::new();
        for (name, cfg) in configs {
            // Expand environment variables
            let expanded_cfg = match cfg.expand_env() {
                Some(cfg) => cfg,
                None => {
                    skipped.push(SkipReason {
                        server_name: name.clone(),
                        reason: "environment_variable_expansion_failed".to_string(),
                    });
                    continue;
                }
            };
            
            // Validation (SPEC C7.3): a `url` entry must say its `type`; the type must be known;
            // a stdio entry needs a command and an http entry a url.
            if let Some(reason) = validate(&expanded_cfg) {
                skipped.push(SkipReason { server_name: name.clone(), reason: reason.to_string() });
                continue;
            }

            valid_configs.insert(name, expanded_cfg);
        }

        for (name, cfg) in valid_configs {
            join.push(tokio::spawn(async move {
                let result = start_one(&cfg, timeout_ms).await;
                (name, result)
            }));
        }

        let mut statuses = Vec::new();
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut transports: Vec<Arc<Transport>> = Vec::new();
        let mut failed = Vec::new();

        for handle in join {
            match handle.await {
                Ok((name, Ok((transport, remote_tools)))) => {
                    for rt in remote_tools {
                        tools.push(Arc::new(McpTool {
                            server: name.clone(),
                            remote_name: rt.name.clone(),
                            full_name: mcp_tool_name(&name, &rt.name),
                            description: rt.description.clone(),
                            schema: rt.schema.clone(),
                            transport: transport.clone(),
                        }));
                    }
                    transports.push(transport);
                    statuses.push((name, "connected".to_string()));
                }
                Ok((name, Err(e))) => {
                    eprintln!("zen: MCP server '{}' failed: {}", name, e);
                    statuses.push((name.clone(), "failed".to_string()));
                    failed.push((name.clone(), e.to_string()));
                }
                Err(e) => {
                    eprintln!("zen: MCP server task panicked: {}", e);
                }
            }
        }

        Self { statuses, tools, transports, skipped, failed }
    }

    /// One Tool per remote tool, named `mcp__<server>__<tool>`; call → tools/call.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    /// (name, "connected" | "failed" | "pending")
    pub fn status(&self) -> Vec<(String, String)> {
        self.statuses.clone()
    }

    /// Get skipped servers with their reasons
    pub fn errors(&self) -> Vec<SkipReason> {
        self.skipped.clone()
    }

    /// Get failed servers with their error messages
    pub fn failures(&self) -> Vec<(String, String)> {
        self.failed.clone()
    }

    pub async fn shutdown(&self) {
        for t in &self.transports {
            t.kill();
        }
    }
}

/// Skip reason for an invalid (already env-expanded) entry, `None` when it can be started.
pub(crate) fn validate(cfg: &McpServerConfig) -> Option<&'static str> {
    match cfg.kind.as_deref() {
        None if cfg.url.is_some() => Some("url_missing_type"),
        None if cfg.command.is_none() => Some("missing_command"),
        None => None,
        Some(k) if k.eq_ignore_ascii_case("stdio") => {
            cfg.command.is_none().then_some("missing_command")
        }
        Some(k) if k.eq_ignore_ascii_case("http") => cfg.url.is_none().then_some("missing_url"),
        Some(_) => Some("unknown_type"),
    }
}

#[allow(clippy::type_complexity)]
async fn start_one(
    cfg: &McpServerConfig,
    timeout_ms: u64,
) -> anyhow::Result<(Arc<Transport>, Vec<RemoteTool>)> {
    let transport = Arc::new(Transport::from_config(cfg, timeout_ms)?);
    let handshake_transport = transport.clone();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        handshake(&handshake_transport),
    )
    .await;
    match result {
        Ok(Ok(tools)) => Ok((transport, tools)),
        Ok(Err(e)) => {
            transport.kill();
            Err(e)
        }
        Err(_) => {
            transport.kill();
            Err(anyhow::anyhow!("timed out after {} ms", timeout_ms))
        }
    }
}