use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiStandard {
    OpenAi,

    Anthropic,
}

impl ApiStandard {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiStandard::OpenAi => "openai",
            ApiStandard::Anthropic => "anthropic",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub key: &'static str,

    pub display_name: &'static str,

    pub openai_base_url: Option<&'static str>,

    pub anthropic_base_url: Option<&'static str>,

    pub api_key_env: Option<&'static str>,

    pub docs_url: &'static str,

    pub model_ids: &'static [&'static str],

    pub pricing: &'static [(&'static str, f64, f64)],

    pub featured: &'static [&'static str],

    pub rate_limit_notes: &'static str,
}

impl ProviderInfo {
    pub fn endpoint(&self, anthropic_available: bool) -> Option<(ApiStandard, &'static str)> {
        if anthropic_available {
            if let Some(url) = self.anthropic_base_url {
                return Some((ApiStandard::Anthropic, url));
            }
        }
        self.openai_base_url.map(|url| (ApiStandard::OpenAi, url))
    }

    pub fn price_for(&self, model: &str) -> Option<(f64, f64)> {
        self.pricing
            .iter()
            .find(|(m, _, _)| *m == model)
            .map(|(_, i, o)| (*i, *o))
    }

    pub fn endpoint_resolved(&self, anthropic_available: bool) -> Option<(ApiStandard, String)> {
        let (standard, url) = self.endpoint(anthropic_available)?;
        Some((standard, resolve_url_template(url)?))
    }
}

pub fn resolve_url_template(url: &str) -> Option<String> {
    if !url.contains('{') {
        return Some(url.to_string());
    }
    let account_id = std::env::var("CLOUDFLARE_ACCOUNT_ID")
        .ok()
        .filter(|v| !v.is_empty())?;
    Some(url.replace("{ACCOUNT_ID}", &account_id))
}

