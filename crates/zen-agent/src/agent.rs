//! The core agent loop (SPEC §3 and §11).
//!
//! `Agent` is intentionally dumb about *where* its language model and tools live: it only depends
//! on [`zen_core::LlmBackend`], [`zen_core::ToolRegistry`] and the small callback traits in
//! `zen_core` (`PermissionChecker`, `Prompter`, `HookRunner`, `EventSink`). That keeps it trivially
//! testable (the unit tests swap in scripted versions of all of those) and keeps the CLI in
//! `zen-cli` free to assemble the real wiring (reqwest client, rule-based permissions, real hooks).
//!
//! Run model → dispatch tool calls → run hooks → repeat. A turn always ends in a [`RunResult`],
//! even on errors, so callers never have to deal with panics from a misbehaving model/gateway.
//!
//! Notable SPEC commitments realized here:
//!
//! - **Per-turn model fallback** ([`AgentConfig::fallback_models`] plus the primary): each model is
//!   tried once, after its own retries are exhausted by the backend.
//! - **Stop-hook loop guard**. A Stop hook that keeps signaling "continue" would otherwise spin
//!   forever; we cap consecutive continues at [`Agent::DEFAULT_STOP_HOOK_CONTINUES`] (overridable by
//!   a human via [`Agent::set_stop_hook_max_continues`]). Re-runs carry `stop_hook_active: true`
//!   so a well-behaved hook can tell it is being asked again (SPEC C3.9).
//! - **History stays OpenAI-shaped.** The loop always appends a `tool` message for every tool_call
//!   it emits (including error/denial results), and [`crate::plan`] keeps compaction from ever
//!   producing an orphan tool result. [`Agent::request_messages`] additionally sanitizes any history
//!   that arrived broken (e.g. from a resumed transcript) before it is sent.
//! - **Plan mode** ([`zen_core::PermissionMode::Plan`]) hides mutating tools in
//!   [`Agent::tools_for_request`] and reports the model's final message as the plan text.

use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use zen_core::*;

use crate::compaction;
use crate::finish;

/// How often one turn requeries after an empty reply before accepting it (r7 C5).
const MAX_EMPTY_REQUERIES: usize = 2;
/// The correction sent when the server could not parse the reply on any retry (r7 C7).
const UNPARSED_REPLY_NUDGE: &str = "[zen: the server could not parse your last reply. Continue the task: make one tool call with plain JSON arguments, or give your final answer.]";

/// A client error that means "the sample came back malformed" (see `client::transient_completion`),
/// as opposed to a network, auth or quota failure.
fn is_unparsed_sample(err: &str) -> bool {
    err.contains("model returned no choices") || err.contains("gateway error in 200 response")
}

/// The correction sent after an empty reply (r7 C5).
const EMPTY_REPLY_NUDGE: &str =
    "[zen: your last reply was empty. Continue the task: call a tool, or give your final answer.]";
use crate::guard;
use crate::plan;
use crate::repair;

/// Default number of times a Stop hook may ask the loop to continue within one [`Agent::run_turn`].
pub const DEFAULT_STOP_HOOK_CONTINUES: usize = 8;

/// Configuration consumed by [`Agent`]. All fields are public so the CLI and tests can build it
/// directly (it derives `Clone` + `Debug`).
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Primary model name passed straight to the backend.
    pub model: String,
    /// Tried, in order, after the primary fails each request.
    pub fallback_models: Vec<String>,
    /// System prompt attached to every request (never stored in history).
    pub system_prompt: String,
    /// Hard cap on model calls per turn; exceeding it yields `error_max_turns`.
    pub max_turns: usize,
    /// Permission mode (default / acceptEdits / plan / bypassPermissions / dontAsk).
    pub permission_mode: PermissionMode,
    /// Approximate model context window in tokens; used only to derive the default
    /// `auto_compact_threshold`.
    pub context_window: u64,
    /// When the prompt exceeds this many tokens the loop auto-compacts. `None` disables it.
    pub auto_compact_threshold: Option<u64>,
    /// Trailing messages kept verbatim after a compaction.
    pub keep_after_compact: usize,
    /// Nesting depth for sub-agents spawned from this agent (0 = main session).
    pub depth: usize,
    /// Check-before-finish (r7): one reminder when files changed and no command ran since.
    pub verify_reminder: bool,
    /// Command the harness runs itself at a would-be stop after file changes (settings
    /// `verify.command`), at most [`finish::MAX_VERIFY_ROUNDS`] times per turn.
    pub verify_command: Option<String>,
    /// Timeout for `verify_command`.
    pub verify_timeout_secs: u64,
    /// Command run after every successful Edit/MultiEdit/Write (settings `verify.afterEdit`,
    /// off by default); a failure's output tail is appended to that tool result (perf p0).
    pub verify_after_edit: Option<String>,
    /// Timeout for `verify_after_edit` (default 60 s).
    pub verify_after_edit_timeout_secs: u64,
    /// Settings `toolTimeouts` (p0b): milliseconds per tool name, `"default"` for tools without
    /// their own entry, 0 = no timeout. See [`AgentConfig::tool_timeout_ms`].
    pub tool_timeouts: std::collections::HashMap<String, u64>,
}

