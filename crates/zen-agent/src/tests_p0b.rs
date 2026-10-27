use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use zen_core::*;

use crate::testutil::*;

struct SlowTool {
    name: &'static str,
    ms: u64,
    seen_timeout: Arc<Mutex<Vec<Option<u64>>>>,
}

#[async_trait]
impl Tool for SlowTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "slow"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn is_read_only(&self) -> bool {
        false
    }
    async fn call(&self, _args: Value, ctx: &ToolContext) -> ToolOutput {
        self.seen_timeout.lock().unwrap().push(ctx.tool_timeout_ms);
        tokio::time::sleep(Duration::from_millis(self.ms)).await;
        ToolOutput::ok("finished")
    }
}

async fn run_slow(name: &'static str, ms: u64, timeouts: &[(&str, u64)]) -> (String, Vec<Option<u64>>, Duration) {
    let seen = Arc::new(Mutex::new(vec![]));
    let tools = registry_with(vec![Arc::new(SlowTool { name, ms, seen_timeout: seen.clone() })]);
    let llm = MockLlm::new(vec![resp_tool(name, "{}", "s1"), resp("done")]);
    let cfg = crate::AgentConfig {
        tool_timeouts: timeouts.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        ..base_cfg()
    };
    let (mut agent, _s, _h) = build_agent(llm, tools, FakePerms::new(PermissionDecision::Allow), cfg);
    let t0 = Instant::now();
    agent.run_turn("go").await;
    let result = agent
        .messages()
        .iter()
        .find(|m| m.role == "tool")
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let seen = seen.lock().unwrap().clone();
    (result, seen, t0.elapsed())
}

#[tokio::test]
async fn p0b_2_configured_tool_timeout_stops_a_slow_call() {
    let (out, _, took) = run_slow("Slow", 5_000, &[("Slow", 50)]).await;
    assert!(out.contains("Slow timed out after 50 ms"), "{out}");
    assert!(out.contains("toolTimeouts"), "{out}");
    assert!(took < Duration::from_secs(3), "{took:?}");
}

#[tokio::test]
async fn p0b_2_default_entry_applies_and_zero_disables() {
    let (out, _, _) = run_slow("Slow", 2_000, &[("default", 30)]).await;
    assert!(out.contains("timed out after 30 ms"), "{out}");
    let (out, _, _) = run_slow("Slow", 100, &[("default", 30), ("Slow", 0)]).await;
    assert_eq!(out, "finished");
}

#[tokio::test]
async fn p0b_2_bash_gets_its_default_through_the_context_not_a_wrapper() {

    let (out, seen, _) = run_slow("Bash", 100, &[("Bash", 40), ("default", 10)]).await;
    assert_eq!(out, "finished");
    assert_eq!(seen, vec![Some(40)]);
}

#[test]
fn p0b_2_timeout_defaults() {
    let c = base_cfg();
    assert_eq!(c.tool_timeout_ms("Read"), Some(crate::DEFAULT_TOOL_TIMEOUT_MS));
    assert_eq!(c.tool_timeout_ms("mcp__srv__x"), Some(300_000));
    assert_eq!(c.tool_timeout_ms("Bash"), None);

    assert_eq!(c.tool_timeout_ms("Task"), None);
    assert_eq!(c.tool_timeout_ms("JobOutput"), None);
    let c = crate::AgentConfig { tool_timeouts: [("Task".to_string(), 9u64)].into(), ..base_cfg() };
    assert_eq!(c.tool_timeout_ms("Task"), Some(9));

    let c = crate::AgentConfig { tool_timeouts: [("Task".to_string(), 0u64)].into(), ..base_cfg() };
    assert_eq!(c.tool_timeout_ms("Task"), None);

    let c = crate::AgentConfig { tool_timeouts: [("default".to_string(), 42u64)].into(), ..base_cfg() };
    assert_eq!(c.tool_timeout_ms("Task"), None);
    assert_eq!(c.tool_timeout_ms("JobOutput"), None);
    assert_eq!(c.tool_timeout_ms("Read"), Some(42));
}

#[test]
fn p0b_2_timeout_source_is_reported() {
    use crate::agent::TimeoutSource;
    let c = base_cfg();
    assert_eq!(c.tool_timeout_with_source("Read"), (Some(crate::DEFAULT_TOOL_TIMEOUT_MS), TimeoutSource::BuiltIn));
    let c = crate::AgentConfig { tool_timeouts: [("default".to_string(), 42u64)].into(), ..base_cfg() };
    assert_eq!(c.tool_timeout_with_source("Read"), (Some(42), TimeoutSource::Default));
    let c = crate::AgentConfig { tool_timeouts: [("Read".to_string(), 7u64)].into(), ..base_cfg() };
    assert_eq!(c.tool_timeout_with_source("Read"), (Some(7), TimeoutSource::PerTool));
    assert_eq!(c.tool_timeout_with_source("Bash"), (None, TimeoutSource::None));
}
