//! Runs a conversation turn. Simple requests are handled in Rust; the model interprets the rest,
//! and Rust validates and executes whatever it proposes.
mod clock;
mod context;
mod direct;
mod phrasing;
mod relevance;
mod route;
mod session;
mod tools;

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::actions::{self, ControlRequest, ExecutionReport};
use crate::credentials::AccessToken;
use crate::history::{Interaction, now_millis};
use crate::home_assistant::{HomeApi, HomeCache};
use crate::inference::{
    ChatMessage, CloudClient, Completion, Engine, FunctionCall, InferenceError, ModelSpec, Role,
    Timings, ToolCall, Warmup,
};
use crate::models::CloudModel;
pub use relevance::{Relevance, classify as relevance};
pub use route::vocabulary;
pub use session::{CONVERSATION_LIFETIME, Memory, Session};
use session::{MAX_REFERENCED, Turn};
use tools::ToolRequest;

const MAX_STEPS: usize = 4;
const OUT_OF_SCOPE: &str = "I can only help with your home.";
const HISTORY_TURNS: usize = 3;

const INSTRUCTIONS: &str = "You are Luna, the voice assistant for one Home Assistant home. \
You help only with this home: its devices, sensors, rooms, and their states. \
Requests like \"I'm heading to bed\" or \"it's too warm\" are about the home. \
For questions unrelated to the home, reply exactly: I can only help with your home. Never use that reply after calling a tool.

Rules:
- Use control to change devices. Use get_states for states not listed in the request context.
- The request context shows the true current states. Never say something changed unless control changed it in this turn. \
If the user says an action did not work, call control again. If the user only states or disputes a state, report the current state and change nothing.
- Only use floor, area, and entity IDs from the home layout, the request context, or tool results. Never invent IDs or devices.
- For everything in a place, target that area or floor. Add domains when the request is about one kind of device. \
Only when the user names an exception, use one control call with exclude_entities or exclude_areas, never a second call that reverses the first.
- Examples: \"turn off everything downstairs except the hallway light\" is control turn_off with target {\"floors\": [\"downstairs\"], \"exclude_entities\": [\"light.hallway\"]}. \
\"It's too bright in the living room\" is control set_brightness with target {\"areas\": [\"living_room\"], \"domains\": [\"light\"]} and a lower value.
- For routines like going to bed, act on the obvious devices in the request context, or ask one short question if unsure.
- \"It\", \"that\", and \"them\" mean the recently referenced entities.
- To undo the last action, use control to restore the states listed before the last action.
- If a request is ambiguous, or a tool call is rejected and you cannot fix it, ask one short question.
- Luna asks the user to confirm unlocking and opening doors. Just call control.
- Never state the time unless the request context gives it.
- Reply in one short sentence. Do not repeat the request. Never use em dashes.";

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Route {
    /// Answered in Rust without the model.
    #[default]
    Direct,
    Model,
}

/// Where the time went for one request. Logged, never shown to users.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metrics {
    pub route: Route,
    pub passes: u32,
    pub inference: Timings,
    /// Wall time spent waiting for the model, including network time for cloud providers.
    pub inference_time: Duration,
    pub service_time: Duration,
    pub verify_time: Duration,
    pub total: Duration,
}

impl Metrics {
    fn record(&mut self, report: &ExecutionReport) {
        self.service_time += report.service_time;
        self.verify_time += report.verify_time;
    }
}

impl fmt::Display for Metrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inference = &self.inference;
        write!(
            f,
            "route {:?}, total {} ms, {} model passes in {} ms, prompt {} tokens ({} cached) \
             in {:.0} ms, generated {} tokens in {:.0} ms, service calls {} ms, verification {} ms",
            self.route,
            self.total.as_millis(),
            self.passes,
            self.inference_time.as_millis(),
            inference.prompt_tokens,
            inference.cached_tokens,
            inference.prompt_ms,
            inference.generated_tokens,
            inference.generation_ms,
            self.service_time.as_millis(),
            self.verify_time.as_millis(),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub text: String,
    /// Verified result lines shown under a reply the model wrote.
    pub results: Vec<String>,
    /// Sensitive actions waiting for the user to confirm them.
    pub confirmation: Vec<ControlRequest>,
    pub metrics: Metrics,
}

impl Reply {
    fn direct(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            results: Vec::new(),
            confirmation: Vec::new(),
            metrics: Metrics::default(),
        }
    }
}

