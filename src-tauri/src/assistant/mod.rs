//! Runs a conversation turn: the model interprets, Rust validates and executes.
mod context;
mod tools;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::actions::{self, ControlRequest, ExecutionReport, Plan};
use crate::history::{Interaction, now_millis};
use crate::home_assistant::{HomeCache, ServiceCaller};
use crate::inference::{ChatMessage, Engine, FunctionCall, InferenceError, ModelSpec, Role};
use tools::ToolRequest;

const MAX_STEPS: usize = 6;
const HISTORY_TURNS: usize = 4;
const HISTORY_WINDOW_MS: i64 = 10 * 60 * 1000;
const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(120);

const INSTRUCTIONS: &str = "You are Luna, a private voice assistant for a Home Assistant smart home. \
Reply in one short, natural sentence. Do not repeat the request back. Never use em dashes.

Rules:
- Use get_states to answer questions about the home. Use control to change devices.
- Only use floor, area, and entity IDs from the home summary or tool results. Never invent IDs.
- For everything in a place, target the floor or area; Luna skips devices that cannot do the action. \
Put exceptions in exclude_entities or exclude_areas.
- Only say a device changed if the control result says it was done and verified. \
Mention anything that failed, was not confirmed, or was left out.
- If a request is ambiguous or a target is unclear, ask one short question instead of guessing.
- Luna asks the user to confirm unlocking and opening doors itself. Just call control.
- Answer general questions briefly without tools.";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AssistantError {
    #[error(transparent)]
    Inference(#[from] InferenceError),
    #[error("model did not finish within {MAX_STEPS} steps")]
    TooManySteps,
    #[error("request cancelled")]
    Cancelled,
    #[error("another request is in progress")]
    Busy,
    #[error("confirmation expired or unknown")]
    ConfirmationExpired,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    pub results: Vec<String>,
    /// Set when a sensitive action is waiting for the user to confirm it.
    pub confirmation: Option<ControlRequest>,
}

/// Something that can answer a chat request. Production always uses the embedded engine.
pub trait Chat {
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> impl Future<Output = Result<ChatMessage, InferenceError>> + Send;
}

pub struct EngineChat<'a> {
    pub engine: &'a Engine,
    pub spec: &'a ModelSpec,
}

impl Chat for EngineChat<'_> {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<ChatMessage, InferenceError> {
        self.engine.chat(self.spec, messages, tools).await
    }
}

pub struct Home<'a, C> {
    pub cache: &'a HomeCache,
    pub caller: &'a C,
    pub connected: bool,
}

pub async fn respond<C: ServiceCaller>(
    model: &impl Chat,
    home: Home<'_, C>,
    history: &[Interaction],
    request: &str,
    cancel: &CancellationToken,
) -> Result<Reply, AssistantError> {
    let tools = tools::definitions();
    let mut messages = vec![ChatMessage::new(
        Role::System,
        system_prompt(&home, history, request),
    )];
    messages.extend(history_messages(history));
    messages.push(ChatMessage::new(Role::User, request));
    let mut results = Vec::new();

    for _ in 0..MAX_STEPS {
        let reply = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(AssistantError::Cancelled),
            reply = model.chat(&messages, &tools) => reply?,
        };
        if reply.tool_calls.is_empty() {
            let text = final_text(&reply.content, &results);
            return Ok(Reply {
                text,
                results,
                confirmation: None,
            });
        }
        messages.push(reply.clone());
        for call in &reply.tool_calls {
            match run_tool(&call.function, &home).await {
                ToolOutcome::Text(text) => messages.push(ChatMessage::tool_result(call, text)),
                ToolOutcome::Executed(report) => {
                    results.extend(report.summary());
                    messages.push(ChatMessage::tool_result(call, report.for_model()));
                }
                ToolOutcome::NeedsConfirmation { prompt, request } => {
                    return Ok(Reply {
                        text: prompt,
                        results,
                        confirmation: Some(*request),
                    });
                }
            }
        }
    }
    Err(AssistantError::TooManySteps)
}

