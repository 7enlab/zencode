pub fn tool_long_docs(name: &str) -> Option<&'static str> {
    Some(match name {
        "Read" => include_str!("../../../docs/tools/read.md"),
        "Write" => include_str!("../../../docs/tools/write.md"),
        "Edit" => include_str!("../../../docs/tools/edit.md"),
        "MultiEdit" => include_str!("../../../docs/tools/multiedit.md"),
        "Bash" => include_str!("../../../docs/tools/bash.md"),
        "Glob" => include_str!("../../../docs/tools/glob.md"),
        "Grep" => include_str!("../../../docs/tools/grep.md"),
        "WebFetch" => include_str!("../../../docs/tools/webfetch.md"),
        "TodoWrite" => include_str!("../../../docs/tools/todowrite.md"),
        "JobOutput" => include_str!("../../../docs/tools/joboutput.md"),
        "JobKill" => include_str!("../../../docs/tools/jobkill.md"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_has_docs() {
        for name in [
            "Read",
            "Write",
            "Edit",
            "MultiEdit",
            "Bash",
            "Glob",
            "Grep",
            "WebFetch",
            "TodoWrite",
            "JobOutput",
            "JobKill",
        ] {
            let docs = tool_long_docs(name).unwrap_or_else(|| panic!("missing docs for {name}"));
            assert!(docs.contains(&format!("# {name}")) || docs.contains(name));
            assert!(!docs.is_empty());
        }
        assert!(tool_long_docs("Nope").is_none());
    }
}
