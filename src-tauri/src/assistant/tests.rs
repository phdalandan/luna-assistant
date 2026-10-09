use std::sync::Mutex;

use serde_json::json;

use super::*;
use crate::home_assistant::model::fixtures::{large_home, state};
use crate::home_assistant::model::{Home as HomeModel, StateEntry};
use crate::home_assistant::{HaError, ServiceCall};

/// Applies the result of each service call to the cache, like Home Assistant's state events.
struct FakeHomeAssistant<'a> {
    cache: &'a HomeCache,
    calls: Mutex<Vec<ServiceCall>>,
    fail: bool,
    /// States Home Assistant reports on a fresh read, when they differ from the cache.
    actual: Mutex<Vec<StateEntry>>,
}

impl HomeApi for FakeHomeAssistant<'_> {
    async fn call_service(&self, call: &ServiceCall) -> Result<(), HaError> {
        self.calls.lock().unwrap().push(call.clone());
        if self.fail {
            return Err(HaError::Timeout);
        }
        for id in &call.entity_ids {
            let mut attributes = self.cache.read().entity(id).unwrap().attributes.clone();
            let new_state = match call.service {
                "turn_off" => "off",
                "open_cover" => "open",
                "close_cover" => "closed",
                "lock" => "locked",
                "unlock" => "unlocked",
                _ => "on",
            };
            if let Some(percent) = call.data.get("brightness_pct").and_then(Value::as_f64) {
                attributes.insert(
                    "brightness".into(),
                    json!((percent * 255.0 / 100.0).round()),
                );
            }
            // Setting a temperature keeps the current mode, as in Home Assistant.
            let new_state = match call.data.get("temperature") {
                Some(temperature) => {
                    attributes.insert("temperature".into(), temperature.clone());
                    self.cache.read().entity(id).unwrap().state.clone()
                }
                None => new_state.to_owned(),
            };
            let entry = StateEntry {
                entity_id: id.clone(),
                state: new_state,
                attributes,
            };
            self.cache.update(|home| home.apply_state(id, Some(entry)));
        }
        Ok(())
    }

    async fn refresh_states(&self) -> Result<(), HaError> {
        for entry in self.actual.lock().unwrap().drain(..) {
            self.cache
                .update(|home| home.apply_state(&entry.entity_id.clone(), Some(entry)));
        }
        Ok(())
    }
}

/// Replies with each scripted message in turn and records what it was sent.
struct ScriptedModel {
    replies: Mutex<Vec<ChatMessage>>,
    received: Mutex<Vec<Vec<ChatMessage>>>,
}

impl ScriptedModel {
    fn new(replies: Vec<ChatMessage>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().rev().collect()),
            received: Mutex::default(),
        }
    }

    /// For requests that must never reach the model.
    fn unused() -> Self {
        Self::new(vec![])
    }

    fn passes(&self) -> usize {
        self.received.lock().unwrap().len()
    }

    fn last_message(&self) -> ChatMessage {
        let received = self.received.lock().unwrap();
        received.last().unwrap().last().unwrap().clone()
    }
}

impl Chat for ScriptedModel {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        _: &Value,
    ) -> Result<Completion, InferenceError> {
        self.received.lock().unwrap().push(messages.to_vec());
        let message = self
            .replies
            .lock()
            .unwrap()
            .pop()
            .ok_or(InferenceError::Request("no scripted reply".into()))?;
        Ok(Completion {
            message,
            timings: Timings::default(),
        })
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

/// Real-model tests can add entities with `LUNA_TEST_EXTRA_ENTITIES` to measure a larger home.
fn test_home() -> HomeModel {
    let extra = std::env::var("LUNA_TEST_EXTRA_ENTITIES").map_or(0, |extra| extra.parse().unwrap());
    let mut home = large_home(extra);
    home.apply_state(
        "light.front_porch",
        Some(state(
            "light.front_porch",
            "off",
            json!({"friendly_name": "Front Porch", "supported_color_modes": ["brightness"]}),
        )),
    );
    home.apply_state(
        "sensor.time",
        Some(state(
            "sensor.time",
            "17:19",
            json!({"friendly_name": "Time"}),
        )),
    );
    home
}

/// One conversation: a cache, a fake Home Assistant, and memory carried between requests.
struct Conversation {
    cache: HomeCache,
    memory: Mutex<Memory>,
}

impl Conversation {
    fn new() -> Self {
        Self {
            cache: HomeCache::new(test_home()),
            memory: Mutex::default(),
        }
    }

    fn api(&self) -> FakeHomeAssistant<'_> {
        FakeHomeAssistant {
            cache: &self.cache,
            calls: Mutex::default(),
            fail: false,
            actual: Mutex::default(),
        }
    }

    async fn ask(&self, model: &ScriptedModel, request: &str) -> Reply {
        let api = self.api();
        self.ask_with(model, &api, request).await
    }

    async fn ask_with(
        &self,
        model: &impl Chat,
        api: &FakeHomeAssistant<'_>,
        request: &str,
    ) -> Reply {
        self.ask_after(model, api, &[], request).await.unwrap()
    }

    async fn ask_after(
        &self,
        model: &impl Chat,
        api: &FakeHomeAssistant<'_>,
        history: &[Interaction],
        request: &str,
    ) -> Result<Reply, AssistantError> {
        let home = Home {
            cache: &self.cache,
            api,
            connected: true,
        };
        let mut memory = self.memory();
        let reply = respond(
            model,
            home,
            history,
            &mut memory,
            request,
            &CancellationToken::new(),
        )
        .await;
        *self.memory.lock().unwrap() = memory;
        reply
    }

    fn memory(&self) -> Memory {
        self.memory.lock().unwrap().clone()
    }

    fn state(&self, id: &str) -> String {
        self.cache.read().entity(id).unwrap().state.clone()
    }

    fn set(&self, id: &str, value: &str, attributes: Value) {
        let mut entry = state(id, value, attributes);
        let current = self
            .cache
            .read()
            .entity(id)
            .map(|entity| entity.name.clone());
        if let Some(name) = current {
            entry.attributes.insert("friendly_name".into(), json!(name));
        }
        self.cache.update(|home| home.apply_state(id, Some(entry)));
    }
}

