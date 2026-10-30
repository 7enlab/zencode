use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const DEFAULT_MAP_TOKENS: usize = 1000;

const ENTRY_FILE_BONUS: u64 = 1_000;

const MAIN_FN_BONUS: u64 = 2_000;

const PUB_BONUS: u64 = 500;

const RECENCY_BONUS_STEP: u64 = 10;

pub const MAX_FILES: usize = 500;

const MAX_LINES_PER_FILE: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Impl,
    Mod,
    Const,
    Static,
    Type,
}

impl SymbolKind {
    fn label(self) -> &'static str {
        match self {
            SymbolKind::Fn => "fn",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Mod => "mod",
            SymbolKind::Const => "const",
            SymbolKind::Static => "static",
            SymbolKind::Type => "type",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub line: usize,
    pub signature: String,
    pub is_pub: bool,

    pub score: u64,

    pub ref_files: usize,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub rel_path: String,
    pub symbols: Vec<Symbol>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoMap {
    pub files: Vec<FileEntry>,
    pub truncated: bool,

    rendered: Option<String>,
}

impl RepoMap {
    pub fn text(&self) -> &str {
        self.rendered.as_deref().unwrap_or("")
    }
}

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    "venv",
    "vendor",
    "coverage",
];

fn collect_rust_files(cwd: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(cwd)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                if name.starts_with('.') {
                    return false;
                }
                return !SKIP_DIRS.contains(&name.as_ref());
            }
            true
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if files.len() >= MAX_FILES {
            break;
        }
        let p = entry.path();
        if p.is_file() && p.extension().map(|e| e == "rs").unwrap_or(false) {
            files.push(p.to_path_buf());
        }
    }
    files.sort();
    files
}

fn strip_noise(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        if c == '/' && chars.get(i + 1) == Some(&'/') {
            break;
        }

        if c == 'r' {
            let mut j = i + 1;
            while chars.get(j) == Some(&'#') {
                j += 1;
            }
            if chars.get(j) == Some(&'"') {
                let hashes = j - (i + 1);
                let closing: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();

                for _ in i..=j {
                    out.push(' ');
                }
                i = j + 1;

                while i < chars.len() {
                    if i + closing.len() <= chars.len()
                        && chars[i..i + closing.len()] == closing[..]
                    {
                        for _ in 0..closing.len() {
                            out.push(' ');
                        }
                        i += closing.len();
                        break;
                    }
                    out.push(' ');
                    i += 1;
                }
                continue;
            }
        }

        if c == '"' {
            out.push(' ');
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < chars.len() {
                    out.push(' ');
                    i += 1;
                }
            }
            if i < chars.len() {
                out.push(' ');
                i += 1;
            }
            continue;
        }

        if c == '\'' {
            match chars.get(i + 1) {
                Some('\\') => {
                    for _ in 0..4 {
                        if i < chars.len() {
                            out.push(' ');
                            i += 1;
                        }
                    }
                }
                Some(_) if chars.get(i + 2) == Some(&'\'') => {
                    for _ in 0..3 {
                        out.push(' ');
                        i += 1;
                    }
                }
                _ => {
                    out.push(' ');
                    i += 1;
                    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        out.push(' ');
                        i += 1;
                    }
                }
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn vis_re() -> &'static str {
    r"(?:pub(?:\((?:crate|super|self)\))?\s+)?"
}

fn fn_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(&format!(
            r"^{vis}fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<]",
            vis = vis_re()
        ))
        .expect("fn regex")
    })
}

fn item_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(&format!(
            r"^{vis}(struct|enum|trait|type|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)",
            vis = vis_re()
        ))
        .expect("item regex")
    })
}

fn impl_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(&format!(
            r"^{vis}impl(?:\s*<[^>]{{0,80}}>)?\s+([A-Za-z_][A-Za-z0-9_:]*)",
            vis = vis_re()
        ))
        .expect("impl regex")
    })
}

