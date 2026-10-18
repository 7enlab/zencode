//! Spend-cap budgeting for the agent loop.
//!
//! Pure logic, no I/O: [`BudgetPolicy`] describes the cap, [`check_budget`]
//! decides whether the next model call may proceed. The agent loop calls
//! [`check_budget`] before every model request; the CLI resolves the policy
//! from `--max-budget-usd` / `ZEN_MAX_BUDGET_USD` / config `maxBudgetUsd`.
//!
//! Pricing (`price_per_1m`) lives here so both `zen-agent` (enforcement) and
//! `zen-cli` (the `zen cost` ledger) share one table. Prices are an
//! informational snapshot (Oct 2026), never quotes. Unknown models return
//! `None` — costs are never guessed, and a cap cannot be enforced on a model
//! whose price is unknown (the call is allowed).

/// Approximate USD price per 1M tokens as `(input, output)` for one bare model
/// name, or `None` when unknown.
fn price_table(model: &str) -> Option<(f64, f64)> {
    Some(match model {
        // OpenAI
        "gpt-4o" => (2.50, 10.00),
        "gpt-4o-mini" => (0.15, 0.60),
        "o1" => (15.00, 60.00),
        "o3-mini" => (1.10, 4.40),
        // Anthropic
        "claude-opus-4-1" => (15.00, 75.00),
        "claude-sonnet-4-5" => (3.00, 15.00),
        "claude-haiku-4-5" | "claude-3-5-haiku-latest" => (0.80, 4.00),
        // DeepSeek
        "deepseek-chat" | "deepseek-v3" => (0.27, 1.10),
        "deepseek-reasoner" | "deepseek-r1" => (0.55, 2.19),
        // Google
        "gemini-2.5-flash" => (0.30, 2.50),
        "gemini-2.5-pro" => (1.25, 10.00),
        // xAI
        "grok-4" => (3.00, 15.00),
        "grok-code-fast-1" => (0.20, 1.50),
        // Moonshot
        "kimi-k2" => (0.60, 2.50),
        _ => return None,
    })
}

/// USD price per 1M tokens as `(input, output)`; `None` = unknown pricing.
/// Known-free models (OpenRouter `:free` suffix, local `ollama/` models)
/// return `Some((0.0, 0.0))`.
pub fn price_per_1m(model: &str) -> Option<(f64, f64)> {
    let m = model.trim().to_lowercase();
    if m.ends_with(":free") || m.starts_with("ollama/") || m == "ollama" {
        return Some((0.0, 0.0));
    }
    if let Some(p) = price_table(&m) {
        return Some(p);
    }
    // Try the bare name after a `provider/` prefix ("openai/gpt-4o" → "gpt-4o").
    if let Some((_, bare)) = m.rsplit_once('/') {
        if let Some(p) = price_table(bare) {
            return Some(p);
        }
    }
    None
}

/// True when the model is known to cost nothing (free tier or local).
/// A spend cap is a no-op for these models: it can never trip.
pub fn is_free_model(model: &str) -> bool {
    matches!(price_per_1m(model), Some((0.0, 0.0)))
}

/// Estimated USD cost of a call with the given token counts.
/// Returns `None` when the model's pricing is unknown (never guessed).
pub fn estimate_cost_usd(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    price_per_1m(model).map(|(pi, po)| {
        input_tokens as f64 / 1_000_000.0 * pi + output_tokens as f64 / 1_000_000.0 * po
    })
}

/// Spend-cap policy for one session.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetPolicy {
    /// Hard cap in USD. The agent loop refuses model calls that would breach it.
    pub max_budget_usd: f64,
    /// Emit a one-time warning when spend crosses this fraction of the cap.
    /// Sensible range is (0, 1]; defaults to 0.8.
    pub warn_at: f64,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_budget_usd: 0.0,
            warn_at: 0.8,
        }
    }
}

impl BudgetPolicy {
    /// Clamp `warn_at` into (0, 1]; non-positive caps are left as-is for the
    /// caller to reject.
    pub fn normalized(mut self) -> Self {
        if self.warn_at <= 0.0 || self.warn_at > 1.0 || !self.warn_at.is_finite() {
            self.warn_at = 0.8;
        }
        self
    }
}

/// Generous output-token ceiling assumed when estimating the next call's
/// cost. The pre-flight check blocks on a ceiling, not a prediction, so it
/// errs toward stopping early rather than overshooting the cap.
pub const DEFAULT_OUTPUT_ESTIMATE_TOKENS: u64 = 4096;

/// The outcome of a pre-flight budget check.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetVerdict {
    /// Proceed with the call.
    Allow,
    /// Proceed, but warn the user once: spend crossed the warning threshold.
    Warn { spent_usd: f64, cap_usd: f64 },
    /// Refuse the call: it would breach the cap.
    Block {
        spent_usd: f64,
        cap_usd: f64,
        est_cost_usd: f64,
    },
}

/// Decide whether the next model call may proceed.
///
/// - `spent_usd`: cumulative session spend so far.
/// - `est_next_cost_usd`: generous ceiling for the upcoming call's cost.
/// - `already_warned`: whether the warn-once warning was already emitted.
///
/// Boundary: `spent + est_next == cap` is allowed (breach means strictly over).
pub fn check_budget(
    policy: &BudgetPolicy,
    spent_usd: f64,
    est_next_cost_usd: f64,
    already_warned: bool,
) -> BudgetVerdict {
    let cap = policy.max_budget_usd;
    if spent_usd + est_next_cost_usd > cap {
        return BudgetVerdict::Block {
            spent_usd,
            cap_usd: cap,
            est_cost_usd: est_next_cost_usd,
        };
    }
    if cap > 0.0 && !already_warned && spent_usd >= policy.warn_at * cap {
        return BudgetVerdict::Warn {
            spent_usd,
            cap_usd: cap,
        };
    }
    BudgetVerdict::Allow
}