#[tokio::test]
async fn simple_commands_skip_the_model_and_report_verified_state() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    let reply = conversation.ask(&model, "Turn on the porch light.").await;
    assert_eq!(reply.text, "Front Porch is on.");
    assert!(reply.results.is_empty());
    assert_eq!(reply.metrics.route, Route::Direct);
    assert_eq!(model.passes(), 0);
    assert_eq!(conversation.state("light.front_porch"), "on");
}

#[tokio::test]
async fn state_queries_and_time_come_from_home_assistant() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    assert_eq!(
        conversation.ask(&model, "Is the garage open?").await.text,
        "Garage door is closed."
    );
    assert_eq!(
        conversation.ask(&model, "What time is it?").await.text,
        "It's 5:19 PM."
    );
    assert_eq!(model.passes(), 0);
}

#[tokio::test]
async fn follow_up_pronouns_refer_to_the_previous_device() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Turn on the porch light.").await;
    let reply = conversation.ask(&model, "Turn it off").await;
    assert_eq!(reply.text, "Front Porch is off.");
    assert_eq!(conversation.state("light.front_porch"), "off");
}

#[tokio::test]
async fn revert_restores_the_recorded_previous_state() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Turn on the porch light.").await;
    let reply = conversation.ask(&model, "oh wait. revert that").await;
    assert_eq!(reply.text, "Restored the previous state.");
    assert_eq!(conversation.state("light.front_porch"), "off");
    assert_eq!(model.passes(), 0);
}

#[tokio::test]
async fn revert_restores_brightness_rather_than_toggling() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Turn off the hallway light").await;
    conversation.ask(&model, "undo").await;
    let hallway = conversation
        .cache
        .read()
        .entity("light.hallway")
        .unwrap()
        .clone();
    assert_eq!(hallway.state, "on");
    assert_eq!(hallway.attributes["brightness"], json!(128.0));
}

#[tokio::test]
async fn repeated_commands_keep_the_original_state_for_revert() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    assert_eq!(
        conversation
            .ask(&model, "Turn on the porch light")
            .await
            .text,
        "Front Porch is on."
    );
    assert_eq!(
        conversation
            .ask(&model, "Turn on the porch light")
            .await
            .text,
        "Front Porch is on."
    );
    conversation.ask(&model, "Revert that").await;
    assert_eq!(conversation.state("light.front_porch"), "off");
}

#[tokio::test]
async fn revert_without_history_changes_nothing() {
    let conversation = Conversation::new();
    let reply = conversation
        .ask(&ScriptedModel::unused(), "Revert that.")
        .await;
    assert_eq!(reply.text, "There's nothing to undo.");
}

#[tokio::test]
async fn are_you_sure_reads_home_assistant_again() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Is the garage open?").await;
    assert_eq!(
        conversation.ask(&model, "Are you sure?").await.text,
        "Yes, I checked. It's closed."
    );

    let api = conversation.api();
    api.actual.lock().unwrap().push(state(
        "cover.garage_door",
        "open",
        json!({"device_class": "garage", "supported_features": 3, "friendly_name": "garage door"}),
    ));
    let reply = conversation.ask_with(&model, &api, "Are you sure?").await;
    assert_eq!(reply.text, "I checked again. Garage door is open.");
}

#[tokio::test]
async fn remembered_states_are_never_treated_as_current() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    assert_eq!(
        conversation
            .ask(&model, "Is the porch light on?")
            .await
            .text,
        "Front Porch is off."
    );
    conversation.set("light.front_porch", "on", json!({}));
    assert_eq!(
        conversation.ask(&model, "Is it on?").await.text,
        "Front Porch is on."
    );
}

