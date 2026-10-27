fn price_table(model: &str) -> Option<(f64, f64)> {
    Some(match model {

        "gpt-4o" => (2.50, 10.00),
        "gpt-4o-mini" => (0.15, 0.60),
        "o1" => (15.00, 60.00),
        "o3-mini" => (1.10, 4.40),

        "claude-opus-4-1" => (15.00, 75.00),
        "claude-sonnet-4-5" => (3.00, 15.00),
        "claude-haiku-4-5" | "claude-3-5-haiku-latest" => (0.80, 4.00),

        "deepseek-chat" | "deepseek-v3" => (0.27, 1.10),
        "deepseek-reasoner" | "deepseek-r1" => (0.55, 2.19),

        "gemini-2.5-flash" => (0.30, 2.50),
        "gemini-2.5-pro" => (1.25, 10.00),

        "grok-4" => (3.00, 15.00),
        "grok-code-fast-1" => (0.20, 1.50),

        "kimi-k2" => (0.60, 2.50),
        _ => return None,
    })
}

pub fn price_per_1m(model: &str) -> Option<(f64, f64)> {
    let m = model.trim().to_lowercase();
    if m.ends_with(":free") || m.starts_with("ollama/") || m == "ollama" {
        return Some((0.0, 0.0));
    }
    if let Some(p) = price_table(&m) {
        return Some(p);
    }

    if let Some((_, bare)) = m.rsplit_once('/') {
        if let Some(p) = price_table(bare) {
            return Some(p);
        }
    }
    None
}

pub fn is_free_model(model: &str) -> bool {
    matches!(price_per_1m(model), Some((0.0, 0.0)))
}

pub fn estimate_cost_usd(model: &str, input_tokens: u64, output_tokens: u64) -> Option<f64> {
    price_per_1m(model).map(|(pi, po)| {
        input_tokens as f64 / 1_000_000.0 * pi + output_tokens as f64 / 1_000_000.0 * po
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct BudgetPolicy {

    pub max_budget_usd: f64,

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

    pub fn normalized(mut self) -> Self {
        if self.warn_at <= 0.0 || self.warn_at > 1.0 || !self.warn_at.is_finite() {
            self.warn_at = 0.8;
        }
        self
    }
}

pub const DEFAULT_OUTPUT_ESTIMATE_TOKENS: u64 = 4096;

#[derive(Debug, Clone, PartialEq)]
pub enum BudgetVerdict {

    Allow,

    Warn { spent_usd: f64, cap_usd: f64 },

    Block {
        spent_usd: f64,
        cap_usd: f64,
        est_cost_usd: f64,
    },
}

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

pub fn block_message(spent_usd: f64, cap_usd: f64, est_cost_usd: f64) -> String {
    format!(
        "Budget exceeded: spent ${:.2} of ${:.2} cap; blocked a model call estimated at ${:.2}. \
         Raise the cap with --max-budget-usd or ZEN_MAX_BUDGET_USD to continue.",
        spent_usd, cap_usd, est_cost_usd
    )
}

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

        let c = estimate_cost_usd("gpt-4o", 1_000_000, 1_000_000).unwrap();
        assert!((c - 12.50).abs() < 1e-9);

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

        assert_eq!(
            check_budget(&policy(1.0), 0.70, 0.30, false),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn warn_once_at_threshold() {

        assert_eq!(
            check_budget(&policy(1.0), 0.85, 0.01, false),
            BudgetVerdict::Warn {
                spent_usd: 0.85,
                cap_usd: 1.0,
            }
        );

        assert_eq!(
            check_budget(&policy(1.0), 0.85, 0.01, true),
            BudgetVerdict::Allow
        );
    }

    #[test]
    fn block_takes_precedence_over_warn() {

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

        assert!(matches!(
            check_budget(&policy(0.0), 0.0, 0.01, false),
            BudgetVerdict::Block { .. }
        ));

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
