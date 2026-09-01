//! One configured OpenAI-compatible model provider. The credential stays here.

use std::error::Error;
use std::fmt;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Match the activation child frame bound so a resumed Profile 1 turn that
/// already includes `voie.toml` in history can still reach the provider.
const MAX_REQUEST_BYTES: usize = 1_048_576;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TOKENS: u32 = 1024;

#[derive(Debug)]
pub enum ModelError {
    Config(&'static str),
    Bounded,
    Transport,
    Response,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Config(message) => write!(f, "configuration: {message}"),
            ModelError::Bounded => write!(f, "model request exceeds the configured bound"),
            ModelError::Transport => write!(f, "model transport failed"),
            ModelError::Response => write!(f, "model response was unusable"),
        }
    }
}

impl Error for ModelError {}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelMessage {
    pub role: String,
    pub content: String,
    /// Assistant tool calls this message carried.
    #[serde(default)]
    pub tool_calls: Vec<ModelToolCall>,
    /// For tool-result messages (`role = "tool"`): the answered call id.
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

impl ModelMessage {
    /// Plain conversational message.
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        ModelMessage {
            role: role.to_owned(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Assistant message requesting the given typed tool calls.
    pub fn assistant_tool_calls(content: &str, calls: Vec<ModelToolCall>) -> Self {
        ModelMessage {
            role: "assistant".to_owned(),
            content: content.to_owned(),
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    /// Tool-role message answering one prior call id.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ModelMessage {
            role: "tool".to_owned(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

impl Serialize for ModelMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde_json::Value;

        let mut object = serde_json::Map::new();
        object.insert("role".into(), Value::String(self.role.clone()));
        object.insert("content".into(), Value::String(self.content.clone()));
        if !self.tool_calls.is_empty() {
            let calls = self
                .tool_calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            // Arguments travel as their JSON text on the wire.
                            "arguments": call.arguments.to_string(),
                        },
                    })
                })
                .collect::<Vec<_>>();
            object.insert("tool_calls".into(), Value::Array(calls));
        }
        if let Some(id) = &self.tool_call_id {
            object.insert("tool_call_id".into(), Value::String(id.clone()));
        }
        Value::Object(object).serialize(serializer)
    }
}

/// Activation-facing request: model input only, no provider credential.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<ModelMessage>,
    /// Tools offered to the model for this completion.
    pub tools: Vec<ModelToolDefinition>,
    pub max_tokens: u32,
}

/// One tool offered to the model, in OpenAI function shape. The single
/// definition path; no provider abstraction lives here.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelToolDefinition {
    /// Stable product identifier. It is never generated per request.
    pub id: String,
    /// Provider-visible function name (currently equal to `id`).
    pub name: String,
    pub description: String,
    /// JSON schema object describing the arguments.
    pub parameters: serde_json::Value,
}

fn no_tools(tools: &[ModelToolDefinition]) -> bool {
    tools.is_empty()
}

impl Serialize for ModelToolDefinition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.id,
                "description": self.description,
                "parameters": self.parameters,
            },
        })
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// One provider-requested tool call inside a completion response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelToolCall {
    pub id: String,
    pub name: String,
    /// Provider-supplied arguments object.
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub content: String,
    /// Tool calls requested by the model; empty for plain text replies.
    pub tool_calls: Vec<ModelToolCall>,
    pub usage: Option<ModelUsage>,
}

/// One base URL, one model, bounded buffered relay.
pub struct ModelRelay {
    http: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl ModelRelay {
    /// `VOIE_MODEL_BASE_URL`, `VOIE_MODEL_NAME`, and the API key from either
    /// `VOIE_MODEL_API_KEY` or `VOIE_MODEL_API_KEY_FILE`.
    pub fn from_env() -> Result<Self, ModelError> {
        let base_url = require_env("VOIE_MODEL_BASE_URL")?;
        let model = require_env("VOIE_MODEL_NAME")?;
        let api_key = credential(
            "VOIE_MODEL_API_KEY",
            "VOIE_MODEL_API_KEY_FILE",
            "required model setting is missing",
        )
        .map_err(ModelError::Config)?;
        Self::new(base_url, model, api_key)
    }

    pub fn new(base_url: String, model: String, api_key: String) -> Result<Self, ModelError> {
        let http = Client::builder()
            .https_only(base_url.starts_with("https://"))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| ModelError::Transport)?;
        Ok(ModelRelay {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            model,
        })
    }