#[tokio::test]
async fn security_sensitive_commands_wait_for_confirmation() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    let api = conversation.api();
    let reply = conversation
        .ask_with(&model, &api, "Unlock the front door")
        .await;
    assert_eq!(reply.text, "Unlock front door?");
    assert_eq!(reply.confirmation.len(), 1);
    assert!(api.calls.lock().unwrap().is_empty());

    let home = Home {
        cache: &conversation.cache,
        api: &api,
        connected: true,
    };
    let confirmed = confirm(home, &mut conversation.memory(), &reply.confirmation).await;
    assert_eq!(confirmed.text, "Front door is unlocked.");
}

#[tokio::test]
async fn reverting_an_unlock_never_skips_confirmation() {
    let conversation = Conversation::new();
    conversation.set("lock.front_door", "unlocked", json!({}));
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Lock the front door").await;
    let reply = conversation.ask(&model, "Revert that").await;
    assert_eq!(reply.text, "Unlock front door?");
    assert_eq!(conversation.state("lock.front_door"), "locked");
}

#[tokio::test]
async fn brightness_and_temperature_follow_ups_skip_the_model() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Turn on the porch light").await;
    assert_eq!(
        conversation.ask(&model, "brightness 50%").await.text,
        "Front Porch is at 50%."
    );
    assert_eq!(
        conversation
            .ask(&model, "Can you set the brightness to 10%")
            .await
            .text,
        "Front Porch is at 10%."
    );
    conversation.ask(&model, "Is the thermostat on?").await;
    assert_eq!(
        conversation.ask(&model, "set it to 22").await.text,
        "Thermostat is set to 22°."
    );
    assert_eq!(model.passes(), 0);
}

#[tokio::test]
async fn failed_commands_are_reported_and_not_remembered_as_changes() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    let mut api = conversation.api();
    api.fail = true;
    let reply = conversation
        .ask_with(&model, &api, "Turn on the porch light")
        .await;
    assert_eq!(reply.text, "Couldn't turn on Front Porch.");
    assert!(conversation.memory().last_action.is_empty());
}

#[tokio::test]
async fn several_matches_ask_which_one_and_a_single_match_acts() {
    let conversation = Conversation::new();
    conversation.set(
        "light.back_porch",
        "off",
        json!({"friendly_name": "Back Porch"}),
    );
    let model = ScriptedModel::unused();
    let reply = conversation.ask(&model, "Turn on the porch light").await;
    assert_eq!(reply.text, "Which one: Back Porch or Front Porch?");
    assert_eq!(conversation.state("light.front_porch"), "off");
    let reply = conversation.ask(&model, "Is the AC on?").await;
    assert_eq!(reply.text, "Thermostat is set to heat.");
}

#[tokio::test]
async fn model_actions_take_one_pass_and_respect_exclusions() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![tool_call(
        "control",
        json!({"action": "turn_off", "target": {"floors": ["downstairs"], "domains": ["light"], "exclude_entities": ["light.hallway"]}}),
    )]);
    let reply = conversation
        .ask(&model, "Turn off the lights downstairs except the hallway")
        .await;
    assert_eq!(reply.text, "Kitchen and living room lamp are off.");
    assert_eq!(reply.metrics.passes, 1);
    assert_eq!(conversation.state("light.hallway"), "on");
    assert_eq!(conversation.state("light.kitchen"), "off");
}

#[tokio::test]
async fn reversing_calls_are_rejected_before_anything_runs() {
    let conversation = Conversation::new();
    let mut both = tool_call(
        "control",
        json!({"action": "turn_off", "target": {"floors": ["downstairs"], "domains": ["light"]}}),
    );
    both.tool_calls.push(ToolCall {
        id: "call-2".into(),
        function: FunctionCall {
            name: "control".into(),
            arguments: json!({"action": "turn_on", "target": {"entities": ["light.hallway"]}}),
        },
    });
    let model = ScriptedModel::new(vec![
        both,
        tool_call(
            "control",
            json!({"action": "turn_off", "target": {"floors": ["downstairs"], "domains": ["light"], "exclude_entities": ["light.hallway"]}}),
        ),
    ]);
    let api = conversation.api();
    let reply = conversation
        .ask_with(
            &model,
            &api,
            "Turn off the lights downstairs except the hallway",
        )
        .await;
    let touched: Vec<String> = api
        .calls
        .lock()
        .unwrap()
        .iter()
        .flat_map(|call| call.entity_ids.clone())
        .collect();
    assert!(!touched.contains(&"light.hallway".to_string()));
    assert_eq!(reply.text, "Kitchen and living room lamp are off.");
    assert_eq!(reply.metrics.passes, 2);
}

