//! Phase-scoped tool bundles: token-saving lever #2.
//!
//! Tool schemas are a hidden per-turn tax: every tool's full JSON schema is
//! re-sent on every request of the agent loop. A *bundle* is a stable, named
//! subset of tools whose full schemas are sent; tools outside the active phase
//! are sent as name-only stubs (discoverable on the wire, zero schema cost).
//!
//! Bundles are FIXED for the whole session (chosen via `--tool-bundle` or
//! settings). They are never recomposed per request: changing the tool list
//! between requests silently invalidates provider prompt caches (Anthropic
//! `cache_control`, OpenAI automatic caching), which would cost more than the
//! schemas save.

use serde_json::json;
use std::str::FromStr;
use zen_core::{FunctionDefinition, ToolDefinition, ToolRegistry};

/// Token estimate for one serialized tool schema.
///
/// Uses chars/4, the same heuristic as [`zen_core::estimate_tokens`], so the
/// audit numbers match what `zen context` reports.
pub fn estimate_schema_tokens(def: &ToolDefinition) -> u64 {
    serde_json::to_string(def).map(|s| s.len() as u64 / 4).unwrap_or(0)
}

/// Total estimated schema tokens for a slice of definitions.
pub fn total_schema_tokens(defs: &[ToolDefinition]) -> u64 {
    defs.iter().map(estimate_schema_tokens).sum()
}

/// Per-tool `(name, tokens)` costs, sorted by tokens descending (name as
/// tiebreak). Feeds the `zen context` schema-audit view.
pub fn per_tool_costs(defs: &[ToolDefinition]) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = defs
        .iter()
        .map(|d| (d.function.name.clone(), estimate_schema_tokens(d)))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Task phase a builtin tool belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolPhase {
    /// Always sent with a full schema (planning / tracking tools).
    Core,
    /// Exploration: finding and reading code.
    Read,
    /// Implementation: creating and modifying files.
    Write,
    /// Verification: running commands and background jobs.
    Exec,
}

impl ToolPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolPhase::Core => "core",
            ToolPhase::Read => "read",
            ToolPhase::Write => "write",
            ToolPhase::Exec => "exec",
        }
    }
}

/// Stable builtin tool -> phase mapping.
///
/// Returns `None` for tools we do not classify (`Task`, `mcp__*`, future
/// tools): unclassified tools keep their full schema in EVERY bundle, so a
/// scoped bundle can never hide a tool the model might need.
pub fn tool_phase(name: &str) -> Option<ToolPhase> {
    match name {
        "TodoWrite" => Some(ToolPhase::Core),
        "Read" | "Glob" | "Grep" | "WebFetch" => Some(ToolPhase::Read),
        "Write" | "Edit" | "MultiEdit" => Some(ToolPhase::Write),
        "Bash" | "JobOutput" | "JobKill" => Some(ToolPhase::Exec),
        _ => None,
    }
}

/// Named tool bundle, selected once per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolBundle {
    /// Current behavior: every tool with its full schema (sorted by name).
    #[default]
    All,
    /// Core tools at full schema; other classified phases stubbed.
    Core,
    /// Core + read-phase tools at full schema.
    Read,
    /// Core + write-phase tools at full schema.
    Write,
    /// Core + exec-phase tools at full schema.
    Exec,
}

impl ToolBundle {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolBundle::All => "all",
            ToolBundle::Core => "core",
            ToolBundle::Read => "read",
            ToolBundle::Write => "write",
            ToolBundle::Exec => "exec",
        }
    }

    /// Phases sent with a full schema in this bundle. `None` means every
    /// phase (the `All` bundle).
    fn full_phases(self) -> Option<&'static [ToolPhase]> {
        match self {
            ToolBundle::All => None,
            ToolBundle::Core => Some(&[ToolPhase::Core]),
            ToolBundle::Read => Some(&[ToolPhase::Core, ToolPhase::Read]),
            ToolBundle::Write => Some(&[ToolPhase::Core, ToolPhase::Write]),
            ToolBundle::Exec => Some(&[ToolPhase::Core, ToolPhase::Exec]),
        }
    }
}

impl FromStr for ToolBundle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "all" => Ok(ToolBundle::All),
            "core" => Ok(ToolBundle::Core),
            "read" => Ok(ToolBundle::Read),
            "write" => Ok(ToolBundle::Write),
            "exec" => Ok(ToolBundle::Exec),
            other => Err(format!(
                "unknown tool bundle '{other}'; expected one of: all, core, read, write, exec"
            )),
        }
    }
}