pub fn provider_registry() -> Vec<ProviderInfo> {
    vec![
        ProviderInfo {
            key: "openrouter",
            display_name: "OpenRouter",
            openai_base_url: Some("https://openrouter.ai/api/v1"),
            anthropic_base_url: None,
            api_key_env: Some("OPENROUTER_API_KEY"),
            docs_url: "https://openrouter.ai/docs",
            model_ids: &[],
            pricing: &[],
            featured: &["qwen/qwen3-coder:free"],
            rate_limit_notes:
                "Rate limits vary by model and tier; free models are heavily throttled.",
        },
        ProviderInfo {
            key: "groq",
            display_name: "Groq",
            openai_base_url: Some("https://api.groq.com/openai/v1"),
            anthropic_base_url: None,
            api_key_env: Some("GROQ_API_KEY"),
            docs_url: "https://console.groq.com/docs",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Generous free tier with per-minute TPM caps.",
        },
        ProviderInfo {
            key: "anthropic",
            display_name: "Anthropic",
            openai_base_url: None,
            anthropic_base_url: Some("https://api.anthropic.com"),
            api_key_env: Some("ANTHROPIC_API_KEY"),
            docs_url: "https://docs.anthropic.com/",
            model_ids: &["claude-opus-4-6", "claude-sonnet-4-6", "claude-haiku-4-5"],
            pricing: &[],
            featured: &[],
            rate_limit_notes:
                "Tiered RPM/TPM; 429 = backoff, 529 = overloaded (retry with backoff).",
        },
        ProviderInfo {
            key: "openai",
            display_name: "OpenAI",
            openai_base_url: Some("https://api.openai.com/v1"),
            anthropic_base_url: None,
            api_key_env: Some("OPENAI_API_KEY"),
            docs_url: "https://platform.openai.com/docs",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Tiered usage limits; respect Retry-After.",
        },
        ProviderInfo {
            key: "ollama",
            display_name: "Ollama (local)",
            openai_base_url: Some("http://localhost:11434/v1"),
            anthropic_base_url: None,
            api_key_env: None,
            docs_url: "https://ollama.com/",
            model_ids: &[],
            pricing: &[],
            featured: &["qwen3-coder", "qwen3-coder:30b"],
            rate_limit_notes: "Local - no rate limits, no pacing needed.",
        },
        ProviderInfo {
            key: "zai",
            display_name: "Z.ai (Zhipu)",
            openai_base_url: Some("https://api.z.ai/api/paas/v4"),
            anthropic_base_url: Some("https://api.z.ai/api/anthropic"),
            api_key_env: Some("ZAI_API_KEY"),
            docs_url: "https://docs.z.ai/",
            model_ids: &["glm-5.2", "glm-4.6", "glm-4.5", "glm-4.5-air"],

            pricing: &[("glm-5.2", 1.40, 4.40)],
            featured: &["glm-5.2"],
            rate_limit_notes: "Tiered RPM/TPM; free 5-day trial tier is strict (5M tokens/day).",
        },
        ProviderInfo {
            key: "moonshot",
            display_name: "Moonshot AI (Kimi)",
            openai_base_url: Some("https://api.moonshot.ai/v1"),
            anthropic_base_url: Some("https://api.moonshot.ai/anthropic"),
            api_key_env: Some("MOONSHOT_API_KEY"),
            docs_url: "https://platform.moonshot.ai/docs",
            model_ids: &["kimi-k2.7", "kimi-k3", "kimi-k2.6"],

            pricing: &[("kimi-k2.7", 0.95, 4.00), ("kimi-k3", 3.00, 15.00)],
            featured: &["kimi-k2.7"],
            rate_limit_notes:
                "Tiered limits; Kimi For Coding plan users route via the same endpoint.",
        },
        ProviderInfo {
            key: "deepseek",
            display_name: "DeepSeek",
            openai_base_url: Some("https://api.deepseek.com/v1"),
            anthropic_base_url: Some("https://api.deepseek.com/anthropic"),
            api_key_env: Some("DEEPSEEK_API_KEY"),
            docs_url: "https://api-docs.deepseek.com/",
            model_ids: &["deepseek-v4-flash", "deepseek-chat", "deepseek-reasoner"],

            pricing: &[("deepseek-v4-flash", 0.14, 0.28)],
            featured: &["deepseek-v4-flash"],
            rate_limit_notes: "Aggressive rate limits on lower tiers; 429s are common - \
                back off with jitter, never hammer (account-safety PR #47 applies).",
        },
        ProviderInfo {
            key: "cloudflare",
            display_name: "Cloudflare Workers AI",

            openai_base_url: Some(
                "https://api.cloudflare.com/client/v4/accounts/{ACCOUNT_ID}/ai/v1",
            ),
            anthropic_base_url: None,
            api_key_env: Some("CLOUDFLARE_API_TOKEN"),
            docs_url: "https://developers.cloudflare.com/workers-ai/",
            model_ids: &[
                "@cf/meta/llama-3.1-8b-instruct",
                "@cf/cloudflare/clef-flash",
            ],

            pricing: &[("@cf/cloudflare/clef-flash", 0.09, 0.0)],

            featured: &[
                "@cf/cloudflare/clef-flash",
                "@cf/meta/llama-3.1-8b-instruct",
            ],
            rate_limit_notes: "Free tier: 10,000 Neurons/day (resets 00:00 UTC), \
                then $0.011/1K Neurons. Best for small tasks (classification, \
                routing, summarisation) - not a coding-model replacement.",
        },
        ProviderInfo {
            key: "together",
            display_name: "Together AI",
            openai_base_url: Some("https://api.together.xyz/v1"),
            anthropic_base_url: None,
            api_key_env: Some("TOGETHER_API_KEY"),
            docs_url: "https://docs.together.ai/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Tiered limits.",
        },
        ProviderInfo {
            key: "mistral",
            display_name: "Mistral",
            openai_base_url: Some("https://api.mistral.ai/v1"),
            anthropic_base_url: None,
            api_key_env: Some("MISTRAL_API_KEY"),
            docs_url: "https://docs.mistral.ai/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Tiered limits.",
        },
        ProviderInfo {
            key: "cerebras",
            display_name: "Cerebras",
            openai_base_url: Some("https://api.cerebras.ai/v1"),
            anthropic_base_url: None,
            api_key_env: Some("CEREBRAS_API_KEY"),
            docs_url: "https://inference-docs.cerebras.ai/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Very high speed; free tier throttled.",
        },
        ProviderInfo {
            key: "fireworks",
            display_name: "Fireworks AI",
            openai_base_url: Some("https://api.fireworks.ai/inference/v1"),
            anthropic_base_url: None,
            api_key_env: Some("FIREWORKS_API_KEY"),
            docs_url: "https://docs.fireworks.ai/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Tiered limits.",
        },
        ProviderInfo {
            key: "google",
            display_name: "Google AI",
            openai_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            anthropic_base_url: None,
            api_key_env: Some("GOOGLE_API_KEY"),
            docs_url: "https://ai.google.dev/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Free tier is quota-gated rather than ban-heavy.",
        },
        ProviderInfo {
            key: "gemini",
            display_name: "Google AI (alias)",
            openai_base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            anthropic_base_url: None,
            api_key_env: Some("GOOGLE_API_KEY"),
            docs_url: "https://ai.google.dev/",
            model_ids: &[],
            pricing: &[],
            featured: &[],
            rate_limit_notes: "Alias of `google`.",
        },
    ]
}