    pub async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let max_tokens = request.max_tokens.min(MAX_TOKENS);
        if max_tokens == 0 {
            return Err(ModelError::Bounded);
        }
        let payload = ChatRequest {
            model: &self.model,
            messages: &request.messages,
            tools: &request.tools,
            max_tokens,
        };
        let body = serde_json::to_vec(&payload).map_err(|_| ModelError::Response)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(ModelError::Bounded);
        }
        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| ModelError::Transport)?;
        if !response.status().is_success() {
            return Err(ModelError::Response);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ModelError::Bounded);
        }
        let body = response.bytes().await.map_err(|_| ModelError::Transport)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(ModelError::Bounded);
        }
        let parsed: ChatResponse =
            serde_json::from_slice(&body).map_err(|_| ModelError::Response)?;
        parse_completion(parsed)
    }

    /// Transport-level reachability probe against `{base_url}/models`. Any
    /// HTTP answer proves reachability; no completion is requested and the
    /// credential is not validated here.
    pub async fn reachable(&self) -> bool {
        let url = format!("{}/models", self.base_url);
        match self.http.get(&url).bearer_auth(&self.api_key).send().await {
            // Fail closed on every non-success answer: 401/403/429/5xx mean
            // the relay cannot serve completions even though the socket
            // connected.
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }
}

/// Typed view of one OpenAI-compatible completion body.
///
/// Tool-call arguments arrive as a JSON-encoded string on the wire and are
/// parsed here, so callers receive structured arguments or an error.
fn parse_completion(parsed: ChatResponse) -> Result<ModelResponse, ModelError> {
    let message = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message)
        .ok_or(ModelError::Response)?;
    let mut tool_calls = Vec::new();
    if let Some(calls) = message.tool_calls {
        for call in calls {
            let arguments: serde_json::Value =
                serde_json::from_str(&call.function.arguments).map_err(|_| ModelError::Response)?;
            tool_calls.push(ModelToolCall {
                id: call.id,
                name: call.function.name,
                arguments,
            });
        }
    }
    let content = message.content.unwrap_or_default();
    if content.is_empty() && tool_calls.is_empty() {
        return Err(ModelError::Response);
    }
    Ok(ModelResponse {
        content,
        tool_calls,
        usage: parsed.usage.map(|usage| ModelUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        }),
    })
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    #[serde(skip_serializing_if = "no_tools")]
    tools: &'a [ModelToolDefinition],
    max_tokens: u32,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Option<ChatMessage>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Deserialize)]
struct RawToolCall {
    id: String,
    function: RawFunctionCall,
}

#[derive(Deserialize)]
struct RawFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct RawUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

fn require_env(name: &'static str) -> Result<String, ModelError> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ModelError::Config("required model setting is missing")),
    }
}

