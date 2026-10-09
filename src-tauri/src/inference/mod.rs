//! Local inference through the bundled llama.cpp server. One model is loaded at a time.
mod process;
mod server;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::models::ChatOptions;
use process::PidFile;
use server::Server;

/// Unloading after inactivity keeps memory free while Luna sits idle.
const IDLE_UNLOAD: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InferenceError {
    #[error("inference runtime not found at {0}")]
    RuntimeMissing(String),
    #[error("model {0} is not installed")]
    ModelMissing(String),
    #[error("model failed to load: {0}")]
    LoadFailed(String),
    #[error("inference request timed out")]
    Timeout,
    #[error("inference request failed: {0}")]
    Request(String),
    #[error("unexpected inference response: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum EngineStatus {
    Idle,
    Loading { model: String },
    Ready { model: String },
    Failed { model: String },
}

/// Everything needed to run one model. Built from the catalogue and settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpec {
    pub id: String,
    pub path: PathBuf,
    pub context_length: u32,
    pub chat: ChatOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    /// Links a tool result to the call it answers.
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: Value,
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn tool_result(call: &ToolCall, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: Some(call.id.clone()),
            ..Self::new(Role::Tool, content)
        }
    }

    /// OpenAI-compatible message format used by the llama.cpp server.
    fn to_wire(&self) -> Value {
        let role = match self.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let mut message = json!({ "role": role, "content": self.content });
        if !self.tool_calls.is_empty() {
            message["tool_calls"] = self
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.function.name,
                            "arguments": call.function.arguments.to_string(),
                        }
                    })
                })
                .collect();
        }
        if let Some(id) = &self.tool_call_id {
            message["tool_call_id"] = json!(id);
        }
        message
    }

    fn from_wire(message: &Value) -> Result<Self, InferenceError> {
        if !message.is_object() {
            return Err(InferenceError::InvalidResponse("missing message".into()));
        }
        let tool_calls = message["tool_calls"]
            .as_array()
            .map(|calls| calls.iter().map(tool_call_from_wire).collect())
            .unwrap_or_default();
        Ok(Self {
            role: Role::Assistant,
            content: message["content"].as_str().unwrap_or_default().to_owned(),
            tool_calls,
            tool_call_id: None,
        })
    }
}

/// Arguments arrive as a JSON string. Unparseable text is kept so validation can reject it.
fn tool_call_from_wire(call: &Value) -> ToolCall {
    let function = &call["function"];
    let arguments = match &function["arguments"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
        other => other.clone(),
    };
    ToolCall {
        id: call["id"].as_str().unwrap_or_default().to_owned(),
        function: FunctionCall {
            name: function["name"].as_str().unwrap_or_default().to_owned(),
            arguments,
        },
    }
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: PathBuf,
    pid_file: PidFile,
    server: Mutex<Option<Server>>,
    status: watch::Sender<EngineStatus>,
    activity: AtomicU64,
}

impl Engine {
    /// `pid_file` records the running server so one left behind by a crash is stopped here.
    pub fn new(runtime: PathBuf, pid_file: PathBuf) -> Self {
        let pid_file = PidFile::new(pid_file);
        pid_file.stop_leftover();
        Self {
            inner: Arc::new(Inner {
                runtime,
                pid_file,
                server: Mutex::new(None),
                status: watch::Sender::new(EngineStatus::Idle),
                activity: AtomicU64::new(0),
            }),
        }
    }

    /// The bundled server binary, installed next to Luna's executable.
    pub fn bundled_runtime() -> std::io::Result<PathBuf> {
        let executable = std::env::current_exe()?;
        let name = format!("llama-server{}", std::env::consts::EXE_SUFFIX);
        Ok(executable.with_file_name(name))
    }

    pub fn status(&self) -> EngineStatus {
        self.inner.status.borrow().clone()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<EngineStatus> {
        self.inner.status.subscribe()
    }

    pub fn is_loaded(&self, model_id: &str) -> bool {
        matches!(
            &*self.inner.status.borrow(),
            EngineStatus::Loading { model } | EngineStatus::Ready { model } if model == model_id
        )
    }

    /// Loads `spec`, unloading any other model first. Loads never run concurrently.
    pub async fn load(&self, spec: &ModelSpec) -> Result<(), InferenceError> {
        let mut server = self.inner.server.lock().await;
        let result = self.ensure_loaded(&mut server, spec).await;
        drop(server);
        self.schedule_idle_unload();
        result
    }

    pub async fn unload(&self) {
        let mut server = self.inner.server.lock().await;
        if let Some(running) = server.take() {
            running.shutdown().await;
            self.inner.pid_file.clear();
        }
        self.inner.status.send_replace(EngineStatus::Idle);
    }

    /// Runs one chat request, loading the model first if needed.
    pub async fn chat(
        &self,
        spec: &ModelSpec,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<ChatMessage, InferenceError> {
        let _activity = IdleGuard(self);
        let mut server = self.inner.server.lock().await;
        self.ensure_loaded(&mut server, spec).await?;
        let running = server.as_ref().expect("model is loaded");
        running.chat(spec, messages, tools).await
    }

    async fn ensure_loaded(
        &self,
        server: &mut Option<Server>,
        spec: &ModelSpec,
    ) -> Result<(), InferenceError> {
        if server.as_ref().is_some_and(|running| running.serves(spec)) {
            return Ok(());
        }
        if let Some(previous) = server.take() {
            previous.shutdown().await;
            self.inner.pid_file.clear();
        }
        self.inner.status.send_replace(EngineStatus::Loading {
            model: spec.id.clone(),
        });
        match Server::start(&self.inner.runtime, spec).await {
            Ok(running) => {
                if let Some(pid) = running.pid() {
                    self.inner.pid_file.record(pid);
                }
                *server = Some(running);
                self.inner.status.send_replace(EngineStatus::Ready {
                    model: spec.id.clone(),
                });
                Ok(())
            }
            Err(error) => {
                self.inner.status.send_replace(EngineStatus::Failed {
                    model: spec.id.clone(),
                });
                Err(error)
            }
        }
    }

    fn schedule_idle_unload(&self) {
        let generation = self.inner.activity.fetch_add(1, Ordering::SeqCst) + 1;
        let engine = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(IDLE_UNLOAD).await;
            if engine.inner.activity.load(Ordering::SeqCst) == generation {
                let mut server = engine.inner.server.lock().await;
                if engine.inner.activity.load(Ordering::SeqCst) == generation
                    && let Some(running) = server.take()
                {
                    running.shutdown().await;
                    engine.inner.pid_file.clear();
                    engine.inner.status.send_replace(EngineStatus::Idle);
                }
            }
        });
    }
}

