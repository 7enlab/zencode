//! Tool-call repair (r7, research item #2).
//!
//! Free models often send a tool call that is *almost* right: arguments wrapped in a Markdown
//! fence, JSON with a trailing comma or single quotes, arguments double-encoded as a JSON string,
//! `"20"` where the schema wants an integer, or a tool name such as `str_replace` / `bash` that
//! does not match the registry. Rejecting those costs a whole model round trip and often starts a
//! loop. This module fixes what can be fixed *before* dispatch and says what it changed, so the
//! loop can run the call and tell the model about the repair in the tool result.
//!
//! Everything here is pure (no I/O) so it is tested table-driven in this file.

use serde_json::{Map, Value};

/// Map a tool name the registry does not know to one it does: exact match ignoring case and
/// punctuation, an optional `functions.` / `tools.` prefix, or a small alias table of names other
/// harnesses use. Returns `None` when nothing plausible matches (the caller reports the unknown
/// tool as before).
pub fn resolve_tool_name(name: &str, known: &[String]) -> Option<String> {
    let cleaned = strip_template_tokens(name);
    let trimmed = cleaned
        .trim()
        .trim_start_matches("functions.")
        .trim_start_matches("tools.");
    let key = normalize_name(trimmed);
    if key.is_empty() {
        return None;
    }
    if let Some(hit) = known.iter().find(|k| normalize_name(k) == key) {
        return Some(hit.clone());
    }
    let target = alias_target(&key)?;
    known.iter().find(|k| k.as_str() == target).cloned()
}

/// Cut chat-template tokens that leaked into a tool name. gpt-oss served by vLLM sometimes
/// returns names like `Bash<|channel|>commentary` (often with a zero-width space after `<`) or
/// `assistant to=functions.Read`; left in history they also make the server reject the next
/// request ("unexpected tokens remaining in message header").
/// Source: https://github.com/vllm-project/vllm/issues/32587 (r7 C4).
pub fn strip_template_tokens(name: &str) -> String {
    let s: String = name.chars().filter(|c| !matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')).collect();
    let s = match s.find("to=") {
        Some(i) => &s[i + 3..],
        None => s.as_str(),
    };
    let s = s.split("<|").next().unwrap_or("");
    s.split(|c: char| c.is_whitespace() || c == '<').find(|p| !p.is_empty()).unwrap_or("").to_string()
}

/// True when [`strip_template_tokens`] would change `name` (used to word the repair note
/// without echoing the leaked tokens back into history).
pub fn has_template_tokens(name: &str) -> bool {
    name.contains("<|") || name.contains("|>") || name.contains("to=") || name.contains('\u{200B}')
}

fn normalize_name(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Names other agents use for the same job, keyed by their normalized form.
fn alias_target(key: &str) -> Option<&'static str> {
    const TABLE: &[(&str, &[&str])] = &[
        ("Edit", &["strreplace", "strreplaceeditor", "replace", "editfile", "replaceinfile", "applyedit"]),
        ("MultiEdit", &["multireplace", "editmany", "batchedit"]),
        (
            "Bash",
            &["shell", "sh", "run", "runcommand", "executecommand", "exec", "terminal", "runshellcommand", "command", "runterminalcmd"],
        ),
        ("Read", &["readfile", "view", "viewfile", "cat", "openfile", "open"]),
        ("Write", &["writefile", "createfile", "writetofile", "savefile"]),
        ("Grep", &["search", "searchfiles", "grepsearch", "ripgrep", "rg", "findtext", "codesearch"]),
        ("Glob", &["findfiles", "listfiles", "filesearch", "ls", "listdir"]),
        ("TodoWrite", &["todo", "todos", "updatetodos", "writetodos", "todolist"]),
        ("WebFetch", &["fetch", "fetchurl", "httpget", "browse"]),
    ];
    TABLE.iter().find(|(_, names)| names.contains(&key)).map(|(t, _)| *t)
}

/// Parse a tool call's argument string into a JSON object, repairing common damage, then coerce
/// it to `schema` when one is given. Returns the object plus one human-readable note per repair
/// (empty when the call was already well-formed). `Err` carries a message for the model that
/// lists the fields the schema expects.
pub fn repair_args(raw: &str, schema: Option<&Value>) -> Result<(Value, Vec<String>), String> {
    let mut notes = Vec::new();
    let mut obj = match parse_object(raw, &mut notes) {
        Ok(o) => o,
        Err(e) => {
            let hint = schema.map(schema_hint).unwrap_or_default();
            return Err(if hint.is_empty() { e } else { format!("{}. {}", e, hint) });
        }
    };
    if let Some(s) = schema {
        coerce_object(&mut obj, s, "", &mut notes);
    }
    Ok((Value::Object(obj), notes))
}

fn parse_object(raw: &str, notes: &mut Vec<String>) -> Result<Map<String, Value>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if !raw.is_empty() {
            notes.push("blank arguments treated as {}".into());
        }
        return Ok(Map::new());
    }
    // Strict first: a well-formed call must come through untouched and without notes.
    let strict_err = match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => return into_object(v, notes),
        Err(e) => e.to_string(),
    };
    let unfenced = strip_fences(trimmed);
    if unfenced != trimmed {
        notes.push("removed code fence around arguments".into());
        if let Ok(v) = serde_json::from_str::<Value>(unfenced) {
            return into_object(v, notes);
        }
    }
    let body = extract_braced(unfenced).unwrap_or(unfenced);
    let fixed = normalize_json_text(body);
    match serde_json::from_str::<Value>(&fixed) {
        Ok(v) => {
            notes.push("repaired malformed JSON".into());
            into_object(v, notes)
        }
        Err(_) => Err(strict_err),
    }
}

