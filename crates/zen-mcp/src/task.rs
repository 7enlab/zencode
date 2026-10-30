use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use zen_core::{AgentDefinition, SubAgentRunner, Tool, ToolContext, ToolOutput};

pub struct TaskTool {
    pub agents: Vec<AgentDefinition>,
    pub runner: Arc<dyn SubAgentRunner>,
}

impl TaskTool {
    pub fn new(agents: Vec<AgentDefinition>, runner: Arc<dyn SubAgentRunner>) -> Self {
        Self { agents, runner }
    }

    fn find(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|a| a.name == name)
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "Task"
    }
    fn description(&self) -> &str {
        "Delegate a self-contained task to a sub-agent."
    }
    fn parameters_schema(&self) -> Value {
        let mut agent_descriptions = String::new();
        for agent in &self.agents {
            if !agent_descriptions.is_empty() {
                agent_descriptions.push_str(", ");
            }
            agent_descriptions.push_str(&agent.description);
        }
        let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A short (3-5 word) label for the task."
                },
                "prompt": {
                    "type": "string",
                    "description": "The full, self-contained instruction for the sub-agent."
                },
                "subagent_type": {
                    "type": "string",
                    "description": format!("Name of the agent to run; defaults to general-purpose. Available agents: {}", agent_descriptions),
                    "enum": names
                }
            },
            "required": ["description", "prompt"]
        })
    }
    fn is_read_only(&self) -> bool {
        true
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> ToolOutput {
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return ToolOutput::err("Error: missing required parameter 'prompt'"),
        };
        let subagent_type = args
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("general-purpose");

        let def = match self.find(subagent_type) {
            Some(d) => d.clone(),
            None => {
                let names: Vec<String> = self.agents.iter().map(|a| a.name.clone()).collect();
                return ToolOutput::err(format!(
                    "Error: unknown subagent_type '{}'. Valid types: {}",
                    subagent_type,
                    if names.is_empty() {
                        "(none)".to_string()
                    } else {
                        names.join(", ")
                    }
                ));
            }
        };

        let parent_id = ctx
            .current_tool_use_id
            .clone()
            .unwrap_or_else(|| "task".to_string());

        match self.runner.run_subagent(&def, &prompt, &parent_id).await {
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::err(format!("Sub-agent '{}' failed: {}", def.name, e)),
        }
    }
}