/// Restarts the idle timer when a request ends, including when it is cancelled.
struct IdleGuard<'a>(&'a Engine);

impl Drop for IdleGuard<'_> {
    fn drop(&mut self) {
        self.0.schedule_idle_unload();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::catalog;

    pub(super) fn runtime() -> PathBuf {
        let target = env!("LUNA_TARGET_TRIPLE");
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!(
                "llama-server-{target}{}",
                std::env::consts::EXE_SUFFIX
            ))
    }

    fn engine(runtime: PathBuf, name: &str) -> Engine {
        let pid_file = std::env::temp_dir().join(format!("luna-{name}-{}.pid", std::process::id()));
        Engine::new(runtime, pid_file)
    }

    fn spec(path: PathBuf, context_length: u32) -> ModelSpec {
        ModelSpec {
            id: "qwen3-8b".into(),
            path,
            context_length,
            chat: catalog::find("qwen3-8b").unwrap().chat.clone(),
        }
    }

    #[test]
    fn tool_calls_round_trip_through_the_wire_format() {
        let wire = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "call-1", "type": "function",
                "function": {"name": "control", "arguments": "{\"action\":\"turn_off\"}"}}]
        });
        let message = ChatMessage::from_wire(&wire).unwrap();
        assert_eq!(message.content, "");
        assert_eq!(
            message.tool_calls[0].function.arguments,
            json!({"action": "turn_off"})
        );

        let result = ChatMessage::tool_result(&message.tool_calls[0], "done");
        assert_eq!(result.to_wire()["tool_call_id"], "call-1");
        assert_eq!(
            message.to_wire()["tool_calls"][0]["function"]["arguments"],
            "{\"action\":\"turn_off\"}"
        );
    }

    #[test]
    fn malformed_arguments_are_kept_for_validation() {
        let call =
            tool_call_from_wire(&json!({"function": {"name": "control", "arguments": "{oops"}}));
        assert_eq!(call.function.arguments, json!("{oops"));
    }

    #[tokio::test]
    async fn missing_runtime_is_reported() {
        let engine = engine(PathBuf::from("/nonexistent/llama-server"), "no-runtime");
        let result = engine
            .load(&spec(PathBuf::from("/nonexistent.gguf"), 4096))
            .await;
        assert!(matches!(result, Err(InferenceError::RuntimeMissing(_))));
    }

    #[tokio::test]
    async fn missing_model_files_are_reported() {
        let engine = engine(runtime(), "no-model");
        let result = engine
            .load(&spec(PathBuf::from("/nonexistent.gguf"), 4096))
            .await;
        assert_eq!(result, Err(InferenceError::ModelMissing("qwen3-8b".into())));
    }

    #[tokio::test]
    async fn invalid_model_files_fail_to_load_without_switching_models() {
        let path = std::env::temp_dir().join(format!("luna-invalid-{}.gguf", std::process::id()));
        std::fs::write(&path, b"not a gguf file").unwrap();
        let engine = engine(runtime(), "invalid");

        let result = engine.load(&spec(path.clone(), 4096)).await;

        assert!(matches!(result, Err(InferenceError::LoadFailed(_))));
        assert_eq!(
            engine.status(),
            EngineStatus::Failed {
                model: "qwen3-8b".into()
            }
        );
        assert!(engine.inner.server.lock().await.is_none());
        std::fs::remove_file(path).unwrap();
    }

    /// Needs a real GGUF model: `LUNA_TEST_MODEL=/path/model.gguf cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn switching_models_unloads_first_and_never_runs_two_servers() {
        let path = PathBuf::from(std::env::var("LUNA_TEST_MODEL").unwrap());
        let engine = engine(runtime(), "switching");
        let (small, large) = (spec(path.clone(), 2048), spec(path, 4096));

        let (first, second) = tokio::join!(engine.load(&small), engine.load(&large));
        first.unwrap();
        second.unwrap();
        assert_eq!(
            engine
                .inner
                .server
                .lock()
                .await
                .as_ref()
                .unwrap()
                .context_length,
            4096
        );
        assert_eq!(
            engine.status(),
            EngineStatus::Ready {
                model: "qwen3-8b".into()
            }
        );

        let reply = engine
            .chat(
                &large,
                &[ChatMessage::new(Role::User, "Say hello.")],
                &json!([]),
            )
            .await
            .unwrap();
        assert!(!reply.content.is_empty());

        engine.unload().await;
        assert_eq!(engine.status(), EngineStatus::Idle);
    }
}
