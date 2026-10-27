use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use zen_core::*;

use crate::budget::{BudgetPolicy, BudgetVerdict};
use crate::tool_bundles::ToolBundle;
use crate::compaction;
use crate::prune::{PruneConfig, PruneStats};
use crate::finish;

const MAX_EMPTY_REQUERIES: usize = 2;

const UNPARSED_REPLY_NUDGE: &str = "[zen: the server could not parse your last reply. Continue the task: make one tool call with plain JSON arguments, or give your final answer.]";

fn is_unparsed_sample(err: &str) -> bool {
    err.contains("model returned no choices") || err.contains("gateway error in 200 response")
}

pub const TRUNCATION_CONTINUE_NUDGE: &str = "[zen: your last reply was cut off by the output token limit. Continue exactly where you stopped, without repeating anything you already wrote.]";

pub const TRUNCATED_CALL_NUDGE: &str = "[zen: your last reply was cut off by the output token limit in the middle of a tool call, so no tool was run. Re-emit the tool call(s) in full. If the arguments are large, split the work into several smaller calls (e.g. write a big file in parts).]";

pub const DEFAULT_MAX_CONTINUATIONS: usize = 3;

pub fn is_truncated(finish_reason: Option<&str>) -> bool {
    finish_reason.is_some_and(|r| {
        let r = r.to_ascii_lowercase();
        r == "length" || r == "max_tokens" || r == "max_output_tokens"
    })
}

pub fn content_filter_message(partial: &str) -> String {
    let mut m = String::from(
        "Blocked by the provider's content filter (finish_reason: content_filter). The model's reply was stopped; ZenCode did not retry. Rephrase the request or use another model.",
    );
    if !partial.trim().is_empty() {
        m.push_str(" Partial reply: ");
        m.push_str(&partial.chars().take(300).collect::<String>());
    }
    m
}

const EMPTY_REPLY_NUDGE: &str =
    "[zen: your last reply was empty. Continue the task: call a tool, or give your final answer.]";
use crate::guard;
use crate::plan;
use crate::repair;

pub const DEFAULT_STOP_HOOK_CONTINUES: usize = 8;

#[derive(Debug, Clone)]
pub struct AgentConfig {

    pub model: String,

    pub fallback_models: Vec<String>,

    pub system_prompt: String,

    pub max_turns: usize,

    pub budget: Option<BudgetPolicy>,

    pub permission_mode: PermissionMode,

    pub context_window: u64,

    pub auto_compact_threshold: Option<u64>,

    pub keep_after_compact: usize,

    pub depth: usize,

    pub verify_reminder: bool,

    pub verify_command: Option<String>,

    pub verify_timeout_secs: u64,

    pub verify_after_edit: Option<String>,

    pub verify_after_edit_timeout_secs: u64,

    pub tool_timeouts: std::collections::HashMap<String, u64>,

    pub max_continuations: usize,

    pub prune: PruneConfig,

    pub max_output_tokens: Option<u32>,

    pub tool_bundle: ToolBundle,

    pub max_parallel_tool_calls: usize,

    pub done_scope: Vec<String>,
}

pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = 300_000;

pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 4;

impl AgentConfig {

    pub fn tool_timeout_ms(&self, tool: &str) -> Option<u64> {
        self.tool_timeout_with_source(tool).0
    }

