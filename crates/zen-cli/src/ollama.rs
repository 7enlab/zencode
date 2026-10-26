













use std::time::Duration;


pub const OLLAMA_PREFIX: &str = "ollama/";


pub const OLLAMA_DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";


pub const OLLAMA_HOST_ENV: &str = "OLLAMA_HOST";


pub fn is_ollama_id(model: &str) -> bool {
    model.starts_with(OLLAMA_PREFIX)
}




pub fn strip_ollama_prefix(model: &str) -> Option<&str> {
    model.strip_prefix(OLLAMA_PREFIX)
}




pub fn normalize_ollama_host(raw: Option<&str>) -> String {
    let raw = raw.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return OLLAMA_DEFAULT_BASE_URL.to_string();
    }
    let with_scheme = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    let trimmed = with_scheme.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}


pub fn ollama_base_url() -> String {
    normalize_ollama_host(std::env::var(OLLAMA_HOST_ENV).ok().as_deref())
}



pub fn api_origin(base_url: &str) -> String {
    let t = base_url.trim_end_matches('/');
    t.strip_suffix("/v1").unwrap_or(t).to_string()
}






pub fn model_matches(available: &str, requested: &str) -> bool {
    if available == requested {
        return true;
    }
    if available == format!("{requested}:latest") {
        return true;
    }
    if !requested.contains(':') {
        return available.split(':').next() == Some(requested);
    }
    false
}













pub fn apply_ollama_routing(
    provider_is_ollama: bool,
    model: &str,
    base_url: &str,
    base_url_explicit: bool,
    api_key: Option<String>,
    ollama_url: &str,
) -> (String, String, Option<String>, bool) {
    let routed = provider_is_ollama || is_ollama_id(model);
    if !routed {
        return (model.to_string(), base_url.to_string(), api_key, false);
    }
    let bare = strip_ollama_prefix(model).unwrap_or(model).to_string();
    let base_url = if base_url_explicit {
        base_url.to_string()
    } else {
        ollama_url.to_string()
    };
    let api_key = if base_url_explicit { api_key } else { None };
    (bare, base_url, api_key, true)
}



fn is_local_origin(origin: &str) -> bool {
    let b = origin.to_ascii_lowercase();
    b.contains("://127.0.0.1") || b.contains("://localhost") || b.contains("://[::1]")
}

fn tags_client(origin: &str) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(5));
    if is_local_origin(origin) {
        builder = builder.no_proxy();
    }
    builder.build()
}


async fn fetch_tags(base_url: &str) -> Result<Vec<String>, reqwest::Error> {
    let origin = api_origin(base_url);
    let url = format!("{origin}/api/tags");
    let resp = tags_client(&origin)?.get(&url).send().await?;
    let resp = resp.error_for_status()?;
    let v: serde_json::Value = resp.json().await?;
    Ok(v
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    m.get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default())
}




pub async fn list_local_models(base_url: &str) -> anyhow::Result<Vec<String>> {
    let origin = api_origin(base_url);
    fetch_tags(base_url)
        .await
        .map_err(|e| anyhow::anyhow!("could not query Ollama at {origin}/api/tags: {e}"))
}