/// Executes a confirmed request after validating it again against the current state.
pub async fn confirm<C: ServiceCaller>(home: Home<'_, C>, request: &ControlRequest) -> Reply {
    let text = if !home.connected {
        "Home Assistant isn't connected, so nothing changed.".to_owned()
    } else {
        let plan = actions::plan(&home.cache.read(), request);
        match plan {
            Ok(plan) => actions::execute(&plan, home.caller, home.cache)
                .await
                .summary()
                .join(" "),
            Err(error) => {
                log::warn!("confirmed request is no longer valid: {error}");
                "That can't be done right now, so nothing changed.".to_owned()
            }
        }
    };
    Reply {
        text,
        results: Vec::new(),
        confirmation: None,
    }
}

enum ToolOutcome {
    Text(String),
    Executed(ExecutionReport),
    NeedsConfirmation {
        prompt: String,
        request: Box<ControlRequest>,
    },
}

async fn run_tool<C: ServiceCaller>(call: &FunctionCall, home: &Home<'_, C>) -> ToolOutcome {
    let request = match tools::parse(call) {
        Ok(request) => request,
        Err(error) => return ToolOutcome::Text(error),
    };
    if !home.connected {
        return ToolOutcome::Text(
            "Home Assistant is not connected. Tell the user to check the connection in Settings."
                .into(),
        );
    }
    match request {
        ToolRequest::GetStates(target) => {
            let snapshot = home.cache.read();
            ToolOutcome::Text(match actions::select(&snapshot, &target) {
                Ok(selection) => {
                    let entities: Vec<_> = selection
                        .entities
                        .iter()
                        .map(|selected| selected.entity)
                        .collect();
                    context::describe_states(&snapshot, &entities)
                }
                Err(error) => format!("Rejected: {error}"),
            })
        }
        ToolRequest::Control(request) => {
            let plan = actions::plan(&home.cache.read(), &request);
            match plan {
                Err(error) => ToolOutcome::Text(format!("Rejected, nothing changed: {error}")),
                Ok(plan) if plan.requires_confirmation => ToolOutcome::NeedsConfirmation {
                    prompt: confirmation_prompt(&plan),
                    request: Box::new(request),
                },
                Ok(plan) => {
                    ToolOutcome::Executed(actions::execute(&plan, home.caller, home.cache).await)
                }
            }
        }
    }
}

fn system_prompt<C>(home: &Home<'_, C>, history: &[Interaction], request: &str) -> String {
    if !home.connected {
        return format!("{INSTRUCTIONS}\n\nHome Assistant is not connected.");
    }
    // Include the previous request so follow-ups like "turn it off" find the same devices.
    let previous = history
        .last()
        .map_or("", |interaction| interaction.request.as_str());
    let summary = context::summarize(&home.cache.read(), &format!("{previous} {request}"));
    format!("{INSTRUCTIONS}\n\nHome summary:\n{summary}")
}

fn history_messages(history: &[Interaction]) -> Vec<ChatMessage> {
    let cutoff = now_millis() - HISTORY_WINDOW_MS;
    let recent: Vec<&Interaction> = history
        .iter()
        .filter(|interaction| {
            interaction.created_at >= cutoff && !interaction.awaiting_confirmation
        })
        .collect();
    recent[recent.len().saturating_sub(HISTORY_TURNS)..]
        .iter()
        .flat_map(|interaction| {
            let mut response = interaction.response.clone();
            if !interaction.results.is_empty() {
                response.push_str(&format!(" (Results: {})", interaction.results.join(" ")));
            }
            [
                ChatMessage::new(Role::User, interaction.request.clone()),
                ChatMessage::new(Role::Assistant, response),
            ]
        })
        .collect()
}

fn confirmation_prompt(plan: &Plan) -> String {
    let names: Vec<&str> = plan
        .entities
        .iter()
        .map(|entity| entity.name.as_str())
        .collect();
    let verb = actions::verb(plan.action);
    actions::capitalize(&format!("{verb} {}?", actions::list(&names)))
}

fn final_text(content: &str, results: &[String]) -> String {
    let text = strip_reasoning(content)
        .replace(" \u{2014} ", ", ")
        .replace('\u{2014}', ", ")
        .trim()
        .to_owned();
    match (text.is_empty(), results.is_empty()) {
        (false, _) => text,
        (true, false) => results.join(" "),
        (true, true) => "Sorry, I don't have an answer for that.".into(),
    }
}

fn strip_reasoning(content: &str) -> &str {
    match content.split_once("</think>") {
        Some((_, rest)) => rest,
        None => content,
    }
}

