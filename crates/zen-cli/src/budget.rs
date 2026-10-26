





use zen_agent::budget::BudgetPolicy;





pub fn resolve_budget(
    cli_max: Option<f64>,
    cli_warn_at: Option<f64>,
    cfg_max: Option<f64>,
    cfg_warn_at: Option<f64>,
) -> Option<BudgetPolicy> {
    let max = cli_max
        .or_else(|| std::env::var("ZEN_MAX_BUDGET_USD").ok().and_then(|v| v.parse().ok()))
        .or(cfg_max);
    let max = match max {
        Some(m) if m > 0.0 && m.is_finite() => m,
        Some(m) => {
            eprintln!(
                "warning: ignoring invalid --max-budget-usd value ({m}): must be a positive number"
            );
            return None;
        }
        None => return None,
    };
    let warn_at = cli_warn_at
        .or_else(|| {
            std::env::var("ZEN_BUDGET_WARN_AT")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .or(cfg_warn_at)
        .unwrap_or(0.8);
    Some(
        BudgetPolicy {
            max_budget_usd: max,
            warn_at,
        }
        .normalized(),
    )
}


pub fn budget_line(policy: &BudgetPolicy, spent_usd: f64) -> String {
    let pct = if policy.max_budget_usd > 0.0 {
        spent_usd / policy.max_budget_usd * 100.0
    } else {
        0.0
    };
    format!(
        "Budget ${:.2} · spent ${:.2} ({:.0}%){}",
        policy.max_budget_usd,
        spent_usd,
        pct,
        if spent_usd > policy.max_budget_usd {
            " - OVER BUDGET"
        } else if pct >= policy.warn_at * 100.0 {
            " - over warn threshold"
        } else {
            ""
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};


    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn unset_env() {
        std::env::remove_var("ZEN_MAX_BUDGET_USD");
        std::env::remove_var("ZEN_BUDGET_WARN_AT");
    }

    #[test]
    fn no_cap_anywhere_is_none() {
        let _g = env_lock();
        unset_env();
        assert_eq!(resolve_budget(None, None, None, None), None);
    }

    #[test]
    fn flag_beats_env_beats_config() {
        let _g = env_lock();
        unset_env();
        std::env::set_var("ZEN_MAX_BUDGET_USD", "3.0");

        let p = resolve_budget(None, None, Some(5.0), None).unwrap();
        assert_eq!(p.max_budget_usd, 3.0);

        let p = resolve_budget(Some(7.0), None, Some(5.0), None).unwrap();
        assert_eq!(p.max_budget_usd, 7.0);
        unset_env();
    }

    #[test]
    fn warn_threshold_precedence_and_default() {
        let _g = env_lock();
        unset_env();
        let p = resolve_budget(Some(5.0), None, None, None).unwrap();
        assert_eq!(p.warn_at, 0.8);
        let p = resolve_budget(Some(5.0), Some(0.5), None, Some(0.9)).unwrap();
        assert_eq!(p.warn_at, 0.5);
        std::env::set_var("ZEN_BUDGET_WARN_AT", "0.6");
        let p = resolve_budget(Some(5.0), None, None, Some(0.9)).unwrap();
        assert_eq!(p.warn_at, 0.6);
        unset_env();
    }

    #[test]
    fn invalid_values_are_ignored() {
        let _g = env_lock();
        unset_env();
        assert_eq!(resolve_budget(Some(0.0), None, None, None), None);
        assert_eq!(resolve_budget(Some(-2.0), None, None, None), None);
        assert_eq!(resolve_budget(Some(f64::INFINITY), None, None, None), None);
        std::env::set_var("ZEN_MAX_BUDGET_USD", "not-a-number");
        assert_eq!(resolve_budget(None, None, None, None), None);
        unset_env();
    }

    #[test]
    fn warn_at_out_of_range_normalized() {
        let _g = env_lock();
        unset_env();
        let p = resolve_budget(Some(5.0), Some(5.0), None, None).unwrap();
        assert_eq!(p.warn_at, 0.8);
    }

    #[test]
    fn budget_line_marks_thresholds() {
        let p = BudgetPolicy {
            max_budget_usd: 4.0,
            warn_at: 0.8,
        };
        let line = budget_line(&p, 1.0);
        assert!(line.contains("$4.00") && line.contains("$1.00") && line.contains("25%"));
        assert!(!line.contains("warn threshold"));
        let line = budget_line(&p, 3.5);
        assert!(line.contains("over warn threshold"));
        let line = budget_line(&p, 4.5);
        assert!(line.contains("OVER BUDGET"));
    }
}
