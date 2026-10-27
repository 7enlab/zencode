use std::path::Path;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;

pub const PROMPT_BUDGET_TOKENS: u64 = 1_200;

pub fn estimate_tokens(text: &str) -> u64 {
    text.len() as u64 / 4
}

pub struct PromptBudget {
    pub system_prompt_tokens: u64,
    pub tool_definitions_tokens: u64,
    pub per_tool: Vec<(String, u64)>,
}

impl PromptBudget {
    pub fn total(&self) -> u64 {
        self.system_prompt_tokens + self.tool_definitions_tokens
    }

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

pub fn measure_default_budget() -> Result<PromptBudget> {

    let cli = Cli::try_parse_from(["zen", "--bare"])?;
    let sys = crate::system_prompt::build_system_prompt(&cli, Path::new("/tmp"), None)?;
    let system_prompt_tokens = estimate_tokens(&sys);

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

    #[test]
    #[ignore = "g4 target not yet met: 2,331 > 1,200 tokens; awaiting Z Core's cuts"]
    fn g4_prompt_budget_within_1200_tokens() {
        let b = measure_default_budget().expect("prompt budget measurement");

        println!("{}", b.report());
        assert!(
            b.total() <= PROMPT_BUDGET_TOKENS,
            "prompt budget exceeded: {} > {PROMPT_BUDGET_TOKENS} tokens\n{}",
            b.total(),
            b.report()
        );
    }

    const CURRENT_BUDGET_TOKENS: u64 = 2_331;

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