/// Something that can answer a chat request. Production uses the provider the user selected.
pub trait Chat {
    fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> impl Future<Output = Result<Completion, InferenceError>> + Send;
}

/// The selected inference provider. Only it answers; another is never used in its place.
pub enum Provider<'a> {
    Local {
        engine: &'a Engine,
        spec: ModelSpec,
    },
    Cloud {
        client: &'a CloudClient,
        model: &'static CloudModel,
        key: AccessToken,
    },
}

impl Chat for Provider<'_> {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<Completion, InferenceError> {
        match self {
            Self::Local { engine, spec } => engine.chat(spec, messages, tools).await,
            Self::Cloud { client, model, key } => client.chat(model, key, messages, tools).await,
        }
    }
}

pub struct Home<'a, A> {
    pub cache: &'a HomeCache,
    pub api: &'a A,
    pub connected: bool,
}

pub async fn respond<A: HomeApi>(
    model: &impl Chat,
    home: Home<'_, A>,
    history: &[Interaction],
    memory: &mut Memory,
    request: &str,
    cancel: &CancellationToken,
) -> Result<Reply, AssistantError> {
    let started = Instant::now();
    let mut reply = match direct::handle(&home, memory, request).await {
        Some(reply) => reply,
        // Without Home Assistant the model has nothing to act on.
        None if !home.connected => Reply::direct(direct::NOT_CONNECTED),
        None => ask_model(model, &home, history, memory, request, cancel).await?,
    };
    reply.metrics.total = started.elapsed();
    Ok(reply)
}

/// Loads the model and prepares the parts of the prompt that every request shares.
pub async fn warm_up<A>(
    engine: &Engine,
    spec: &ModelSpec,
    home: Home<'_, A>,
) -> Result<Warmup, InferenceError> {
    let messages = [
        ChatMessage::new(Role::System, system_prompt(&home)),
        ChatMessage::new(Role::User, "Hello"),
    ];
    engine.warm(spec, &messages, &tools::definitions()).await
}

/// Executes confirmed requests after validating them again against the current state.
pub async fn confirm<A: HomeApi>(
    home: Home<'_, A>,
    memory: &mut Memory,
    requests: &[ControlRequest],
) -> Reply {
    if !home.connected {
        return Reply::direct("Home Assistant isn't connected, so nothing changed.");
    }
    let plans = match direct::plan_all(home.cache, requests) {
        Ok(plans) => plans,
        Err(error) => {
            log::warn!("confirmed request is no longer valid: {error}");
            return Reply::direct("That can't be done right now, so nothing changed.");
        }
    };
    let mut reply = Reply::direct("");
    let mut turn = Turn::default();
    let reports = direct::execute_all(&home, &plans, &mut turn, &mut reply.metrics).await;
    memory.record(turn);
    // Security-sensitive results always name the device.
    reply.text = phrasing::acknowledge(&reports, false, memory.next_variant());
    reply
}

