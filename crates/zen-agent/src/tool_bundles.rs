












use serde_json::json;
use std::str::FromStr;
use zen_core::{FunctionDefinition, ToolDefinition, ToolRegistry};





pub fn estimate_schema_tokens(def: &ToolDefinition) -> u64 {
    serde_json::to_string(def).map(|s| s.len() as u64 / 4).unwrap_or(0)
}


pub fn total_schema_tokens(defs: &[ToolDefinition]) -> u64 {
    defs.iter().map(estimate_schema_tokens).sum()
}



pub fn per_tool_costs(defs: &[ToolDefinition]) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = defs
        .iter()
        .map(|d| (d.function.name.clone(), estimate_schema_tokens(d)))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolPhase {

    Core,

    Read,

    Write,

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






pub fn tool_phase(name: &str) -> Option<ToolPhase> {
    match name {
        "TodoWrite" => Some(ToolPhase::Core),
        "Read" | "Glob" | "Grep" | "WebFetch" => Some(ToolPhase::Read),
        "Write" | "Edit" | "MultiEdit" => Some(ToolPhase::Write),
        "Bash" | "JobOutput" | "JobKill" => Some(ToolPhase::Exec),
        _ => None,
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolBundle {

    #[default]
    All,

    Core,

    Read,

    Write,

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






pub const SCHEMA_TOKEN_BUDGETS: &[(ToolBundle, u64)] = &[
    (ToolBundle::Core, 1_000),
    (ToolBundle::Read, 1_700),
    (ToolBundle::Write, 1_500),
    (ToolBundle::Exec, 1_500),
];



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
                None => true,
                Some(phases) => match tool_phase(name) {
                    None => true,
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

        assert!(!is_deferred(&r, ToolBundle::Read, "Read"));
        assert!(!is_deferred(&r, ToolBundle::Read, "TodoWrite"));
        assert!(is_deferred(&r, ToolBundle::Read, "Write"));
        assert!(is_deferred(&r, ToolBundle::Read, "Bash"));

        assert!(!is_deferred(&r, ToolBundle::All, "Write"));

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

        let names: Vec<&str> = costs.iter().map(|(n, _)| n.as_str()).collect();
        for expected in ["Bash", "Read", "Write", "TodoWrite", "mcp__srv__ext"] {
            assert!(names.contains(&expected), "missing {}", expected);
        }

        assert!(costs.iter().all(|(_, t)| *t > 0));
    }
}
