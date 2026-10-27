use std::sync::Arc;
use zen_agent::tool_bundles::{
    definitions_for_bundle, estimate_schema_tokens, per_tool_costs, total_schema_tokens,
    ToolBundle, SCHEMA_TOKEN_BUDGETS,
};
use zen_core::{Tool, ToolRegistry};

fn builtin_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    for t in zen_tools::builtin_tools() {
        r.register(t);
    }
    r.register(Arc::new(zen_tools::JobOutputTool));
    r.register(Arc::new(zen_tools::JobKillTool));
    r
}

fn all_builtin_tools() -> Vec<Arc<dyn Tool>> {
    let mut v: Vec<Arc<dyn Tool>> = zen_tools::builtin_tools();
    v.push(Arc::new(zen_tools::JobOutputTool));
    v.push(Arc::new(zen_tools::JobKillTool));
    v
}

#[test]
fn schema_token_budgets_hold() {
    let r = builtin_registry();
    for (bundle, budget) in SCHEMA_TOKEN_BUDGETS {
        let defs = definitions_for_bundle(&r, *bundle);
        let total = total_schema_tokens(&defs);

        println!("bundle {:?}: {} tokens (budget {})", bundle, total, budget);
        assert!(
            total <= *budget,
            "tool bundle {:?} costs {} tokens, over budget {}",
            bundle,
            total,
            budget
        );
    }
}

#[test]
fn builtin_descriptions_are_one_liners() {
    for t in all_builtin_tools() {
        let d = t.description();
        assert!(!d.is_empty(), "{} has an empty description", t.name());
        assert!(
            d.len() <= 160,
            "{} description is {} chars (max 160): {:?}",
            t.name(),
            d.len(),
            d
        );
        assert!(
            !d.contains('\n'),
            "{} description spans multiple lines",
            t.name()
        );
    }
}

#[test]
fn tool_bundle_flag_parses() {
    use clap::Parser;
    use zen_cli::Cli;

    for (flag, expected) in [
        ("all", ToolBundle::All),
        ("core", ToolBundle::Core),
        ("read", ToolBundle::Read),
        ("write", ToolBundle::Write),
        ("exec", ToolBundle::Exec),
    ] {
        let cli = Cli::try_parse_from(["zen", "--tool-bundle", flag, "-p", "hi"])
            .expect("flag should parse");
        assert_eq!(cli.tool_bundle, Some(expected), "flag {flag}");
    }
    assert!(Cli::try_parse_from(["zen", "--tool-bundle", "nope", "-p", "hi"]).is_err());
}

#[test]
fn tool_help_known_and_unknown() {
    let out = zen_cli::commands::tool_help("Read");
    assert!(out.contains("phase: read"), "missing phase:\n{out}");
    assert!(out.contains("schema tokens"), "missing token cost:\n{out}");
    assert!(out.contains("# Read"), "missing full docs:\n{out}");

    let out2 = zen_cli::commands::tool_help("read");
    assert!(out2.contains("# Read"));

    let unknown = zen_cli::commands::tool_help("NoSuchTool");
    assert!(unknown.contains("Unknown tool"), "unexpected:\n{unknown}");
    assert!(unknown.contains("Read"), "should list available tools");
}

#[test]
fn read_bundle_stubs_write_tools() {
    let r = builtin_registry();
    let defs = definitions_for_bundle(&r, ToolBundle::Read);
    let by_name: std::collections::HashMap<&str, &zen_core::ToolDefinition> =
        defs.iter().map(|d| (d.function.name.as_str(), d)).collect();

    let read = by_name["Read"];
    assert!(
        !read.function.description.contains("[deferred]"),
        "Read should be full in the read bundle"
    );
    let write = by_name["Write"];
    assert!(
        write.function.description.contains("[deferred]"),
        "Write should be stubbed in the read bundle"
    );
    assert!(
        estimate_schema_tokens(write) < estimate_schema_tokens(read) / 2,
        "stub should be much cheaper than a full schema"
    );

    let costs = per_tool_costs(&defs);
    assert_eq!(costs.len(), defs.len());
}
