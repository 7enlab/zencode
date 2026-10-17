//! Sub-agent runner backing the `Task` tool (SPEC §9).
//!
//! The `Task` tool lives in `zen-mcp`/`zen-tools`, but running a sub-agent requires an
//! [`Agent`] plus the `zen-agent` loop — which is this crate. To avoid a crate cycle (zen-tools
//! cannot depend on zen-agent), zen-core exposes the [`SubAgentRunner`] trait and `zen-agent`
//! provides this concrete implementation. The Task tool holds an `Arc<dyn SubAgentRunner>` and
//! just calls it.
//!
//! A sub-agent is a fresh [`Agent`] sharing the parent's backend, tools, permissions, hooks and
//! sink, but:
//!
//! - **empty history** and **depth+1**, so its transcript is independent;
//! - **a restricted tool set**: either the names from the agent definition's `tools` field, or
//!   everything (minus `Task` itself once the depth limit is reached, so nesting can't recurse);
//! - **model `inherit`** keeps the parent's model/fallbacks;
//! - **parent event wiring**: every event the sub-agent emits carries its `parent_tool_use_id`
//!   (the Task call id) so the CLI can attribute them; `SubagentStart` fires before the run
//!   and `SubagentStop` after it (a blocking SubagentStop continues the sub-agent).
//!
//! Only the final assistant text comes back to the parent.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use zen_core::*;

use crate::agent::{Agent, AgentConfig};

/// Runs sub-agents for the `Task` tool.
///
/// Build it once (usually at CLI session start) and reuse it for every Task call.
pub struct SubAgentExecutor {
    /// Model backend shared with sub-agents (so they see the same relay/key).
    pub llm: Arc<dyn LlmBackend>,
    /// The full tool set the parent could offer; filtered per definition.
    pub tools: ToolRegistry,
    /// Permission policy handed to each sub-agent unchanged.
    pub permissions: Arc<dyn PermissionChecker>,
    /// Hooks (incl. `SubagentStop`) handed to each sub-agent unchanged.
    pub hooks: Arc<dyn HookRunner>,
    /// Event sink: sub-agent events carry the Task call id as `parent_tool_use_id`.
    pub sink: Arc<dyn EventSink>,
    /// Session context (cwd, env, todos) cloned into each sub-agent.
    pub ctx: ToolContext,
    /// Template config; `model`/`system_prompt` are overridden per definition.
    pub base_config: AgentConfig,
    /// Maximum sub-agent nesting depth (1 = one level, the default per SPEC §9).
    pub max_depth: usize,
    /// Known skills for a definition's `skills:` preload list. Names not found here are looked
    /// up as `<cwd>/.zen/skills/<name>/SKILL.md`, then `~/.zen/skills/<name>/SKILL.md`.
    pub skills: Vec<SkillInfo>,
}

impl SubAgentExecutor {
    /// Construct a runner. `max_depth` defaults to 1; override for deeper research trees.
    pub fn new(
        llm: Arc<dyn LlmBackend>,
        tools: ToolRegistry,
        permissions: Arc<dyn PermissionChecker>,
        hooks: Arc<dyn HookRunner>,
        sink: Arc<dyn EventSink>,
        ctx: ToolContext,
        base_config: AgentConfig,
    ) -> Self {
        Self { llm, tools, permissions, hooks, sink, ctx, base_config, max_depth: 1, skills: Vec::new() }
    }

    /// Set the maximum allowed sub-agent nesting depth.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth.max(1);
    }

    /// Body of a skill to preload (SPEC C6.1 `skills`), `${ZEN_SKILL_DIR}` substituted.
    fn skill_body(&self, name: &str) -> Option<String> {
        let (path, body) = match self.skills.iter().find(|s| s.name == name) {
            Some(s) => (s.path.clone(), s.body.clone()),
            None => {
                let cwd = self.ctx.cwd.lock().map(|c| c.clone()).ok()?;
                let mut candidates = vec![cwd.join(".zen").join("skills").join(name).join("SKILL.md")];
                if let Some(h) = dirs::home_dir() {
                    candidates.push(h.join(".zen").join("skills").join(name).join("SKILL.md"));
                }
                let path = candidates.into_iter().find(|p| p.is_file())?;
                let raw = std::fs::read_to_string(&path).ok()?;
                (path, strip_frontmatter(&raw).to_string())
            }
        };
        let dir = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
        Some(body.replace("${ZEN_SKILL_DIR}", &dir).trim().to_string())
    }
}