#[tokio::test]
async fn turning_on_and_setting_a_value_together_is_not_a_conflict() {
    let conversation = Conversation::new();
    conversation.set(
        "light.bedroom",
        "off",
        json!({"supported_color_modes": ["brightness"]}),
    );
    let mut both = tool_call(
        "control",
        json!({"action": "turn_on", "target": {"entities": ["light.bedroom"]}}),
    );
    both.tool_calls.push(ToolCall {
        id: "call-2".into(),
        function: FunctionCall {
            name: "control".into(),
            arguments: json!({"action": "set_brightness", "target": {"entities": ["light.bedroom"]}, "value": 40}),
        },
    });
    let model = ScriptedModel::new(vec![both]);
    let reply = conversation
        .ask(&model, "Turn on the bedroom light at 40%")
        .await;
    assert_eq!(reply.text, "Bedroom is on. Bedroom is at 40%.");
    assert_eq!(reply.metrics.passes, 1);
}

#[tokio::test]
async fn a_device_that_cannot_do_the_action_is_reported_with_any_substitute() {
    let conversation = Conversation::new();
    let mut both = tool_call(
        "control",
        json!({"action": "turn_on", "target": {"entities": ["climate.thermostat"]}}),
    );
    both.tool_calls.push(ToolCall {
        id: "call-2".into(),
        function: FunctionCall {
            name: "control".into(),
            arguments: json!({"action": "set_temperature", "target": {"entities": ["climate.thermostat"]}, "value": 26}),
        },
    });
    let model = ScriptedModel::new(vec![both, text("")]);
    let reply = conversation
        .ask(&model, "Turn the thermostat on and set it to 26")
        .await;
    assert_eq!(
        reply.text,
        "Thermostat can't turn on. Thermostat is set to 26°."
    );
}

#[tokio::test]
async fn the_out_of_scope_reply_is_never_used_after_a_tool_call() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![
        tool_call(
            "control",
            json!({"action": "set_brightness", "target": {"entities": ["light.made_up"]}, "value": 50}),
        ),
        text("I can only help with your home."),
    ]);
    let reply = conversation.ask(&model, "dim the thing over there").await;
    assert_eq!(
        reply.text,
        "I couldn't do that. Try naming the device and what to change."
    );
}

#[tokio::test]
async fn reading_states_takes_a_second_pass() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![
        tool_call("get_states", json!({"target": {"areas": ["bedroom"]}})),
        text("It's 18.2°C in the bedroom."),
    ]);
    let reply = conversation
        .ask(&model, "Is it colder upstairs than in the hallway?")
        .await;
    assert_eq!(reply.text, "It's 18.2°C in the bedroom.");
    assert_eq!(reply.metrics.passes, 2);
    assert!(model.last_message().content.contains("18.2°C"));
}

#[tokio::test]
async fn model_requests_include_the_conversation_context() {
    let conversation = Conversation::new();
    conversation
        .ask(&ScriptedModel::unused(), "Turn on the porch light")
        .await;
    let model = ScriptedModel::new(vec![tool_call(
        "control",
        json!({"action": "set_brightness", "target": {"entities": ["light.front_porch"]}, "value": 80}),
    )]);
    let reply = conversation.ask(&model, "Make it brighter").await;
    assert_eq!(reply.text, "Front Porch is at 80%.");
    let sent = model.received.lock().unwrap()[0].clone();
    let user = &sent.last().unwrap().content;
    assert!(user.contains("Recently referenced"));
    assert!(user.contains("Front Porch [light.front_porch]: on"));
    assert!(
        user.contains("States before the last action:\n- Front Porch [light.front_porch]: off")
    );
    assert!(user.ends_with("Request: Make it brighter"));
}

#[tokio::test]
async fn the_shared_prompt_prefix_does_not_change_between_requests() {
    let conversation = Conversation::new();
    let first = ScriptedModel::new(vec![text("I can only help with your home.")]);
    let second = ScriptedModel::new(vec![text("Which room?")]);
    conversation.ask(&first, "Who wrote Hamlet?").await;
    conversation.ask(&second, "It's too bright").await;
    let system = |model: &ScriptedModel| model.received.lock().unwrap()[0][0].clone();
    assert_eq!(system(&first), system(&second));
    assert!(
        system(&first)
            .content
            .contains("I can only help with your home.")
    );
    assert!(!system(&first).content.contains("Possibly relevant"));
}

#[tokio::test]
async fn out_of_scope_requests_get_the_model_reply_without_actions() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![text("I can only help with your home.")]);
    let api = conversation.api();
    let reply = conversation
        .ask_with(&model, &api, "Who wrote Hamlet?")
        .await;
    assert_eq!(reply.text, "I can only help with your home.");
    assert!(api.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_tool_calls_are_returned_to_the_model() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![
        tool_call(
            "control",
            json!({"action": "turn_off", "target": {"entities": ["light.made_up"]}}),
        ),
        text("I couldn't find that light. Which one did you mean?"),
    ]);
    let api = conversation.api();
    let reply = conversation
        .ask_with(&model, &api, "Turn off the made up thing")
        .await;
    assert!(api.calls.lock().unwrap().is_empty());
    assert_eq!(
        reply.text,
        "I couldn't find that light. Which one did you mean?"
    );
    assert!(
        model
            .last_message()
            .content
            .contains("unknown entity id: light.made_up")
    );
}

