use zen_agent::safety as agent_safety;

pub use agent_safety::{
    auth_login_warning, concurrency_cap_warning, first_429_notice, parallel_sessions_warning,
    quota_exhausted_error, repeated_429_warning, SAFETY_DISCLAIMER,
};

pub fn check_parallel_sessions(
    provider: &str,
    api_key: Option<&str>,
    session_id: &str,
) -> Option<String> {
    let key = api_key.filter(|k| !k.is_empty())?;
    let fingerprint = agent_safety::key_fingerprint(key);
    let mut registry = agent_safety::SessionRegistry::load();
    let others = registry.register(provider, &fingerprint, session_id);
    if others > agent_safety::PARALLEL_SESSION_WARN_THRESHOLD {
        Some(parallel_sessions_warning(others + 1, provider))
    } else {
        None
    }
}

pub fn doctor_safety_checklist(version: &str) -> String {
    let mut out = String::new();
    out.push_str("  account-safety:\n");
    out.push_str(&format!(
        "    - User-Agent identifies ZenCode/{version} on all requests: yes\n"
    ));
    out.push_str(&format!(
        "    - exponential backoff with jitter (max {} attempts): enabled\n",
        agent_safety::MAX_RETRY_ATTEMPTS + 1
    ));
    out.push_str("    - `insufficient_quota` never retried: yes\n");
    out.push_str(&format!(
        "    - per-provider concurrency cap: {}\n",
        agent_safety::DEFAULT_CONCURRENCY_CAP
    ));
    out.push_str(&format!(
        "    - inter-request floor: {} ms\n",
        agent_safety::MIN_INTER_REQUEST_MS
    ));
    out.push_str(&format!("    - {SAFETY_DISCLAIMER}\n"));
    out
}

pub fn auth_login_safety_summary(provider: &str) -> String {
    auth_login_warning(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checklist_mentions_key_safeguards() {
        let c = doctor_safety_checklist("0.6.0");
        assert!(c.contains("User-Agent"));
        assert!(c.contains("insufficient_quota"));
        assert!(c.contains("backoff"));
        assert!(c.contains("0.6.0"));
    }

    #[test]
    fn parallel_check_none_without_key() {
        assert!(check_parallel_sessions("OpenAI", None, "sess-1").is_none());
        assert!(check_parallel_sessions("OpenAI", Some(""), "sess-1").is_none());
    }
}