    pub fn tool_timeout_with_source(&self, tool: &str) -> (Option<u64>, TimeoutSource) {
        if tool == "Bash" {
            return (None, TimeoutSource::None);
        }
        match self.tool_timeouts.get(tool) {
            Some(t) => ((t > &0).then_some(*t), TimeoutSource::PerTool),

            None if matches!(tool, "Task" | "JobOutput") => (None, TimeoutSource::None),
            None => match self.tool_timeouts.get("default") {
                Some(t) => ((t > &0).then_some(*t), TimeoutSource::Default),
                None => (Some(DEFAULT_TOOL_TIMEOUT_MS), TimeoutSource::BuiltIn),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutSource {

    PerTool,

    Default,

    BuiltIn,

    None,
}

async fn run_tool(
    tool: Arc<dyn Tool>,
    args: Value,
    ctx: ToolContext,
    timeout: (Option<u64>, TimeoutSource),
) -> ToolOutput {
    let (timeout_ms, source) = timeout;
    let Some(ms) = timeout_ms else { return tool.call(args, &ctx).await };

    let src = match source {
        TimeoutSource::PerTool => format!("settings toolTimeouts.{}", tool.name()),
        TimeoutSource::Default => "settings toolTimeouts.default".to_string(),
        TimeoutSource::BuiltIn => "the built-in default timeout".to_string(),
        TimeoutSource::None => "a timeout that was then disabled".to_string(),
    };
    match tokio::time::timeout(std::time::Duration::from_millis(ms), tool.call(args, &ctx)).await {
        Ok(out) => out,
        Err(_) => ToolOutput::err(format!(
            "Error: {} timed out after {} ms and was stopped ({})",
            tool.name(),
            ms,
            src
        )),
    }
}

const TOOL_PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_with_heartbeat(
    tool: Arc<dyn Tool>,
    args: Value,
    ctx: ToolContext,
    timeout_ms: (Option<u64>, TimeoutSource),
    sink: Arc<dyn EventSink>,
    tool_use_id: String,
    tool_name: String,
    interval: std::time::Duration,
) -> ToolOutput {
    let hb_sink = sink.clone();
    let start = std::time::Instant::now();
    let heartbeat = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);

        ticker.tick().await;
        loop {
            ticker.tick().await;
            hb_sink.emit(AgentEvent::ToolProgress {
                tool_use_id: tool_use_id.clone(),
                tool_name: tool_name.clone(),
                elapsed_ms: start.elapsed().as_millis() as u64,
            });
        }
    });
    let out = run_tool(tool, args, ctx, timeout_ms).await;
    heartbeat.abort();
    out
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            model: "google/gemini-4-argon:free".into(),
            fallback_models: vec![],
            system_prompt: String::new(),
            max_turns: 50,
            budget: None,
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
            max_continuations: DEFAULT_MAX_CONTINUATIONS,
            prune: PruneConfig::default(),
            max_output_tokens: None,
            tool_bundle: ToolBundle::All,
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            done_scope: vec![],
        }
    }
}

pub struct Agent {

    pub llm: Arc<dyn LlmBackend>,

    pub tools: ToolRegistry,

    pub permissions: Arc<dyn PermissionChecker>,

    pub prompter: Option<Arc<dyn Prompter>>,

    pub hooks: Arc<dyn HookRunner>,

    pub sink: Arc<dyn EventSink>,

    pub ctx: ToolContext,

    pub config: AgentConfig,

    pub messages: Vec<ChatMessage>,

    pub usage: UsageStats,

    pub denials: Vec<PermissionDenial>,

    pub parent_tool_use_id: Option<String>,

    pub last_prompt_tokens: u64,

    pub stop_hook_max_continues: usize,

    pub budget_warned: bool,

    pub model_calls_since_todo: usize,

    pub journal: Option<Arc<dyn ToolJournal>>,

    pub prune_turn_starts: Vec<usize>,

    pub prune_stats: PruneStats,
}