async fn ask_model<A: HomeApi>(
    model: &impl Chat,
    home: &Home<'_, A>,
    history: &[Interaction],
    memory: &mut Memory,
    request: &str,
    cancel: &CancellationToken,
) -> Result<Reply, AssistantError> {
    let tools = tools::definitions();
    let mut messages = vec![ChatMessage::new(Role::System, system_prompt(home))];
    messages.extend(history_messages(history));
    messages.push(ChatMessage::new(
        Role::User,
        user_prompt(home, memory, request),
    ));
    let mut reply = Reply {
        metrics: Metrics {
            route: Route::Model,
            ..Metrics::default()
        },
        ..Reply::direct("")
    };
    let mut turn = Turn::default();
    let mut prompts = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    for _ in 0..MAX_STEPS {
        let started = Instant::now();
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(AssistantError::Cancelled),
            completion = model.chat(&messages, &tools) => completion.map_err(AssistantError::from),
        };
        reply.metrics.inference_time += started.elapsed();
        let completion = match result {
            Ok(completion) => completion,
            Err(error) => {
                memory.record(turn);
                if reply.results.is_empty() || error == AssistantError::Cancelled {
                    return Err(error);
                }
                // Verified actions are still reported, so the user never repeats them.
                log::error!("model failed after actions were executed: {error}");
                notes.append(&mut reply.results);
                reply.text = notes.join(" ");
                return Ok(reply);
            }
        };
        reply.metrics.passes += 1;

        reply.metrics.inference.add(completion.timings);
        let message = completion.message;
        if message.tool_calls.is_empty() {
            memory.record(turn);
            let model_wrote = !strip_reasoning(&message.content).trim().is_empty();
            reply.text = final_text(&message.content, &reply.results);
            if !model_wrote && !notes.is_empty() {
                reply.text = format!("{} {}", notes.join(" "), reply.text);
            }
            // A rejected tool call means the request was about the home, just not doable as asked.
            if reply.metrics.passes > 1 && reply.text == OUT_OF_SCOPE {
                reply.text = if notes.is_empty() {
                    "I couldn't do that. Try naming the device and what to change.".into()
                } else {
                    notes.join(" ")
                };
            }
            return Ok(reply);
        }
        for call in &message.tool_calls {
            log::info!(
                "model called {} {}",
                call.function.name,
                call.function.arguments
            );
        }

        messages.push(message.clone());
        if let Some(conflict) = conflicting_calls(&message.tool_calls, home) {
            log::info!("rejected conflicting tool calls: {conflict}");
            for call in &message.tool_calls {
                messages.push(ChatMessage::tool_result(call, conflict.clone()));
            }
            continue;
        }
        let mut needs_model = false;
        for call in &message.tool_calls {
            let content = match run_tool(&call.function, home, request, &mut turn).await {
                ToolOutcome::Text(text) => {
                    needs_model = true;
                    text
                }
                ToolOutcome::Rejected { message, note } => {
                    needs_model = true;
                    notes.extend(note.filter(|note| !notes.contains(note)));
                    message
                }
                ToolOutcome::Executed(report) => {
                    reply.metrics.record(&report);
                    reply
                        .results
                        .extend(phrasing::result_lines(&report, memory.next_variant()));
                    report.for_model()
                }
                ToolOutcome::NeedsConfirmation { prompt, request } => {
                    prompts.push(prompt);
                    reply.confirmation.push(*request);
                    "Waiting for the user to confirm.".into()
                }
            };
            messages.push(ChatMessage::tool_result(call, content));
        }
        // Verified results already say what happened, so no second pass is needed.
        if !needs_model || !reply.confirmation.is_empty() {
            memory.record(turn);
            reply.results.append(&mut prompts);
            notes.append(&mut reply.results);
            reply.text = notes.join(" ");
            return Ok(reply);
        }
    }
    memory.record(turn);
    Err(AssistantError::TooManySteps)
}

/// Calls that leave the same entity in opposite states, such as turning a floor off and one
/// light back on, would briefly change a device the user asked to leave alone.
fn conflicting_calls<A>(calls: &[ToolCall], home: &Home<'_, A>) -> Option<String> {
    let snapshot = home.cache.read();
    let mut planned: HashMap<String, Vec<(actions::Action, Option<f64>)>> = HashMap::new();
    for call in calls {
        let Ok(ToolRequest::Control(request)) = tools::parse(&call.function) else {
            continue;
        };
        let Ok(plan) = actions::plan(&snapshot, &request) else {
            continue;
        };
        for entity in &plan.entities {
            planned
                .entry(entity.id.clone())
                .or_default()
                .push((plan.action, plan.value));
        }
    }
    let mut conflicts: Vec<&str> = planned
        .iter()
        .filter(|(_, steps)| {
            steps.iter().enumerate().any(|(index, first)| {
                steps[index + 1..]
                    .iter()
                    .any(|second| opposed(*first, *second))
            })
        })
        .map(|(id, _)| id.as_str())
        .collect();
    conflicts.sort_unstable();
    (!conflicts.is_empty()).then(|| {
        format!(
            "Rejected, nothing changed: these calls conflict on {}. Use one call with exclude_entities for exceptions.",
            conflicts.join(", ")
        )
    })
}

/// Turning on and setting brightness or temperature go together; on and off do not.
fn opposed(first: (actions::Action, Option<f64>), second: (actions::Action, Option<f64>)) -> bool {
    use actions::Action::*;
    let leaves_on = |(action, value): (actions::Action, Option<f64>)| match action {
        TurnOn | Open | Unlock => Some(true),
        TurnOff | Close | Lock => Some(false),
        SetBrightness => Some(value.is_some_and(|value| value > 0.0)),
        SetTemperature | Activate => None,
    };
    let same_action_other_value = first.0 == second.0 && first.1 != second.1;
    let opposite_states =
        matches!((leaves_on(first), leaves_on(second)), (Some(a), Some(b)) if a != b);
    same_action_other_value || opposite_states
}