pub fn find_provider(key: &str) -> Option<ProviderInfo> {
    let key = key.to_ascii_lowercase();
    provider_registry().into_iter().find(|p| p.key == key)
}

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub provider: String,

    pub model: String,

    pub api_standard: ApiStandard,

    pub base_url: String,

    pub api_key_env: Option<String>,
}

impl ModelSpec {
    pub fn parse(spec: &str, default_base_url: &str, anthropic_available: bool) -> Self {
        let spec = spec.trim();
        match spec.split_once('/') {
            Some((provider, model)) => {
                let key = provider.to_ascii_lowercase();
                if let Some(info) = find_provider(&key) {
                    if let Some((standard, url)) = info.endpoint_resolved(anthropic_available) {
                        return Self {
                            provider: key,
                            model: model.to_string(),
                            api_standard: standard,
                            base_url: url,
                            api_key_env: info.api_key_env.map(str::to_string),
                        };
                    }
                }

                Self {
                    provider: "default".to_string(),
                    model: spec.to_string(),
                    api_standard: ApiStandard::OpenAi,
                    base_url: default_base_url.to_string(),
                    api_key_env: None,
                }
            }
            None => Self {
                provider: "default".to_string(),
                model: spec.to_string(),
                api_standard: ApiStandard::OpenAi,
                base_url: default_base_url.to_string(),
                api_key_env: None,
            },
        }
    }

    pub fn api_key(&self, ambient: Option<&str>) -> Option<String> {
        if let Some(env) = &self.api_key_env {
            if let Ok(v) = std::env::var(env) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        ambient.map(str::to_string)
    }
}

pub fn model_pricing(provider_key: &str, model: &str) -> Option<(f64, f64)> {
    find_provider(provider_key)?.price_for(model)
}

pub fn call_cost_usd(input_tokens: u64, output_tokens: u64, pricing: (f64, f64)) -> f64 {
    (input_tokens as f64) * pricing.0 / 1_000_000.0
        + (output_tokens as f64) * pricing.1 / 1_000_000.0
}

pub fn featured_models() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for info in provider_registry() {
        for m in info.featured {
            out.push((info.key.to_string(), m.to_string()));
        }
    }
    out
}

pub fn authable_providers() -> Vec<ProviderInfo> {
    let preferred_order = [
        "deepseek",
        "zai",
        "moonshot",
        "cloudflare",
        "openrouter",
        "groq",
        "anthropic",
        "openai",
        "google",
        "together",
        "mistral",
        "cerebras",
        "fireworks",
    ];
    let reg: HashMap<&str, ProviderInfo> = provider_registry()
        .into_iter()
        .map(|p| (p.key, p))
        .collect();
    preferred_order
        .into_iter()
        .filter_map(|k| reg.get(k).cloned())
        .filter(|p| p.api_key_env.is_some())
        .collect()
}