/// Body of a markdown file after an optional `---` frontmatter block.
fn strip_frontmatter(raw: &str) -> &str {
    let Some(rest) = raw.strip_prefix("---") else { return raw };
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

        // Tool set: explicit allow-list from the definition, else inherit everything.
        let mut tools = match &def.tools {
            Some(list) if !list.is_empty() => self.tools.filtered(list),
            _ => self.tools.clone(),
        };
        // `disallowedTools` removes names from whatever set we ended up with (SPEC C6.1).
        if !def.disallowed_tools.is_empty() {
            let remaining: Vec<String> = tools
                .names()
                .into_iter()
                .filter(|n| !def.disallowed_tools.iter().any(|d| d == n))
                .collect();
            tools = tools.filtered(&remaining);
        }
        // No nested Task calls once the depth limit is reached (SPEC §9).
        if depth >= self.max_depth {
            let remaining: Vec<String> = tools
                .names()
                .into_iter()
                .filter(|n| n != "Task")
                .collect();
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

        // Preloaded skills: full content goes into the sub-agent's system prompt (SPEC C6.1).
        for name in &def.skills {
            match self.skill_body(name) {
                Some(body) => {
                    system_prompt.push_str(&format!("\n\n## Skill: {}\n{}", name, body));
                }
                None => self.sink.emit(AgentEvent::SystemMessage {
                    event: "SubagentStart".into(),
                    text: format!("sub-agent '{}': skill '{}' not found, not preloaded", def.name, name),
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
            permission_mode: subagent_mode(
                self.base_config.permission_mode,
                def.permission_mode.as_deref(),
            ),
            context_window: self.base_config.context_window,
            auto_compact_threshold: self.base_config.auto_compact_threshold,
            keep_after_compact: self.base_config.keep_after_compact,
            depth,
            // The finish gate belongs to the main turn; sub-agents never run it.
            verify_reminder: false,
            verify_command: None,
            verify_timeout_secs: self.base_config.verify_timeout_secs,
            // Per-edit checks are cheap feedback on the sub-agent's own edits: inherit them.
            verify_after_edit: self.base_config.verify_after_edit.clone(),
            verify_after_edit_timeout_secs: self.base_config.verify_after_edit_timeout_secs,
            tool_timeouts: self.base_config.tool_timeouts.clone(),
            max_continuations: self.base_config.max_continuations,
        };

        let mut agent = Agent::new(
            self.llm.clone(),
            tools,
            self.permissions.clone(),
            None, // sub-agents inherit the parent's permission policy; never interactive.
            self.hooks.clone(),
            self.sink.clone(),
            self.ctx.clone(),
            config,
        );
        agent.parent_tool_use_id = Some(parent_tool_use_id.to_string());

        // SubagentStart: informational; the agent type is the matcher target (SPEC C6.3).
        let start = self
            .hooks
            .run(
                HookEvent::SubagentStart,
                &def.name,
                json!({ "agent_type": def.name, "parent_tool_use_id": parent_tool_use_id }),
            )
            .await;
        if let Some(text) = start.system_message {
            self.sink.emit(AgentEvent::SystemMessage { event: "SubagentStart".into(), text });
        }

        let mut result = agent.run_turn(prompt).await;

        // SubagentStop fires for every sub-agent with the definition name as matcher target.
        // A block (exit 2 / decision "block") sends the reason back as the next user turn, with
        // the same loop guard as the main Stop hook (SPEC C3.9).
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

/// The permission mode a sub-agent runs in.
///
/// A definition may pick a mode (e.g. a read-only `plan` agent), but an agent file checked into a
/// repository must not be able to *escalate* past the session: `bypassPermissions` is only kept
/// when the parent already runs in it, and `acceptEdits` only when the parent is acceptEdits or
/// bypass. Unknown values fall back to the parent's mode.
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
