//! # zen-agent
//!
//! The agent loop, the OpenAI-compatible HTTP client, context compaction and the sub-agent
//! runner for ZenCode.
//!
//! `zen-agent` is the *mechanism*: it does not know about CLI flags, settings JSON or files on
//! disk. Those concerns live in `zen-ext` and `zen-cli`; this crate only consumes the shared
//! traits in `zen-core` ([`zen_core::LlmBackend`], [`zen_core::ToolRegistry`], the permission /
//! hooks / event-callback traits) and produces a [`zen_core::RunResult`]. That split lets the CLI
//! run the same loop headless (streaming JSON) and interactively (REPL) and lets the test suite
//! swap in scripted backends without touching HTTP.
//!
//! ## Module layout
//!
//! - [`client`] — `OpenAiClient`: POSTs `{base_url}/chat/completions`, retries 429/5xx/network with
//!   exponential backoff and emits `ApiRetry` events. Also owns the small request-level helpers
//!   shared by the loop and by compaction.
//! - [`agent`] — `AgentConfig` and `Agent`, the SPEC §3 loop: per-turn model fallback, the tool
//!   dispatch pipeline (bad-JSON → error, PreToolUse → permission → execute → PostToolUse) and the
//!   Stop-hook loop guard.
//! - [`compaction`] — `compact` implementation and the token accounting that drives auto-compaction.
//! - [`plan`] — plan-mode tool hiding (SPEC §5 / Appendix B4), plan-text extraction and the
//!   tool-call-pair repair helpers used by both compaction and the request builder.
//! - [`finish`] — check-before-finish gate: one reminder after unverified edits, or an automatic
//!   `verify.command` run (max 2 rounds) at the would-be stop (r7).
//! - [`guard`] — per-turn repeat-edit / repeated-failure nudges and todo re-injection (r7).
//! - [`repair`] — tool-call repair before rejection: near-miss tool names, fenced/malformed JSON,
//!   schema type coercion (r7).
//! - [`subagent`] — `SubAgentExecutor` backing the `Task` tool (SPEC §9).
//!
//! ## Public API (zen-cli codes against these exact signatures; add freely)
//!
//! ```text
//! OpenAiClient::new(base_url, api_key, sink) + impl LlmBackend
//! AgentConfig, Agent::new(...), Agent::run_turn, Agent::compact,
//! Agent::set_messages / Agent::messages, Agent::set_stop_hook_max_continues
//! SubAgentExecutor (impl SubAgentRunner), SubAgentExecutor::set_max_depth
//! ```
//!
//! Seed code from the step-2 prototype: `../../.seed-step2/src/{llm.rs,agent.rs}` (adapted).

#![allow(clippy::module_inception)]

mod client;
pub mod providers;
pub mod budget;
pub mod compaction;
pub mod finish;
pub mod guard;
pub mod plan;
pub mod prune;
pub mod repair;
pub mod safety;
mod subagent;

pub mod agent;
pub mod rategate;
pub mod tool_bundles;
pub use agent::{Agent, AgentConfig, DEFAULT_MAX_CONTINUATIONS, DEFAULT_MAX_PARALLEL_TOOL_CALLS, DEFAULT_STOP_HOOK_CONTINUES, DEFAULT_TOOL_TIMEOUT_MS};
pub use tool_bundles::{ToolBundle, ToolPhase};
pub use client::{
    is_anthropic_url, make_backend, make_backend_with, request_id_from_error, AnthropicClient, HttpTimeouts,
    OpenAiClient,
};
pub use safety::{user_agent, ErrorClass, classify_error};
pub use providers::{ApiStandard, ModelSpec, ProviderInfo};
pub use rategate::{RateGate, RateGateSettings, DEFAULT_DAILY_LIMIT};
pub use subagent::SubAgentExecutor;

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_p0;
#[cfg(test)]
mod tests_p0b;
#[cfg(test)]
mod tests_trunc;
#[cfg(test)]
mod tests_timeout;
#[cfg(test)]
mod tests_p045;