fn mod_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(&format!(
            r"^{vis}mod\s+([A-Za-z_][A-Za-z0-9_]*)",
            vis = vis_re()
        ))
        .expect("mod regex")
    })
}

fn is_mod_tests_decl(clean: &str) -> bool {
    let t = clean.trim_start();

    let t = match t.strip_prefix("#[") {
        Some(rest) => match rest.find(']') {
            Some(end) => rest[end + 1..].trim_start(),
            None => t,
        },
        None => t,
    };
    t.strip_prefix("mod tests")
        .map(|rest| {
            rest.chars()
                .next()
                .map(|c| c == '{' || c == ';' || c.is_whitespace())
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn extract_symbols(path: &Path) -> Vec<Symbol> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut symbols = Vec::new();
    let mut in_test_mod = false;

    let mut test_mod_depth = 0i32;
    let mut brace_depth = 0i32;

    for (idx, raw) in text.lines().take(MAX_LINES_PER_FILE).enumerate() {
        let line_no = idx + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let clean = strip_noise(trimmed);

        if !in_test_mod && is_mod_tests_decl(&clean) {
            if clean.contains('{') {
                in_test_mod = true;
                test_mod_depth = brace_depth;
            }

            brace_depth += clean.matches('{').count() as i32 - clean.matches('}').count() as i32;
            continue;
        }

        brace_depth += clean.matches('{').count() as i32 - clean.matches('}').count() as i32;

        if in_test_mod {
            if brace_depth > test_mod_depth {
                continue;
            }

            in_test_mod = false;
        }

        let is_pub = clean.starts_with("pub");
        let sig: String = trimmed
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(120)
            .collect();

        if let Some(caps) = fn_re().captures(&clean) {
            symbols.push(Symbol {
                kind: SymbolKind::Fn,
                name: caps[1].to_string(),
                line: line_no,
                signature: sig,
                is_pub,
                score: 0,
                ref_files: 0,
            });
        } else if let Some(caps) = item_re().captures(&clean) {
            let kind = match &caps[1] {
                "struct" => SymbolKind::Struct,
                "enum" => SymbolKind::Enum,
                "trait" => SymbolKind::Trait,
                "type" => SymbolKind::Type,
                "const" => SymbolKind::Const,
                _ => SymbolKind::Static,
            };
            symbols.push(Symbol {
                kind,
                name: caps[2].to_string(),
                line: line_no,
                signature: sig,
                is_pub,
                score: 0,
                ref_files: 0,
            });
        } else if let Some(caps) = impl_re().captures(&clean) {
            symbols.push(Symbol {
                kind: SymbolKind::Impl,
                name: caps[1].to_string(),
                line: line_no,
                signature: sig,
                is_pub,
                score: 0,
                ref_files: 0,
            });
        } else if let Some(caps) = mod_re().captures(&clean) {
            if &caps[1] == "tests" {
                continue;
            }
            symbols.push(Symbol {
                kind: SymbolKind::Mod,
                name: caps[1].to_string(),
                line: line_no,
                signature: sig,
                is_pub,
                score: 0,
                ref_files: 0,
            });
        }
    }
    symbols
}

fn rank_symbols(entries: &mut [(PathBuf, Vec<Symbol>)]) {
    let mut names: Vec<String> = entries
        .iter()
        .flat_map(|(_, syms)| syms.iter().map(|s| s.name.clone()))
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return;
    }

    let pattern = names
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|");
    let Ok(re) = regex::Regex::new(&format!(r"\b({pattern})\b")) else {
        return;
    };

    let mut def_sites: HashSet<(String, usize, usize)> = HashSet::new();
    for (fi, (_, syms)) in entries.iter().enumerate() {
        for sym in syms {
            def_sites.insert((sym.name.clone(), fi, sym.line));
        }
    }

    let mut counts: HashMap<(String, usize), u64> = HashMap::new();
    for (fi, (path, _)) in entries.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        for (idx, line) in text.lines().take(MAX_LINES_PER_FILE).enumerate() {
            let line_no = idx + 1;
            for caps in re.captures_iter(line) {
                let name = caps[1].to_string();
                if def_sites.contains(&(name.clone(), fi, line_no)) {
                    continue;
                }
                *counts.entry((name, fi)).or_insert(0) += 1;
            }
        }
    }

    let per_sym: Vec<Vec<(usize, u64)>> = {
        let ordered: Vec<String> = entries
            .iter()
            .flat_map(|(_, syms)| syms.iter().map(|s| s.name.clone()))
            .collect();
        ordered
            .iter()
            .map(|name| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(fj, _)| {
                        let c = counts.get(&(name.clone(), fj)).copied().unwrap_or(0);
                        (fj, c)
                    })
                    .filter(|(_, c)| *c > 0)
                    .collect()
            })
            .collect()
    };
    let mut name_idx = 0usize;
    for (fi, (_, syms)) in entries.iter_mut().enumerate() {
        for sym in syms.iter_mut() {
            let mut cross = 0u64;
            let mut own = 0u64;
            let mut ref_files = 0usize;
            for (fj, c) in &per_sym[name_idx] {
                ref_files += 1;
                if *fj == fi {
                    own += c;
                } else {
                    cross += c;
                }
            }
            name_idx += 1;
            sym.score = 2 * cross + own;
            sym.ref_files = ref_files;
        }
    }
    apply_priority_boosts(entries);
    for (_, syms) in entries.iter_mut() {
        syms.sort_by(|a, b| b.score.cmp(&a.score).then(a.line.cmp(&b.line)));
    }
}