enum ToolOutcome {
    Text(String),
    /// A control call that failed validation, with a note for the user if the device itself
    /// cannot do it, so a substitute action is never reported alone.
    Rejected {
        message: String,
        note: Option<String>,
    },
    Executed(ExecutionReport),
    NeedsConfirmation {
        prompt: String,
        request: Box<ControlRequest>,
    },
}

async fn run_tool<A: HomeApi>(
    call: &FunctionCall,
    home: &Home<'_, A>,
    user_request: &str,
    turn: &mut Turn,
) -> ToolOutcome {
    let request = match tools::parse(call) {
        Ok(request) => request,
        Err(error) => {
            log::info!("tool call rejected: {error}");
            return ToolOutcome::Text(error);
        }
    };
    let target = match &request {
        ToolRequest::GetStates(target) => target,
        ToolRequest::Control(control) => &control.target,
    };
    let excludes = !target.exclude_entities.is_empty() || !target.exclude_areas.is_empty();
    if excludes && !names_an_exception(user_request) {
        log::info!("tool call rejected: exclusions without an exception in the request");
        return ToolOutcome::Text(
            "Rejected, nothing changed: the user named no exception. Remove exclude_entities and exclude_areas."
                .into(),
        );
    }
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
                    if entities.len() <= MAX_REFERENCED {
                        for entity in &entities {
                            turn.refer(&entity.id, direct::state_phrase(entity));
                        }
                    }
                    context::describe_states(&snapshot, &entities)
                }
                Err(error) => {
                    log::info!("tool call rejected: {error}");
                    format!("Rejected: {error}")
                }
            })
        }
        ToolRequest::Control(request) => {
            let plan = actions::plan(&home.cache.read(), &request);
            match plan {
                Err(error) => {
                    log::info!("tool call rejected: {error}");
                    ToolOutcome::Rejected {
                        message: format!("Rejected, nothing changed: {error}"),
                        note: direct::device_limit(&error),
                    }
                }
                Ok(plan) if plan.requires_confirmation => ToolOutcome::NeedsConfirmation {
                    prompt: direct::confirmation_prompt(std::slice::from_ref(&plan)),
                    request: Box::new(request),
                },
                Ok(plan) => {
                    let report = actions::execute(&plan, home.api, home.cache).await;
                    direct::record_execution(&report, home.cache, turn);
                    ToolOutcome::Executed(report)
                }
            }
        }
    }
}

/// Exclusions are only valid when the user asked for one, as in "everything except the hallway".
fn names_an_exception(request: &str) -> bool {
    const EXCEPTION_WORDS: &[&str] = &[
        "except",
        "but",
        "not",
        "without",
        "besides",
        "excluding",
        "other",
        "apart",
        "leave",
        "keep",
        "skip",
    ];
    request
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| EXCEPTION_WORDS.contains(&word))
}

/// Instructions and the home layout. Identical across requests so the model reuses its cache.
fn system_prompt<A>(home: &Home<'_, A>) -> String {
    let layout = context::layout(&home.cache.read());
    format!("{INSTRUCTIONS}\n\nHome layout:\n{layout}")
}

fn user_prompt<A>(home: &Home<'_, A>, memory: &Memory, request: &str) -> String {
    let snapshot = home.cache.read();
    let mentions_time = request
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word == "time");
    let time = match mentions_time.then(|| clock::read(&snapshot)) {
        Some(clock::ClockReading::Time(time)) => Some(time),
        _ => None,
    };
    let context = context::request_context(&snapshot, memory, request, time.as_deref());
    format!("Request context:\n{context}\nRequest: {request}")
}

/// Recent turns as plain text. Facts about devices come from the request context instead.
fn history_messages(history: &[Interaction]) -> Vec<ChatMessage> {
    let cutoff = now_millis() - session::CONVERSATION_LIFETIME.as_millis() as i64;
    let recent: Vec<&Interaction> = history
        .iter()
        .filter(|interaction| {
            interaction.created_at >= cutoff && !interaction.awaiting_confirmation
        })
        .collect();
    recent[recent.len().saturating_sub(HISTORY_TURNS)..]
        .iter()
        .flat_map(|interaction| {
            [
                ChatMessage::new(Role::User, interaction.request.clone()),
                ChatMessage::new(Role::Assistant, interaction.response.clone()),
            ]
        })
        .collect()
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

#[cfg(test)]
mod tests;