#[tokio::test]
async fn sensitive_model_actions_wait_for_confirmation() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![tool_call(
        "control",
        json!({"action": "open", "target": {"entities": ["cover.garage_door"]}}),
    )]);
    let api = conversation.api();
    let reply = conversation
        .ask_with(&model, &api, "I'm leaving, let the car out")
        .await;
    assert_eq!(reply.text, "Open garage door?");
    assert_eq!(reply.confirmation.len(), 1);
    assert!(api.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn disconnected_home_assistant_is_reported() {
    let cache = HomeCache::new(test_home());
    let api = FakeHomeAssistant {
        cache: &cache,
        calls: Mutex::default(),
        fail: false,
        actual: Mutex::default(),
    };
    let home = Home {
        cache: &cache,
        api: &api,
        connected: false,
    };
    let reply = respond(
        &ScriptedModel::unused(),
        home,
        &[],
        &mut Memory::default(),
        "What time is it?",
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(reply.text, direct::NOT_CONNECTED);
}

#[tokio::test]
async fn inference_errors_are_returned() {
    let cache = HomeCache::new(test_home());
    let api = FakeHomeAssistant {
        cache: &cache,
        calls: Mutex::default(),
        fail: false,
        actual: Mutex::default(),
    };
    let home = Home {
        cache: &cache,
        api: &api,
        connected: true,
    };
    let result = respond(
        &ScriptedModel::unused(),
        home,
        &[],
        &mut Memory::default(),
        "hi",
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(result, Err(AssistantError::Inference(_))));
}

#[tokio::test]
async fn cancelled_requests_stop_before_the_model_answers() {
    let cache = HomeCache::new(test_home());
    let api = FakeHomeAssistant {
        cache: &cache,
        calls: Mutex::default(),
        fail: false,
        actual: Mutex::default(),
    };
    let home = Home {
        cache: &cache,
        api: &api,
        connected: true,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let model = ScriptedModel::new(vec![text("hello")]);
    let result = respond(&model, home, &[], &mut Memory::default(), "hi", &cancel).await;
    assert_eq!(result, Err(AssistantError::Cancelled));
}

#[test]
fn history_never_includes_result_lines() {
    let interaction = Interaction {
        id: 1,
        created_at: now_millis(),
        request: "turn on porch light".into(),
        response: "Front Porch is on.".into(),
        results: vec!["Turned on Front Porch.".into()],
        awaiting_confirmation: false,
    };
    let messages = history_messages(&[interaction]);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].content, "Front Porch is on.");
}

#[test]
fn final_text_removes_reasoning_and_em_dashes() {
    assert_eq!(
        final_text("<think>hmm</think> It's 20° \u{2014} warm.", &[]),
        "It's 20°, warm."
    );
    assert_eq!(
        final_text("", &["Kitchen is off.".into()]),
        "Kitchen is off."
    );
}

/// Prints each tool call the real model proposes.
struct PrintingChat<'a>(EngineChat<'a>);

impl Chat for PrintingChat<'_> {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> Result<Completion, InferenceError> {
        let completion = self.0.chat(messages, tools).await?;
        for call in &completion.message.tool_calls {
            println!("    {} {}", call.function.name, call.function.arguments);
        }
        Ok(completion)
    }
}

fn real_engine() -> (Engine, ModelSpec) {
    let path = std::path::PathBuf::from(std::env::var("LUNA_TEST_MODEL").unwrap());
    let runtime = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "binaries/llama-server-{}{}",
        env!("LUNA_TARGET_TRIPLE"),
        std::env::consts::EXE_SUFFIX
    ));
    let engine = Engine::new(
        runtime,
        std::env::temp_dir().join("luna-real-model.pid"),
        std::env::temp_dir().join("luna-real-model-cache"),
    );
    let qwen = crate::models::catalog::find("qwen3-8b").unwrap();
    let mut chat = qwen.chat.clone();
    if let Ok(temperature) = std::env::var("LUNA_TEST_TEMPERATURE") {
        chat.temperature = temperature.parse().unwrap();
    }
    let spec = ModelSpec {
        id: qwen.id.clone(),
        path,
        context_length: 4096,
        chat,
    };
    (engine, spec)
}

/// Pauses between real-model requests so a fanless laptop is not measured while throttled.
async fn cool_down() {
    let seconds = std::env::var("LUNA_TEST_COOLDOWN").map_or(0, |value| value.parse().unwrap());
    tokio::time::sleep(Duration::from_secs(seconds)).await;
}