/// Timeout for a tool call when settings `toolTimeouts` has no entry for it (p0b).
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 300_000;

impl AgentConfig {
    /// The timeout the agent enforces around one call of `tool` (None = none).
    ///
    /// Bash enforces its own timeout (it kills the process and keeps partial semantics), so the
    /// agent hands Bash its configured default through `ToolContext::tool_timeout_ms` instead.
    /// `Task` (a whole sub-agent run) and `JobOutput` (bounded by its own `wait_ms`) have no
    /// timeout unless configured by name.
    pub fn tool_timeout_ms(&self, tool: &str) -> Option<u64> {
        if tool == "Bash" {
            return None;
        }
        let t = match self.tool_timeouts.get(tool) {
            Some(t) => *t,
            None if matches!(tool, "Task" | "JobOutput") => 0,
            None => self.tool_timeouts.get("default").copied().unwrap_or(DEFAULT_TOOL_TIMEOUT_MS),
        };
        (t > 0).then_some(t)
    }
}

/// Run one tool call under its timeout (p0b).
async fn run_tool(tool: Arc<dyn Tool>, args: Value, ctx: ToolContext, timeout_ms: Option<u64>) -> ToolOutput {
    let Some(ms) = timeout_ms else { return tool.call(args, &ctx).await };
    match tokio::time::timeout(std::time::Duration::from_millis(ms), tool.call(args, &ctx)).await {
        Ok(out) => out,
        Err(_) => ToolOutput::err(format!(
            "Error: {} timed out after {} ms and was stopped (settings toolTimeouts.{} or toolTimeouts.default)",
            tool.name(),
            ms,
            tool.name()
        )),
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "google/gemini-4-argon:free".into(),
            fallback_models: vec![],
            system_prompt: String::new(),
            max_turns: 50,
            permission_mode: PermissionMode::Default,
            context_window: 128_000,
            auto_compact_threshold: Some(100_000),
            keep_after_compact: 4,
            depth: 0,
            verify_reminder: false,
            verify_command: None,
            verify_timeout_secs: 300,
            verify_after_edit: None,
            verify_after_edit_timeout_secs: 60,
            tool_timeouts: Default::default(),
        }
    }
}

/// The running agent: one model backend, one tool registry, one set of callbacks, one transcript.
///
/// Constructed mostly by the CLI; tests build their own scripted instances. All fields are public so
/// tests can assert on and mutate them freely.
pub struct Agent {
    /// The language backend (OpenAI-compatible, or a scripted test double).
    pub llm: Arc<dyn LlmBackend>,
    /// All tools available to this agent.
    pub tools: ToolRegistry,
    /// Permission policy; consulted per tool call except where a hook overrides.
    pub permissions: Arc<dyn PermissionChecker>,
    /// Human-in-the-loop; `None` makes the agent headless (Ask denies).
    pub prompter: Option<Arc<dyn Prompter>>,
    /// Hooks run at well-defined points (UserPromptSubmit, PreToolUse, …).
    pub hooks: Arc<dyn HookRunner>,
    /// Where assistant/tool/retry events are streamed to (e.g. a CLI formatter).
    pub sink: Arc<dyn EventSink>,
    /// Shared per-session state carried to every tool call.
    pub ctx: ToolContext,
    /// Per-turn configuration.
    pub config: AgentConfig,
    /// Conversation history WITHOUT the system prompt (system is prepended per request).
    pub messages: Vec<ChatMessage>,
    /// Accumulated token usage across the whole turn.
    pub usage: UsageStats,
    /// Permission denials observed this turn (surfaces in the json `result`).
    pub denials: Vec<PermissionDenial>,
    /// `Task` tool_call id this agent is a sub-agent for; copied onto emitted events.
    pub parent_tool_use_id: Option<String>,
    /// Most recent `prompt_tokens`, for auto-compaction sizing (0 = use estimate).
    pub last_prompt_tokens: u64,
    /// Cap on consecutive Stop-hook "continue" signals (prevents an infinite loop).
    pub stop_hook_max_continues: usize,
    /// Model calls since the todo list was last in front of the model (TodoWrite, a
    /// re-injection, or compaction reset it).
    pub model_calls_since_todo: usize,
}