/// Tracks the in-flight request and any action awaiting confirmation.
#[derive(Default)]
pub struct Session {
    active: Mutex<Option<CancellationToken>>,
    pending: Mutex<Option<PendingConfirmation>>,
}

struct PendingConfirmation {
    interaction_id: i64,
    request: ControlRequest,
    created: Instant,
}

impl Session {
    pub fn begin(&self) -> Result<CancellationToken, AssistantError> {
        let mut active = lock(&self.active);
        if active.is_some() {
            return Err(AssistantError::Busy);
        }
        // A new request replaces any unanswered confirmation.
        lock(&self.pending).take();
        let token = CancellationToken::new();
        *active = Some(token.clone());
        Ok(token)
    }

    pub fn finish(&self) {
        lock(&self.active).take();
    }

    pub fn cancel(&self) {
        if let Some(token) = lock(&self.active).as_ref() {
            token.cancel();
        }
    }

    pub fn await_confirmation(&self, interaction_id: i64, request: ControlRequest) {
        *lock(&self.pending) = Some(PendingConfirmation {
            interaction_id,
            request,
            created: Instant::now(),
        });
    }

    pub fn take_confirmation(&self, interaction_id: i64) -> Result<ControlRequest, AssistantError> {
        match lock(&self.pending).take() {
            Some(confirmation)
                if confirmation.interaction_id == interaction_id
                    && confirmation.created.elapsed() < CONFIRMATION_TIMEOUT =>
            {
                Ok(confirmation.request)
            }
            _ => Err(AssistantError::ConfirmationExpired),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use serde_json::json;

    use super::*;
    use crate::home_assistant::model::fixtures::{home, state};
    use crate::home_assistant::{HaError, ServiceCall};
    use crate::inference::ToolCall;

    struct RecordingHomeAssistant<'a> {
        cache: &'a HomeCache,
        calls: StdMutex<Vec<ServiceCall>>,
    }

    impl ServiceCaller for RecordingHomeAssistant<'_> {
        async fn call_service(&self, call: &ServiceCall) -> Result<(), HaError> {
            self.calls.lock().unwrap().push(call.clone());
            let new_state = if call.service == "turn_off" {
                "off"
            } else {
                "on"
            };
            for id in &call.entity_ids {
                self.cache
                    .update(|home| home.apply_state(id, Some(state(id, new_state, json!({})))));
            }
            Ok(())
        }
    }

    /// Replies with each scripted message in turn and records what it was sent.
    struct ScriptedModel {
        replies: StdMutex<Vec<ChatMessage>>,
        received: StdMutex<Vec<Vec<ChatMessage>>>,
    }

    impl ScriptedModel {
        fn new(replies: Vec<ChatMessage>) -> Self {
            Self {
                replies: StdMutex::new(replies.into_iter().rev().collect()),
                received: StdMutex::default(),
            }
        }

        fn last_message(&self) -> ChatMessage {
            self.received
                .lock()
                .unwrap()
                .last()
                .unwrap()
                .last()
                .unwrap()
                .clone()
        }
    }

    impl Chat for ScriptedModel {
        async fn chat(
            &self,
            messages: &[ChatMessage],
            _: &Value,
        ) -> Result<ChatMessage, InferenceError> {
            self.received.lock().unwrap().push(messages.to_vec());
            self.replies
                .lock()
                .unwrap()
                .pop()
                .ok_or(InferenceError::Request("no scripted reply".into()))
        }
    }

    fn tool_call(name: &str, arguments: Value) -> ChatMessage {
        ChatMessage {
            tool_calls: vec![ToolCall {
                id: "call-1".into(),
                function: FunctionCall {
                    name: name.into(),
                    arguments,
                },
            }],
            ..ChatMessage::new(Role::Assistant, "")
        }
    }

    fn text(content: &str) -> ChatMessage {
        ChatMessage::new(Role::Assistant, content)
    }

    async fn run(
        model: &ScriptedModel,
        cache: &HomeCache,
        fake: &RecordingHomeAssistant<'_>,
        request: &str,
    ) -> Result<Reply, AssistantError> {
        let home = Home {
            cache,
            caller: fake,
            connected: true,
        };
        respond(model, home, &[], request, &CancellationToken::new()).await
    }