/// Repeats complex requests on fresh conversations: `cargo test accuracy -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn accuracy_with_real_model() {
    let (engine, spec) = real_engine();
    let model = PrintingChat(EngineChat {
        engine: &engine,
        spec: &spec,
    });
    let runs: usize = std::env::var("LUNA_TEST_RUNS").map_or(3, |runs| runs.parse().unwrap());
    for request in [
        "It's too bright in the living room.",
        "Turn off everything downstairs except the hallway light.",
        "I'm heading to bed.",
        "Make the house comfortable.",
        "Who wrote Hamlet?",
        "What's the temperature in the bedroom?",
        "Which lights are on?",
    ] {
        if std::env::var("LUNA_TEST_ONLY")
            .is_ok_and(|only| !only.split(',').any(|part| request.contains(part)))
        {
            continue;
        }
        for _ in 0..runs {
            cool_down().await;
            let conversation = Conversation::new();
            let api = conversation.api();
            println!("{request}");
            match conversation.ask_after(&model, &api, &[], request).await {
                Ok(reply) => println!(
                    "  -> {} {:?} ({} ms, {} passes, prompt {} new + {} cached tokens in {:.0} ms, generated {} tokens in {:.0} ms)",
                    reply.text,
                    reply.results,
                    reply.metrics.total.as_millis(),
                    reply.metrics.passes,
                    reply.metrics.inference.prompt_tokens,
                    reply.metrics.inference.cached_tokens,
                    reply.metrics.inference.prompt_ms,
                    reply.metrics.inference.generated_tokens,
                    reply.metrics.inference.generation_ms
                ),
                Err(error) => println!("  -> ERROR {error}"),
            }
        }
    }
    engine.unload().await;
}

/// Real model benchmark: `LUNA_TEST_MODEL=/path/Qwen3-8B-Q4_K_M.gguf cargo test benchmark -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn benchmark_with_real_model() {
    let path = std::path::PathBuf::from(std::env::var("LUNA_TEST_MODEL").unwrap());
    let runtime = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "binaries/llama-server-{}{}",
        env!("LUNA_TARGET_TRIPLE"),
        std::env::consts::EXE_SUFFIX
    ));
    let engine = Engine::new(
        runtime,
        std::env::temp_dir().join("luna-benchmark.pid"),
        std::env::temp_dir().join("luna-benchmark-cache"),
    );
    let qwen = crate::models::catalog::find("qwen3-8b").unwrap();
    let spec = ModelSpec {
        id: qwen.id.clone(),
        path,
        context_length: 4096,
        chat: qwen.chat.clone(),
    };
    let conversation = Conversation::new();
    let api = conversation.api();

    let started = Instant::now();
    let home = Home {
        cache: &conversation.cache,
        api: &api,
        connected: true,
    };
    let warm = warm_up(&engine, &spec, home).await.unwrap();
    println!(
        "load and warm-up: {} ms ({:?})",
        started.elapsed().as_millis(),
        warm
    );

    let model = PrintingChat(EngineChat {
        engine: &engine,
        spec: &spec,
    });
    let mut history: Vec<Interaction> = Vec::new();
    for request in [
        "Turn on the porch light.",
        "Revert that.",
        "Are you sure?",
        "Turn off the kitchen lights.",
        "Is the garage open?",
        "What time is it?",
        "Turn on the porch light.",
        "Make it brighter.",
        "It's too bright in the living room.",
        "Turn off everything downstairs except the hallway light.",
        "I'm heading to bed.",
        "Who wrote Hamlet?",
    ] {
        cool_down().await;
        let result = conversation
            .ask_after(&model, &api, &history, request)
            .await;
        match result {
            Ok(reply) => {
                println!(
                    "{:>6} ms {:?} | {request} -> {} {:?}{}\n         {}",
                    reply.metrics.total.as_millis(),
                    reply.metrics.route,
                    reply.text,
                    reply.results,
                    if reply.confirmation.is_empty() {
                        ""
                    } else {
                        " [confirm]"
                    },
                    reply.metrics
                );
                history.push(Interaction {
                    id: history.len() as i64,
                    created_at: now_millis(),
                    request: request.into(),
                    response: reply.text,
                    results: reply.results,
                    awaiting_confirmation: !reply.confirmation.is_empty(),
                });
            }
            Err(error) => println!("ERROR | {request} -> {error}"),
        }
    }
    let called: Vec<String> = api
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|call| {
            format!(
                "{}.{} {:?} {:?}",
                call.domain, call.service, call.entity_ids, call.data
            )
        })
        .collect();
    println!("service calls:\n  {}", called.join("\n  "));
    engine.unload().await;
}

/// Replays a user pushing back after a turn that left the AC off:
/// `LUNA_TEST_MODEL=... cargo test pushback -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn pushback_with_real_model() {
    let (engine, spec) = real_engine();
    let model = PrintingChat(EngineChat {
        engine: &engine,
        spec: &spec,
    });
    // 385 also advertises turn_on and turn_off; 1 is a thermostat that only takes a temperature.
    for features in [385, 1] {
        for _ in 0..3 {
            cool_down().await;
            let conversation = Conversation::new();
            conversation.set(
                "climate.thermostat",
                "off",
                json!({"supported_features": features, "temperature": 26, "current_temperature": 29, "min_temp": 16, "max_temp": 30, "hvac_modes": ["off", "cool", "auto"]}),
            );
            let api = conversation.api();
            conversation
                .ask_with(&ScriptedModel::unused(), &api, "Is the thermostat on?")
                .await;
            let history = [Interaction {
                id: 1,
                created_at: now_millis(),
                request: "Turn it on and set it to 26".into(),
                response: "Thermostat is set to 26°.".into(),
                results: vec![],
                awaiting_confirmation: false,
            }];
            println!("features {features}: Right. I said turn it on?");
            match conversation
                .ask_after(&model, &api, &history, "Right. I said turn it on?")
                .await
            {
                Ok(reply) => println!(
                    "  -> {} (thermostat is now {})",
                    reply.text,
                    conversation.state("climate.thermostat")
                ),
                Err(error) => println!("  -> ERROR {error}"),
            }
        }
    }
    engine.unload().await;
}

