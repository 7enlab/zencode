














use std::collections::HashMap;


#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub text: String,
    pub count: usize,

    pub note: Option<String>,
}


pub fn apply_edit(text: &str, old: &str, new: &str, replace_all: bool, label: &str) -> Result<Applied, String> {
    if old.is_empty() {
        return Err("'old_string' must not be empty".into());
    }
    if old == new {
        return Err("'old_string' and 'new_string' are identical; nothing to do".into());
    }
    match apply_ladder(text, old, new, replace_all, label)? {
        Some(a) => Ok(a),
        None => {
            let (o, _) = strip_line_numbers(old, new).unwrap_or((old.to_string(), new.to_string()));
            Err(format!("'old_string' was not found in {}.\n{}", label, closest_candidates(text, &o)))
        }
    }
}


fn apply_ladder(text: &str, old: &str, new: &str, replace_all: bool, label: &str) -> Result<Option<Applied>, String> {
    let occurrences = text.matches(old).count();
    if occurrences > 1 && !replace_all {
        return Err(format!(
            "'old_string' occurs {} times in {}. Add more context or set replace_all=true.",
            occurrences, label
        ));
    }
    if occurrences >= 1 {
        let updated = if replace_all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        return Ok(Some(Applied { text: updated, count: occurrences, note: None }));
    }
    for strategy in [Strategy::TrailingWhitespace, Strategy::Indentation] {
        if let Some(a) = fuzzy_replace(text, old, new, strategy, label)? {
            return Ok(Some(a));
        }
    }
    if let Some((o, n)) = strip_line_numbers(old, new) {
        if let Some(mut a) = apply_ladder(text, &o, &n, replace_all, label)? {
            let base = "line-number prefixes copied from Read were removed from old_string";
            a.note = Some(match a.note {
                Some(more) => format!("{}; {}", base, more),
                None => base.to_string(),
            });
            return Ok(Some(a));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy)]
enum Strategy {
    TrailingWhitespace,
    Indentation,
}

impl Strategy {
    fn same(self, file_line: &str, old_line: &str) -> bool {
        match self {
            Strategy::TrailingWhitespace => file_line.trim_end() == old_line.trim_end(),
            Strategy::Indentation => file_line.trim() == old_line.trim(),
        }
    }
    fn describe(self) -> &'static str {
        match self {
            Strategy::TrailingWhitespace => "matched after ignoring trailing whitespace and line endings",
            Strategy::Indentation => "matched after ignoring indentation; new_string was re-indented to fit",
        }
    }
}