    fn recorder(cache: &HomeCache) -> RecordingHomeAssistant<'_> {
        RecordingHomeAssistant {
            cache,
            calls: StdMutex::default(),
        }
    }

    #[tokio::test]
    async fn executes_validated_tool_calls_and_reports_results() {
        let model = ScriptedModel::new(vec![
            tool_call(
                "control",
                json!({"action": "turn_off", "target": {"floors": ["downstairs"], "domains": ["light"], "exclude_entities": ["light.hallway"]}}),
            ),
            text("Done, the lights downstairs are off except the hallway."),
        ]);
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);

        let reply = run(
            &model,
            &cache,
            &fake,
            "Turn off the lights downstairs except the hallway",
        )
        .await
        .unwrap();

        assert_eq!(reply.results, ["Turned off kitchen and living room lamp."]);
        assert_eq!(cache.read().entity("light.hallway").unwrap().state, "on");
        let tool_message = model.last_message();
        assert_eq!(tool_message.role, Role::Tool);
        assert_eq!(tool_message.tool_call_id.as_deref(), Some("call-1"));
    }

    #[tokio::test]
    async fn sensitive_actions_wait_for_confirmation() {
        let model = ScriptedModel::new(vec![tool_call(
            "control",
            json!({"action": "unlock", "target": {"entities": ["lock.front_door"]}}),
        )]);
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);

        let reply = run(&model, &cache, &fake, "Unlock the front door")
            .await
            .unwrap();

        assert_eq!(reply.text, "Unlock front door?");
        assert!(reply.confirmation.is_some());
        assert!(fake.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_tool_calls_are_returned_to_the_model() {
        let model = ScriptedModel::new(vec![
            tool_call(
                "control",
                json!({"action": "turn_off", "target": {"entities": ["light.made_up"]}}),
            ),
            text("I couldn't find that light. Which one did you mean?"),
        ]);
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);

        let reply = run(&model, &cache, &fake, "Turn off the made up light")
            .await
            .unwrap();

        assert!(reply.results.is_empty());
        assert!(fake.calls.lock().unwrap().is_empty());
        assert!(
            model
                .last_message()
                .content
                .contains("unknown entity id: light.made_up")
        );
    }

    #[tokio::test]
    async fn inference_errors_are_returned() {
        let model = ScriptedModel::new(vec![]);
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);
        let result = run(&model, &cache, &fake, "hi").await;
        assert!(matches!(result, Err(AssistantError::Inference(_))));
    }

    #[tokio::test]
    async fn cancelled_requests_stop_before_the_model_answers() {
        let model = ScriptedModel::new(vec![text("hello")]);
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let home = Home {
            cache: &cache,
            caller: &fake,
            connected: true,
        };
        let result = respond(&model, home, &[], "hi", &cancel).await;
        assert_eq!(result, Err(AssistantError::Cancelled));
    }

    #[tokio::test]
    async fn confirmed_requests_are_validated_and_executed() {
        let cache = HomeCache::new(home());
        let fake = recorder(&cache);
        let request: ControlRequest = serde_json::from_value(
            json!({"action": "turn_off", "target": {"entities": ["light.kitchen"]}}),
        )
        .unwrap();
        let home = Home {
            cache: &cache,
            caller: &fake,
            connected: true,
        };
        assert_eq!(confirm(home, &request).await.text, "Turned off kitchen.");
    }

    #[test]
    fn session_rejects_parallel_requests_and_expires_confirmations() {
        let session = Session::default();
        let _token = session.begin().unwrap();
        assert_eq!(session.begin().err(), Some(AssistantError::Busy));
        session.finish();

        let request: ControlRequest = serde_json::from_value(
            json!({"action": "unlock", "target": {"entities": ["lock.front_door"]}}),
        )
        .unwrap();
        session.await_confirmation(7, request.clone());
        assert_eq!(
            session.take_confirmation(8).err(),
            Some(AssistantError::ConfirmationExpired)
        );
        session.await_confirmation(7, request.clone());
        assert_eq!(session.take_confirmation(7), Ok(request));
        assert!(session.take_confirmation(7).is_err());
    }

    #[test]
    fn final_text_removes_reasoning_and_em_dashes() {
        assert_eq!(
            final_text("<think>hmm</think> It's 20° \u{2014} warm.", &[]),
            "It's 20°, warm."
        );
        assert_eq!(
            final_text("", &["Turned off kitchen.".into()]),
            "Turned off kitchen."
        );
    }
}