impl Agent {
    /// Human-facing constructor. The cap defaults to [`DEFAULT_STOP_HOOK_CONTINUES`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm: Arc<dyn LlmBackend>,
        tools: ToolRegistry,
        permissions: Arc<dyn PermissionChecker>,
        prompter: Option<Arc<dyn Prompter>>,
        hooks: Arc<dyn HookRunner>,
        sink: Arc<dyn EventSink>,
        ctx: ToolContext,
        config: AgentConfig,
    ) -> Self {
        Self {
            llm,
            tools,
            permissions,
            prompter,
            hooks,
            sink,
            ctx,
            config,
            messages: vec![],
            usage: UsageStats::default(),
            denials: vec![],
            parent_tool_use_id: None,
            last_prompt_tokens: 0,
            stop_hook_max_continues: DEFAULT_STOP_HOOK_CONTINUES,
            model_calls_since_todo: 0,
        }
    }

    /// Replace the conversation history (used by `--resume`/`--continue` and `/clear`).
    pub fn set_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
        // A new history invalidates the API-reported context size.
        self.last_prompt_tokens = 0;
    }

    /// The conversation history (excluding the system prompt).
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Override the Stop-hook continue cap (used by CLI flag / tests).
    pub fn set_stop_hook_max_continues(&mut self, n: usize) {
        self.stop_hook_max_continues = n.max(1);
    }

    /// Tools to send in the `tools` field of the request. In plan mode mutating tools are hidden
    /// (SPEC §5 / Appendix B4), not merely denied.
    fn tools_for_request(&self) -> Vec<ToolDefinition> {
        let defs = self.tools.definitions();
        if self.config.permission_mode == PermissionMode::Plan {
            plan::plan_mode_tools(defs)
        } else {
            defs
        }
    }

    /// Full message list for the request: system prompt prepended, history sanitized so any
    /// orphaned/dangling tool-call exchange from a resumed transcript can't make the API reject
    /// the request.
    fn request_messages(&self) -> Vec<ChatMessage> {
        let mut msgs: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        if !self.config.system_prompt.is_empty() {
            msgs.push(ChatMessage::system(self.config.system_prompt.clone()));
        }
        msgs.extend(self.messages.iter().cloned());
        crate::plan::ensure_tool_responses(msgs)
    }

    /// Assemble the final `[{"type":"result",...}]` object for a turn.
    fn run_result(
        &self,
        subtype: &str,
        is_error: bool,
        result: String,
        duration_ms: u64,
        duration_api_ms: u64,
        num_turns: usize,
    ) -> RunResult {
        RunResult {
            record_type: "result".into(),
            subtype: subtype.into(),
            is_error,
            duration_ms,
            duration_api_ms,
            num_turns,
            result,
            session_id: self.ctx.session_id.clone(),
            total_cost_usd: 0.0,
            usage: self.usage.clone(),
            model: self.config.model.clone(),
            permission_denials: self.denials.clone(),
        }
    }

    /// Record a permission denial (emit + remember for the result object).
    fn record_denial(&mut self, tool_name: &str, tool_use_id: &str, input: &Value) {
        let denial = PermissionDenial {
            tool_name: tool_name.to_string(),
            tool_use_id: tool_use_id.to_string(),
            tool_input: input.clone(),
        };
        self.sink.emit(AgentEvent::PermissionDenied(denial.clone()));
        self.denials.push(denial);
    }

    /// Surface a hook's `systemMessage` to the user (never to the model).
    /// Notification hook fired right before the user is asked for a permission (C3.2).
    async fn notify_permission_prompt(&self, tool_name: &str) {
        let note = self
            .hooks
            .run(
                HookEvent::Notification,
                "permission_prompt",
                json!({
                    "message": format!("zen needs your permission to use {}", tool_name),
                    "notification_type": "permission_prompt",
                }),
            )
            .await;
        self.emit_system_message(HookEvent::Notification, &note);
    }

    fn emit_system_message(&self, event: HookEvent, outcome: &HookOutcome) {
        if let Some(text) = &outcome.system_message {
            if !text.is_empty() {
                self.sink.emit(AgentEvent::SystemMessage {
                    event: event.as_str().to_string(),
                    text: text.clone(),
                });
            }
        }
    }

    /// Emit a tool result event and return the textual content (for logging).
    fn emit_tool_result(&self, tool_use_id: &str, content: &str, is_error: bool) {
        self.sink.emit(AgentEvent::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error,
            parent_tool_use_id: self.parent_tool_use_id.clone(),
        });
    }

    /// In plan mode, the turn's result is the model's plan text: the assistant content since the
    /// last user turn. Falls back to the last text if history shape is unexpected.
    fn plan_text(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        for m in self.messages.iter().rev() {
            if m.role == "user" {
                break;
            }
            if m.role == "assistant" {
                if let Some(c) = &m.content {
                    if !c.trim().is_empty() {
                        parts.push(c.clone());
                    }
                }
            }
        }
        parts.reverse();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n\n"))
        }
    }

    /// Run one user turn through the full loop (SPEC §3 & §11).
    ///
    /// Steps: UserPromptSubmit hook → loop (auto-compact if needed → model with fallback →
    /// PreToolUse hook → permission → tool → PostToolUse hook → append tool result → Stop
    /// hook) until the model returns no tool calls or `max_turns` is hit. Never returns `Err`;
    /// a failing turn surfaces as a `RunResult` with `is_error = true`.
    pub async fn run_turn(&mut self, prompt: &str) -> RunResult {
        let started = Instant::now();
        let mut duration_api_ms: u64 = 0;

        // 1. UserPromptSubmit hook.
        let ups = self
            .hooks
            .run(HookEvent::UserPromptSubmit, "", json!({ "prompt": prompt }))
            .await;
        self.emit_system_message(HookEvent::UserPromptSubmit, &ups);
        if ups.stop_run {
            // `continue: false`: the prompt is not processed; stopReason is what the user sees.
            return self.run_result(
                "success",
                false,
                ups.stop_reason.or(ups.feedback).unwrap_or_default(),
                started.elapsed().as_millis() as u64,
                duration_api_ms,
                0,
            );
        }
        if ups.blocked {
            let feedback = ups
                .feedback
                .unwrap_or_else(|| "user prompt rejected by hook".into());
            return self.run_result(
                "error_during_execution",
                true,
                feedback,
                started.elapsed().as_millis() as u64,
                duration_api_ms,
                0,
            );
        }
        let mut user_content = prompt.to_string();
        if let Some(extra) = ups.feedback {
            if !extra.is_empty() {
                user_content.push_str("\n\n");
                user_content.push_str(&extra);
            }
        }
        self.messages.push(ChatMessage::user(user_content));

        let mut num_turns: usize = 0;
        let mut final_text: String;
        // How many times the Stop hook has asked us to continue in this turn.
        let mut stop_continues: usize = 0;
        let plan_mode = self.config.permission_mode == PermissionMode::Plan;
        let mut turn_guard = guard::TurnGuard::default();
        let mut finish_gate = finish::FinishGate::default();
        let mut empty_requeries = 0usize;

        loop {
            if num_turns >= self.config.max_turns {
                return self.run_result(
                    "error_max_turns",
                    true,
                    format!("Reached maximum number of turns ({})", self.config.max_turns),
                    started.elapsed().as_millis() as u64,
                    duration_api_ms,
                    num_turns,
                );
            }

            // 4. Auto-compaction before the request (SPEC §11). Failure is non-fatal.
            if let Some(threshold) = self.config.auto_compact_threshold {
                let used = compaction::context_tokens(self);
                if used > threshold && !self.messages.is_empty() {
                    let _ = compaction::compact_impl(self, None, "auto").await;
                }
            }

            num_turns += 1;
            if self.config.depth == 0 {
                self.maybe_reinject_todos();
            }
            let messages = self.request_messages();
            let tools = self.tools_for_request();
            let (resp, api_ms, _model) = match
                crate::client::chat_with_fallback(
                    &self.llm,
                    &self.config.model,
                    &self.config.fallback_models,
                    messages,
                    tools,
                )
                .await
            {
                Ok(v) => v,
                Err(e) if is_unparsed_sample(&e.to_string()) && empty_requeries < MAX_EMPTY_REQUERIES => {
                    // The server failed to parse the sampled reply (gpt-oss Harmony header) on every
                    // retry: requery with a correction instead of ending the run (r7 bench C7,
                    // same budget as the empty-reply requery).
                    empty_requeries += 1;
                    self.messages.push(ChatMessage::user(UNPARSED_REPLY_NUDGE));
                    continue;
                }
                Err(e) => {
                    return self.run_result(
                        "error_during_execution",
                        true,
                        e.to_string(),
                        started.elapsed().as_millis() as u64,
                        duration_api_ms,
                        num_turns,
                    );
                }
            };
            duration_api_ms += api_ms;

            self.usage.input_tokens += resp.usage.prompt_tokens;
            self.usage.output_tokens += resp.usage.completion_tokens;
            self.last_prompt_tokens = resp.usage.prompt_tokens;

            let mut resp = resp;
            // Repair (r7): a tool call written as the message text becomes a real call.
            let mut text_call_id: Option<String> = None;
            if resp.message.tool_calls.as_ref().is_none_or(|c| c.is_empty()) {
                let text = resp.message.content.clone().unwrap_or_default();
                if let Some((name, arguments)) = repair::tool_call_in_text(&text, &self.tools.names()) {
                    let id = format!("zen_text_call_{}", num_turns);
                    resp.message.content = None;
                    resp.message.tool_calls = Some(vec![ToolCall {
                        id: id.clone(),
                        call_type: "function".into(),
                        function: FunctionCall { name, arguments },
                    }]);
                    text_call_id = Some(id);
                }
            }

            self.messages.push(resp.message.clone());
            self.sink.emit(AgentEvent::Assistant {
                message: resp.message.clone(),
                parent_tool_use_id: self.parent_tool_use_id.clone(),
            });

            let assistant = &resp.message;
            let tool_calls = assistant.tool_calls.clone().unwrap_or_default();
            final_text = assistant.content.clone().unwrap_or_default();

            if tool_calls.is_empty() && final_text.trim().is_empty() && empty_requeries < MAX_EMPTY_REQUERIES {
                // An empty reply (no text, no tool call) is a malformed response, not an answer:
                // drop it and requery with a short correction, the way SWE-agent requeries on a
                // format error (https://swe-agent.com/latest/config/templates/) (r7 C5).
                empty_requeries += 1;
                self.messages.pop();
                self.messages.push(ChatMessage::user(EMPTY_REPLY_NUDGE));
                continue;
            }

            if tool_calls.is_empty() {
                // 4. Stop hooks.
                // Sub-agents fire SubagentStop from the executor instead (SPEC C3.2).
                let stop = if self.config.depth == 0 {
                    self.hooks
                        .run(
                            HookEvent::Stop,
                            "",
                            json!({
                                "stop_hook_active": stop_continues > 0,
                                "last_assistant_message": final_text,
                            }),
                        )
                        .await
                } else {
                    HookOutcome::default()
                };
                self.emit_system_message(HookEvent::Stop, &stop);
                if stop.stop_run {
                    return self.run_result(
                        "success",
                        false,
                        stop.stop_reason.or(stop.feedback).unwrap_or_else(|| final_text.clone()),
                        started.elapsed().as_millis() as u64,
                        duration_api_ms,
                        num_turns,
                    );
                }
                if stop.blocked {
                    // Loop guard: too many consecutive continues means the hook is misbehaving.
                    if stop_continues >= self.stop_hook_max_continues {
                        return self.run_result(
                            "success",
                            false,
                            final_text,
                            started.elapsed().as_millis() as u64,
                            duration_api_ms,
                            num_turns,
                        );
                    }
                    stop_continues += 1;
                    let fb = stop.feedback.unwrap_or_else(|| "continue".into());
                    self.messages.push(ChatMessage::user(fb));
                    continue;
                }
                // Built-in check-before-finish gate (r7), on the same continue path and
                // loop guard as a blocking Stop hook.
                if self.config.depth == 0 && !plan_mode {
                    let decision = finish_gate
                        .decide(
                            self.config.verify_reminder,
                            self.config.verify_command.as_deref(),
                            std::time::Duration::from_secs(self.config.verify_timeout_secs.max(1)),
                            &self.ctx.cwd(),
                            &self.ctx.env,
                        )
                        .await;
                    match decision {
                        finish::Decision::Continue(text)
                            if stop_continues < self.stop_hook_max_continues =>
                        {
                            stop_continues += 1;
                            self.messages.push(ChatMessage::user(text));
                            continue;
                        }
                        finish::Decision::FinishWithNote(text) => {
                            self.sink.emit(AgentEvent::SystemMessage { event: "Verify".into(), text });
                        }
                        _ => {}
                    }
                }
                let result_text = if plan_mode {
                    self.plan_text().unwrap_or_else(|| final_text.clone())
                } else {
                    final_text
                };
                return self.run_result(
                    "success",
                    false,
                    result_text,
                    started.elapsed().as_millis() as u64,
                    duration_api_ms,
                    num_turns,
                );
            }

            // Iterate the tool calls; a stop_run signal mid-iteration finalizes remaining calls.
            // Consecutive parallel-safe read-only calls (Read/Glob/Grep/LS) are prepared in order,
            // executed concurrently, then finished in the original order (perf p0). Any other
            // tool flushes the batch first and runs alone, so writes and Bash stay sequential.
            let mut stop_run: Option<String> = None;
            let mut batch: Vec<Slot> = Vec::new();
            for tc in tool_calls {
                let slot = self.prepare_call(&tc, text_call_id.as_deref(), plan_mode, &stop_run).await;
                if let Slot::Stop(id, content, reason) = slot {
                    batch.push(Slot::Done(id, content, true));
                    stop_run = Some(reason);
                    continue;
                }
                let alone = matches!(&slot, Slot::Run(p) if !p.parallel);
                if alone {
                    let pending = std::mem::take(&mut batch);
                    self.flush_calls(pending, &mut stop_run, &mut finish_gate, &mut turn_guard, &final_text).await;
                    self.flush_calls(vec![slot], &mut stop_run, &mut finish_gate, &mut turn_guard, &final_text).await;
                } else {
                    batch.push(slot);
                }
            }
            self.flush_calls(batch, &mut stop_run, &mut finish_gate, &mut turn_guard, &final_text).await;
            if let Some(text) = stop_run {
                let result_text = if plan_mode {
                    self.plan_text().unwrap_or_else(|| text.clone())
                } else {
                    text
                };
                return self.run_result(
                    "success",
                    false,
                    result_text,
                    started.elapsed().as_millis() as u64,
                    duration_api_ms,
                    num_turns,
                );
            }
        }
    }

    /// Per-call ctx clone carrying the tool_use_id.
    fn call_ctx(&self, tool_use_id: &str, tool_name: &str) -> ToolContext {
        let mut call_ctx = self.ctx.clone();
        call_ctx.current_tool_use_id = Some(tool_use_id.to_string());
        // Bash's default timeout from settings `toolTimeouts.Bash` (p0b).
        if tool_name == "Bash" {
            call_ctx.tool_timeout_ms = self.config.tool_timeouts.get("Bash").copied().filter(|t| *t > 0);
        }
        call_ctx
    }

    /// Everything before execution for one tool call: repair, PreToolUse, unknown tool, plan
    /// mode, permission. Returns the finished result (not yet recorded) or a call ready to run.
    async fn prepare_call(
        &mut self,
        tc: &ToolCall,
        text_call_id: Option<&str>,
        plan_mode: bool,
        stop_run: &Option<String>,
    ) -> Slot {
        let tool_use_id = tc.id.clone();

        // Repair before rejecting (r7): near-miss tool names, fenced / malformed JSON,
        // values of the wrong type. What cannot be repaired becomes a tool error that
        // lists the expected fields, never a crash.
        let mut repair_notes: Vec<String> = Vec::new();
        if text_call_id == Some(tool_use_id.as_str()) {
            repair_notes.push("the call was written as message text; it was run as a tool call".into());
        }
        let mut tool_name = tc.function.name.clone();
        if self.tools.get(&tool_name).is_none() {
            if let Some(mapped) = repair::resolve_tool_name(&tool_name, &self.tools.names()) {
                if repair::has_template_tokens(&tool_name) {
                    // Never echo leaked template tokens back into history (r7 C4).
                    repair_notes.push(format!("tool name had chat-template tokens; mapped to '{}'", mapped));
                } else {
                    repair_notes.push(format!("tool name '{}' mapped to '{}'", tool_name, mapped));
                }
                tool_name = mapped;
            }
        }
        let schema = self.tools.get(&tool_name).map(|t| t.parameters_schema());
        let args: Value = match repair::repair_args(&tc.function.arguments, schema.as_ref()) {
            Ok((v, notes)) => {
                repair_notes.extend(notes);
                v
            }
            Err(e) => {
                let content = format!("Invalid JSON arguments for tool '{}': {}", tool_name, e);
                return Slot::Done(tool_use_id, content, true);
            }
        };
        if !repair_notes.is_empty() {
            // Store the repaired call so the history (and the next request) only ever
            // shows the model well-formed calls to imitate.
            self.rewrite_tool_call(&tool_use_id, &tool_name, &args);
        }

        // A prior stop signal: still record a tool result so the exchange is valid.
        if stop_run.is_some() {
            return Slot::Done(tool_use_id, "[tool call skipped: run stopping]".to_string(), false);
        }

        // 2. PreToolUse hook.
        let pre = self
            .hooks
            .run(
                HookEvent::PreToolUse,
                &tool_name,
                json!({
                    "tool_name": tool_name,
                    "tool_input": args,
                    "tool_use_id": tool_use_id,
                }),
            )
            .await;
        self.emit_system_message(HookEvent::PreToolUse, &pre);
        if pre.stop_run {
            // `continue: false` stops the run; this call still needs a tool message so the
            // history stays a valid OpenAI exchange.
            let reason = pre
                .stop_reason
                .clone()
                .or(pre.feedback.clone())
                .unwrap_or_else(|| "stopped by PreToolUse hook".into());
            let content = format!("[tool call skipped: {}]", reason);
            return Slot::Stop(tool_use_id, content, reason);
        }
        if pre.blocked {
            let content = pre
                .feedback
                .unwrap_or_else(|| format!("Tool '{}' blocked by PreToolUse hook", tool_name));
            return Slot::Done(tool_use_id, content, true);
        }
        // `updatedInput` replaces the arguments for the permission check and the call.
        let args = match pre.updated_input.clone() {
            Some(Value::Object(o)) => Value::Object(o),
            _ => args,
        };

        // Unknown tool.
        let tool = match self.tools.get(&tool_name) {
            Some(t) => t,
            None => {
                let content = format!(
                    "Unknown tool '{}'. Available tools: {}",
                    tool_name,
                    self.tools.names().join(", ")
                );
                return Slot::Done(tool_use_id, content, true);
            }
        };
        let read_only = tool.is_read_only();

        // Plan mode: mutating tools should never reach here (they are hidden), but if a
        // model hallucinates one, report it without recording a denial so the result stays
        // a clean plan.
        if plan_mode && !read_only {
            let content = format!("Tool '{}' is not available in plan mode", tool_name);
            return Slot::Done(tool_use_id, content, true);
        }

        // 3. Permission. The checker (rules + mode) always runs; a PreToolUse decision can
        // tighten it (deny/ask) or skip a prompt (allow), but never loosen a deny
        // (SPEC C2.13).
        let checked = self.permissions.check(&tool_name, &args, read_only, &self.ctx);
        let ask_rule = matches!(checked, PermissionDecision::Ask)
            && self.permissions.ask_rule_matches(&tool_name, &args, &self.ctx);
        let decision = combine_permission(checked, pre.permission_override.clone(), ask_rule);
        let mut allowed = true;
        let mut deny_reason = String::new();
        match decision {
            PermissionDecision::Allow => {}
            PermissionDecision::Deny(reason) => {
                allowed = false;
                deny_reason = reason;
            }
            PermissionDecision::Ask => match self.prompter.clone() {
                Some(p) => {
                    self.notify_permission_prompt(&tool_name).await;
                    match p.ask_permission(&tool_name, &args) {
                        PromptAnswer::Yes => {}
                        // One rule per line: a compound Bash command yields one
                        // rule per sub-command (SPEC C2.12).
                        PromptAnswer::Always(rules) => {
                            for rule in rules.lines() {
                                self.permissions.add_allow_rule(rule)
                            }
                        }
                        PromptAnswer::No => {
                            allowed = false;
                            deny_reason = format!("User denied permission for '{}'", tool_name);
                        }
                    }
                }
                None => {
                    allowed = false;
                    deny_reason =
                        format!("Permission required for '{}' in non-interactive mode", tool_name);
                }
            },
        }
        if !allowed {
            self.record_denial(&tool_name, &tool_use_id, &args);
            return Slot::Done(tool_use_id, deny_reason, true);
        }
        let parallel = read_only && is_parallel_safe(&tool_name);
        Slot::Run(PendingCall { tool_use_id, tool_name, args, repair_notes, tool, parallel })
    }

    /// Run the prepared calls of one batch and record every result in order. With more than
    /// one runnable call they execute concurrently (only parallel-safe read-only calls are ever
    /// batched together). A PostToolUse stop skips the results of the calls after it.
    async fn flush_calls(
        &mut self,
        slots: Vec<Slot>,
        stop_run: &mut Option<String>,
        finish_gate: &mut finish::FinishGate,
        turn_guard: &mut guard::TurnGuard,
        final_text: &str,
    ) {
        if slots.is_empty() {
            return;
        }
        let runnable = slots.iter().filter(|s| matches!(s, Slot::Run(_))).count();
        let mut outs: Vec<Option<ToolOutput>> = Vec::with_capacity(slots.len());
        if runnable > 1 {
            let mut handles = Vec::with_capacity(slots.len());
            for s in &slots {
                handles.push(match s {
                    Slot::Run(p) => {
                        let tool = p.tool.clone();
                        let args = p.args.clone();
                        let ctx = self.call_ctx(&p.tool_use_id, &p.tool_name);
                        let timeout = self.config.tool_timeout_ms(&p.tool_name);
                        Some(tokio::spawn(run_tool(tool, args, ctx, timeout)))
                    }
                    _ => None,
                });
            }
            for h in handles {
                outs.push(match h {
                    Some(h) => Some(
                        h.await
                            .unwrap_or_else(|e| ToolOutput::err(format!("tool task failed: {}", e))),
                    ),
                    None => None,
                });
            }
        }
        let mut skip = false;
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Slot::Done(id, content, is_error) => {
                    self.emit_tool_result(&id, &content, is_error);
                    self.messages.push(ChatMessage::tool(id, content));
                }
                Slot::Stop(id, content, _) => {
                    self.emit_tool_result(&id, &content, true);
                    self.messages.push(ChatMessage::tool(id, content));
                }
                Slot::Run(p) => {
                    if skip {
                        let content = "[tool call skipped: run stopping]".to_string();
                        self.emit_tool_result(&p.tool_use_id, &content, false);
                        self.messages.push(ChatMessage::tool(p.tool_use_id, content));
                        continue;
                    }
                    let out = match outs.get_mut(i).and_then(Option::take) {
                        Some(o) => o,
                        None => {
                            let ctx = self.call_ctx(&p.tool_use_id, &p.tool_name);
                            let timeout = self.config.tool_timeout_ms(&p.tool_name);
                            run_tool(p.tool.clone(), p.args.clone(), ctx, timeout).await
                        }
                    };
                    if let Some(reason) = self.finish_call(p, out, finish_gate, turn_guard, final_text).await {
                        *stop_run = Some(reason);
                        skip = true;
                    }
                }
            }
        }
    }

    /// PostToolUse (or PostToolUseFailure), finish gate, loop guard, repair note; records the
    /// result. Returns the stop reason when a PostToolUse hook stops the run.
    async fn finish_call(
        &mut self,
        p: PendingCall,
        mut out: ToolOutput,
        finish_gate: &mut finish::FinishGate,
        turn_guard: &mut guard::TurnGuard,
        final_text: &str,
    ) -> Option<String> {
        let PendingCall { tool_use_id, tool_name, args, repair_notes, .. } = p;
        // 4. PostToolUse (or PostToolUseFailure when the tool reported an error).
        let post_event = if out.is_error {
            HookEvent::PostToolUseFailure
        } else {
            HookEvent::PostToolUse
        };
        let mut post_payload = json!({
            "tool_name": tool_name,
            "tool_input": args,
            "tool_use_id": tool_use_id,
        });
        if out.is_error {
            post_payload["error"] = json!(out.content);
        } else {
            post_payload["tool_response"] = json!(out.content);
        }
        let post = self.hooks.run(post_event, &tool_name, post_payload).await;
        self.emit_system_message(post_event, &post);
        if let Some(replaced) = post.updated_output.clone() {
            out.content = replaced;
        }
        if let Some(fb) = post.feedback.clone() {
            if !fb.is_empty() {
                out.content.push('\n');
                out.content.push_str(&fb);
            }
        }
        let verify = self.verify_after_edit(&tool_name, out.is_error).await;
        if let Some(Some(note)) = &verify {
            out.content.push_str("\n\n");
            out.content.push_str(note);
        }
        finish_gate.observe(&tool_name, args.get("file_path").and_then(|v| v.as_str()), out.is_error);
        if verify.is_some() {
            // The harness just checked this change; no "run your checks" reminder for it.
            finish_gate.observe("Bash", None, false);
        }
        if let Some(note) = turn_guard.observe(&tool_name, &args, &out) {
            out.content.push_str("\n\n");
            out.content.push_str(&note);
        }
        if tool_name == "TodoWrite" && !out.is_error {
            self.model_calls_since_todo = 0;
        }
        if !repair_notes.is_empty() {
            out.content.push_str(&format!(
                "\n[zen: your tool call was repaired before running: {}. Send well-formed calls next time.]",
                repair_notes.join("; ")
            ));
        }
        self.emit_tool_result(&tool_use_id, &out.content, out.is_error);
        self.messages.push(ChatMessage::tool(tool_use_id, out.content));
        if post.stop_run {
            Some(post.stop_reason.unwrap_or_else(|| final_text.to_string()))
        } else {
            None
        }
    }

    /// Run `verify_after_edit` after a successful file change. `None` when it did not run,
    /// `Some(None)` when it passed, `Some(Some(note))` when it failed.
    async fn verify_after_edit(&self, tool_name: &str, is_error: bool) -> Option<Option<String>> {
        let cmd = self.config.verify_after_edit.as_deref()?;
        if is_error || !matches!(tool_name, "Edit" | "MultiEdit" | "Write") {
            return None;
        }
        let timeout = std::time::Duration::from_secs(self.config.verify_after_edit_timeout_secs.max(1));
        let (passed, status, tail) = finish::run_command(cmd, &self.ctx.cwd(), &self.ctx.env, timeout).await;
        if passed {
            return Some(None);
        }
        Some(Some(format!(
            "[zen: verify `{}` failed ({}) after this change. Last lines of output:\n{}]",
            cmd, status, tail
        )))
    }

    /// Every [`guard::TODO_REINJECT_EVERY`] model calls, append the non-empty todo list to the
    /// newest tool result / user message so a long turn keeps its plan in view.
    fn maybe_reinject_todos(&mut self) {
        self.model_calls_since_todo += 1;
        if self.model_calls_since_todo < guard::TODO_REINJECT_EVERY {
            return;
        }
        let todos = self.ctx.todos.lock().unwrap().clone();
        let Some(note) = guard::todo_reminder(&todos) else { return };
        if let Some(last) = self.messages.last_mut() {
            if last.role == "tool" || last.role == "user" {
                let content = last.content.get_or_insert_with(String::new);
                content.push_str("\n\n");
                content.push_str(&note);
                self.model_calls_since_todo = 0;
            }
        }
    }

    /// Point the stored assistant tool_call `id` at the repaired name and arguments.
    fn rewrite_tool_call(&mut self, id: &str, name: &str, args: &Value) {
        let Some(msg) = self.messages.iter_mut().rev().find(|m| m.role == "assistant") else { return };
        if let Some(call) = msg.tool_calls.iter_mut().flatten().find(|c| c.id == id) {
            call.function.name = name.to_string();
            call.function.arguments = args.to_string();
        }
    }

    /// Replace the history with a model-written summary plus the trailing messages
    /// (SPEC §11). `trigger` is `"manual"` or `"auto"`; `focus` steers the summary.
    pub async fn compact(&mut self, focus: Option<&str>, trigger: &str) -> anyhow::Result<()> {
        compaction::compact_impl(self, focus, trigger).await
    }
}

