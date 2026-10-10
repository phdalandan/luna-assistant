//! Cloud inference through the OpenAI and Anthropic APIs. Providers only propose tool calls;
//! Rust validates and executes them locally and never gives a provider access to Home Assistant.
mod anthropic;
mod openai;

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::{Map, Value};

use super::{ChatMessage, Completion, InferenceError, Timings};
use crate::credentials::AccessToken;
use crate::models::{CloudModel, CloudProvider};

/// Replies take seconds; waiting longer only delays telling the user something is wrong.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudFailure {
    Unreachable,
    Unauthorized,
    ModelUnavailable,
    RateLimited,
    Billing,
    Unavailable,
    Rejected,
    InvalidResponse,
}

/// Where each provider's API lives. Tests point these at a local mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub openai: String,
    pub anthropic: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            openai: "https://api.openai.com/v1/chat/completions".into(),
            anthropic: "https://api.anthropic.com/v1/messages".into(),
        }
    }
}

/// Token counts reported by a provider for one request.
#[derive(Debug, Default, PartialEq)]
struct Usage {
    input: u64,
    cached: u64,
    output: u64,
}

pub struct CloudClient {
    http: reqwest::Client,
    endpoints: Endpoints,
    timeout: Duration,
}

impl CloudClient {
    pub fn new(endpoints: Endpoints, timeout: Duration) -> Result<Self, InferenceError> {
        crate::tls::install_crypto_provider();
        let http = reqwest::Client::builder()
            .build()
            .map_err(|error| InferenceError::Request(error.to_string()))?;
        Ok(Self {
            http,
            endpoints,
            timeout,
        })
    }

    /// Sends one request without retrying. Dropping the future aborts it, which cancels it.
    pub async fn chat(
        &self,
        model: &CloudModel,
        key: &AccessToken,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<Completion, InferenceError> {
        let started = Instant::now();
        let request = match model.provider {
            CloudProvider::OpenAi => self
                .http
                .post(&self.endpoints.openai)
                .bearer_auth(key.expose())
                .json(&openai::body(model, messages, tools)),
            CloudProvider::Anthropic => self
                .http
                .post(&self.endpoints.anthropic)
                .header("x-api-key", key.expose())
                .header("anthropic-version", anthropic::VERSION)
                .json(&anthropic::body(model, messages, tools)),
        };
        let fail = |failure, detail: String| InferenceError::Cloud {
            provider: model.provider,
            failure,
            detail,
        };
        let response = request
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    InferenceError::Timeout
                } else if error.is_builder() {
                    fail(CloudFailure::Unauthorized, "unusable API key".into())
                } else {
                    fail(CloudFailure::Unreachable, error.to_string())
                }
            })?;
        let status = response.status();
        let body = match response.json::<Value>().await {
            Ok(body) => body,
            Err(error) if error.is_timeout() => return Err(InferenceError::Timeout),
            Err(_) if !status.is_success() => Value::Null,
            Err(error) => return Err(fail(CloudFailure::InvalidResponse, error.to_string())),
        };
        if !status.is_success() {
            let (failure, detail) = classify(status, &body);
            return Err(fail(failure, detail));
        }
        let parsed = match model.provider {
            CloudProvider::OpenAi => openai::parse(&body),
            CloudProvider::Anthropic => anthropic::parse(&body),
        };
        let (message, usage) =
            parsed.map_err(|error| fail(CloudFailure::InvalidResponse, error))?;
        log::info!(
            "cloud inference: {} {}, input {} tokens ({} cached), output {} tokens in {} ms",
            model.provider.name(),
            model.id,
            usage.input,
            usage.cached,
            usage.output,
            started.elapsed().as_millis()
        );
        Ok(Completion {
            message,
            timings: Timings {
                prompt_tokens: usage.input,
                cached_tokens: usage.cached,
                generated_tokens: usage.output,
                ..Timings::default()
            },
        })
    }
}

/// Both providers send `{"error": {"type", ...}}`. Only the status, type, and code are kept,
/// because error messages can quote part of the API key.
fn classify(status: StatusCode, body: &Value) -> (CloudFailure, String) {
    let error = &body["error"];
    let names: Vec<&str> = [&error["type"], &error["code"]]
        .into_iter()
        .filter_map(Value::as_str)
        .collect();
    let failure = match status.as_u16() {
        401 | 403 => CloudFailure::Unauthorized,
        402 => CloudFailure::Billing,
        404 => CloudFailure::ModelUnavailable,
        429 if names.contains(&"insufficient_quota") => CloudFailure::Billing,
        429 => CloudFailure::RateLimited,
        500.. => CloudFailure::Unavailable,
        _ => CloudFailure::Rejected,
    };
    (failure, format!("HTTP {status} {}", names.join(" ")))
}

