use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::Instant;

use super::{ChatMessage, InferenceError, ModelSpec};

const LOAD_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_INTERVAL: Duration = Duration::from_millis(250);
/// Generation on CPU-only machines can take minutes for long answers.
const CHAT_TIMEOUT: Duration = Duration::from_secs(240);
const MAX_RESPONSE_TOKENS: u32 = 1024;
const STDERR_LINES: usize = 30;

/// A running llama.cpp server holding one model in memory.
pub struct Server {
    pub model_id: String,
    pub context_length: u32,
    child: Child,
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl Server {
    pub async fn start(runtime: &Path, spec: &ModelSpec) -> Result<Self, InferenceError> {
        if !runtime.is_file() {
            return Err(InferenceError::RuntimeMissing(
                runtime.display().to_string(),
            ));
        }
        if !spec.path.is_file() {
            return Err(InferenceError::ModelMissing(spec.id.clone()));
        }
        let port = free_port()?;
        let api_key = random_key();
        crate::tls::install_crypto_provider();
        let mut command = Command::new(runtime);
        command
            .arg("--model")
            .arg(&spec.path)
            .args(["--ctx-size", &spec.context_length.to_string()])
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--parallel", "1", "--cache-ram", "0", "--fit", "off"])
            .args(["--no-webui", "--offline", "--jinja"])
            .args(gpu_arguments())
            .env("LLAMA_API_KEY", &api_key)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console_window(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| InferenceError::LoadFailed(format!("spawn failed: {error}")))?;

        let output = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(collect_output(stderr, output.clone()));
        }
        let mut server = Self {
            model_id: spec.id.clone(),
            context_length: spec.context_length,
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            api_key,
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .map_err(|error| InferenceError::LoadFailed(error.to_string()))?,
        };
        if let Err(error) = server.wait_until_ready().await {
            let recent: Vec<String> = lock(&output).iter().cloned().collect();
            log::error!(
                "llama-server failed to load {}: {error}\n{}",
                spec.id,
                recent.join("\n")
            );
            server.shutdown().await;
            return Err(error);
        }
        log::info!(
            "loaded model {} with {} context",
            spec.id,
            spec.context_length
        );
        Ok(server)
    }

    pub fn serves(&self, spec: &ModelSpec) -> bool {
        self.model_id == spec.id && self.context_length == spec.context_length
    }

    async fn wait_until_ready(&mut self) -> Result<(), InferenceError> {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let health = format!("{}/health", self.base_url);
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| InferenceError::LoadFailed(error.to_string()))?
            {
                return Err(InferenceError::LoadFailed(format!("exited with {status}")));
            }
            let response = self
                .http
                .get(&health)
                .timeout(HEALTH_INTERVAL * 4)
                .send()
                .await;
            if response.is_ok_and(|response| response.status().is_success()) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(InferenceError::LoadFailed("timed out while loading".into()));
            }
            tokio::time::sleep(HEALTH_INTERVAL).await;
        }
    }

    /// Dropping the returned future closes the connection, which cancels generation.
    pub async fn chat(
        &self,
        spec: &ModelSpec,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<ChatMessage, InferenceError> {
        let mut body = json!({
            "messages": messages.iter().map(ChatMessage::to_wire).collect::<Vec<_>>(),
            "tools": tools,
            "tool_choice": "auto",
            "stream": false,
            "max_tokens": MAX_RESPONSE_TOKENS,
            "temperature": spec.chat.temperature,
            "top_p": spec.chat.top_p,
            "top_k": spec.chat.top_k,
        });
        if spec.chat.disable_thinking {
            body["chat_template_kwargs"] = json!({ "enable_thinking": false });
        }
        let response = self
            .http
            .post(format!("{}/v1/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .timeout(CHAT_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    InferenceError::Timeout
                } else {
                    InferenceError::Request(error.to_string())
                }
            })?;
        let status = response.status();
        let body: Value = response
            .json()
            .await
            .map_err(|error| InferenceError::InvalidResponse(error.to_string()))?;
        if !status.is_success() {
            return Err(InferenceError::Request(format!(
                "{status}: {}",
                body["error"]
            )));
        }
        ChatMessage::from_wire(&body["choices"][0]["message"])
    }

    pub async fn shutdown(mut self) {
        if let Err(error) = self.child.start_kill() {
            log::warn!("failed to stop llama-server: {error}");
        }
        match tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(Ok(_)) => log::info!("unloaded model {}", self.model_id),
            Ok(Err(error)) => log::warn!("failed to wait for llama-server: {error}"),
            Err(_) => log::warn!("llama-server did not exit in time"),
        }
    }
}

#[cfg(target_os = "macos")]
fn gpu_arguments() -> [&'static str; 2] {
    // Apple Silicon shares memory with the GPU, so every layer runs on Metal.
    ["--n-gpu-layers", "all"]
}

#[cfg(not(target_os = "macos"))]
fn gpu_arguments() -> [&'static str; 2] {
    ["--n-gpu-layers", "0"]
}

#[cfg(windows)]
fn hide_console_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_: &mut Command) {}

fn free_port() -> Result<u16, InferenceError> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|error| InferenceError::LoadFailed(format!("no free port: {error}")))
}

/// Prevents other local programs from using the server Luna started.
fn random_key() -> String {
    let state = RandomState::new();
    format!("{:016x}{:016x}", state.hash_one(1u8), state.hash_one(2u8))
}

async fn collect_output(stderr: tokio::process::ChildStderr, output: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        log::debug!(target: "llama-server", "{line}");
        let mut output = lock(&output);
        if output.len() == STDERR_LINES {
            output.pop_front();
        }
        output.push_back(line);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_long() {
        let (first, second) = (random_key(), random_key());
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
    }
}