/// CI regression budgets: estimated schema tokens per scoped bundle.
///
/// Schema bloat is silent -- a well-meaning description edit can add hundreds
/// of tokens to every turn of every session. These budgets fail the build when
/// a description regresses. Tune only with a measured justification.
pub const SCHEMA_TOKEN_BUDGETS: &[(ToolBundle, u64)] = &[
    (ToolBundle::Core, 1_000),
    (ToolBundle::Read, 1_700),
    (ToolBundle::Write, 1_500),
    (ToolBundle::Exec, 1_500),
];

/// Name-only stub for a tool outside the active phase: discoverable on the
/// wire (the model knows the tool exists) but carrying no schema cost.
pub fn deferred_stub(name: &str) -> ToolDefinition {
    ToolDefinition {
        tool_type: "function".into(),
        function: FunctionDefinition {
            name: name.to_string(),
            description: "[deferred] Full schema not loaded in the active tool bundle; \
                          do not call this tool."
                .to_string(),
            parameters: json!({"type": "object", "properties": {}}),
        },
    }
}

/// Definitions for a bundle in deterministic (name-sorted) order.
///
/// Stable ordering keeps the prompt prefix byte-identical across requests,
/// which is what provider prompt caches key on. Unclassified tools
/// (`Task`, `mcp__*`, ...) always keep their full schema.
pub fn definitions_for_bundle(
    registry: &ToolRegistry,
    bundle: ToolBundle,
) -> Vec<ToolDefinition> {
    let mut defs: Vec<ToolDefinition> = registry
        .definitions()
        .into_iter()
        .map(|d| {
            let name = d.function.name.as_str();
            let full = match bundle.full_phases() {
                None => true, // All: everything at full schema
                Some(phases) => match tool_phase(name) {
                    None => true, // unclassified: always full, never hidden
                    Some(p) => phases.contains(&p),
                },
            };
            if full {
                d
            } else {
                deferred_stub(name)
            }
        })
        .collect();
    defs.sort_by(|a, b| a.function.name.cmp(&b.function.name));
    defs
}