pub async fn ensure_ollama_ready(base_url: &str, model: &str) -> anyhow::Result<()> {
    let origin = api_origin(base_url);
    let tags = fetch_tags(base_url).await.map_err(|e| {
        anyhow::anyhow!(
            "Ollama did not answer at {origin}/api/tags ({e}). \
             Is Ollama running? Start it with `ollama serve`, \
             or point elsewhere with OLLAMA_HOST / --base-url."
        )
    })?;
    if tags.iter().any(|t| model_matches(t, model)) {
        return Ok(());
    }
    anyhow::bail!(
        "Model '{model}' is not available in Ollama at {origin}. \
         Pull it first: `ollama pull {model}`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prefix_cases() {
        assert_eq!(strip_ollama_prefix("ollama/qwen3-coder:30b"), Some("qwen3-coder:30b"));
        assert_eq!(strip_ollama_prefix("ollama/llama3.1"), Some("llama3.1"));
        assert_eq!(strip_ollama_prefix("ollama/"), Some(""));
        assert_eq!(strip_ollama_prefix("qwen3-coder:30b"), None);
        assert_eq!(strip_ollama_prefix(""), None);
    }

    #[test]
    fn is_ollama_id_cases() {
        assert!(is_ollama_id("ollama/foo"));
        assert!(is_ollama_id("ollama/"));
        assert!(!is_ollama_id("foo"));
        assert!(!is_ollama_id(""));
    }

    #[test]
    fn normalize_host_cases() {
        assert_eq!(normalize_ollama_host(None), "http://localhost:11434/v1");
        assert_eq!(normalize_ollama_host(Some("")), "http://localhost:11434/v1");
        assert_eq!(normalize_ollama_host(Some("   ")), "http://localhost:11434/v1");
        assert_eq!(
            normalize_ollama_host(Some("192.168.1.5:11434")),
            "http://192.168.1.5:11434/v1"
        );
        assert_eq!(
            normalize_ollama_host(Some("http://192.168.1.5:11434")),
            "http://192.168.1.5:11434/v1"
        );
        assert_eq!(
            normalize_ollama_host(Some("http://192.168.1.5:11434/v1")),
            "http://192.168.1.5:11434/v1"
        );
        assert_eq!(
            normalize_ollama_host(Some("http://192.168.1.5:11434/v1/")),
            "http://192.168.1.5:11434/v1"
        );
        assert_eq!(
            normalize_ollama_host(Some("https://ollama.example.com")),
            "https://ollama.example.com/v1"
        );
    }

    #[test]
    fn api_origin_cases() {
        assert_eq!(api_origin("http://localhost:11434/v1"), "http://localhost:11434");
        assert_eq!(api_origin("http://localhost:11434/v1/"), "http://localhost:11434");
        assert_eq!(api_origin("http://localhost:11434"), "http://localhost:11434");
        assert_eq!(api_origin("https://x.example/v1"), "https://x.example");
    }

    #[test]
    fn model_matches_cases() {
        assert!(model_matches("qwen3-coder:30b", "qwen3-coder:30b"));
        assert!(model_matches("qwen3-coder:latest", "qwen3-coder"));
        assert!(model_matches("qwen3-coder:30b", "qwen3-coder"));
        assert!(model_matches("llama3.1:8b", "llama3.1"));
        assert!(!model_matches("qwen3-coder:30b", "llama3.1"));
        assert!(!model_matches("qwen3-coder:30b", "qwen3-coder:8b"));
        assert!(!model_matches("qwen3-coder:30b", "qwen"));
    }

    #[test]
    fn routing_prefix_strips_and_defaults() {
        let (m, u, k, o) = apply_ollama_routing(
            false,
            "ollama/qwen3-coder:30b",
            "https://api.experientiallabs.ai/api/v1",
            false,
            Some("secret".into()),
            "http://localhost:11434/v1",
        );
        assert_eq!(m, "qwen3-coder:30b");
        assert_eq!(u, "http://localhost:11434/v1");
        assert_eq!(k, None);
        assert!(o);
    }

    #[test]
    fn routing_provider_flag_without_prefix() {
        let (m, u, k, o) = apply_ollama_routing(
            true,
            "llama3.1",
            "https://default.example/v1",
            false,
            None,
            "http://localhost:11434/v1",
        );
        assert_eq!(m, "llama3.1");
        assert_eq!(u, "http://localhost:11434/v1");
        assert_eq!(k, None);
        assert!(o);
    }

    #[test]
    fn routing_explicit_base_url_keeps_url_and_key() {
        let (m, u, k, o) = apply_ollama_routing(
            false,
            "ollama/qwen3-coder:30b",
            "http://gateway.internal:11434/v1",
            true,
            Some("tok".into()),
            "http://localhost:11434/v1",
        );
        assert_eq!(m, "qwen3-coder:30b");
        assert_eq!(u, "http://gateway.internal:11434/v1");
        assert_eq!(k, Some("tok".to_string()));
        assert!(o);
    }

    #[test]
    fn routing_non_ollama_untouched() {
        let (m, u, k, o) = apply_ollama_routing(
            false,
            "gpt-4o",
            "https://api.openai.com/v1",
            true,
            Some("tok".into()),
            "http://localhost:11434/v1",
        );
        assert_eq!(m, "gpt-4o");
        assert_eq!(u, "https://api.openai.com/v1");
        assert_eq!(k, Some("tok".to_string()));
        assert!(!o);
    }




    async fn fake_tags_server(tags_json: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};


            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    tags_json.len(),
                    tags_json
                );
                if sock.write_all(resp.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        format!("http://{addr}/v1")
    }

    const TAGS: &str = r#"{"models":[{"name":"qwen3-coder:30b"},{"name":"llama3.1:latest"}]}"#;

    #[tokio::test]
    async fn ready_when_model_pulled() {
        let base = fake_tags_server(TAGS).await;
        assert!(ensure_ollama_ready(&base, "qwen3-coder:30b").await.is_ok());

        assert!(ensure_ollama_ready(&base, "llama3.1").await.is_ok());
    }

    #[tokio::test]
    async fn missing_model_names_ollama_pull() {
        let base = fake_tags_server(TAGS).await;
        let err = ensure_ollama_ready(&base, "mistral:7b").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mistral:7b"), "unexpected: {msg}");
        assert!(msg.contains("ollama pull mistral:7b"), "unexpected: {msg}");
    }

    #[tokio::test]
    async fn not_running_gives_actionable_error() {

        let err = ensure_ollama_ready("http://127.0.0.1:9/v1", "qwen3-coder:30b")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Ollama"), "unexpected: {msg}");
        assert!(msg.contains("ollama serve"), "unexpected: {msg}");
    }

    #[tokio::test]
    async fn list_local_models_parses_tags() {
        let base = fake_tags_server(TAGS).await;
        let names = list_local_models(&base).await.unwrap();
        assert_eq!(names, vec!["qwen3-coder:30b", "llama3.1:latest"]);
    }
}