/// Measures a fresh launch with and without a saved prompt:
/// `LUNA_TEST_MODEL=... cargo test prompt_cache -- --ignored --nocapture`.
#[tokio::test]
#[ignore]
async fn prompt_cache_with_real_model() {
    let (engine, spec) = real_engine();
    let cache = std::env::temp_dir().join("luna-real-model-cache");
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let conversation = Conversation::new();
    let api = conversation.api();
    let model = EngineChat {
        engine: &engine,
        spec: &spec,
    };
    for launch in ["without a saved prompt", "with the saved prompt"] {
        cool_down().await;
        let started = Instant::now();
        let home = Home {
            cache: &conversation.cache,
            api: &api,
            connected: true,
        };
        let warmup = warm_up(&engine, &spec, home).await.unwrap();
        let ready = started.elapsed();
        let reply = conversation
            .ask_after(&model, &api, &[], "It's too bright in the living room.")
            .await
            .unwrap();
        println!(
            "{launch}: ready in {} ms ({warmup:?}); first request {} ms, prompt {} new + {} cached tokens",
            ready.as_millis(),
            reply.metrics.total.as_millis(),
            reply.metrics.inference.prompt_tokens,
            reply.metrics.inference.cached_tokens
        );
        engine.unload().await;
    }
    cool_down().await;
    let reply = conversation
        .ask_after(&model, &api, &[], "It's too bright in the living room.")
        .await
        .unwrap();
    println!(
        "request before any warm-up, with the saved prompt: {} ms including load, prompt {} new + {} cached tokens",
        reply.metrics.total.as_millis(),
        reply.metrics.inference.prompt_tokens,
        reply.metrics.inference.cached_tokens
    );
    engine.unload().await;
    let saved: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().collect();
    assert_eq!(saved.len(), 1);
}

#[tokio::test]
async fn follow_up_questions_reuse_the_previous_question_without_the_model() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Is the porch light on?").await;
    assert_eq!(
        conversation.ask(&model, "What about the AC?").await.text,
        "Thermostat is set to heat."
    );
}

#[tokio::test]
async fn follow_ups_after_other_turns_go_to_the_model() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![text("The AC is set to heat.")]);
    conversation
        .ask(&ScriptedModel::unused(), "What time is it?")
        .await;
    let reply = conversation.ask(&model, "What about the AC?").await;
    assert_eq!(reply.metrics.route, Route::Model);
}

#[tokio::test]
async fn answers_to_which_one_finish_the_original_request_without_the_model() {
    let conversation = Conversation::new();
    conversation.set(
        "light.back_porch",
        "off",
        json!({"friendly_name": "Back Porch"}),
    );
    let model = ScriptedModel::unused();
    conversation.ask(&model, "Is the porch light on?").await;
    assert_eq!(
        conversation.ask(&model, "the first one").await.text,
        "Back Porch is off."
    );
    assert_eq!(conversation.state("light.back_porch"), "off");

    conversation.ask(&model, "Turn on the porch light").await;
    conversation.ask(&model, "front").await;
    assert_eq!(conversation.state("light.front_porch"), "on");
    assert_eq!(conversation.state("light.back_porch"), "off");
}

#[tokio::test]
async fn a_device_word_prefers_devices_of_that_kind() {
    let conversation = Conversation::new();
    conversation.set(
        "switch.thermostat_display_ac",
        "on",
        json!({"friendly_name": "AC Display light"}),
    );
    let reply = conversation
        .ask(&ScriptedModel::unused(), "Is the AC on?")
        .await;
    assert_eq!(reply.text, "Thermostat is set to heat.");
}

#[tokio::test]
async fn exclusions_the_user_never_asked_for_are_rejected() {
    let conversation = Conversation::new();
    let model = ScriptedModel::new(vec![
        tool_call(
            "control",
            json!({"action": "turn_on", "target": {"domains": ["light"], "exclude_entities": ["light.hallway"]}}),
        ),
        text("Which lights?"),
    ]);
    let api = conversation.api();
    let reply = conversation
        .ask_with(&model, &api, "Brighten up the place")
        .await;
    assert_eq!(reply.text, "Which lights?");
    assert!(api.calls.lock().unwrap().is_empty());
}

