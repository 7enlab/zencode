





use std::collections::HashMap;
use std::sync::Arc;

use zen_core::{ChatMessage, ChatRequest, LlmBackend};



pub const BTW_SYSTEM_PROMPT: &str = "You are a helpful assistant answering a quick side question. \
    Answer concisely and directly. Do not ask follow-up questions. Do not use tools.";


pub const BTW_MAX_TOKENS: u32 = 1024;


pub fn settings_btw_model(extra: &HashMap<String, serde_json::Value>) -> Option<String> {
    extra
        .get("btw")?
        .get("model")?
        .as_str()
        .map(|s| s.to_string())
}







pub async fn run_btw(
    llm: &Arc<dyn LlmBackend>,
    model: &str,
    question: &str,
) -> anyhow::Result<String> {
    let req = ChatRequest {
        model: model.to_string(),
        messages: vec![
            ChatMessage::system(BTW_SYSTEM_PROMPT),
            ChatMessage::user(question),
        ],
        tools: vec![],
        max_tokens: Some(BTW_MAX_TOKENS),
    };
    let resp = llm.chat(req).await?;
    Ok(resp.message.content.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use zen_core::{ChatResponse, Usage};

    struct RecordingLlm {
        requests: Mutex<Vec<ChatRequest>>,
        answer: String,
    }

    #[async_trait]
    impl LlmBackend for RecordingLlm {
        async fn chat(&self, req: ChatRequest) -> anyhow::Result<ChatResponse> {
            self.requests.lock().unwrap().push(req);
            Ok(ChatResponse {
                message: ChatMessage::assistant(Some(self.answer.clone()), None),
                usage: Usage::default(),
                finish_reason: Some("stop".into()),
            })
        }
    }

    fn mock_llm(answer: &str) -> (Arc<RecordingLlm>, Arc<dyn LlmBackend>) {
        let inner = Arc::new(RecordingLlm {
            requests: Mutex::new(Vec::new()),
            answer: answer.to_string(),
        });
        (inner.clone(), inner as Arc<dyn LlmBackend>)
    }

    #[tokio::test]
    async fn btw_sends_minimal_context() {
        let (rec, llm) = mock_llm("42");
        let answer = run_btw(&llm, "cheap-model", "what is 6*7?").await.unwrap();
        assert_eq!(answer, "42");
        let reqs = rec.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1, "side question is a single model call");
        let req = &reqs[0];

        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.messages[0].role, "system");
        assert_eq!(
            req.messages[0].content.as_deref(),
            Some(BTW_SYSTEM_PROMPT),
            "side questions use the short btw prompt, not the agent system prompt"
        );
        assert_eq!(req.messages[1].role, "user");
        assert_eq!(req.messages[1].content.as_deref(), Some("what is 6*7?"));

        assert!(req.tools.is_empty());
    }

    #[tokio::test]
    async fn btw_uses_btw_model_not_main_model() {
        let (rec, llm) = mock_llm("hi");
        run_btw(&llm, "tiny/free-model", "q").await.unwrap();
        let reqs = rec.requests.lock().unwrap();
        assert_eq!(reqs[0].model, "tiny/free-model");
    }

    #[tokio::test]
    async fn btw_caps_output_tokens() {
        let (rec, llm) = mock_llm("x");
        run_btw(&llm, "m", "q").await.unwrap();
        let reqs = rec.requests.lock().unwrap();
        assert_eq!(reqs[0].max_tokens, Some(BTW_MAX_TOKENS));
    }

    #[test]
    fn settings_btw_model_reads_extra() {
        let extra: HashMap<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({"btw": {"model": "tiny"}})).unwrap();
        assert_eq!(settings_btw_model(&extra).as_deref(), Some("tiny"));
        let empty: HashMap<String, serde_json::Value> = HashMap::new();
        assert_eq!(settings_btw_model(&empty), None);
        let wrong_shape: HashMap<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({"btw": {"model": 42}})).unwrap();
        assert_eq!(settings_btw_model(&wrong_shape), None);
    }

    #[test]
    fn btw_is_a_registered_builtin() {
        assert!(crate::commands::BUILTIN_COMMANDS.contains(&"/btw"));
        let (name, args) = crate::commands::parse_builtin("/btw what is 2+2?").unwrap();
        assert_eq!(name, "btw");
        assert_eq!(args, "what is 2+2?");
    }
}