/// Tools whose read-only calls may run concurrently within one assistant message (perf p0).
/// Anything not listed (Bash, edits, Task, MCP, TodoWrite, Skill) runs alone, in order.
pub(crate) fn is_parallel_safe(tool_name: &str) -> bool {
    matches!(tool_name, "Read" | "Glob" | "Grep" | "LS")
}

/// A tool call ready to execute (repaired, hooked, permitted).
struct PendingCall {
    tool_use_id: String,
    tool_name: String,
    args: Value,
    repair_notes: Vec<String>,
    tool: Arc<dyn Tool>,
    parallel: bool,
}

/// One tool call of an assistant message after [`Agent::prepare_call`].
enum Slot {
    /// Result known without running the tool: `(id, content, is_error)`.
    Done(String, String, bool),
    /// A PreToolUse hook stopped the run: `(id, content, reason)`.
    Stop(String, String, String),
    /// Ready to run.
    Run(PendingCall),
}

/// Merge the rule/mode decision with a PreToolUse hook's `permissionDecision` (SPEC C2.13).
///
/// - a rule **deny** always wins (a hook cannot unlock a denied tool);
/// - a hook **deny** or **ask** tightens anything else;
/// - a hook **allow** turns a mode-default would-ask into allow, but never a deny or an
///   explicit ask rule (`ask_rule`).
pub(crate) fn combine_permission(
    checked: PermissionDecision,
    hook: Option<PermissionDecision>,
    ask_rule: bool,
) -> PermissionDecision {
    match (checked, hook) {
        (PermissionDecision::Deny(r), _) => PermissionDecision::Deny(r),
        (_, Some(PermissionDecision::Deny(r))) => PermissionDecision::Deny(r),
        (_, Some(PermissionDecision::Ask)) => PermissionDecision::Ask,
        // A hook `allow` skips a mode-default prompt, but an explicit ask rule still asks.
        (PermissionDecision::Ask, Some(PermissionDecision::Allow)) if !ask_rule => {
            PermissionDecision::Allow
        }
        (c, _) => c,
    }
}