fn into_object(v: Value, notes: &mut Vec<String>) -> Result<Map<String, Value>, String> {
    match v {
        Value::Object(o) => Ok(o),
        Value::Null => Ok(Map::new()),
        // Double-encoded: `"{\"a\":1}"`. Unwrap once (with the same repairs).
        Value::String(inner) => {
            notes.push("arguments were a JSON string; decoded it".into());
            let mut inner_notes = Vec::new();
            let o = parse_object(&inner, &mut inner_notes)?;
            notes.extend(inner_notes);
            Ok(o)
        }
        other => Err(format!("arguments must be a JSON object, got {}", type_name(&other))),
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// ```` ```json {..} ``` ```` → `{..}`; also a bare leading `json` word.
fn strip_fences(s: &str) -> &str {
    let mut t = s;
    if let Some(rest) = t.strip_prefix("```") {
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        t = rest.trim();
        if let Some(body) = t.strip_suffix("```") {
            t = body.trim();
        }
    } else if let Some(rest) = t.strip_prefix("json") {
        if rest.starts_with(|c: char| c.is_whitespace() || c == '{') {
            t = rest.trim();
        }
    }
    t
}

/// Text from the first `{` to the last `}` when the model wrapped JSON in prose.
fn extract_braced(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    match s.rfind('}') {
        Some(end) if end > start => Some(&s[start..=end]),
        // Truncated object: keep everything after the brace and let balancing close it.
        _ => Some(&s[start..]),
    }
}

/// One pass over JSON-ish text: single-quoted strings become double-quoted, raw control
/// characters inside strings are escaped, trailing commas are dropped, Python literals become JSON
/// literals, and unclosed strings / brackets are closed at the end.
fn normalize_json_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut stack: Vec<char> = Vec::new();
    // Quote char of the string we are inside, if any.
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            match c {
                '\\' if i + 1 < chars.len() => {
                    let n = chars[i + 1];
                    if q == '\'' && n == '\'' {
                        out.push('\'');
                    } else {
                        out.push('\\');
                        out.push(n);
                    }
                    i += 2;
                    continue;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '"' if q == '\'' => out.push_str("\\\""),
                _ if c == q => {
                    out.push('"');
                    quote = None;
                }
                _ => out.push(c),
            }
            i += 1;
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push('"');
            }
            '{' => {
                stack.push('}');
                out.push(c);
            }
            '[' => {
                stack.push(']');
                out.push(c);
            }
            '}' | ']' => {
                if stack.last() == Some(&c) {
                    stack.pop();
                }
                out.push(c);
            }
            ',' => {
                let next = chars[i + 1..].iter().find(|ch| !ch.is_whitespace());
                if !matches!(next, Some('}') | Some(']') | None) {
                    out.push(c);
                }
            }
            _ if c.is_ascii_alphabetic() => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                out.push_str(match word.as_str() {
                    "True" => "true",
                    "False" => "false",
                    "None" => "null",
                    w => w,
                });
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    if quote.is_some() {
        out.push('"');
    }
    // A dangling comma before the closers we are about to add.
    while out.trim_end().ends_with(',') {
        let keep = out.trim_end().len() - 1;
        out.truncate(keep);
    }
    while let Some(close) = stack.pop() {
        out.push(close);
    }
    out
}

/// The first non-null JSON-schema type of a property.
fn schema_type(prop: &Value) -> Option<&str> {
    match prop.get("type")? {
        Value::String(s) => Some(s.as_str()),
        Value::Array(a) => a.iter().filter_map(|t| t.as_str()).find(|t| *t != "null"),
        _ => None,
    }
}

/// Coerce the values of `obj` to the property types of an object `schema`, rename near-miss keys
/// and drop unknown keys when the schema forbids extra properties. `path` prefixes notes for
/// nested objects (e.g. `edits[1].`).
fn coerce_object(obj: &mut Map<String, Value>, schema: &Value, path: &str, notes: &mut Vec<String>) {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else { return };
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));

    let keys: Vec<String> = obj.keys().cloned().collect();
    for key in keys {
        if props.contains_key(&key) {
            continue;
        }
        if let Some(target) = near_miss_key(&key, props, obj) {
            let v = obj.remove(&key).unwrap();
            notes.push(format!("renamed field '{}{}' to '{}'", path, key, target));
            obj.insert(target, v);
        } else if closed {
            obj.remove(&key);
            notes.push(format!("dropped unknown field '{}{}'", path, key));
        }
    }

    for (key, prop) in props {
        let Some(v) = obj.get_mut(key) else { continue };
        let Some(ty) = schema_type(prop) else { continue };
        if let Some(new) = coerce_value(v, ty) {
            notes.push(format!("converted '{}{}' to {}", path, key, ty));
            *v = new;
        }
        match (ty, obj.get_mut(key)) {
            ("object", Some(Value::Object(inner))) => {
                coerce_object(inner, prop, &format!("{}{}.", path, key), notes)
            }
            ("array", Some(Value::Array(items))) => {
                if let Some(item_schema) = prop.get("items") {
                    if schema_type(item_schema) == Some("object") {
                        for (i, item) in items.iter_mut().enumerate() {
                            if let Value::Object(inner) = item {
                                let p = format!("{}{}[{}].", path, key, i);
                                coerce_object(inner, item_schema, &p, notes);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `filePath` → `file_path`, `path`/`file`/`filename` → `file_path`, `cmd` → `command`, when the
/// target is a declared property the call did not already set.
fn near_miss_key(key: &str, props: &Map<String, Value>, obj: &Map<String, Value>) -> Option<String> {
    let norm = normalize_name(key);
    let free = |name: &str| props.contains_key(name) && !obj.contains_key(name);
    if let Some(hit) = props.keys().find(|p| normalize_name(p) == norm && !obj.contains_key(p.as_str())) {
        return Some(hit.clone());
    }
    let target = match norm.as_str() {
        "path" | "file" | "filename" | "filepath" => "file_path",
        "cmd" | "script" => "command",
        _ => return None,
    };
    free(target).then(|| target.to_string())
}

/// The coerced value when `v` does not already have JSON type `ty` and converts cleanly.
fn coerce_value(v: &Value, ty: &str) -> Option<Value> {
    match (ty, v) {
        ("integer", Value::String(s)) => s.trim().parse::<i64>().ok().map(Value::from),
        ("integer", Value::Number(n)) if n.is_f64() => {
            let f = n.as_f64()?;
            (f.fract() == 0.0).then(|| Value::from(f as i64))
        }
        ("number", Value::String(s)) => s.trim().parse::<f64>().ok().and_then(|f| serde_json::Number::from_f64(f).map(Value::Number)),
        ("boolean", Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        ("string", Value::Number(n)) => Some(Value::String(n.to_string())),
        ("string", Value::Bool(b)) => Some(Value::String(b.to_string())),
        // e.g. Write `content` sent as a JSON object for a package.json.
        ("string", Value::Object(_)) | ("string", Value::Array(_)) => serde_json::to_string_pretty(v).ok().map(Value::String),
        ("array", Value::String(s)) => match serde_json::from_str::<Value>(s.trim()) {
            Ok(a @ Value::Array(_)) => Some(a),
            _ => None,
        },
        ("object", Value::String(s)) => match serde_json::from_str::<Value>(s.trim()) {
            Ok(o @ Value::Object(_)) => Some(o),
            _ => None,
        },
        _ => None,
    }
}

/// A model that "calls" a tool by writing the call as text (`{"name": "Write", "parameters":
/// {...}}`, optionally fenced or wrapped in `<tool_call>` tags) instead of using the tool-call
/// channel. Returns the resolved tool name and the arguments as a JSON string when the whole
/// message is exactly one such call for a known tool; prose that merely contains JSON is left
/// alone.
pub fn tool_call_in_text(text: &str, known: &[String]) -> Option<(String, String)> {
    let mut t = text.trim();
    if let Some(inner) = t.strip_prefix("<tool_call>") {
        t = inner.trim().strip_suffix("</tool_call>").unwrap_or(inner).trim();
    }
    let t = strip_fences(t);
    if !t.starts_with('{') || !t.ends_with('}') {
        return None;
    }
    let v: Value = serde_json::from_str(t).or_else(|_| serde_json::from_str(&normalize_json_text(t))).ok()?;
    let obj = v.as_object()?;
    let raw_name = ["name", "tool", "function"].iter().find_map(|k| obj.get(*k)?.as_str())?;
    let name = if known.iter().any(|k| k == raw_name) {
        raw_name.to_string()
    } else {
        resolve_tool_name(raw_name, known)?
    };
    let args = ["arguments", "parameters", "input", "args"].iter().find_map(|k| obj.get(*k))?;
    let args = match args {
        Value::String(s) => s.clone(),
        Value::Object(_) => args.to_string(),
        _ => return None,
    };
    Some((name, args))
}

/// "Expected a JSON object with fields: file_path (string, required), limit (integer)".
pub fn schema_hint(schema: &Value) -> String {
    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else { return String::new() };
    if props.is_empty() {
        return String::new();
    }
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let mut names: Vec<&String> = props.keys().collect();
    // Required fields first, then the schema's own order.
    names.sort_by_key(|n| !required.contains(&n.as_str()));
    let fields: Vec<String> = names
        .iter()
        .map(|n| {
            let ty = schema_type(&props[n.as_str()]).unwrap_or("any");
            if required.contains(&n.as_str()) {
                format!("{} ({}, required)", n, ty)
            } else {
                format!("{} ({})", n, ty)
            }
        })
        .collect();
    format!("Expected a JSON object with fields: {}", fields.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edit_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean"}
            },
            "required": ["file_path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn read_schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "offset": {"type": "integer"},
                "limit": {"type": "integer"}
            },
            "required": ["file_path"],
            "additionalProperties": false
        })
    }

    /// Malformed argument strings in the shapes free models actually emit, each with the object
    /// it must repair to.
    #[test]
    fn malformed_arguments_repair_to_expected_objects() {
        let cases: Vec<(&str, Value)> = vec![
            ("```json\n{\"file_path\": \"a.rs\"}\n```", json!({"file_path": "a.rs"})),
            ("```json{\"file_path\":\"a.rs\"}```", json!({"file_path": "a.rs"})),
            ("```\n{\"file_path\": \"a.rs\"}\n```", json!({"file_path": "a.rs"})),
            ("json {\"file_path\": \"a.rs\"}", json!({"file_path": "a.rs"})),
            ("{\"file_path\": \"a.rs\",}", json!({"file_path": "a.rs"})),
            ("{'file_path': 'a.rs'}", json!({"file_path": "a.rs"})),
            ("{'file_path': 'say \"hi\".rs'}", json!({"file_path": "say \"hi\".rs"})),
            ("\"{\\\"file_path\\\": \\\"a.rs\\\"}\"", json!({"file_path": "a.rs"})),
            ("{\"file_path\": \"a.rs\"", json!({"file_path": "a.rs"})),
            ("{\"file_path\": \"a.rs", json!({"file_path": "a.rs"})),
            ("Here you go: {\"file_path\": \"a.rs\"} hope that helps", json!({"file_path": "a.rs"})),
            ("{\"file_path\": \"a\nb.rs\"}", json!({"file_path": "a\nb.rs"})),
            ("{\"file_path\": \"a.rs\", \"limit\": \"20\", \"offset\": 3.0}", json!({"file_path": "a.rs", "limit": 20, "offset": 3})),
            ("{\"filePath\": \"a.rs\"}", json!({"file_path": "a.rs"})),
            ("{\"path\": \"a.rs\", \"bogus\": 1}", json!({"file_path": "a.rs"})),
            ("  ", json!({})),
        ];
        for (raw, want) in cases {
            let (got, notes) = repair_args(raw, Some(&read_schema()))
                .unwrap_or_else(|e| panic!("{:?} should repair, got error {}", raw, e));
            assert_eq!(got, want, "input {:?}", raw);
            assert!(!notes.is_empty(), "input {:?} was repaired but produced no note", raw);
        }
    }

    #[test]
    fn well_formed_call_is_untouched_and_silent() {
        let (v, notes) = repair_args(r#"{"file_path":"a.rs","limit":5}"#, Some(&read_schema())).unwrap();
        assert_eq!(v, json!({"file_path": "a.rs", "limit": 5}));
        assert!(notes.is_empty(), "{:?}", notes);
        let (v, notes) = repair_args("", None).unwrap();
        assert_eq!(v, json!({}));
        assert!(notes.is_empty());
    }

    #[test]
    fn types_coerce_to_schema() {
        let raw = r#"{"file_path": "a", "old_string": 1, "new_string": true, "replace_all": "TRUE"}"#;
        let (v, _) = repair_args(raw, Some(&edit_schema())).unwrap();
        assert_eq!(v, json!({"file_path": "a", "old_string": "1", "new_string": "true", "replace_all": true}));
        let write = json!({"type":"object","properties":{"content":{"type":"string"}}});
        let (v, notes) = repair_args(r#"{"content": {"name": "x"}}"#, Some(&write)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(v["content"].as_str().unwrap()).unwrap(), json!({"name": "x"}));
        assert_eq!(notes, vec!["converted 'content' to string".to_string()]);
    }

    #[test]
    fn nested_array_items_are_coerced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "edits": {"type": "array", "items": edit_schema()}
            }
        });
        let raw = r#"{"file_path":"a","edits":"[{\"old_string\":\"x\",\"new_string\":2,\"replace_all\":\"false\"}]"}"#;
        let (v, _) = repair_args(raw, Some(&schema)).unwrap();
        assert_eq!(v["edits"], json!([{"old_string": "x", "new_string": "2", "replace_all": false}]));
    }

    #[test]
    fn unrepairable_arguments_error_lists_schema_fields() {
        for raw in ["{not json", "[1,2]", "42", "{\"a\": }"] {
            let err = repair_args(raw, Some(&read_schema())).unwrap_err();
            assert!(err.contains("file_path (string, required)"), "{:?} -> {}", raw, err);
            assert!(err.contains("limit (integer)"), "{}", err);
        }
    }

    #[test]
    fn coercion_never_invents_values() {
        let (v, _) = repair_args(r#"{"file_path":"a","limit":"lots"}"#, Some(&read_schema())).unwrap();
        assert_eq!(v["limit"], json!("lots"), "an unconvertible value is left for the tool to reject");
    }

    #[test]
    fn tool_call_written_as_text_is_recognized() {
        let known: Vec<String> = ["Read", "Write"].iter().map(|s| s.to_string()).collect();
        let (n, a) = tool_call_in_text(r#"{"name": "Write", "parameters": {"file_path": "a.py", "content": "x"}}"#, &known).unwrap();
        assert_eq!(n, "Write");
        assert_eq!(serde_json::from_str::<Value>(&a).unwrap(), json!({"file_path": "a.py", "content": "x"}));
        let fenced = "```json\n{\"name\": \"read_file\", \"arguments\": {\"file_path\": \"b\"}}\n```";
        let (n, a) = tool_call_in_text(fenced, &known).unwrap();
        assert_eq!(n, "Read");
        assert_eq!(serde_json::from_str::<Value>(&a).unwrap(), json!({"file_path": "b"}));
        assert!(tool_call_in_text("<tool_call>{\"name\":\"Read\",\"arguments\":{\"file_path\":\"c\"}}</tool_call>", &known).is_some());
        // Not a call: prose around JSON, unknown tool, missing arguments, plain answers.
        assert!(tool_call_in_text(r#"Run this: {"name": "Read", "arguments": {}}"#, &known).is_none());
        assert!(tool_call_in_text(r#"{"name": "Deploy", "arguments": {}}"#, &known).is_none());
        assert!(tool_call_in_text(r#"{"name": "Read"}"#, &known).is_none());
        assert!(tool_call_in_text(r#"{"name": "Ann", "age": 3}"#, &known).is_none());
        assert!(tool_call_in_text("All tests pass.", &known).is_none());
    }

    #[test]
    fn tool_names_map_to_registry() {
        let known: Vec<String> =
            ["Read", "Write", "Edit", "Bash", "Glob", "Grep", "TodoWrite", "mcp__fs__list"].iter().map(|s| s.to_string()).collect();
        let cases = [
            ("bash", Some("Bash")),
            ("BASH", Some("Bash")),
            ("str_replace", Some("Edit")),
            ("str_replace_editor", Some("Edit")),
            ("read_file", Some("Read")),
            ("functions.Read", Some("Read")),
            ("todo_write", Some("TodoWrite")),
            ("run_command", Some("Bash")),
            ("mcp-fs-list", Some("mcp__fs__list")),
            ("Bash<\u{200B}|channel|>commentary", Some("Bash")),
            ("Edit<|channel|>commentary<|channel|>commentary", Some("Edit")),
            ("Read<|channel|>", Some("Read")),
            ("assistant to=functions.Read", Some("Read")),
            ("to=functions.Bash <commentary>", Some("Bash")),
            ("<|channel|>commentary", None),
            ("fetch", None), // WebFetch is not registered here
            ("frobnicate", None),
            ("", None),
        ];
        for (input, want) in cases {
            assert_eq!(resolve_tool_name(input, &known).as_deref(), want, "{}", input);
        }
    }
}