/// Human-readable budget-block message for the user.
pub fn block_message(spent_usd: f64, cap_usd: f64, est_cost_usd: f64) -> String {
    format!(
        "Budget exceeded: spent ${:.2} of ${:.2} cap; blocked a model call estimated at ${:.2}. \
         Raise the cap with --max-budget-usd or ZEN_MAX_BUDGET_USD to continue.",
        spent_usd, cap_usd, est_cost_usd
    )
}

/// Human-readable budget-warning message for the user (emitted once per session).
pub fn warn_message(spent_usd: f64, cap_usd: f64) -> String {
    format!(
        "Budget warning: spent ${:.2} of ${:.2} cap ({:.0}% used). \
         The session will hard-stop if the cap is breached.",
        spent_usd,
        cap_usd,
        spent_usd / cap_usd * 100.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_models_price_at_zero() {
        assert_eq!(price_per_1m("nvidia/nemotron-3:free"), Some((0.0, 0.0)));
        assert_eq!(price_per_1m("qwen/qwen3-coder:FREE"), Some((0.0, 0.0)));
        assert_eq!(price_per_1m("ollama/qwen3-coder:30b"), Some((0.0, 0.0)));
        assert!(is_free_model("deepseek/deepseek-chat:free"));
        assert!(!is_free_model("gpt-4o"));
    }

    #[test]
    fn known_models_have_prices() {
        assert_eq!(price_per_1m("gpt-4o"), Some((2.50, 10.00)));
        assert_eq!(price_per_1m("openai/gpt-4o-mini"), Some((0.15, 0.60)));
        assert_eq!(price_per_1m("anthropic/claude-sonnet-4-5"), Some((3.00, 15.00)));
    }

    #[test]
    fn unknown_models_are_none_not_guessed() {
        assert_eq!(price_per_1m("some-future-model-9000"), None);
        assert_eq!(price_per_1m(""), None);
        assert_eq!(estimate_cost_usd("some-future-model-9000", 1000, 1000), None);
    }

    #[test]
    fn estimate_math() {
        // 1M input + 1M output tokens of gpt-4o = $2.50 + $10.00.
        let c = estimate_cost_usd("gpt-4o", 1_000_000, 1_000_000).unwrap();
        assert!((c - 12.50).abs() < 1e-9);
        // Free model: always zero.
        assert_eq!(estimate_cost_usd("x:free", 999_999, 999_999), Some(0.0));
    }

    fn policy(cap: f64) -> BudgetPolicy {
        BudgetPolicy {
            max_budget_usd: cap,
            warn_at: 0.8,
        }
    }

    #[test]
    fn allow_well_under_cap() {
        assert_eq!(
            check_budget(&policy(1.0), 0.10, 0.05, false),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn block_when_next_call_breaches() {
        assert_eq!(
            check_budget(&policy(1.0), 0.95, 0.10, false),
            BudgetVerdict::Block {
                spent_usd: 0.95,
                cap_usd: 1.0,
                est_cost_usd: 0.10,
            }
        );
    }

    #[test]
    fn exact_boundary_is_allowed() {
        // spent + est == cap is NOT a breach (strictly-over rule).
        // (spent stays below the 0.8 warn threshold so no warning fires.)
        assert_eq!(
            check_budget(&policy(1.0), 0.70, 0.30, false),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn warn_once_at_threshold() {
        // 0.85 >= 0.8 * 1.0 → warn (first time).
        assert_eq!(
            check_budget(&policy(1.0), 0.85, 0.01, false),
            BudgetVerdict::Warn {
                spent_usd: 0.85,
                cap_usd: 1.0,
            }
        );
        // Already warned → allow (no repeat warning).
        assert_eq!(
            check_budget(&policy(1.0), 0.85, 0.01, true),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn block_takes_precedence_over_warn() {
        // Over the warn threshold AND breaching → block, not warn.
        assert_eq!(
            check_budget(&policy(1.0), 0.95, 0.10, false),
            BudgetVerdict::Block {
                spent_usd: 0.95,
                cap_usd: 1.0,
                est_cost_usd: 0.10,
            }
        );
    }

    #[test]
    fn zero_cap_blocks_any_priced_call() {
        // A $0 cap means "no paid calls": any positive estimate blocks.
        assert!(matches!(
            check_budget(&policy(0.0), 0.0, 0.01, false),
            BudgetVerdict::Block { .. }
        ));
        // ...but a zero-cost estimate is still allowed (free-tier safety).
        assert_eq!(
            check_budget(&policy(0.0), 0.0, 0.0, false),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn warn_at_normalized() {
        let p = BudgetPolicy {
            max_budget_usd: 5.0,
            warn_at: 99.0,
        }
        .normalized();
        assert_eq!(p.warn_at, 0.8);
        let p = BudgetPolicy {
            max_budget_usd: 5.0,
            warn_at: 0.5,
        }
        .normalized();
        assert_eq!(p.warn_at, 0.5);
    }

    #[test]
    fn messages_format_sane() {
        let m = block_message(1.25, 2.0, 0.90);
        assert!(m.contains("$1.25") && m.contains("$2.00") && m.contains("--max-budget-usd"));
        let w = warn_message(1.60, 2.0);
        assert!(w.contains("$1.60") && w.contains("80%"));
    }
}
