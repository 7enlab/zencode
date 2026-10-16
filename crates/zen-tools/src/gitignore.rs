//! Minimal `.gitignore` support: collect rules from every `.gitignore` file under a root
//! and answer "is this path ignored?" with last-match-wins semantics.
//!
//! This is deliberately a *simple* implementation (SPEC §4 allows it): support for `#` comments,
//! `!` negation, bare names (match at any depth), leading `/` anchoring, trailing `/` (directory
//! only), and the usual `*` / `**` globs. Not supported: escaped whitespace, `.git/info/exclude`,
//! global excludes, and re-including files inside an ignored directory.

use globset::{GlobBuilder, GlobMatcher};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub struct IgnoreRule {
    base: PathBuf,
    matcher: GlobMatcher,
    negated: bool,
    dir_only: bool,
}

/// Walk `root` collecting rules from all `.gitignore` files. `.git` itself is skipped.
pub fn load_ignore_rules(root: &Path) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if name == ".git" {
                return false;
            }
            if e.file_type().is_dir() {
                true
            } else {
                name == ".gitignore"
            }
        })
        .flatten()
    {
        if entry.file_type().is_file() {
            let base = entry.path().parent().unwrap_or(root).to_path_buf();
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                parse_gitignore(&text, &base, &mut rules);
            }
        }
    }
    rules
}

fn parse_gitignore(text: &str, base: &Path, out: &mut Vec<IgnoreRule>) {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let negated = line.starts_with('!');
        let pat = if negated { &line[1..] } else { line };
        let pat = pat.trim();
        if pat.is_empty() {
            continue;
        }
        let dir_only = pat.ends_with('/');
        let pat = pat.trim_end_matches('/');
        if pat.is_empty() {
            continue;
        }
        let anchored = pat.starts_with('/');
        let pat = pat.trim_start_matches('/');
        // A pattern with no slash (other than a trailing one) matches at any depth.
        let rel = if anchored || pat.contains('/') {
            pat.to_string()
        } else {
            format!("**/{}", pat)
        };
        // `literal_separator(false)` so `**/name` also matches a top-level `name`.
        let literal_separator = rel.contains('/') && !rel.starts_with("**/");
        if let Ok(glob) = GlobBuilder::new(&rel)
            .literal_separator(literal_separator)
            .build()
        {
            out.push(IgnoreRule {
                base: base.to_path_buf(),
                matcher: glob.compile_matcher(),
                negated,
                dir_only,
            });
        }
    }
}

/// True if `path` (which is `is_dir` or not) is ignored by the collected rules.
pub fn is_ignored(rules: &[IgnoreRule], path: &Path, is_dir: bool) -> bool {
    let mut ignored = false;
    for rule in rules {
        let Ok(rel) = path.strip_prefix(&rule.base) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        if rule.dir_only && !is_dir {
            // The rule would ignore the directory itself; we never descend into it, so
            // the file cannot be matched here.
            continue;
        }
        if rule.matcher.is_match(rel) {
            ignored = !rule.negated;
        }
    }
    ignored
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rules_match_files_and_dirs() {
        let dir = std::env::temp_dir().join(format!("zen-gi-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join(".gitignore"), "target/\n*.log\n!keep.log\n/rootonly.txt\n").unwrap();
        fs::write(dir.join("a.log"), "x").unwrap();
        fs::write(dir.join("keep.log"), "x").unwrap();
        fs::write(dir.join("rootonly.txt"), "x").unwrap();
        fs::write(dir.join("sub/rootonly.txt"), "x").unwrap();

        let rules = load_ignore_rules(&dir);
        assert!(is_ignored(&rules, &dir.join("target"), true));
        assert!(is_ignored(&rules, &dir.join("a.log"), false));
        assert!(!is_ignored(&rules, &dir.join("keep.log"), false));
        assert!(is_ignored(&rules, &dir.join("rootonly.txt"), false));
        assert!(!is_ignored(&rules, &dir.join("sub/rootonly.txt"), false));
        let _ = fs::remove_dir_all(&dir);
    }
}