fn split_keep(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

fn body(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}


fn old_lines(old: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = old.lines().collect();
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn indent_of(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

fn fuzzy_replace(text: &str, old: &str, new: &str, strategy: Strategy, label: &str) -> Result<Option<Applied>, String> {
    let olds = old_lines(old);
    if olds.is_empty() {
        return Ok(None);
    }
    let file = split_keep(text);
    let n = olds.len();
    if file.len() < n {
        return Ok(None);
    }
    let hits: Vec<usize> = (0..=file.len() - n)
        .filter(|&i| (0..n).all(|k| strategy.same(body(file[i + k]), olds[k])))
        .collect();
    match hits.len() {
        0 => return Ok(None),
        1 => {}
        many => {
            return Err(format!(
                "'old_string' was not found exactly, and the loose match ({}) hits {} places in {}. Copy the exact text, with more surrounding lines.",
                strategy.describe(),
                many,
                label
            ))
        }
    }
    let start = hits[0];
    let eol = if file[start].ends_with("\r\n") { "\r\n" } else { "\n" };
    let last_has_eol = file[start + n - 1].ends_with('\n');

    let mut new_lines: Vec<String> = new.lines().map(str::to_string).collect();
    if let Strategy::Indentation = strategy {
        let model_indent = olds.iter().find(|l| !l.trim().is_empty()).map(|l| indent_of(l)).unwrap_or("");
        let file_indent = (0..n)
            .find(|&k| !olds[k].trim().is_empty())
            .map(|k| indent_of(body(file[start + k])))
            .unwrap_or("");



        let level_map: HashMap<&str, &str> = (0..n)
            .filter(|&k| !olds[k].trim().is_empty())
            .map(|k| (indent_of(olds[k]), indent_of(body(file[start + k]))))
            .collect();
        for line in new_lines.iter_mut() {
            if line.trim().is_empty() {
                continue;
            }
            let ind = indent_of(line);
            if let Some(mapped) = level_map.get(ind) {
                *line = format!("{}{}", mapped, line.trim_start());
            } else if let Some(rest) = line.strip_prefix(model_indent) {
                *line = format!("{}{}", file_indent, rest);
            }
        }
    }
    let mut out = String::with_capacity(text.len() + new.len());
    for l in &file[..start] {
        out.push_str(l);
    }
    if !new_lines.is_empty() {
        out.push_str(&new_lines.join(eol));
        if last_has_eol {
            out.push_str(eol);
        }
    }
    for l in &file[start + n..] {
        out.push_str(l);
    }
    Ok(Some(Applied { text: out, count: 1, note: Some(strategy.describe().to_string()) }))
}







fn strip_line_numbers(old: &str, new: &str) -> Option<(String, String)> {
    fn numbered(l: &str) -> Option<(u64, &str)> {
        let t = l.trim_start();
        let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        let num: u64 = t[..digits].parse().ok()?;
        let rest = t[digits..].strip_prefix('\t')?;
        Some((num, rest))
    }
    fn strip_all(s: &str) -> Option<String> {
        let lines: Vec<&str> = s.lines().collect();
        if !lines.iter().any(|l| !l.trim().is_empty()) {
            return None;
        }
        let mut out = Vec::with_capacity(lines.len());
        let mut prev: Option<u64> = None;
        for l in lines {
            if l.trim().is_empty() {
                out.push(String::new());
                continue;
            }
            let (num, rest) = numbered(l)?;

            if let Some(p) = prev {
                if num <= p {
                    return None;
                }
            }
            prev = Some(num);
            out.push(rest.to_string());
        }
        Some(out.join("\n"))
    }
    let o = strip_all(old)?;
    let n = strip_all(new).unwrap_or_else(|| new.to_string());
    Some((o, n))
}


pub fn closest_candidates(text: &str, old: &str) -> String {
    const MIN_SCORE: f64 = 0.5;
    const SHOW_LINES: usize = 8;
    let file: Vec<&str> = text.lines().collect();
    let mut olds = old_lines(old);
    if olds.is_empty() || file.is_empty() {
        return "Read the file again to see its current content.".into();
    }

    if file.len().saturating_mul(olds.len()) > 400_000 {
        olds.truncate(1);
    }
    let n = olds.len().min(file.len());
    let mut scored: Vec<(f64, usize)> = (0..=file.len() - n)
        .map(|i| {
            let s: f64 = (0..n).map(|k| similarity(file[i + k].trim(), olds[k].trim())).sum();
            (s / n as f64, i)
        })
        .filter(|(s, _)| *s >= MIN_SCORE)
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)));
    let mut picked: Vec<(f64, usize)> = Vec::new();
    for (s, i) in scored {
        if picked.iter().all(|(_, j)| i + n <= *j || *j + n <= i) {
            picked.push((s, i));
            if picked.len() == 3 {
                break;
            }
        }
    }
    if picked.is_empty() {
        return "No similar lines were found. Read the file again to see its current content.".into();
    }
    let mut out = String::from("Closest matches in the current file (fix old_string to match one exactly):\n");
    for (s, i) in picked {
        out.push_str(&format!("--- lines {}-{} ({:.0}% similar)\n", i + 1, i + n, s * 100.0));
        for (k, line) in file.iter().enumerate().skip(i).take(n.min(SHOW_LINES)) {
            out.push_str(&format!("{:>6}\t{}\n", k + 1, line));
        }
        if n > SHOW_LINES {
            out.push_str("   ...\n");
        }
    }
    out
}