/// True when `name` would be sent as a name-only stub under `bundle`
/// (and therefore must not be dispatched).
pub fn is_deferred(registry: &ToolRegistry, bundle: ToolBundle, name: &str) -> bool {
    if registry.get(name).is_none() {
        return false;
    }
    match bundle.full_phases() {
        None => false,
        Some(phases) => match tool_phase(name) {
            None => false,
            Some(p) => !phases.contains(&p),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::Arc;
    use zen_core::{Tool, ToolContext, ToolOutput};

    struct FakeTool {
        name: &'static str,
        description: &'static str,
    }

    #[async_trait::async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.description
        }
        fn parameters_schema(&self) -> Value {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Target path for the operation."},
                    "limit": {"type": "integer", "description": "Maximum items to return."},
                    "options": {"type": "object", "description": "Extra options bag."},
                },
                "required": ["path"]
            })
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn call(&self, _args: Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput::ok("ok")
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        // Intentionally scrambled registration order; bundles must sort.
        // Descriptions are realistically sized (real builtin schemas run
        // 150-400 tokens): stubs only save when the schema they replace is
        // bigger than the stub itself.
        for (name, desc) in [
            (
                "Write",
                "Create a file or overwrite it completely. Parent directories are created. \
                 If the file already exists it must have been read earlier in this session, \
                 otherwise the call is rejected to prevent accidental overwrites of unseen content.",
            ),
            (
                "Bash",
                "Run a shell command with bash -c in the session working directory. The directory \
                 is persistent across calls. Long output is cut to head and tail with the full text \
                 saved to a file whose path is reported, along with the exit code.",
            ),
            (
                "Read",
                "Read a text file from disk and return its lines with 1-based line numbers. Use \
                 offset and limit to page through large files without loading them fully.",
            ),
            ("TodoWrite", "Replace the session task list with a new one."),
            (
                "mcp__srv__ext",
                "External MCP tool with a longer description for realism, plus extra detail.",
            ),
        ] {
            r.register(Arc::new(FakeTool { name, description: desc }));
        }
        r
    }

    /// FakeTool with a fatter parameter schema, closer to real builtins
    /// (which carry several described parameters).

    #[test]
    fn bundle_parsing_roundtrip() {
        for (s, b) in [
            ("all", ToolBundle::All),
            ("core", ToolBundle::Core),
            ("read", ToolBundle::Read),
            ("write", ToolBundle::Write),
            ("exec", ToolBundle::Exec),
            ("READ", ToolBundle::Read),
        ] {
            assert_eq!(s.parse::<ToolBundle>().unwrap(), b);
            assert_eq!(b.as_str(), s.to_ascii_lowercase());
        }
        assert!("nope".parse::<ToolBundle>().is_err());
    }

    #[test]
    fn stable_ordering_sorted_by_name() {
        let r = registry();
        for bundle in [
            ToolBundle::All,
            ToolBundle::Core,
            ToolBundle::Read,
            ToolBundle::Write,
            ToolBundle::Exec,
        ] {
            let defs = definitions_for_bundle(&r, bundle);
            let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
            let mut sorted = names.clone();
            sorted.sort_unstable();
            assert_eq!(names, sorted, "bundle {:?} not sorted", bundle);
            // Deterministic across calls.
            let again = definitions_for_bundle(&r, bundle);
            let names2: Vec<&str> = again.iter().map(|d| d.function.name.as_str()).collect();
            assert_eq!(names, names2);
        }
    }

    #[test]
    fn phase_scoping_reduces_tokens() {
        let r = registry();
        let all = total_schema_tokens(&definitions_for_bundle(&r, ToolBundle::All));
        for bundle in [
            ToolBundle::Core,
            ToolBundle::Read,
            ToolBundle::Write,
            ToolBundle::Exec,
        ] {
            let scoped = total_schema_tokens(&definitions_for_bundle(&r, bundle));
            assert!(
                scoped < all,
                "bundle {:?}: scoped {} not < all {}",
                bundle,
                scoped,
                all
            );
        }
    }

    #[test]
    fn stubs_carry_no_schema_cost() {
        let stub = deferred_stub("Write");
        assert_eq!(stub.function.name, "Write");
        assert!(stub.function.description.contains("[deferred]"));
        // A stub is ~1/10th the cost of a real schema in practice; here just
        // assert it is tiny and stable.
        assert!(estimate_schema_tokens(&stub) < 60);
    }

    #[test]
    fn unclassified_tools_keep_full_schema() {
        let r = registry();
        for bundle in [
            ToolBundle::Core,
            ToolBundle::Read,
            ToolBundle::Write,
            ToolBundle::Exec,
        ] {
            let defs = definitions_for_bundle(&r, bundle);
            let mcp = defs
                .iter()
                .find(|d| d.function.name == "mcp__srv__ext")
                .expect("mcp tool missing");
            assert!(
                !mcp.function.description.contains("[deferred]"),
                "unclassified tool stubbed in {:?}",
                bundle
            );
            assert!(!is_deferred(&r, bundle, "mcp__srv__ext"));
        }
    }

    #[test]
    fn is_deferred_matches_bundle_contents() {
        let r = registry();
        // Read bundle: Read full, Write stubbed.
        assert!(!is_deferred(&r, ToolBundle::Read, "Read"));
        assert!(!is_deferred(&r, ToolBundle::Read, "TodoWrite")); // core always full
        assert!(is_deferred(&r, ToolBundle::Read, "Write"));
        assert!(is_deferred(&r, ToolBundle::Read, "Bash"));
        // All bundle: nothing deferred.
        assert!(!is_deferred(&r, ToolBundle::All, "Write"));
        // Unknown tool: never "deferred", just unknown.
        assert!(!is_deferred(&r, ToolBundle::Read, "Nope"));
    }

    #[test]
    fn per_tool_costs_sorted_desc() {
        let r = registry();
        let defs = definitions_for_bundle(&r, ToolBundle::All);
        let costs = per_tool_costs(&defs);
        assert_eq!(costs.len(), defs.len());
        for w in costs.windows(2) {
            assert!(w[0].1 >= w[1].1, "costs not descending: {:?}", costs);
        }
        // every registered tool is represented exactly once
        let names: Vec<&str> = costs.iter().map(|(n, _)| n.as_str()).collect();
        for expected in ["Bash", "Read", "Write", "TodoWrite", "mcp__srv__ext"] {
            assert!(names.contains(&expected), "missing {}", expected);
        }
        // all costs are nonzero
        assert!(costs.iter().all(|(_, t)| *t > 0));
    }
}