pub fn cheap_first_order() -> Vec<String> {
    let mut priced: Vec<(&str, f64)> = Vec::new();
    let mut unpriced: Vec<&str> = Vec::new();
    for info in provider_registry() {
        let cheapest = info
            .pricing
            .iter()
            .map(|(_, i, _)| *i)
            .fold(None, |a: Option<f64>, b| {
                Some(a.map_or(b, |x: f64| x.min(b)))
            });
        match cheapest {
            Some(p) => priced.push((info.key, p)),
            None => unpriced.push(info.key),
        }
    }
    priced.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    priced
        .into_iter()
        .map(|(k, _)| k.to_string())
        .chain(unpriced.into_iter().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_fifteen_providers() {
        assert_eq!(provider_registry().len(), 15);
    }

    #[test]
    fn chinese_providers_have_anthropic_endpoints() {
        for key in ["zai", "moonshot", "deepseek"] {
            let p = find_provider(key).unwrap_or_else(|| panic!("missing provider {key}"));
            assert!(
                p.anthropic_base_url.is_some(),
                "{key} must document an Anthropic-compatible endpoint"
            );
            assert!(
                p.openai_base_url.is_some(),
                "{key} must document an OpenAI-compatible endpoint"
            );
        }
    }

    #[test]
    fn zai_endpoints_are_correct() {
        let p = find_provider("zai").unwrap();
        assert_eq!(p.anthropic_base_url, Some("https://api.z.ai/api/anthropic"));
        assert_eq!(p.openai_base_url, Some("https://api.z.ai/api/paas/v4"));
        assert_eq!(p.api_key_env, Some("ZAI_API_KEY"));
    }

    #[test]
    fn moonshot_endpoints_are_correct() {
        let p = find_provider("moonshot").unwrap();
        assert_eq!(
            p.anthropic_base_url,
            Some("https://api.moonshot.ai/anthropic")
        );
        assert_eq!(p.openai_base_url, Some("https://api.moonshot.ai/v1"));
        assert_eq!(p.api_key_env, Some("MOONSHOT_API_KEY"));
    }

    #[test]
    fn deepseek_endpoints_are_correct() {
        let p = find_provider("deepseek").unwrap();
        assert_eq!(
            p.anthropic_base_url,
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(p.openai_base_url, Some("https://api.deepseek.com/v1"));
        assert_eq!(p.api_key_env, Some("DEEPSEEK_API_KEY"));
    }

    #[test]
    fn endpoint_prefers_anthropic_when_available() {
        let p = find_provider("zai").unwrap();
        let (std, url) = p.endpoint(true).unwrap();
        assert_eq!(std, ApiStandard::Anthropic);
        assert_eq!(url, "https://api.z.ai/api/anthropic");
    }

    #[test]
    fn endpoint_falls_back_to_openai() {
        let p = find_provider("zai").unwrap();
        let (std, url) = p.endpoint(false).unwrap();
        assert_eq!(std, ApiStandard::OpenAi);
        assert_eq!(url, "https://api.z.ai/api/paas/v4");
    }

    #[test]
    fn anthropic_only_provider_has_no_openai_fallback() {
        let p = find_provider("anthropic").unwrap();
        assert!(p.endpoint(false).is_none());
        assert!(p.endpoint(true).is_some());
    }

    #[test]
    fn parse_zai_spec() {
        let s = ModelSpec::parse("zai/glm-5.2", "http://default/v1", false);
        assert_eq!(s.provider, "zai");
        assert_eq!(s.model, "glm-5.2");
        assert_eq!(s.api_standard, ApiStandard::OpenAi);
        assert_eq!(s.base_url, "https://api.z.ai/api/paas/v4");
        assert_eq!(s.api_key_env.as_deref(), Some("ZAI_API_KEY"));
    }

    #[test]
    fn parse_zai_spec_anthropic_client() {
        let s = ModelSpec::parse("zai/glm-5.2", "http://default/v1", true);
        assert_eq!(s.api_standard, ApiStandard::Anthropic);
        assert_eq!(s.base_url, "https://api.z.ai/api/anthropic");

        assert_eq!(s.model, "glm-5.2");
    }

    #[test]
    fn parse_moonshot_and_deepseek_specs() {
        let s = ModelSpec::parse("moonshot/kimi-k2.7", "http://default/v1", false);
        assert_eq!(s.provider, "moonshot");
        assert_eq!(s.model, "kimi-k2.7");
        assert_eq!(s.base_url, "https://api.moonshot.ai/v1");

        let s = ModelSpec::parse("deepseek/deepseek-v4-flash", "http://default/v1", false);
        assert_eq!(s.provider, "deepseek");
        assert_eq!(s.model, "deepseek-v4-flash");
        assert_eq!(s.base_url, "https://api.deepseek.com/v1");
    }

    #[test]
    fn parse_is_case_insensitive() {
        let s = ModelSpec::parse("ZAI/glm-5.2", "http://default/v1", false);
        assert_eq!(s.provider, "zai");
        assert_eq!(s.model, "glm-5.2");
    }

    #[test]
    fn parse_bare_model_uses_default() {
        let s = ModelSpec::parse("gpt-4o", "http://default/v1", false);
        assert_eq!(s.provider, "default");
        assert_eq!(s.model, "gpt-4o");
        assert_eq!(s.base_url, "http://default/v1");
    }

    #[test]
    fn parse_unknown_provider_keeps_whole_spec() {
        let s = ModelSpec::parse("futureai/new-model", "http://default/v1", false);
        assert_eq!(s.provider, "default");
        assert_eq!(s.model, "futureai/new-model");
        assert_eq!(s.base_url, "http://default/v1");
    }

    #[test]
    fn pricing_math_for_chinese_models() {
        let p = model_pricing("deepseek", "deepseek-v4-flash").unwrap();
        assert_eq!(p, (0.14, 0.28));
        assert!((call_cost_usd(1_000_000, 1_000_000, p) - 0.42).abs() < 1e-9);

        let p = model_pricing("zai", "glm-5.2").unwrap();
        assert_eq!(p, (1.40, 4.40));

        let p = model_pricing("moonshot", "kimi-k2.7").unwrap();
        assert_eq!(p, (0.95, 4.00));
        let p = model_pricing("moonshot", "kimi-k3").unwrap();
        assert_eq!(p, (3.00, 15.00));
    }

    #[test]
    fn pricing_unknown_returns_none() {
        assert_eq!(model_pricing("zai", "glm-9.9"), None);
        assert_eq!(model_pricing("nope", "x"), None);

        assert_eq!(model_pricing("zai", "glm-4.6"), None);
    }

    #[test]
    fn featured_models_include_chinese_floors() {
        let f = featured_models();
        assert!(f.contains(&("zai".to_string(), "glm-5.2".to_string())));
        assert!(f.contains(&("moonshot".to_string(), "kimi-k2.7".to_string())));
        assert!(f.contains(&("deepseek".to_string(), "deepseek-v4-flash".to_string())));

        assert!(f.contains(&("ollama".to_string(), "qwen3-coder".to_string())));
        assert!(f.contains(&(
            "openrouter".to_string(),
            "qwen/qwen3-coder:free".to_string()
        )));
    }

    #[test]
    fn authable_providers_start_with_price_floors() {
        let keys: Vec<&str> = authable_providers().iter().map(|p| p.key).collect();
        assert_eq!(&keys[..3], &["deepseek", "zai", "moonshot"]);
        assert!(!keys.contains(&"ollama"));
        assert!(!keys.contains(&"gemini"));
    }

    #[test]
    fn cheap_first_order_puts_cloudflare_first() {
        let order = cheap_first_order();

        assert_eq!(order[0], "cloudflare");
        assert_eq!(order[1], "deepseek");

        let zi = order.iter().position(|k| k == "zai").unwrap();
        let mi = order.iter().position(|k| k == "moonshot").unwrap();
        assert!(mi < zi, "moonshot ($0.95) should sort before zai ($1.40)");
    }

    #[test]
    fn api_key_prefers_provider_env_then_ambient() {
        let s = ModelSpec::parse("zai/glm-5.2", "http://default/v1", false);

        assert_eq!(
            s.api_key(Some("ambient-key")),
            Some("ambient-key".to_string())
        );
        assert_eq!(s.api_key(None), None);
    }

    fn with_account_id_env(value: Option<&str>, f: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var("CLOUDFLARE_ACCOUNT_ID").ok();
        match value {
            Some(v) => std::env::set_var("CLOUDFLARE_ACCOUNT_ID", v),
            None => std::env::remove_var("CLOUDFLARE_ACCOUNT_ID"),
        }
        f();
        match saved {
            Some(v) => std::env::set_var("CLOUDFLARE_ACCOUNT_ID", v),
            None => std::env::remove_var("CLOUDFLARE_ACCOUNT_ID"),
        }
    }

    #[test]
    fn cloudflare_entry_is_correct() {
        let p = find_provider("cloudflare").unwrap();
        assert_eq!(p.display_name, "Cloudflare Workers AI");
        assert_eq!(p.api_key_env, Some("CLOUDFLARE_API_TOKEN"));
        assert!(p.anthropic_base_url.is_none());

        let (std, url) = p.endpoint(false).unwrap();
        assert_eq!(std, ApiStandard::OpenAi);
        assert!(
            url.contains("{ACCOUNT_ID}"),
            "Workers AI embeds the account id in the URL path"
        );
    }

    #[test]
    fn cloudflare_template_resolves_with_account_id() {
        with_account_id_env(Some("test-acct-123"), || {
            let p = find_provider("cloudflare").unwrap();
            let (std, url) = p.endpoint_resolved(false).expect("must resolve");
            assert_eq!(std, ApiStandard::OpenAi);
            assert_eq!(
                url,
                "https://api.cloudflare.com/client/v4/accounts/test-acct-123/ai/v1"
            );

            let s = ModelSpec::parse(
                "cloudflare/@cf/meta/llama-3.1-8b-instruct",
                "http://default/v1",
                false,
            );
            assert_eq!(s.provider, "cloudflare");
            assert_eq!(s.model, "@cf/meta/llama-3.1-8b-instruct");
            assert_eq!(
                s.base_url,
                "https://api.cloudflare.com/client/v4/accounts/test-acct-123/ai/v1"
            );
            assert_eq!(s.api_key_env.as_deref(), Some("CLOUDFLARE_API_TOKEN"));
        });
    }

    #[test]
    fn cloudflare_without_account_id_falls_back_to_default() {
        with_account_id_env(None, || {
            let p = find_provider("cloudflare").unwrap();
            assert!(p.endpoint_resolved(false).is_none());
            let s = ModelSpec::parse("cloudflare/some-model", "http://default/v1", false);
            assert_eq!(s.provider, "default");
            assert_eq!(s.base_url, "http://default/v1");
        });
    }

    #[test]
    fn resolve_url_template_passes_plain_urls_through() {
        assert_eq!(
            resolve_url_template("https://api.deepseek.com/v1"),
            Some("https://api.deepseek.com/v1".to_string())
        );

        with_account_id_env(Some(""), || {
            assert_eq!(resolve_url_template("https://x/{ACCOUNT_ID}/y"), None);
        });
    }

    #[test]
    fn cloudflare_clef_flash_pricing_is_verified() {
        let p = model_pricing("cloudflare", "@cf/cloudflare/clef-flash").unwrap();
        assert_eq!(p, (0.09, 0.0));

        assert_eq!(
            model_pricing("cloudflare", "@cf/meta/llama-3.1-8b-instruct"),
            None
        );
    }

    #[test]
    fn featured_models_lead_with_cloudflare_free_tier() {
        let f = featured_models();
        assert!(f.contains(&(
            "cloudflare".to_string(),
            "@cf/cloudflare/clef-flash".to_string()
        )));
        assert!(f.contains(&(
            "cloudflare".to_string(),
            "@cf/meta/llama-3.1-8b-instruct".to_string()
        )));
    }

    #[test]
    fn authable_providers_include_cloudflare_after_moonshot() {
        let keys: Vec<&str> = authable_providers().iter().map(|p| p.key).collect();
        assert_eq!(&keys[..4], &["deepseek", "zai", "moonshot", "cloudflare"]);
    }
}