fn apply_priority_boosts(entries: &mut [(PathBuf, Vec<Symbol>)]) {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| {
        std::cmp::Reverse(
            std::fs::metadata(&entries[i].0)
                .and_then(|m| m.modified())
                .ok(),
        )
    });
    let n = entries.len();
    for (rank, &i) in order.iter().enumerate() {
        let recency_bonus = n.saturating_sub(1 + rank) as u64 * RECENCY_BONUS_STEP;
        let file_name = entries[i]
            .0
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let entry_bonus = if file_name == "main.rs" || file_name == "lib.rs" {
            ENTRY_FILE_BONUS
        } else {
            0
        };
        for sym in entries[i].1.iter_mut() {
            let mut bonus = recency_bonus + entry_bonus;
            if sym.is_pub {
                bonus += PUB_BONUS;
            }
            if sym.kind == SymbolKind::Fn && sym.name == "main" {
                bonus += MAIN_FN_BONUS;
            }
            sym.score = sym.score.saturating_add(bonus);
        }
    }
}

pub fn repo_map(cwd: &Path, map_tokens: usize) -> Option<RepoMap> {
    if map_tokens == 0 {
        return None;
    }
    let files = collect_rust_files(cwd);
    if files.is_empty() {
        return None;
    }
    let mut entries: Vec<(PathBuf, Vec<Symbol>)> = files
        .into_iter()
        .map(|p| {
            let syms = extract_symbols(&p);
            (p, syms)
        })
        .filter(|(_, syms)| !syms.is_empty())
        .collect();
    if entries.is_empty() {
        return None;
    }
    rank_symbols(&mut entries);

    entries.sort_by(|a, b| {
        let sa = a.1.first().map(|s| s.score).unwrap_or(0);
        let sb = b.1.first().map(|s| s.score).unwrap_or(0);
        sb.cmp(&sa)
    });

    let mut map = RepoMap::default();
    let trunc_marker = "[map truncated: token budget exhausted]\n";
    let marker_tokens = trunc_marker.chars().count().div_ceil(4);
    let mut used = 0usize;
    let header = format!(
        "Repository map (top Rust symbols by cross-file references, {}-token budget):\n",
        map_tokens
    );
    used += header.chars().count().div_ceil(4);
    let mut out = header;

    'files: for (path, syms) in &entries {
        let rel = path
            .strip_prefix(cwd)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let file_line = format!("{}\n", rel);
        if used + file_line.chars().count().div_ceil(4) + marker_tokens > map_tokens {
            map.truncated = true;
            break;
        }
        used += file_line.chars().count().div_ceil(4);
        out.push_str(&file_line);

        let mut file_entry = FileEntry {
            rel_path: rel,
            symbols: Vec::new(),
        };
        for sym in syms {
            let vis = if sym.is_pub { "pub " } else { "" };
            let line = format!(
                "  {}{} {}  # line {}, referenced in {} file(s)\n",
                vis,
                sym.kind.label(),
                sym.signature,
                sym.line,
                sym.ref_files
            );
            if used + line.chars().count().div_ceil(4) + marker_tokens > map_tokens {
                map.truncated = true;
                break 'files;
            }
            used += line.chars().count().div_ceil(4);
            out.push_str(&line);
            file_entry.symbols.push(sym.clone());
        }
        map.files.push(file_entry);
    }

    if map.truncated {
        out.push_str(trunc_marker);
    }
    map.rendered = Some(out.trim_end().to_string());
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("repomap-{tag}-{}", std::process::id()))
    }

    #[test]
    fn extracts_symbols_with_visibility() {
        let dir = unique_dir("extract");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_file(
            &dir,
            "a.rs",
            "pub fn hello() {}\nfn private_fn() {}\npub struct Config {}\nenum Hidden {}\nimpl Config {}\nmod inner {}\n",
        );
        let syms = extract_symbols(&p);
        let names: Vec<(&str, bool)> = syms.iter().map(|s| (s.name.as_str(), s.is_pub)).collect();
        assert!(names.contains(&("hello", true)));
        assert!(names.contains(&("private_fn", false)));
        assert!(names.contains(&("Config", true)));
        assert!(names.contains(&("Hidden", false)));
        assert!(names.iter().any(|(n, _)| *n == "inner"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_test_modules() {
        let dir = unique_dir("tests");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_file(
            &dir,
            "b.rs",
            "pub fn real() {}\n#[cfg(test)]\nmod tests {\n    fn fake_helper() {}\n}\n",
        );
        let syms = extract_symbols(&p);
        assert!(syms.iter().any(|s| s.name == "real"));
        assert!(!syms.iter().any(|s| s.name == "fake_helper"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranking_prefers_cross_file_references() {
        let dir = unique_dir("rank");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, "lib.rs", "pub fn popular() {}\npub fn lonely() {}\n");
        write_file(
            &dir,
            "a.rs",
            "use lib::popular;\nfn x() { popular(); popular(); }\n",
        );
        write_file(&dir, "b.rs", "fn y() { popular(); }\n");
        let mut entries: Vec<(PathBuf, Vec<Symbol>)> = collect_rust_files(&dir)
            .into_iter()
            .map(|p| {
                let s = extract_symbols(&p);
                (p, s)
            })
            .filter(|(_, s)| !s.is_empty())
            .collect();
        rank_symbols(&mut entries);
        let lib = entries.iter().find(|(p, _)| p.ends_with("lib.rs")).unwrap();
        let popular = lib.1.iter().find(|s| s.name == "popular").unwrap();
        let lonely = lib.1.iter().find(|s| s.name == "lonely").unwrap();
        assert!(
            popular.score > lonely.score,
            "popular should outrank lonely"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn budget_truncates() {
        let dir = unique_dir("budget");
        let _ = std::fs::remove_dir_all(&dir);
        let mut src = String::new();
        for i in 0..50 {
            src.push_str(&format!("pub fn func_{i}() {{}}\n"));
        }
        write_file(&dir, "big.rs", &src);
        let map = repo_map(&dir, 8).expect("map");
        assert!(map.truncated, "tiny budget should truncate");

        let est = map.text().chars().count() / 4;
        assert!(est <= 8 + 20, "within budget + slack, got {}", est);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_budget_disables() {
        let dir = unique_dir("zero");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, "c.rs", "pub fn f() {}\n");
        assert!(repo_map(&dir, 0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_files_kept_hidden_dirs_skipped() {
        let dir = unique_dir("hidden");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, ".hidden.rs", "pub fn hidden_fn() {}\n");
        write_file(&dir, ".hiddendir/x.rs", "pub fn concealed() {}\n");
        write_file(&dir, "visible.rs", "pub fn visible_fn() {}\n");
        let files = collect_rust_files(&dir);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.contains(&".hidden.rs".to_string()),
            "dotfile kept: {names:?}"
        );
        assert!(names.contains(&"visible.rs".to_string()));
        assert!(
            !names.contains(&"x.rs".to_string()),
            "hiddendir skipped: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_noise_handles_raw_strings_chars_lifetimes() {
        let s = strip_noise(r##"let x = r#"fn not_a_fn() {}"#;"##);
        assert!(!s.contains("not_a_fn"), "raw string leaked: {s:?}");

        let s = strip_noise("let q = '\"'; fn real() {}");
        assert!(s.contains("fn real"), "char literal broke parsing: {s:?}");

        let s = strip_noise("fn f<'a>(x: &'a str) {}");
        assert!(s.contains("fn f"), "lifetime broke parsing: {s:?}");

        let s = strip_noise("let s = \"a\\\"b\"; fn after() {}");
        assert!(s.contains("fn after"), "escaped quote broke parsing: {s:?}");

        let s = strip_noise("fn kept() {} // fn dropped() {}");
        assert!(s.contains("fn kept") && !s.contains("fn dropped"));
    }

    #[test]
    fn cfg_test_attr_does_not_swallow_following_code() {
        let dir = unique_dir("cfgtest");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_file(
            &dir,
            "d.rs",
            "pub fn before() {}\n#[cfg(test)]\nfn test_only() {}\npub fn after() {}\n",
        );
        let syms = extract_symbols(&p);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"before"), "{names:?}");
        assert!(names.contains(&"test_only"), "{names:?}");
        assert!(names.contains(&"after"), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mod_tests_declaration_without_braces_is_safe() {
        let dir = unique_dir("moddecl");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_file(&dir, "e.rs", "mod tests;\npub fn after_decl() {}\n");
        let syms = extract_symbols(&p);
        assert!(
            syms.iter().any(|s| s.name == "after_decl"),
            "code after `mod tests;` must not be skipped"
        );
        assert!(!syms.iter().any(|s| s.name == "tests"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn code_after_test_module_is_kept() {
        let dir = unique_dir("aftermod");
        let _ = std::fs::remove_dir_all(&dir);
        let p = write_file(
            &dir,
            "f.rs",
            "pub fn first() {}\nmod tests {\n    fn helper() {}\n}\npub fn last() {}\n",
        );
        let syms = extract_symbols(&p);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"first"), "{names:?}");
        assert!(names.contains(&"last"), "{names:?}");
        assert!(!names.contains(&"helper"), "{names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn regexes_do_not_shadow_each_other() {
        let cases: &[(&str, SymbolKind, &str)] = &[
            ("fn plain() {", SymbolKind::Fn, "plain"),
            ("pub fn public_fn<T>(x: T) {", SymbolKind::Fn, "public_fn"),
            ("struct S;", SymbolKind::Struct, "S"),
            ("pub enum E {", SymbolKind::Enum, "E"),
            ("trait T {", SymbolKind::Trait, "T"),
            ("type Alias = u32;", SymbolKind::Type, "Alias"),
            ("const C: u8 = 1;", SymbolKind::Const, "C"),
            ("static S: u8 = 1;", SymbolKind::Static, "S"),
            ("impl S {", SymbolKind::Impl, "S"),
            ("impl<T> Generic<T> {", SymbolKind::Impl, "Generic"),
            ("mod inner {", SymbolKind::Mod, "inner"),
        ];
        for (line, want_kind, want_name) in cases {
            let clean = strip_noise(line);
            let got = if let Some(c) = fn_re().captures(&clean) {
                (SymbolKind::Fn, c[1].to_string())
            } else if let Some(c) = item_re().captures(&clean) {
                let k = match &c[1] {
                    "struct" => SymbolKind::Struct,
                    "enum" => SymbolKind::Enum,
                    "trait" => SymbolKind::Trait,
                    "type" => SymbolKind::Type,
                    "const" => SymbolKind::Const,
                    _ => SymbolKind::Static,
                };
                (k, c[2].to_string())
            } else if let Some(c) = impl_re().captures(&clean) {
                (SymbolKind::Impl, c[1].to_string())
            } else if let Some(c) = mod_re().captures(&clean) {
                (SymbolKind::Mod, c[1].to_string())
            } else {
                panic!("no regex matched {line:?}");
            };
            assert_eq!(got.0, *want_kind, "kind for {line:?}");
            assert_eq!(got.1, *want_name, "name for {line:?}");
        }
    }

    #[test]
    fn definition_line_excluded_from_ranking() {
        let dir = unique_dir("defline");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, "solo.rs", "fn only_here() {}\n");
        let mut entries: Vec<(PathBuf, Vec<Symbol>)> = collect_rust_files(&dir)
            .into_iter()
            .map(|p| {
                let s = extract_symbols(&p);
                (p, s)
            })
            .filter(|(_, s)| !s.is_empty())
            .collect();
        rank_symbols(&mut entries);
        let sym = &entries[0].1[0];
        assert_eq!(sym.score, 0, "self-mention must not inflate score");
        assert_eq!(sym.ref_files, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_budget_is_1000() {
        assert_eq!(
            DEFAULT_MAP_TOKENS, 1000,
            "v0.5.0 requires the repo map to fit in 1000 tokens"
        );
    }

    #[test]
    fn entry_point_files_rank_first() {
        let dir = unique_dir("entrypoint");
        let _ = std::fs::remove_dir_all(&dir);

        write_file(&dir, "zzz.rs", "pub fn popular() {}\n");
        write_file(&dir, "user.rs", "fn u() { popular(); popular(); }\n");
        write_file(&dir, "main.rs", "fn main() {}\n");
        let map = repo_map(&dir, 1000).expect("map");
        assert!(
            map.files[0].rel_path.ends_with("main.rs"),
            "entry point first, got {}",
            map.files[0].rel_path
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pub_symbols_rank_before_private() {
        let dir = unique_dir("pubfirst");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, "a.rs", "fn private_z() {}\npub fn public_a() {}\n");
        let map = repo_map(&dir, 1000).expect("map");
        let names: Vec<&str> = map.files[0]
            .symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(names[0], "public_a", "public API first: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recently_modified_files_rank_first() {
        let dir = unique_dir("recent");
        let _ = std::fs::remove_dir_all(&dir);
        write_file(&dir, "old.rs", "pub fn old_fn() {}\n");

        std::thread::sleep(std::time::Duration::from_millis(50));
        write_file(&dir, "new.rs", "pub fn new_fn() {}\n");
        let map = repo_map(&dir, 1000).expect("map");
        assert!(
            map.files[0].rel_path.ends_with("new.rs"),
            "recent file first, got {}",
            map.files[0].rel_path
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_repo_map_stays_within_1k_budget() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let map = repo_map(&workspace_root, DEFAULT_MAP_TOKENS).expect("map");
        let est = map.text().chars().count() / 4;
        assert!(
            est <= DEFAULT_MAP_TOKENS,
            "repo map of own workspace exceeds budget: {est} > {}",
            DEFAULT_MAP_TOKENS
        );
    }
}
