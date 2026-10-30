use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zen_core::*;

use crate::agent::{Agent, AgentConfig};

pub struct SubAgentExecutor {
    pub llm: Arc<dyn LlmBackend>,

    pub tools: ToolRegistry,

    pub permissions: Arc<dyn PermissionChecker>,

    pub hooks: Arc<dyn HookRunner>,

    pub sink: Arc<dyn EventSink>,

    pub ctx: ToolContext,

    pub base_config: AgentConfig,

    pub max_depth: usize,

    pub skills: Vec<SkillInfo>,
}

impl SubAgentExecutor {
    pub fn new(
        llm: Arc<dyn LlmBackend>,
        tools: ToolRegistry,
        permissions: Arc<dyn PermissionChecker>,
        hooks: Arc<dyn HookRunner>,
        sink: Arc<dyn EventSink>,
        ctx: ToolContext,
        base_config: AgentConfig,
    ) -> Self {
        Self {
            llm,
            tools,
            permissions,
            hooks,
            sink,
            ctx,
            base_config,
            max_depth: 1,
            skills: Vec::new(),
        }
    }

    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth.max(1);
    }

    fn skill_body(&self, name: &str) -> Option<String> {
        let (path, body) = match self.skills.iter().find(|s| s.name == name) {
            Some(s) => (s.path.clone(), s.body.clone()),
            None => {
                let cwd = self.ctx.cwd.lock().map(|c| c.clone()).ok()?;
                let mut candidates =
                    vec![cwd.join(".zen").join("skills").join(name).join("SKILL.md")];
                if let Some(h) = dirs::home_dir() {
                    candidates.push(h.join(".zen").join("skills").join(name).join("SKILL.md"));
                }
                let path = candidates.into_iter().find(|p| p.is_file())?;
                let raw = std::fs::read_to_string(&path).ok()?;
                (path, strip_frontmatter(&raw).to_string())
            }
        };
        let dir = path
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        Some(body.replace("${ZEN_SKILL_DIR}", &dir).trim().to_string())
    }
}

fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else {
        return raw;
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n').skip(1) {
        offset += line.len();
        if line.trim() == "---" {
            let first_len = rest.split_inclusive('\n').next().map(str::len).unwrap_or(0);
            return &rest[first_len + offset..];
        }
    }
    raw
}

