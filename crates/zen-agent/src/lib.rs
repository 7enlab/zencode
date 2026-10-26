








































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