impl Agent {

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
            budget_warned: false,
            model_calls_since_todo: 0,
            journal: None,
            prune_turn_starts: vec![0],
            prune_stats: PruneStats::default(),
        }
    }

    pub fn set_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;

        self.last_prompt_tokens = 0;

        self.prune_turn_starts = vec![0];
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn set_stop_hook_max_continues(&mut self, n: usize) {
        self.stop_hook_max_continues = n.max(1);
    }

    pub fn tools_for_request(&self) -> Vec<ToolDefinition> {
        let defs = crate::tool_bundles::definitions_for_bundle(&self.tools, self.config.tool_bundle);
        if self.config.permission_mode == PermissionMode::Plan {
            plan::plan_mode_tools(defs)
        } else {
            defs
        }
    }

    fn request_messages(&self) -> Vec<ChatMessage> {
        let mut msgs: Vec<ChatMessage> = Vec::with_capacity(self.messages.len() + 1);
        if !self.config.system_prompt.is_empty() {
            msgs.push(ChatMessage::system(self.config.system_prompt.clone()));
        }
        msgs.extend(self.messages.iter().cloned());
        crate::plan::ensure_tool_responses(msgs)
    }

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

            total_cost_usd: crate::budget::estimate_cost_usd(
                &self.config.model,
                self.usage.input_tokens,
                self.usage.output_tokens,
            )
            .unwrap_or(0.0),
            usage: self.usage.clone(),
            model: self.config.model.clone(),
            permission_denials: self.denials.clone(),
        }
    }

    fn record_denial(&mut self, tool_name: &str, tool_use_id: &str, input: &Value) {
        let denial = PermissionDenial {
            tool_name: tool_name.to_string(),
            tool_use_id: tool_use_id.to_string(),
            tool_input: input.clone(),
        };
        self.sink.emit(AgentEvent::PermissionDenied(denial.clone()));
        self.denials.push(denial);
    }

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

    fn emit_tool_result(&self, tool_use_id: &str, content: &str, is_error: bool, parallel: bool) {
        self.sink.emit(AgentEvent::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error,
            parent_tool_use_id: self.parent_tool_use_id.clone(),
            parallel,
        });
    }

    fn journal_result(&self, tool_use_id: &str, content: &str, is_error: bool, final_result: bool) {
        if let Some(j) = &self.journal {
            j.tool_result(tool_use_id, content, is_error, final_result);
        }
    }

    fn plan_text(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();

        let mut glue = false;
        for m in self.messages.iter().rev() {
            if m.role == "user" {
                if m.content.as_deref() == Some(TRUNCATION_CONTINUE_NUDGE) {
                    glue = true;
                    continue;
                }
                break;
            }
            if m.role == "assistant" {
                if let Some(c) = &m.content {
                    if glue && !parts.is_empty() {
                        let last = parts.last_mut().unwrap();
                        last.insert_str(0, c);
                    } else if !c.trim().is_empty() {
                        parts.push(c.clone());
                    }
                    glue = false;
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

    pub async fn run_turn(&mut self, prompt: &str) -> RunResult {
        let started = Instant::now();
        let mut duration_api_ms: u64 = 0;

        let ups = self
            .hooks
            .run(HookEvent::UserPromptSubmit, "", json!({ "prompt": prompt }))
            .await;
        self.emit_system_message(HookEvent::UserPromptSubmit, &ups);
        if ups.stop_run {

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

        let mut stop_continues: usize = 0;
        let plan_mode = self.config.permission_mode == PermissionMode::Plan;
        let mut turn_guard = guard::TurnGuard::default();
        turn_guard.set_cwd(self.ctx.cwd.lock().unwrap().clone());
        turn_guard.set_scope(self.config.done_scope.clone());
        let mut finish_gate = finish::FinishGate::default();
        let mut empty_requeries = 0usize;

        let mut continuations = 0usize;
        let mut stitched = String::new();

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

            if let Some(threshold) = self.config.auto_compact_threshold {
                let used = compaction::context_tokens(self);
                if used > threshold && !self.messages.is_empty() {
                    let _ = compaction::compact_impl(self, None, "auto").await;
                }
            }

            self.prune_turn_starts.push(self.messages.len());
            self.prune_old_results();

            num_turns += 1;
            if self.config.depth == 0 {
                self.maybe_reinject_todos();
            }
            let messages = self.request_messages();
            let tools = self.tools_for_request();

            if let Some(policy) = self.config.budget.clone() {
                let model = self.config.model.clone();
                if !crate::budget::is_free_model(&model) {
                    let spent = crate::budget::estimate_cost_usd(
                        &model,
                        self.usage.input_tokens,
                        self.usage.output_tokens,
                    );
                    let est_next = crate::budget::estimate_cost_usd(
                        &model,
                        crate::compaction::context_tokens(self),
                        crate::budget::DEFAULT_OUTPUT_ESTIMATE_TOKENS,
                    );
                    if let (Some(spent_usd), Some(est_cost_usd)) = (spent, est_next) {
                        match crate::budget::check_budget(
                            &policy,
                            spent_usd,
                            est_cost_usd,
                            self.budget_warned,
                        ) {
                            BudgetVerdict::Allow => {}
                            BudgetVerdict::Warn { spent_usd, cap_usd } => {
                                self.budget_warned = true;
                                self.sink.emit(AgentEvent::SystemMessage {
                                    event: "budget".to_string(),
                                    text: crate::budget::warn_message(spent_usd, cap_usd),
                                });
                            }
                            BudgetVerdict::Block {
                                spent_usd,
                                cap_usd,
                                est_cost_usd,
                            } => {
                                return self.run_result(
                                    "error_budget_exceeded",
                                    true,
                                    crate::budget::block_message(
                                        spent_usd, cap_usd, est_cost_usd,
                                    ),
                                    started.elapsed().as_millis() as u64,
                                    duration_api_ms,
                                    num_turns,
                                );
                            }
                        }
                    }
                }
            }

            let (resp, api_ms, _model) = match
                crate::client::chat_with_fallback(
                    &self.llm,
                    &self.config.model,
                    &self.config.fallback_models,
                    messages,
                    tools,
                    self.config.max_output_tokens,
                )
                .await
            {
                Ok(v) => v,
                Err(e) if is_unparsed_sample(&e.to_string()) && empty_requeries < MAX_EMPTY_REQUERIES => {

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
            self.usage.cache_read_input_tokens += resp.usage.cache_read_input_tokens;
            self.last_prompt_tokens = resp.usage.prompt_tokens;

            let mut resp = resp;

            if crate::client::is_content_filtered(resp.finish_reason.as_deref())
                && resp.message.tool_calls.as_ref().is_none_or(|c| c.is_empty())
            {
                let text = resp.message.content.clone().unwrap_or_default();
                if !text.trim().is_empty() {
                    self.messages.push(resp.message.clone());
                    self.sink.emit(AgentEvent::Assistant {
                        message: resp.message.clone(),
                        parent_tool_use_id: self.parent_tool_use_id.clone(),
                    });
                    if let Some(j) = &self.journal {
                        j.assistant(&resp.message);
                    }
                }
                return self.run_result(
                    "error_during_execution",
                    true,
                    content_filter_message(&text),
                    started.elapsed().as_millis() as u64,
                    duration_api_ms,
                    num_turns,
                );
            }
            if is_truncated(resp.finish_reason.as_deref()) {
                let calls = resp.message.tool_calls.clone().unwrap_or_default();
                let broken = calls
                    .iter()
                    .any(|c| serde_json::from_str::<Value>(&c.function.arguments).map_or(true, |v| !v.is_object()));
                if calls.is_empty() || broken {
                    let mut msg = resp.message.clone();
                    msg.tool_calls = None;
                    let text = msg.content.clone().unwrap_or_default();

                    if !text.is_empty() {
                        self.messages.push(msg.clone());
                        self.sink.emit(AgentEvent::Assistant {
                            message: msg.clone(),
                            parent_tool_use_id: self.parent_tool_use_id.clone(),
                        });
                        if let Some(j) = &self.journal {
                            j.assistant(&msg);
                        }
                    }
                    stitched.push_str(&text);
                    if continuations >= self.config.max_continuations {
                        let elapsed = started.elapsed().as_millis() as u64;
                        if broken {
                            return self.run_result(
                                "error_during_execution",
                                true,
                                format!(
                                    "The model's tool call was cut off by the output token limit {} time(s) in a row; no tool was run. Ask for smaller steps or use a model with a larger output limit.",
                                    continuations + 1
                                ),
                                elapsed,
                                duration_api_ms,
                                num_turns,
                            );
                        }
                        self.sink.emit(AgentEvent::SystemMessage {
                            event: "Truncation".into(),
                            text: format!(
                                "The reply was cut off by the output token limit {} time(s) in a row; the answer may be incomplete.",
                                continuations + 1
                            ),
                        });
                        return self.run_result("success", false, stitched, elapsed, duration_api_ms, num_turns);
                    }
                    continuations += 1;
                    let nudge = if broken { TRUNCATED_CALL_NUDGE } else { TRUNCATION_CONTINUE_NUDGE };
                    self.messages.push(ChatMessage::user(nudge));
                    continue;
                }
            }
            continuations = 0;

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

                empty_requeries += 1;
                self.messages.pop();
                self.messages.push(ChatMessage::user(EMPTY_REPLY_NUDGE));
                continue;
            }
            if let Some(j) = &self.journal {
                j.assistant(assistant);
            }

            if !stitched.is_empty() {
                let pieces = std::mem::take(&mut stitched);
                if tool_calls.is_empty() {
                    final_text.insert_str(0, &pieces);
                }
            }

            if tool_calls.is_empty() {

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

    fn call_ctx(&self, tool_use_id: &str, tool_name: &str) -> ToolContext {
        let mut call_ctx = self.ctx.clone();
        call_ctx.current_tool_use_id = Some(tool_use_id.to_string());

        if tool_name == "Bash" {
            call_ctx.tool_timeout_ms = self.config.tool_timeouts.get("Bash").copied().filter(|t| *t > 0);
        }
        call_ctx
    }

    async fn prepare_call(
        &mut self,
        tc: &ToolCall,
        text_call_id: Option<&str>,
        plan_mode: bool,
        stop_run: &Option<String>,
    ) -> Slot {
        let tool_use_id = tc.id.clone();

        let mut repair_notes: Vec<String> = Vec::new();
        if text_call_id == Some(tool_use_id.as_str()) {
            repair_notes.push("the call was written as message text; it was run as a tool call".into());
        }
        let mut tool_name = tc.function.name.clone();
        if self.tools.get(&tool_name).is_none() {
            if let Some(mapped) = repair::resolve_tool_name(&tool_name, &self.tools.names()) {
                if repair::has_template_tokens(&tool_name) {

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

            self.rewrite_tool_call(&tool_use_id, &tool_name, &args);
        }

        if stop_run.is_some() {
            return Slot::Done(tool_use_id, "[tool call skipped: run stopping]".to_string(), false);
        }

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

        let args = match pre.updated_input.clone() {
            Some(Value::Object(o)) => Value::Object(o),
            _ => args,
        };

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

        if crate::tool_bundles::is_deferred(&self.tools, self.config.tool_bundle, &tool_name) {
            let content = format!(
                "Tool '{}' is deferred in tool bundle '{}': its schema was not sent to save context. \
                 Do not call it; use a tool from the active bundle instead.",
                tool_name,
                self.config.tool_bundle.as_str()
            );
            return Slot::Done(tool_use_id, content, true);
        }
        let read_only = tool.is_read_only();

        if plan_mode && !read_only {
            let content = format!("Tool '{}' is not available in plan mode", tool_name);
            return Slot::Done(tool_use_id, content, true);
        }

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
        let mut outs: Vec<Option<ToolOutput>> = (0..slots.len()).map(|_| None).collect();

        let mut ran_parallel = vec![false; slots.len()];
        if runnable > 1 {

            let limit = self.config.max_parallel_tool_calls.max(1);
            let run_idx: Vec<usize> = slots
                .iter()
                .enumerate()
                .filter(|(_, s)| matches!(s, Slot::Run(_)))
                .map(|(i, _)| i)
                .collect();
            for chunk in run_idx.chunks(limit) {
                let mut handles = Vec::with_capacity(chunk.len());
                for &i in chunk {
                    let Slot::Run(p) = &slots[i] else { continue };
                    let tool = p.tool.clone();
                    let args = p.args.clone();
                    let ctx = self.call_ctx(&p.tool_use_id, &p.tool_name);
                    let timeout = self.config.tool_timeout_with_source(&p.tool_name);
                    let journal = self.journal.clone();
                    let id = p.tool_use_id.clone();
                    let sink = self.sink.clone();
                    let tool_name = p.tool_name.clone();
                    handles.push((
                        i,
                        tokio::spawn(async move {
                            let out = run_tool_with_heartbeat(tool, args, ctx, timeout, sink, id.clone(), tool_name, TOOL_PROGRESS_INTERVAL).await;

                            if let Some(j) = journal {
                                j.tool_result(&id, &out.content, out.is_error, false);
                            }
                            out
                        }),
                    ));
                }
                if chunk.len() > 1 {
                    for &i in chunk {
                        ran_parallel[i] = true;
                    }
                }
                for (i, h) in handles {
                    outs[i] = Some(
                        h.await
                            .unwrap_or_else(|e| ToolOutput::err(format!("tool task failed: {}", e))),
                    );
                }
            }
        }
        let mut skip = false;
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Slot::Done(id, content, is_error) => {
                    self.journal_result(&id, &content, is_error, true);
                    self.emit_tool_result(&id, &content, is_error, false);
                    self.messages.push(ChatMessage::tool(id, content));
                }
                Slot::Stop(id, content, _) => {
                    self.journal_result(&id, &content, true, true);
                    self.emit_tool_result(&id, &content, true, false);
                    self.messages.push(ChatMessage::tool(id, content));
                }
                Slot::Run(mut p) => {
                    if skip {
                        let content = "[tool call skipped: run stopping]".to_string();
                        self.journal_result(&p.tool_use_id, &content, false, true);
                        self.emit_tool_result(&p.tool_use_id, &content, false, false);
                        self.messages.push(ChatMessage::tool(p.tool_use_id, content));
                        continue;
                    }

                    p.parallel = ran_parallel[i];
                    let out = match outs.get_mut(i).and_then(Option::take) {
                        Some(o) => o,
                        None => {
                            let ctx = self.call_ctx(&p.tool_use_id, &p.tool_name);
                            let timeout = self.config.tool_timeout_with_source(&p.tool_name);
                            let out = run_tool_with_heartbeat(
                                p.tool.clone(),
                                p.args.clone(),
                                ctx,
                                timeout,
                                self.sink.clone(),
                                p.tool_use_id.clone(),
                                p.tool_name.clone(),
                                TOOL_PROGRESS_INTERVAL,
                            )
                            .await;
                            self.journal_result(&p.tool_use_id, &out.content, out.is_error, false);
                            out
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

    async fn finish_call(
        &mut self,
        p: PendingCall,
        mut out: ToolOutput,
        finish_gate: &mut finish::FinishGate,
        turn_guard: &mut guard::TurnGuard,
        final_text: &str,
    ) -> Option<String> {
        let PendingCall { tool_use_id, tool_name, args, repair_notes, parallel, .. } = p;

        let raw = (self.journal.is_some()).then(|| out.content.clone());

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
        if raw.as_deref() != Some(out.content.as_str()) {
            self.journal_result(&tool_use_id, &out.content, out.is_error, true);
        }

        if self.config.prune.enabled {
            let max = self.config.prune.max_result_tokens;
            if crate::prune::est_tokens(&out.content) > max {
                let label = crate::prune::tool_call_label(
                    &tool_name,
                    &serde_json::to_string(&args).unwrap_or_default(),
                );
                let (capped, saved) = crate::prune::cap_content(&out.content, &label, max);
                out.content = capped;
                self.prune_stats.capped += 1;
                self.prune_stats.capped_tokens += saved;
            }
        }
        self.emit_tool_result(&tool_use_id, &out.content, out.is_error, parallel);
        self.messages.push(ChatMessage::tool(tool_use_id, out.content));
        if post.stop_run {
            Some(post.stop_reason.unwrap_or_else(|| final_text.to_string()))
        } else {
            None
        }
    }

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

    fn rewrite_tool_call(&mut self, id: &str, name: &str, args: &Value) {
        let Some(msg) = self.messages.iter_mut().rev().find(|m| m.role == "assistant") else { return };
        if let Some(call) = msg.tool_calls.iter_mut().flatten().find(|c| c.id == id) {
            call.function.name = name.to_string();
            call.function.arguments = args.to_string();
        }
    }

    fn prune_old_results(&mut self) {
        if !self.config.prune.enabled {
            return;
        }
        let current_turn = self.prune_turn_starts.len().saturating_sub(1);
        let min_age = self.config.prune.min_age();

        let mut protected = self.plan_text().unwrap_or_default();
        protected.push('\n');
        for (i, m) in self.messages.iter().enumerate() {
            let age =
                current_turn.saturating_sub(crate::prune::turn_of(&self.prune_turn_starts, i));
            if age >= min_age {
                continue;
            }
            if let Some(calls) = &m.tool_calls {
                for c in calls {
                    if let Some(id) = crate::prune::extract_identifier(
                        &c.function.name,
                        &c.function.arguments,
                    ) {
                        protected.push_str(&id);
                        protected.push('\n');
                    }
                }
            }
        }
        let stats = crate::prune::prune_messages(
            &mut self.messages,
            &self.prune_turn_starts,
            current_turn,
            &self.config.prune,
            &protected,
        );
        if stats.pruned > 0 {
            self.prune_stats.pruned += stats.pruned;
            self.prune_stats.pruned_tokens += stats.pruned_tokens;
            self.sink.emit(AgentEvent::Pruned {
                pruned: stats.pruned,
                tokens_saved: stats.pruned_tokens,
            });
        }
    }

    pub fn prune_stats(&self) -> &PruneStats {
        &self.prune_stats
    }

    pub async fn compact(&mut self, focus: Option<&str>, trigger: &str) -> anyhow::Result<()> {
        compaction::compact_impl(self, focus, trigger).await
    }
}

pub(crate) fn is_parallel_safe(tool_name: &str) -> bool {
    matches!(tool_name, "Read" | "Glob" | "Grep" | "LS")
}

struct PendingCall {
    tool_use_id: String,
    tool_name: String,
    args: Value,
    repair_notes: Vec<String>,
    tool: Arc<dyn Tool>,
    parallel: bool,
}

enum Slot {

    Done(String, String, bool),

    Stop(String, String, String),

    Run(PendingCall),
}

pub(crate) fn combine_permission(
    checked: PermissionDecision,
    hook: Option<PermissionDecision>,
    ask_rule: bool,
) -> PermissionDecision {
    match (checked, hook) {
        (PermissionDecision::Deny(r), _) => PermissionDecision::Deny(r),
        (_, Some(PermissionDecision::Deny(r))) => PermissionDecision::Deny(r),
        (_, Some(PermissionDecision::Ask)) => PermissionDecision::Ask,

        (PermissionDecision::Ask, Some(PermissionDecision::Allow)) if !ask_rule => {
            PermissionDecision::Allow
        }
        (c, _) => c,
    }
}