/// Resolves one credential from its direct variable or its credential file.
/// Exactly one source must be present; the value is never logged.
fn credential(direct: &str, file: &str, missing: &'static str) -> Result<String, &'static str> {
    if let Ok(path) = std::env::var(file) {
        if !path.trim().is_empty() {
            return std::fs::read_to_string(path.trim())
                .map(|value| value.trim().to_owned())
                .map_err(|_| "credential file is unreadable");
        }
    }
    match std::env::var(direct) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(missing),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_body(body: &str) -> Result<ModelResponse, ModelError> {
        let parsed: ChatResponse = serde_json::from_str(body).map_err(|_| ModelError::Response)?;
        parse_completion(parsed)
    }

    #[test]
    fn plain_text_response_parses_with_usage() {
        let response = parse_body(
            r#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],
               "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}}"#,
        )
        .expect("text response parses");
        assert_eq!(response.content, "pong");
        assert!(response.tool_calls.is_empty());
        assert_eq!(
            response.usage,
            Some(ModelUsage {
                prompt_tokens: 3,
                completion_tokens: 1,
                total_tokens: 4,
            })
        );
    }

    #[test]
    fn tool_calls_parse_into_typed_arguments() {
        let response = parse_body(
            r#"{"choices":[{"message":{"role":"assistant","content":"",
               "tool_calls":[{"id":"call-1","type":"function",
               "function":{"name":"bash","arguments":"{\"command\":[\"echo hi\"]}"}}]}}],
               "usage":null}"#,
        )
        .expect("tool-call response parses");
        assert_eq!(response.content, "");
        assert_eq!(response.tool_calls.len(), 1);
        let call = &response.tool_calls[0];
        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "bash");
        assert_eq!(call.arguments["command"][0], "echo hi");
    }

    #[test]
    fn unparsable_tool_arguments_are_a_response_error() {
        assert!(matches!(
            parse_body(
                r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"c","type":"function",
                   "function":{"name":"bash","arguments":"not-json"}}]}}]}"#,
            ),
            Err(ModelError::Response)
        ));
    }

    #[test]
    fn empty_message_is_refused() {
        assert!(matches!(
            parse_body(r#"{"choices":[{"message":{"content":""}}]}"#),
            Err(ModelError::Response)
        ));
        assert!(matches!(
            parse_body(r#"{"choices":[]}"#),
            Err(ModelError::Response)
        ));
    }

    #[test]
    fn plain_message_keeps_the_exact_wire_shape() {
        let value = serde_json::to_value(ModelMessage::text("user", "hi"))
            .expect("plain message serializes");
        assert_eq!(
            value,
            serde_json::json!({ "role": "user", "content": "hi" }),
            "no tool fields leak into plain messages"
        );
    }

    #[test]
    fn prior_tool_history_serializes_in_provider_shape() {
        let history = vec![
            ModelMessage::text("user", "run echo"),
            ModelMessage::assistant_tool_calls(
                "",
                vec![ModelToolCall {
                    id: "call-1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({ "command": ["echo hi"] }),
                }],
            ),
            ModelMessage::tool_result("call-1", "hi\n"),
        ];
        let encoded = serde_json::to_string(&history).expect("history serializes");
        assert!(encoded.contains(r#""role":"assistant""#));
        assert!(encoded.contains(r#""type":"function""#));
        assert!(
            encoded.contains(r#""arguments":"{\"command\":[\"echo hi\"]}""#,),
            "typed arguments travel as their JSON text"
        );
        assert!(encoded.contains(r#""role":"tool""#));
        assert!(encoded.contains(r#""tool_call_id":"call-1""#));
        assert!(encoded.contains(r#""content":"hi\n""#));
    }

    #[test]
    fn offered_tools_serialize_in_openai_function_shape() {
        let request = ChatRequest {
            model: "m",
            messages: &[ModelMessage::text("user", "hi")],
            tools: &[ModelToolDefinition {
                id: "bash".into(),
                name: "bash".into(),
                description: "Run one bounded shell command".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                }),
            }],
            max_tokens: 16,
        };
        let encoded = serde_json::to_string(&request).expect("request serializes");
        let parsed: serde_json::Value = serde_json::from_str(&encoded).expect("request parses");
        let offered = parsed["tools"][0].clone();
        assert_eq!(
            offered,
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "bash",
                    "description": "Run one bounded shell command",
                    "parameters": {
                        "type": "object",
                        "properties": { "command": { "type": "string" } },
                    },
                },
            }),
            "offered tools serialize in exact OpenAI function shape"
        );
    }

    #[test]
    fn requests_without_tools_omit_the_wire_field() {
        let request = ChatRequest {
            model: "m",
            messages: &[ModelMessage::text("user", "hi")],
            tools: &[],
            max_tokens: 16,
        };
        let encoded = serde_json::to_string(&request).expect("request serializes");
        assert!(!encoded.contains("tools"), "no empty tools array is sent");
    }
}