fn add_request_fields(body: &mut Value, fields: &Map<String, Value>) {
    for (key, value) in fields {
        body[key] = value.clone();
    }
}

fn count(value: &Value) -> u64 {
    value.as_u64().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::inference::{FunctionCall, Role};
    use crate::models::catalog;

    const OPENAI_PATH: &str = "/v1/chat/completions";
    const ANTHROPIC_PATH: &str = "/v1/messages";

    fn client(server: &MockServer, timeout: Duration) -> CloudClient {
        CloudClient::new(
            Endpoints {
                openai: format!("{}{OPENAI_PATH}", server.uri()),
                anthropic: format!("{}{ANTHROPIC_PATH}", server.uri()),
            },
            timeout,
        )
        .unwrap()
    }

    fn model(provider: CloudProvider) -> &'static CloudModel {
        catalog::cloud_models()
            .iter()
            .find(|model| model.provider == provider)
            .unwrap()
    }

    fn key() -> AccessToken {
        AccessToken::new("test-key".into()).unwrap()
    }

    fn messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::System, "Instructions"),
            ChatMessage::new(Role::User, "Turn off the porch light"),
        ]
    }

    fn tools() -> Value {
        json!([{"type": "function", "function": {"name": "control", "description": "Change devices.",
            "parameters": {"type": "object", "properties": {"action": {"type": "string"}}}}}])
    }

    fn path_for(provider: CloudProvider) -> &'static str {
        match provider {
            CloudProvider::OpenAi => OPENAI_PATH,
            CloudProvider::Anthropic => ANTHROPIC_PATH,
        }
    }

    async fn reply_with(provider: CloudProvider, response: ResponseTemplate) -> InferenceError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(path_for(provider)))
            .respond_with(response)
            .mount(&server)
            .await;
        client(&server, DEFAULT_TIMEOUT)
            .chat(model(provider), &key(), &messages(), &tools())
            .await
            .unwrap_err()
    }

    fn failure(error: &InferenceError) -> Option<CloudFailure> {
        match error {
            InferenceError::Cloud { failure, .. } => Some(*failure),
            _ => None,
        }
    }

    #[tokio::test]
    async fn both_providers_produce_the_same_tool_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(OPENAI_PATH))
            .and(header("authorization", "Bearer test-key"))
            .and(body_partial_json(json!({
                "model": "gpt-6-luna", "reasoning_effort": "none", "store": false,
                "max_completion_tokens": 256, "tool_choice": "auto"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"role": "assistant", "content": null, "tool_calls": [{
                    "id": "call_1", "type": "function",
                    "function": {"name": "control", "arguments": "{\"action\":\"turn_off\"}"}
                }]}}],
                "usage": {"prompt_tokens": 900, "completion_tokens": 12,
                    "prompt_tokens_details": {"cached_tokens": 768}}
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(ANTHROPIC_PATH))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", anthropic::VERSION))
            .and(body_partial_json(json!({
                "model": "claude-haiku-5-5", "max_tokens": 256,
                "thinking": {"type": "disabled"}, "output_config": {"effort": "low"},
                "system": [{"type": "text", "text": "Instructions", "cache_control": {"type": "ephemeral"}}],
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Turn off the porch light"}]}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "control",
                    "input": {"action": "turn_off"}}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 100, "cache_read_input_tokens": 800,
                    "cache_creation_input_tokens": 0, "output_tokens": 20}
            })))
            .mount(&server)
            .await;
        let client = client(&server, DEFAULT_TIMEOUT);

        let openai = client
            .chat(model(CloudProvider::OpenAi), &key(), &messages(), &tools())
            .await
            .unwrap();
        let anthropic = client
            .chat(
                model(CloudProvider::Anthropic),
                &key(),
                &messages(),
                &tools(),
            )
            .await
            .unwrap();

        let expected = FunctionCall {
            name: "control".into(),
            arguments: json!({"action": "turn_off"}),
        };
        assert_eq!(openai.message.tool_calls[0].function, expected);
        assert_eq!(anthropic.message.tool_calls[0].function, expected);
        assert_eq!(anthropic.message.role, Role::Assistant);
        assert_eq!(
            (openai.timings.prompt_tokens, openai.timings.cached_tokens),
            (900, 768)
        );
        assert_eq!(
            (
                anthropic.timings.prompt_tokens,
                anthropic.timings.cached_tokens,
                anthropic.timings.generated_tokens
            ),
            (900, 800, 20)
        );
    }

    #[tokio::test]
    async fn provider_errors_are_classified_without_their_messages() {
        let error_body = |kind: &str, code: Option<&str>| {
            json!({"type": "error", "error": {"type": kind, "code": code,
                "message": "Incorrect API key provided: test-key"}})
        };
        let cases = [
            (
                CloudProvider::OpenAi,
                401,
                Some(error_body("invalid_request_error", Some("invalid_api_key"))),
                CloudFailure::Unauthorized,
            ),
            (
                CloudProvider::Anthropic,
                401,
                Some(error_body("authentication_error", None)),
                CloudFailure::Unauthorized,
            ),
            (
                CloudProvider::OpenAi,
                429,
                Some(error_body("requests", Some("rate_limit_exceeded"))),
                CloudFailure::RateLimited,
            ),
            (
                CloudProvider::Anthropic,
                429,
                Some(error_body("rate_limit_error", None)),
                CloudFailure::RateLimited,
            ),
            (
                CloudProvider::OpenAi,
                429,
                Some(error_body("insufficient_quota", Some("insufficient_quota"))),
                CloudFailure::Billing,
            ),
            (
                CloudProvider::Anthropic,
                402,
                Some(error_body("billing_error", None)),
                CloudFailure::Billing,
            ),
            (
                CloudProvider::OpenAi,
                404,
                Some(error_body("invalid_request_error", Some("model_not_found"))),
                CloudFailure::ModelUnavailable,
            ),
            (
                CloudProvider::Anthropic,
                529,
                Some(error_body("overloaded_error", None)),
                CloudFailure::Unavailable,
            ),
            (CloudProvider::OpenAi, 502, None, CloudFailure::Unavailable),
            (
                CloudProvider::Anthropic,
                400,
                Some(error_body("invalid_request_error", None)),
                CloudFailure::Rejected,
            ),
        ];
        for (provider, status, body, expected) in cases {
            let response = match body {
                Some(body) => ResponseTemplate::new(status).set_body_json(body),
                None => ResponseTemplate::new(status).set_body_string("<html>Bad gateway</html>"),
            };
            let error = reply_with(provider, response).await;
            assert_eq!(failure(&error), Some(expected), "{provider:?} {status}");
            assert!(!error.to_string().contains("test-key"), "{error}");
        }
    }

    #[tokio::test]
    async fn slow_providers_time_out() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let error = client(&server, Duration::from_millis(100))
            .chat(model(CloudProvider::OpenAi), &key(), &messages(), &tools())
            .await
            .unwrap_err();
        assert_eq!(error, InferenceError::Timeout);
    }

    #[tokio::test]
    async fn unreachable_providers_are_reported() {
        let client = CloudClient::new(
            Endpoints {
                openai: "http://127.0.0.1:1/v1/chat/completions".into(),
                anthropic: "http://127.0.0.1:1/v1/messages".into(),
            },
            DEFAULT_TIMEOUT,
        )
        .unwrap();
        for provider in CloudProvider::ALL {
            let error = client
                .chat(model(provider), &key(), &messages(), &tools())
                .await
                .unwrap_err();
            assert_eq!(failure(&error), Some(CloudFailure::Unreachable));
        }
    }

    #[tokio::test]
    async fn malformed_replies_are_rejected() {
        let error = reply_with(
            CloudProvider::Anthropic,
            ResponseTemplate::new(200).set_body_json(json!({"unexpected": true})),
        )
        .await;
        assert_eq!(failure(&error), Some(CloudFailure::InvalidResponse));
    }

    /// Paid requests against the real APIs, never run in CI:
    /// `LUNA_TEST_OPENAI_KEY=… LUNA_TEST_ANTHROPIC_KEY=… cargo test -- --ignored real_provider`
    #[tokio::test]
    #[ignore]
    async fn real_providers_call_the_tool() {
        let client = CloudClient::new(Endpoints::default(), DEFAULT_TIMEOUT).unwrap();
        let keys = [
            (CloudProvider::OpenAi, "LUNA_TEST_OPENAI_KEY"),
            (CloudProvider::Anthropic, "LUNA_TEST_ANTHROPIC_KEY"),
        ];
        for (provider, variable) in keys {
            let Some(key) = std::env::var(variable).ok().and_then(AccessToken::new) else {
                continue;
            };
            for model in catalog::cloud_models()
                .iter()
                .filter(|model| model.provider == provider)
            {
                let messages = [
                    ChatMessage::new(Role::System, "Use the control tool to change devices."),
                    ChatMessage::new(Role::User, "Turn off the porch light."),
                ];
                let completion = client
                    .chat(model, &key, &messages, &tools())
                    .await
                    .unwrap_or_else(|error| panic!("{}: {error}", model.id));
                assert_eq!(
                    completion.message.tool_calls[0].function.name, "control",
                    "{}",
                    model.id
                );
            }
        }
    }
}
