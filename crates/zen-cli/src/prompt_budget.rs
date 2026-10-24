//! Prompt budget: default system prompt + all default tool definitions.
//!
//! Roadmap item g4 (v0.3.0): measure what the CLI sends as the fixed prompt prefix on every
//! request and fail CI if it grows past the budget. Token counting follows the project's
//! existing chars/4 convention (see `zen_core::estimate_tokens` and the `/context` command).
//!
//! STATUS 2026-10-17: currently OVER the 1,200-token g4 target (2,331 measured). The
//! `g4_prompt_budget_within_1200_tokens` test is `#[ignore]`d until Z Core decides what to
//! cut; `g4_prompt_budget_no_growth` guards against further growth in the meantime.
//!
//! The measurement uses `--bare` mode for determinism: it skips the project snapshot,
//! memory (ZEN.md) and skills list, which all depend on the working directory. The
//! environment block (cwd, platform, date) is included — its size is stable.

use std::path::Path;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;

/// Token budget for the default system prompt + default tool definitions (roadmap g4).
pub const PROMPT_BUDGET_TOKENS: u64 = 1_200;

/// Rough token estimate, matching the project's chars/4 convention.
pub fn estimate_tokens(text: &str) -> u64 {
    text.len() as u64 / 4
}

/// Per-section breakdown of the prompt budget.
pub struct PromptBudget {
    pub system_prompt_tokens: u64,
    pub tool_definitions_tokens: u64,
    pub per_tool: Vec<(String, u64)>,
}

impl PromptBudget {
    pub fn total(&self) -> u64 {
        self.system_prompt_tokens + self.tool_definitions_tokens
    }

    /// Human-readable breakdown, one line per section.
    pub fn report(&self) -> String {
        let mut out = String::from("prompt budget breakdown (tokens, chars/4):\n");
        out.push_str(&format!("  system prompt: {}\n", self.system_prompt_tokens));
        for (name, tok) in &self.per_tool {
            out.push_str(&format!("  tool {name}: {tok}\n"));
        }
        out.push_str(&format!(
            "  tool definitions total: {}\n  TOTAL: {} / {PROMPT_BUDGET_TOKENS}\n",
            self.tool_definitions_tokens,
            self.total()
        ));
        out
    }
}

/// Measure the default prompt budget: the `--bare` system prompt plus every default tool
/// definition serialized exactly as it is sent to the LLM (`ChatRequest.tools`).
pub fn measure_default_budget() -> Result<PromptBudget> {
    // --bare for determinism (no project snapshot / memory / skills).
    let cli = Cli::try_parse_from(["zen", "--bare"])?;
    let sys = crate::system_prompt::build_system_prompt(&cli, Path::new("/tmp"), None)?;
    let system_prompt_tokens = estimate_tokens(&sys);

    // Same default tool set as `agent_run::assemble`: builtins + background-job tools.
    let mut registry = zen_core::ToolRegistry::new();
    for t in zen_tools::builtin_tools() {
        registry.register(t);
    }
    for t in zen_tools::job_tools() {
        registry.register(t);
    }

    let mut per_tool = Vec::new();
    let mut tool_definitions_tokens = 0u64;
    for d in registry.definitions() {
        let json = serde_json::to_string(&d)?;
        let tok = estimate_tokens(&json);
        per_tool.push((d.function.name.clone(), tok));
        tool_definitions_tokens += tok;
    }

    Ok(PromptBudget {
        system_prompt_tokens,
        tool_definitions_tokens,
        per_tool,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Roadmap g4 target: 1,200 tokens. Currently OVER BUDGET (2,331 as of 2026-10-17,
    /// see breakdown below) — Z Core must decide what to cut before this can be enabled.
    /// Biggest sections: Grep (325), Bash (305), MultiEdit (219), system prompt (374).
    /// Run explicitly with: cargo test -p zen-cli --lib prompt_budget -- --ignored
    #[test]
    #[ignore = "g4 target not yet met: 2,331 > 1,200 tokens; awaiting Z Core's cuts"]
    fn g4_prompt_budget_within_1200_tokens() {
        let b = measure_default_budget().expect("prompt budget measurement");
        // Always print the breakdown so CI logs show where the tokens go.
        println!("{}", b.report());
        assert!(
            b.total() <= PROMPT_BUDGET_TOKENS,
            "prompt budget exceeded: {} > {PROMPT_BUDGET_TOKENS} tokens\n{}",
            b.total(),
            b.report()
        );
    }

    /// Regression guard: the budget must not grow past its measured value while the
    /// g4 target (above) is still over budget. Update CURRENT_BUDGET_TOKENS only when
    /// the prompt or tool defs intentionally change — never to silence a failure.
    const CURRENT_BUDGET_TOKENS: u64 = 2_331; // measured 2026-10-17, chars/4

    #[test]
    fn g4_prompt_budget_no_growth() {
        let b = measure_default_budget().expect("prompt budget measurement");
        println!("{}", b.report());
        assert!(
            b.total() <= CURRENT_BUDGET_TOKENS,
            "prompt budget grew: {} > {CURRENT_BUDGET_TOKENS} tokens (was 2,331 on 2026-10-17)\n{}",
            b.total(),
            b.report()
        );
    }

    #[test]
    fn g4_budget_report_lists_every_default_tool() {
        let b = measure_default_budget().expect("prompt budget measurement");
        let names: Vec<&str> = b.per_tool.iter().map(|(n, _)| n.as_str()).collect();
        for expected in [
            "Read", "Write", "Edit", "MultiEdit", "Bash", "Glob", "Grep", "WebFetch", "TodoWrite",
            "JobOutput", "JobKill",
        ] {
            assert!(names.contains(&expected), "missing tool {expected} in {names:?}");
        }
        assert_eq!(
            b.total(),
            b.system_prompt_tokens + b.tool_definitions_tokens,
            "total must equal the sum of its parts"
        );
    }
}