/// Everyday phrasings that must never wait on the model. Each case is one conversation.
#[tokio::test]
async fn everyday_requests_are_answered_without_the_model() {
    let single: &[&str] = &[
        "turn on the porch light",
        "turn the porch light on",
        "porch light on",
        "porch light off",
        "switch off the kitchen light",
        "can you turn off the kitchen light",
        "could you please turn off the kitchen light",
        "please turn off the kitchen light",
        "turn off kitchen light please",
        "kitchen light off please",
        "turn off the light in the kitchen",
        "turn the kitchen lights off",
        "shut the garage",
        "close the garage door",
        "open the blinds",
        "is the garage open",
        "is the garage door closed?",
        "is the front door locked",
        "is the porch light on?",
        "are the kitchen lights on",
        "is the tv on",
        "check the garage",
        "garage status",
        "what's the status of the front door",
        "what is the temperature in the bedroom",
        "how warm is the bedroom",
        "bedroom temperature",
        "what's the temperature on the thermostat",
        "what's the thermostat set to",
        "set the thermostat to 21",
        "set the hallway light to 50%",
        "dim the hallway light to 30%",
        "make the hallway light 40%",
        "set hallway brightness to 60",
        "what time is it",
        "lock the front door",
        "unlock the front door",
        "thanks",
        "thank you luna",
    ];
    let conversations: &[&[&str]] = &[
        &["is the porch light on", "turn it on"],
        &["turn on the porch light", "turn it off"],
        &["turn on the porch light", "revert that"],
        &["turn on the porch light", "undo"],
        &["turn on the porch light", "sorry. turn it back off"],
        &["turn on the porch light", "turn it back off please"],
        &["turn on the porch light", "turn it on again"],
        &["is the porch light on", "what about the kitchen light"],
        &["is the porch light on", "and the kitchen?"],
        &["is the porch light on", "how about the hallway"],
        &["is the porch light on", "are you sure"],
        &["is the porch light on", "it's on I think"],
        &["is the porch light on", "no it is off"],
        &["set the thermostat to 21", "set it back to 20"],
        &["set the thermostat to 21", "make it 22"],
        &["set the thermostat to 21", "actually 22"],
        &["turn off the kitchen light", "and the hallway"],
        &["turn off the kitchen light", "the hallway too"],
    ];
    let with_two_porches: &[&[&str]] = &[
        &["turn on the porch light", "the front one"],
        &["turn on the porch light", "front porch"],
        &["turn on the porch light", "first one"],
        &["turn on the porch light", "both"],
        &["is the porch light on", "the second one"],
    ];

    let mut cases: Vec<(bool, Vec<&str>)> = single.iter().map(|r| (false, vec![*r])).collect();
    cases.extend(conversations.iter().map(|turns| (false, turns.to_vec())));
    cases.extend(with_two_porches.iter().map(|turns| (true, turns.to_vec())));

    let mut failures = Vec::new();
    for (two_porches, turns) in cases {
        let conversation = Conversation::new();
        conversation.set(
            "light.front_porch",
            "off",
            json!({"friendly_name": "Front Porch"}),
        );
        if two_porches {
            conversation.set(
                "light.back_porch",
                "off",
                json!({"friendly_name": "Back Porch"}),
            );
        }
        for turn in &turns {
            let model = ScriptedModel::new(vec![text("model")]);
            let reply = conversation.ask(&model, turn).await;
            if reply.metrics.route != Route::Direct {
                failures.push(format!("{turns:?} at {turn:?}"));
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sent to the model:\n{}",
        failures.join("\n")
    );
}

#[tokio::test]
async fn temperature_questions_read_the_temperature() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    assert_eq!(
        conversation
            .ask(&model, "How warm is the bedroom?")
            .await
            .text,
        "Bedroom temperature is 18.2°C."
    );
    assert_eq!(
        conversation
            .ask(&model, "what's the temperature on the thermostat")
            .await
            .text,
        "Thermostat is 19.5°, set to 20°."
    );
}

#[tokio::test]
async fn follow_ups_repeat_a_command_and_both_picks_every_choice() {
    let conversation = Conversation::new();
    let model = ScriptedModel::unused();
    conversation.ask(&model, "turn off the kitchen light").await;
    conversation.ask(&model, "the hallway too").await;
    assert_eq!(conversation.state("light.hallway"), "off");

    conversation.set(
        "light.front_porch",
        "off",
        json!({"friendly_name": "Front Porch"}),
    );
    conversation.set(
        "light.back_porch",
        "off",
        json!({"friendly_name": "Back Porch"}),
    );
    conversation.ask(&model, "turn on the porch light").await;
    conversation.ask(&model, "both").await;
    assert_eq!(conversation.state("light.front_porch"), "on");
    assert_eq!(conversation.state("light.back_porch"), "on");
}

#[tokio::test]
async fn blinds_never_match_the_garage_door() {
    let conversation = Conversation::new();
    conversation
        .ask(&ScriptedModel::unused(), "open the blinds")
        .await;
    assert_eq!(conversation.state("cover.garage_door"), "closed");
}
