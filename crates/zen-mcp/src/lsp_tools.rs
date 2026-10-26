




use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zen_core::{Tool, ToolContext, ToolOutput};

use crate::lsp::{LspLocation, LspManager};


fn resolve_file(ctx: &ToolContext, file: &str) -> PathBuf {
    let p = PathBuf::from(file);
    if p.is_absolute() {
        p
    } else {
        ctx.cwd().join(p)
    }
}

fn arg_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing required string argument '{key}'"))
}

fn arg_usize(args: &Value, key: &str) -> anyhow::Result<usize> {
    let n = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("missing required integer argument '{key}'"))?
        as usize;
    if n < 1 {
        anyhow::bail!("'{key}' must be >= 1, got {n}");
    }
    Ok(n)
}


fn format_locations(
    tool: &str,
    query_path: &Path,
    line: usize,
    col: usize,
    locs: &[LspLocation],
) -> String {
    let mut out = format!(
        "{tool} {}:{line}:{col} → {} result(s):\n",
        query_path.display(),
        locs.len()
    );
    if locs.is_empty() {
        out.push_str("(no results)\n");
    } else {
        out.push('\n');
        for loc in locs {
            out.push_str(&format!("{}:{}:{}\n", loc.path, loc.line, loc.col));
        }
    }
    out.push_str("\n```json\n");
    out.push_str(&serde_json::to_string_pretty(locs).unwrap_or_else(|_| "[]".to_string()));
    out.push_str("\n```\n");
    out
}




pub struct GotoDefinitionTool {
    manager: Arc<LspManager>,
}

impl GotoDefinitionTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for GotoDefinitionTool {
    fn name(&self) -> &str {
        "goto_definition"
    }

    fn description(&self) -> &str {
        "Jump to the definition of the symbol at a file position, using a real language \
         server (rust-analyzer, pyright, typescript-language-server). More precise than text \
         search: it understands imports, types and scopes. Arguments: file (path, absolute \
         or relative to the project root), line (1-based), col (1-based character column)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": {"type": "string",
                         "description": "Path to the source file (absolute or relative to the project root)"},
                "line": {"type": "integer", "minimum": 1,
                         "description": "1-based line number"},
                "col": {"type": "integer", "minimum": 1,
                        "description": "1-based character column"}
            },
            "required": ["file", "line", "col"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let file = match arg_str(&args, "file") {
            Ok(f) => f,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let line = match arg_usize(&args, "line") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let col = match arg_usize(&args, "col") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let path = resolve_file(ctx, &file);
        match self.manager.goto_definition(&path, line, col).await {
            Ok(locs) => ToolOutput::ok(format_locations("goto_definition", &path, line, col, &locs)),
            Err(e) => ToolOutput::err(format!("goto_definition failed: {e}")),
        }
    }
}




pub struct HoverTool {
    manager: Arc<LspManager>,
}

impl HoverTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for HoverTool {
    fn name(&self) -> &str {
        "hover"
    }

    fn description(&self) -> &str {
        "Show the language server's hover information (type signature, documentation) for the \
         symbol at a file position. Arguments: file (path, absolute or relative to the project \
         root), line (1-based), col (1-based character column)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": {"type": "string",
                         "description": "Path to the source file (absolute or relative to the project root)"},
                "line": {"type": "integer", "minimum": 1,
                         "description": "1-based line number"},
                "col": {"type": "integer", "minimum": 1,
                        "description": "1-based character column"}
            },
            "required": ["file", "line", "col"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let file = match arg_str(&args, "file") {
            Ok(f) => f,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let line = match arg_usize(&args, "line") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let col = match arg_usize(&args, "col") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let path = resolve_file(ctx, &file);
        match self.manager.hover(&path, line, col).await {
            Ok(text) => {
                let body = if text.trim().is_empty() {
                    "(no hover information)".to_string()
                } else {
                    text
                };
                let mut out = format!("hover {}:{line}:{col}:\n\n{body}\n", path.display());
                out.push_str("\n```json\n");
                let raw = json!({"contents": body});
                out.push_str(&serde_json::to_string_pretty(&raw).unwrap_or_else(|_| "{}".to_string()));
                out.push_str("\n```\n");
                ToolOutput::ok(out)
            }
            Err(e) => ToolOutput::err(format!("hover failed: {e}")),
        }
    }
}




pub struct FindReferencesTool {
    manager: Arc<LspManager>,
}

impl FindReferencesTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for FindReferencesTool {
    fn name(&self) -> &str {
        "find_references"
    }

    fn description(&self) -> &str {
        "Find all references to the symbol at a file position, using a real language server \
         (rust-analyzer, pyright, typescript-language-server). Understands scopes and renames, \
         unlike text search. Arguments: file (path), line (1-based), col (1-based character \
         column), include_declaration (default true)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file": {"type": "string",
                         "description": "Path to the source file (absolute or relative to the project root)"},
                "line": {"type": "integer", "minimum": 1,
                         "description": "1-based line number"},
                "col": {"type": "integer", "minimum": 1,
                        "description": "1-based character column"},
                "include_declaration": {"type": "boolean", "default": true,
                         "description": "Include the declaration itself in the results"}
            },
            "required": ["file", "line", "col"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let file = match arg_str(&args, "file") {
            Ok(f) => f,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let line = match arg_usize(&args, "line") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let col = match arg_usize(&args, "col") {
            Ok(n) => n,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let include_declaration = args
            .get("include_declaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let path = resolve_file(ctx, &file);
        match self
            .manager
            .find_references(&path, line, col, include_declaration)
            .await
        {
            Ok(locs) => ToolOutput::ok(format_locations(
                "find_references",
                &path,
                line,
                col,
                &locs,
            )),
            Err(e) => ToolOutput::err(format!("find_references failed: {e}")),
        }
    }
}