#[async_trait]
impl SubAgentRunner for SubAgentExecutor {
    async fn run_subagent(
        &self,
        def: &AgentDefinition,
        prompt: &str,
        parent_tool_use_id: &str,
    ) -> anyhow::Result<String> {
        let depth = self.base_config.depth + 1;
        if depth > self.max_depth {
            anyhow::bail!(
                "sub-agent depth limit reached (max_depth = {}): refusing to spawn '{}'",
                self.max_depth,
                def.name
            );
        }

        let mut tools = match &def.tools {
            Some(list) if !list.is_empty() => self.tools.filtered(list),
            _ => self.tools.clone(),
        };

        if !def.disallowed_tools.is_empty() {
            let remaining: Vec<String> = tools
                .names()
                .into_iter()
                .filter(|n| !def.disallowed_tools.iter().any(|d| d == n))
                .collect();
            tools = tools.filtered(&remaining);
        }

        if depth >= self.max_depth {
            let remaining: Vec<String> =
                tools.names().into_iter().filter(|n| n != "Task").collect();
            tools = tools.filtered(&remaining);
        }

        let mut system_prompt = if def.system_prompt.is_empty() {
            "You are a sub-agent working on one delegated task. Use your tools as needed, \
             then reply with your final result as plain text. Do not ask follow-up questions."
                .to_string()
        } else {
            format!(
                "{}\n\nYou are a sub-agent working on one delegated task. Use your tools as needed, \
                 then reply with your final result as plain text. Do not ask follow-up questions.",
                def.system_prompt
            )
        };

        for name in &def.skills {
            match self.skill_body(name) {
                Some(body) => {
                    system_prompt.push_str(&format!("\n\n## Skill: {}\n{}", name, body));
                }
                None => self.sink.emit(AgentEvent::SystemMessage {
                    event: "SubagentStart".into(),
                    text: format!(
                        "sub-agent '{}': skill '{}' not found, not preloaded",
                        def.name, name
                    ),
                }),
            }
        }

        let model = match def.model.as_deref() {
            Some(m) if !m.is_empty() && m != "inherit" => m.to_string(),
            _ => self.base_config.model.clone(),
        };

        let config = AgentConfig {
            model,
            fallback_models: self.base_config.fallback_models.clone(),
            system_prompt,
            max_turns: def.max_turns.unwrap_or(self.base_config.max_turns),
            budget: self.base_config.budget.clone(),
            permission_mode: subagent_mode(
                self.base_config.permission_mode,
                def.permission_mode.as_deref(),
            ),
            context_window: self.base_config.context_window,
            auto_compact_threshold: self.base_config.auto_compact_threshold,
            keep_after_compact: self.base_config.keep_after_compact,
            depth,

            verify_reminder: false,
            verify_command: None,
            verify_timeout_secs: self.base_config.verify_timeout_secs,

            verify_after_edit: self.base_config.verify_after_edit.clone(),
            verify_after_edit_timeout_secs: self.base_config.verify_after_edit_timeout_secs,
            tool_timeouts: self.base_config.tool_timeouts.clone(),
            max_continuations: self.base_config.max_continuations,
            prune: self.base_config.prune.clone(),
            max_output_tokens: self.base_config.max_output_tokens,

            tool_bundle: self.base_config.tool_bundle,
            max_parallel_tool_calls: self.base_config.max_parallel_tool_calls,
            done_scope: self.base_config.done_scope.clone(),
        };

        let mut agent = Agent::new(
            self.llm.clone(),
            tools,
            self.permissions.clone(),
            None,
            self.hooks.clone(),
            self.sink.clone(),
            self.ctx.clone(),
            config,
        );
        agent.parent_tool_use_id = Some(parent_tool_use_id.to_string());

        let start = self
            .hooks
            .run(
                HookEvent::SubagentStart,
                &def.name,
                json!({ "agent_type": def.name, "parent_tool_use_id": parent_tool_use_id }),
            )
            .await;
        if let Some(text) = start.system_message {
            self.sink.emit(AgentEvent::SystemMessage {
                event: "SubagentStart".into(),
                text,
            });
        }

        let mut result = agent.run_turn(prompt).await;

        let mut continues = 0usize;
        loop {
            let stop = self
                .hooks
                .run(
                    HookEvent::SubagentStop,
                    &def.name,
                    json!({
                        "agent_type": def.name,
                        "parent_tool_use_id": parent_tool_use_id,
                        "stop_hook_active": continues > 0,
                        "last_assistant_message": result.result,
                    }),
                )
                .await;
            if result.is_error
                || !stop.blocked
                || stop.stop_run
                || continues >= agent.stop_hook_max_continues
            {
                break;
            }
            continues += 1;
            let feedback = stop.feedback.unwrap_or_else(|| "continue".into());
            result = agent.run_turn(&feedback).await;
        }

        if result.is_error {
            anyhow::bail!(result.result);
        }
        Ok(result.result)
    }
}

pub(crate) fn subagent_mode(parent: PermissionMode, requested: Option<&str>) -> PermissionMode {
    let Some(req) = requested.and_then(PermissionMode::parse) else {
        return parent;
    };
    match req {
        PermissionMode::BypassPermissions if parent != PermissionMode::BypassPermissions => parent,
        PermissionMode::AcceptEdits
            if !matches!(
                parent,
                PermissionMode::AcceptEdits | PermissionMode::BypassPermissions
            ) =>
        {
            parent
        }
        other => other,
    }
}