fn similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let bigrams = |s: &str| {
        let cs: Vec<char> = s.chars().collect();
        let mut m: HashMap<(char, char), usize> = HashMap::new();
        for w in cs.windows(2) {
            *m.entry((w[0], w[1])).or_default() += 1;
        }
        m
    };
    let (x, y) = (bigrams(a), bigrams(b));
    let total: usize = x.values().sum::<usize>() + y.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    let common: usize = x.iter().map(|(k, c)| (*c).min(*y.get(k).unwrap_or(&0))).sum();
    2.0 * common as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str, old: &str, new: &str) -> Applied {
        apply_edit(text, old, new, false, "f.rs").unwrap_or_else(|e| panic!("expected a match: {}", e))
    }

    #[test]
    fn exact_match_wins_without_note() {
        let a = ok("a\n  b\nc\n", "  b", "  B");
        assert_eq!(a.text, "a\n  B\nc\n");
        assert_eq!(a.note, None);
    }

    #[test]
    fn indentation_shifted_old_string_is_applied_and_new_reindented() {
        let text = "fn main() {\n    if x {\n        run();\n    }\n}\n";
        let a = ok(text, "if x {\n    run();\n}", "if x {\n    run();\n    log();\n}");
        assert_eq!(a.text, "fn main() {\n    if x {\n        run();\n        log();\n    }\n}\n");
        assert!(a.note.unwrap().contains("indentation"));
    }

    #[test]
    fn tabs_vs_spaces_match_by_indentation() {
        let text = "fn f() {\n\tlet a = 1;\n\tlet b = 2;\n}\n";
        let a = ok(text, "    let a = 1;\n    let b = 2;", "    let a = 10;\n    let b = 2;");
        assert_eq!(a.text, "fn f() {\n\tlet a = 10;\n\tlet b = 2;\n}\n");
    }

    #[test]
    fn crlf_file_and_trailing_spaces_match_and_keep_crlf() {
        let text = "one\r\ntwo  \r\nthree\r\n";
        let a = ok(text, "one\ntwo\n", "ONE\nTWO\n");
        assert_eq!(a.text, "ONE\r\nTWO\r\nthree\r\n");
        assert!(a.note.unwrap().contains("trailing whitespace"));
    }

    #[test]
    fn line_number_prefixes_from_read_are_stripped() {
        let text = "alpha\nbeta\ngamma\n";
        let a = ok(text, "     2\tbeta\n     3\tgamma", "     2\tBETA\n     3\tgamma");
        assert_eq!(a.text, "alpha\nBETA\ngamma\n");
        assert!(a.note.unwrap().contains("line-number"));
    }

    #[test]
    fn non_monotonic_number_prefixes_are_not_stripped() {


        assert!(strip_line_numbers("123\tfoo\n123\tbar", "x").is_none());
        assert!(strip_line_numbers("5\tfoo\n3\tbar", "x").is_none());
        assert!(strip_line_numbers("1\tfoo\n1\tbar", "x").is_none());

        let (o, _) = strip_line_numbers("     7\tfoo\n     8\tbar", "x").expect("should strip");
        assert_eq!(o, "foo\nbar");

        let (o, _) = strip_line_numbers("7\tfoo\n\n8\tbar", "x").expect("should strip");
        assert_eq!(o, "foo\n\nbar");
    }

    #[test]
    fn ambiguous_fuzzy_match_is_rejected() {
        let text = "a {\n  x();\n}\nb {\n    x();\n}\n";
        let err = apply_edit(text, "x();\n", "y();\n", false, "f.rs").unwrap_err();

        assert!(err.contains("occurs 2 times"), "{}", err);
        let err = apply_edit(text, "      x();", "y();", false, "f.rs").unwrap_err();
        assert!(err.contains("hits 2 places"), "{}", err);
    }

    #[test]
    fn miss_reports_closest_lines_with_numbers() {
        let text = "fn alpha() {}\nfn beta(x: u32) -> u32 { x + 1 }\nfn gamma() {}\n";
        let err = apply_edit(text, "fn beta(x: u64) -> u64 { x + 1 }", "z", false, "f.rs").unwrap_err();
        assert!(err.contains("not found"), "{}", err);
        assert!(err.contains("lines 2-2"), "{}", err);
        assert!(err.contains("     2\tfn beta(x: u32)"), "{}", err);
        let err = apply_edit(text, "completely unrelated words here", "z", false, "f.rs").unwrap_err();
        assert!(err.contains("No similar lines"), "{}", err);
    }

    #[test]
    fn deleting_lines_via_fuzzy_match() {
        let a = ok("keep\n    drop me\nkeep2\n", "drop me \n", "");
        assert_eq!(a.text, "keep\nkeep2\n");
    }
}
