//! Cost tracking and the `zen cost` dashboard (v0.6.0 "The Free Agent").
//!
//! Every agent turn records its token usage into a per-session ledger at
//! `<home>/.zen/cost/<session-id>.json`. `zen cost` renders the ledger as a
//! dashboard: per-model token counts, estimated USD cost, and a prominent
//! `$0.00` when everything ran on free models. Unknown models are reported as
//! "pricing unknown" — costs are never guessed.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One recorded chunk of token usage (a single `-p` run or REPL turn).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostEntry {
    pub model: String,
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub timestamp: String,
}

/// Durable per-session cost ledger.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SessionCost {
    pub session_id: String,
    pub entries: Vec<CostEntry>,
}

/// Per-model rollup inside a [`CostSummary`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    pub model: String,
    pub provider: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` when the model is not in the pricing table (never guessed).
    pub cost_usd: Option<f64>,
}

/// Aggregated cost view of a session.
#[derive(Debug, Clone, PartialEq)]
pub struct CostSummary {
    pub session_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_cost_usd: f64,
    pub per_model: Vec<ModelCost>,
    /// True when every recorded token ran on a known-free model.
    pub all_free: bool,
    /// Models with no pricing entry (excluded from the total).
    pub unknown_pricing: Vec<String>,
}

/// USD price per 1M tokens; single source of truth is `zen_agent::budget`.
pub use zen_agent::budget::price_per_1m;

/// Friendly provider name from an OpenAI-compatible base URL.
pub fn provider_from_base_url(base_url: &str) -> String {
    let u = base_url.trim().to_lowercase();
    let host = u.rsplit("://").next().unwrap_or(&u);
    let host = host.split('/').next().unwrap_or(host);
    let host = host.split('@').next_back().unwrap_or(host);
    // Local endpoints first: they may carry no recognisable host.
    if host.starts_with("localhost")
        || host.starts_with("127.")
        || host.starts_with("0.0.0.0")
        || host.contains(":11434")
    {
        return "local".to_string();
    }
    for (frag, name) in [
        ("openrouter.ai", "openrouter"),
        ("openai.com", "openai"),
        ("anthropic.com", "anthropic"),
        ("generativelanguage.googleapis.com", "google"),
        ("aiplatform.googleapis.com", "google"),
        ("deepseek.com", "deepseek"),
        ("groq.com", "groq"),
        ("mistral.ai", "mistral"),
        ("api.x.ai", "xai"),
        ("experientiallabs.ai", "experientiallabs"),
        ("cerebras.ai", "cerebras"),
        ("nvidia.com", "nvidia"),
    ] {
        if host.contains(frag) {
            return name.to_string();
        }
    }
    if host.is_empty() {
        "unknown".to_string()
    } else {
        host.to_string()
    }
}

fn ledger_path(home: &Path, session_id: &str) -> std::path::PathBuf {
    home.join(".zen")
        .join("cost")
        .join(format!("{}.json", session_id))
}

/// Append one usage entry to the session ledger. Zero-token entries are
/// skipped; write failures are silent — cost tracking must never break a run.
pub fn record(
    home: Option<&Path>,
    session_id: &str,
    model: &str,
    base_url: &str,
    input_tokens: u64,
    output_tokens: u64,
) {
    if input_tokens == 0 && output_tokens == 0 {
        return;
    }
    let Some(home) = home else { return };
    let path = ledger_path(home, session_id);
    let mut ledger = load(home, session_id).unwrap_or(SessionCost {
        session_id: session_id.to_string(),
        entries: Vec::new(),
    });
    ledger.entries.push(CostEntry {
        model: model.to_string(),
        provider: provider_from_base_url(base_url),
        input_tokens,
        output_tokens,
        timestamp: chrono::Utc::now().to_rfc3339(),
    });
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(json) = serde_json::to_string_pretty(&ledger) {
        let _ = std::fs::write(&path, json);
    }
}

/// Load a session's cost ledger; `None` when nothing was recorded.
pub fn load(home: &Path, session_id: &str) -> Option<SessionCost> {
    let data = std::fs::read_to_string(ledger_path(home, session_id)).ok()?;
    serde_json::from_str(&data).ok()
}

/// Session id of the most recently modified transcript across all project
/// directories (used as the `zen cost` default).
pub fn latest_session_id(home: &Path) -> Option<String> {
    let root = home.join(".zen").join("projects");
    let mut latest: Option<(std::time::SystemTime, String)> = None;
    for e in walkdir::WalkDir::new(&root).into_iter().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let mtime = e.metadata().ok().and_then(|m| m.modified().ok())?;
        let id = p.file_stem()?.to_string_lossy().into_owned();
        if latest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            latest = Some((mtime, id));
        }
    }
    latest.map(|(_, id)| id)
}

/// Roll entries up into per-model totals and an overall cost.
pub fn summarize(session_id: &str, entries: &[CostEntry]) -> CostSummary {
    // Group by (model, provider), preserving first-seen order.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();
    for e in entries {
        let key = (e.model.clone(), e.provider.clone());
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        let g = groups.entry(key).or_insert((0, 0));
        g.0 += e.input_tokens;
        g.1 += e.output_tokens;
    }
    let mut per_model = Vec::new();
    let mut unknown_pricing = Vec::new();
    let mut total_in = 0u64;
    let mut total_out = 0u64;
    let mut total_cost = 0.0f64;
    for (model, provider) in order {
        let key = (model.clone(), provider.clone());
        let (input_tokens, output_tokens) = groups[&key];
        total_in += input_tokens;
        total_out += output_tokens;
        let cost_usd = price_per_1m(&model).map(|(pi, po)| {
            input_tokens as f64 / 1_000_000.0 * pi + output_tokens as f64 / 1_000_000.0 * po
        });
        match cost_usd {
            Some(c) => total_cost += c,
            None => {
                if !unknown_pricing.contains(&model) {
                    unknown_pricing.push(model.clone());
                }
            }
        }
        per_model.push(ModelCost {
            model,
            provider,
            input_tokens,
            output_tokens,
            cost_usd,
        });
    }
    let all_free = !entries.is_empty() && unknown_pricing.is_empty() && total_cost == 0.0;
    CostSummary {
        session_id: session_id.to_string(),
        input_tokens: total_in,
        output_tokens: total_out,
        total_cost_usd: total_cost,
        per_model,
        all_free,
        unknown_pricing,
    }
}

/// Format USD for display: `$0.00` for zero, 4 decimals under a cent, 2 above.
pub fn format_usd(v: f64) -> String {
    if v <= 0.0 {
        "$0.00".to_string()
    } else if v < 0.01 {
        format!("${:.4}", v)
    } else {
        format!("${:.2}", v)
    }
}

/// Thousands separator for token counts.
fn fmt_int(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The `zen cost` dashboard (text). Free sessions get a prominent `$0.00`.
pub fn render_text(summary: &CostSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!("Session cost: {}\n", summary.session_id));
    if summary.input_tokens == 0 && summary.output_tokens == 0 {
        out.push_str("No token usage recorded yet.\n");
        return out;
    }
    for m in &summary.per_model {
        let cost = match m.cost_usd {
            Some(c) => format_usd(c),
            None => "pricing unknown".to_string(),
        };
        out.push_str(&format!(
            "  {} ({})\n    {} input · {} output tokens · {}\n",
            m.model,
            m.provider,
            fmt_int(m.input_tokens),
            fmt_int(m.output_tokens),
            cost
        ));
    }
    out.push_str(&format!(
        "Totals: {} input · {} output · {} tokens\n",
        fmt_int(summary.input_tokens),
        fmt_int(summary.output_tokens),
        fmt_int(summary.input_tokens + summary.output_tokens)
    ));
    out.push('\n');
    out.push_str(&format!(
        "Total cost: {}\n",
        format_usd(summary.total_cost_usd)
    ));
    if summary.all_free {
        out.push_str("All usage was on free models — this session cost $0.00.\n");
    } else if !summary.unknown_pricing.is_empty() {
        out.push_str(&format!(
            "Note: pricing unknown for {} — total excludes them.\n",
            summary.unknown_pricing.join(", ")
        ));
    }
    out
}

/// Machine-readable cost summary (for `zen cost --output-format json|stream-json`).
pub fn to_json(summary: &CostSummary) -> serde_json::Value {
    serde_json::json!({
        "type": "cost",
        "session_id": summary.session_id,
        "input_tokens": summary.input_tokens,
        "output_tokens": summary.output_tokens,
        "total_tokens": summary.input_tokens + summary.output_tokens,
        "total_cost_usd": summary.total_cost_usd,
        "all_free": summary.all_free,
        "models": summary.per_model.iter().map(|m| serde_json::json!({
            "model": m.model,
            "provider": m.provider,
            "input_tokens": m.input_tokens,
            "output_tokens": m.output_tokens,
            "cost_usd": m.cost_usd,
            "pricing_known": m.cost_usd.is_some(),
        })).collect::<Vec<_>>(),
        "unknown_pricing": summary.unknown_pricing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(model: &str, provider: &str, input: u64, output: u64) -> CostEntry {
        CostEntry {
            model: model.to_string(),
            provider: provider.to_string(),
            input_tokens: input,
            output_tokens: output,
            timestamp: "2026-10-17T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn free_models_price_at_zero() {
        // OpenRouter `:free` suffix, any case.
        assert_eq!(price_per_1m("nvidia/nemotron-3:free"), Some((0.0, 0.0)));
        assert_eq!(price_per_1m("qwen/qwen3-coder:FREE"), Some((0.0, 0.0)));
        // Local Ollama models.
        assert_eq!(price_per_1m("ollama/qwen3-coder:30b"), Some((0.0, 0.0)));
        assert_eq!(price_per_1m("ollama"), Some((0.0, 0.0)));
    }

    #[test]
    fn known_models_have_prices() {
        assert_eq!(price_per_1m("gpt-4o"), Some((2.50, 10.00)));
        // Provider prefix is stripped before lookup.
        assert_eq!(price_per_1m("openai/gpt-4o-mini"), Some((0.15, 0.60)));
        assert_eq!(price_per_1m("anthropic/claude-sonnet-4-5"), Some((3.00, 15.00)));
    }

    #[test]
    fn unknown_models_are_none_not_guessed() {
        assert_eq!(price_per_1m("some-future-model-9000"), None);
        assert_eq!(price_per_1m(""), None);
    }

    #[test]
    fn provider_detection() {
        assert_eq!(
            provider_from_base_url("https://openrouter.ai/api/v1"),
            "openrouter"
        );
        assert_eq!(
            provider_from_base_url("https://api.openai.com/v1"),
            "openai"
        );
        assert_eq!(
            provider_from_base_url("http://localhost:11434/v1"),
            "local"
        );
        assert_eq!(
            provider_from_base_url("http://127.0.0.1:11434/v1"),
            "local"
        );
        assert_eq!(
            provider_from_base_url("https://api.experientiallabs.ai/api/v1"),
            "experientiallabs"
        );
        assert_eq!(
            provider_from_base_url("https://llm.example-corp.net/v1"),
            "llm.example-corp.net"
        );
    }

    #[test]
    fn usd_formatting() {
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(-1.0), "$0.00");
        assert_eq!(format_usd(0.0012), "$0.0012");
        assert_eq!(format_usd(0.0421), "$0.04");
        assert_eq!(format_usd(12.5), "$12.50");
    }

    #[test]
    fn int_formatting() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1000), "1,000");
        assert_eq!(fmt_int(1234567), "1,234,567");
    }

    #[test]
    fn summarize_free_session() {
        let entries = vec![
            entry("nvidia/nemotron:free", "openrouter", 100_000, 20_000),
            entry("nvidia/nemotron:free", "openrouter", 50_000, 10_000),
            entry("ollama/qwen3-coder:30b", "local", 10_000, 5_000),
        ];
        let s = summarize("sess-1", &entries);
        assert_eq!(s.input_tokens, 160_000);
        assert_eq!(s.output_tokens, 35_000);
        assert_eq!(s.total_cost_usd, 0.0);
        assert!(s.all_free);
        assert!(s.unknown_pricing.is_empty());
        // Grouped by (model, provider): 2 groups.
        assert_eq!(s.per_model.len(), 2);
        assert_eq!(s.per_model[0].input_tokens, 150_000);
    }

    #[test]
    fn summarize_paid_session() {
        let entries = vec![entry("gpt-4o", "openai", 1_000_000, 500_000)];
        let s = summarize("sess-2", &entries);
        // 1M * $2.50 + 0.5M * $10.00 = $7.50
        assert!((s.total_cost_usd - 7.50).abs() < 1e-9);
        assert!(!s.all_free);
    }

    #[test]
    fn summarize_unknown_pricing_flagged() {
        let entries = vec![
            entry("mystery-model-x", "openrouter", 1_000, 500),
            entry("gpt-4o-mini", "openrouter", 1_000, 500),
        ];
        let s = summarize("sess-3", &entries);
        assert_eq!(s.unknown_pricing, vec!["mystery-model-x".to_string()]);
        assert!(!s.all_free, "unknown pricing must not count as free");
        // Known model's cost still counted.
        assert!(s.total_cost_usd > 0.0);
    }

    #[test]
    fn dashboard_shows_prominent_zero_for_free() {
        let entries = vec![entry("qwen/qwen3-coder:free", "openrouter", 12_400, 3_200)];
        let text = render_text(&summarize("sess-4", &entries));
        assert!(text.contains("Total cost: $0.00"));
        assert!(text.contains("this session cost $0.00"));
        assert!(text.contains("12,400 input"));
    }

    #[test]
    fn dashboard_notes_unknown_pricing() {
        let entries = vec![entry("mystery-model-x", "openrouter", 1_000, 500)];
        let text = render_text(&summarize("sess-5", &entries));
        assert!(text.contains("pricing unknown"));
        assert!(text.contains("mystery-model-x"));
    }

    #[test]
    fn dashboard_empty_session() {
        let text = render_text(&summarize("sess-6", &[]));
        assert!(text.contains("No token usage recorded yet"));
    }

    #[test]
    fn record_and_load_roundtrip() {
        let home = std::env::temp_dir().join(format!("zen_cost_{}", uuid::Uuid::new_v4()));
        record(
            Some(&home),
            "sess-rt",
            "nvidia/nemotron:free",
            "https://openrouter.ai/api/v1",
            1_000,
            200,
        );
        // Zero-token records are skipped.
        record(Some(&home), "sess-rt", "x", "https://openrouter.ai/api/v1", 0, 0);
        let loaded = load(&home, "sess-rt").expect("ledger written");
        assert_eq!(loaded.session_id, "sess-rt");
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].model, "nvidia/nemotron:free");
        assert_eq!(loaded.entries[0].provider, "openrouter");
        assert_eq!(loaded.entries[0].input_tokens, 1_000);
        // Missing session → None.
        assert!(load(&home, "nope").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn json_shape_is_machine_readable() {
        let entries = vec![entry("gpt-4o", "openai", 2_000_000, 1_000_000)];
        let v = to_json(&summarize("sess-7", &entries));
        assert_eq!(v["type"], "cost");
        assert_eq!(v["session_id"], "sess-7");
        assert_eq!(v["input_tokens"], 2_000_000);
        assert_eq!(v["all_free"], false);
        assert_eq!(v["models"][0]["pricing_known"], true);
        // 2M * $2.50 + 1M * $10.00 = $15.00
        assert!((v["total_cost_usd"].as_f64().unwrap() - 15.0).abs() < 1e-9);
    }
}
