use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use super::Action;
use super::validate::Plan;
use crate::home_assistant::model::Entity;
use crate::home_assistant::{HomeCache, ServiceCaller};

const VERIFY_TIMEOUT: Duration = Duration::from_secs(8);
const BRIGHTNESS_TOLERANCE: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// The device reports it is moving towards the requested state, such as "opening".
    InProgress,
    NotConfirmed,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntityOutcome {
    pub name: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    pub action: Action,
    pub value: Option<f64>,
    pub outcomes: Vec<EntityOutcome>,
    pub left_out: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Check {
    Satisfied,
    InProgress,
    Pending,
}

/// Runs the plan's service calls, then waits for state changes that confirm each one.
pub async fn execute(
    plan: &Plan,
    caller: &impl ServiceCaller,
    cache: &HomeCache,
) -> ExecutionReport {
    let before: HashMap<String, Entity> = {
        let home = cache.read();
        plan.entities
            .iter()
            .filter_map(|planned| Some((planned.id.clone(), home.entity(&planned.id)?.clone())))
            .collect()
    };
    let mut changes = cache.subscribe();
    let mut failed = HashSet::new();
    for call in &plan.calls {
        if let Err(error) = caller.call_service(call).await {
            log::error!("{}.{} failed: {error}", call.domain, call.service);
            failed.extend(call.entity_ids.iter().cloned());
        }
    }

    let checks = |cache: &HomeCache| -> HashMap<String, Check> {
        let home = cache.read();
        plan.entities
            .iter()
            .filter(|planned| !failed.contains(&planned.id))
            .map(|planned| {
                let check = match (home.entity(&planned.id), before.get(&planned.id)) {
                    (Some(now), Some(before)) => check(plan.action, plan.value, before, now),
                    _ => Check::Pending,
                };
                (planned.id.clone(), check)
            })
            .collect()
    };

    let deadline = Instant::now() + VERIFY_TIMEOUT;
    let mut results = checks(cache);
    while results.values().any(|check| *check != Check::Satisfied) {
        match tokio::time::timeout_at(deadline, changes.changed()).await {
            Ok(Ok(())) => results = checks(cache),
            _ => break,
        }
    }

    let outcomes = plan
        .entities
        .iter()
        .map(|planned| EntityOutcome {
            name: planned.name.clone(),
            outcome: match results.get(&planned.id) {
                None => Outcome::Failed,
                Some(Check::Satisfied) => Outcome::Done,
                Some(Check::InProgress) => Outcome::InProgress,
                Some(Check::Pending) => Outcome::NotConfirmed,
            },
        })
        .collect();
    ExecutionReport {
        action: plan.action,
        value: plan.value,
        outcomes,
        left_out: plan.left_out.clone(),
    }
}

fn check(action: Action, value: Option<f64>, before: &Entity, now: &Entity) -> Check {
    let state = now.state.as_str();
    let satisfied = match action {
        Action::TurnOn => !matches!(state, "off" | "standby" | "unavailable" | "unknown"),
        Action::TurnOff => matches!(state, "off" | "standby"),
        Action::SetBrightness => brightness_matches(now, value.unwrap_or_default()),
        Action::SetTemperature => number(now, "temperature")
            .zip(value)
            .is_some_and(|(actual, wanted)| (actual - wanted).abs() < 0.05),
        Action::Open => state == "open",
        Action::Close => state == "closed",
        Action::Lock => state == "locked",
        Action::Unlock => matches!(state, "unlocked" | "open"),
        Action::Activate => match now.domain() {
            "script" => {
                now.attributes.get("last_triggered") != before.attributes.get("last_triggered")
            }
            _ => now.state != before.state,
        },
    };
    let in_progress = match action {
        Action::Open => state == "opening",
        Action::Close => state == "closing",
        Action::Lock => state == "locking",
        Action::Unlock => matches!(state, "unlocking" | "opening"),
        _ => false,
    };
    if satisfied {
        Check::Satisfied
    } else if in_progress {
        Check::InProgress
    } else {
        Check::Pending
    }
}

fn brightness_matches(entity: &Entity, percent: f64) -> bool {
    if percent == 0.0 {
        return entity.state == "off";
    }
    let expected = percent * 255.0 / 100.0;
    entity.state == "on"
        && number(entity, "brightness")
            .is_some_and(|actual| (actual - expected).abs() <= BRIGHTNESS_TOLERANCE)
}

fn number(entity: &Entity, attribute: &str) -> Option<f64> {
    entity.attributes.get(attribute).and_then(Value::as_f64)
}

impl ExecutionReport {
    /// Short user-facing result lines. Success is only claimed for confirmed changes.
    pub fn summary(&self) -> Vec<String> {
        let names = |wanted: Outcome| -> Vec<&str> {
            self.outcomes
                .iter()
                .filter(|outcome| outcome.outcome == wanted)
                .map(|outcome| outcome.name.as_str())
                .collect()
        };
        let mut lines = Vec::new();
        let done = names(Outcome::Done);
        if !done.is_empty() {
            lines.push(self.done_line(&done));
        }
        let moving = names(Outcome::InProgress);
        if !moving.is_empty() {
            let verb = if moving.len() == 1 { "is" } else { "are" };
            let progress = progress_word(self.action);
            lines.push(format!("{} {verb} {progress}.", capitalize(&list(&moving))));
        }
        let unconfirmed = names(Outcome::NotConfirmed);
        if !unconfirmed.is_empty() {
            lines.push(format!("Couldn't confirm {} changed.", list(&unconfirmed)));
        }
        let failed = names(Outcome::Failed);
        if !failed.is_empty() {
            lines.push(format!("Couldn't {} {}.", verb(self.action), list(&failed)));
        }
        if !self.left_out.is_empty() {
            let left_out: Vec<&str> = self.left_out.iter().map(String::as_str).collect();
            lines.push(format!("Left out {}.", list(&left_out)));
        }
        lines
    }

    /// Result text for the model so it can describe what actually happened.
    pub fn for_model(&self) -> String {
        let mut text = String::from("Execution results (only report these):\n");
        for outcome in &self.outcomes {
            let status = match outcome.outcome {
                Outcome::Done => "done and verified",
                Outcome::InProgress => "in progress",
                Outcome::NotConfirmed => "sent but not confirmed",
                Outcome::Failed => "failed",
            };
            text.push_str(&format!("- {}: {status}\n", outcome.name));
        }
        if !self.left_out.is_empty() {
            text.push_str(&format!(
                "Left out because they are protected (must be named directly) or unavailable: {}\n",
                self.left_out.join(", ")
            ));
        }
        text
    }

    fn done_line(&self, names: &[&str]) -> String {
        let target = list(names);
        match (self.action, self.value) {
            (Action::SetBrightness, Some(value)) => format!("Set {target} to {}%.", value.round()),
            (Action::SetTemperature, Some(value)) => format!("Set {target} to {value}°."),
            (action, _) => format!("{} {target}.", past_tense(action)),
        }
    }
}

pub fn verb(action: Action) -> &'static str {
    match action {
        Action::TurnOn => "turn on",
        Action::TurnOff => "turn off",
        Action::SetBrightness => "change the brightness of",
        Action::SetTemperature => "change the temperature of",
        Action::Open => "open",
        Action::Close => "close",
        Action::Lock => "lock",
        Action::Unlock => "unlock",
        Action::Activate => "start",
    }
}

fn past_tense(action: Action) -> &'static str {
    match action {
        Action::TurnOn => "Turned on",
        Action::TurnOff => "Turned off",
        Action::SetBrightness => "Changed the brightness of",
        Action::SetTemperature => "Changed the temperature of",
        Action::Open => "Opened",
        Action::Close => "Closed",
        Action::Lock => "Locked",
        Action::Unlock => "Unlocked",
        Action::Activate => "Started",
    }
}

fn progress_word(action: Action) -> &'static str {
    match action {
        Action::Open | Action::Unlock => "opening",
        Action::Close => "closing",
        Action::Lock => "locking",
        _ => "changing",
    }
}

/// Joins names naturally, collapsing long lists into a count.
pub fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third] => format!("{first}, {second}, and {third}"),
        _ => format!("{} devices", names.len()),
    }
}

pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::actions::validate::plan;
    use crate::actions::{ControlRequest, Target};
    use crate::home_assistant::model::fixtures::{home, state};
    use crate::home_assistant::{HaError, ServiceCall};

    /// Applies `result_state` to each targeted entity, or fails the listed services.
    struct FakeHomeAssistant<'a> {
        cache: &'a HomeCache,
        result_state: Option<(&'static str, Value)>,
        failing_services: Vec<&'static str>,
        calls: Mutex<Vec<ServiceCall>>,
    }

    impl ServiceCaller for FakeHomeAssistant<'_> {
        async fn call_service(&self, call: &ServiceCall) -> Result<(), HaError> {
            self.calls.lock().unwrap().push(call.clone());
            if self.failing_services.contains(&call.domain) {
                return Err(HaError::Request {
                    code: "home_assistant_error".into(),
                    message: "failed".into(),
                });
            }
            if let Some((new_state, attributes)) = &self.result_state {
                for id in &call.entity_ids {
                    self.cache.update(|home| {
                        home.apply_state(id, Some(state(id, new_state, attributes.clone())));
                    });
                }
            }
            Ok(())
        }
    }

    fn fake<'a>(
        cache: &'a HomeCache,
        result_state: Option<(&'static str, Value)>,
    ) -> FakeHomeAssistant<'a> {
        FakeHomeAssistant {
            cache,
            result_state,
            failing_services: vec![],
            calls: Mutex::default(),
        }
    }

    fn turn_off_downstairs_except_hallway(cache: &HomeCache) -> Plan {
        let request = ControlRequest {
            action: Action::TurnOff,
            target: Target {
                floors: vec!["downstairs".into()],
                exclude_entities: vec!["light.hallway".into()],
                ..Target::default()
            },
            value: None,
        };
        plan(&cache.read(), &request).unwrap()
    }

    fn single(action: Action, entity: &str, value: Option<f64>) -> ControlRequest {
        ControlRequest {
            action,
            target: Target {
                entities: vec![entity.into()],
                ..Target::default()
            },
            value,
        }
    }

    #[tokio::test]
    async fn verified_changes_are_reported_done() {
        let cache = HomeCache::new(home());
        let plan = turn_off_downstairs_except_hallway(&cache);
        let fake = fake(&cache, Some(("off", json!({}))));
        let report = execute(&plan, &fake, &cache).await;

        assert!(report.outcomes.iter().all(|outcome| outcome.outcome == Outcome::Done));
        assert_eq!(report.summary(), ["Turned off kitchen, living room lamp, and tv plug."]);
        assert_eq!(cache.read().entity("light.hallway").unwrap().state, "on");
        let called: Vec<String> = fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .flat_map(|call| call.entity_ids.clone())
            .collect();
        assert!(!called.contains(&"light.hallway".to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn unchanged_states_are_not_reported_as_success() {
        let cache = HomeCache::new(home());
        let plan = turn_off_downstairs_except_hallway(&cache);
        let report = execute(&plan, &fake(&cache, None), &cache).await;

        assert!(
            report
                .outcomes
                .iter()
                .all(|outcome| outcome.outcome == Outcome::NotConfirmed)
        );
        assert_eq!(
            report.summary(),
            ["Couldn't confirm kitchen, living room lamp, and tv plug changed."]
        );
    }

    #[tokio::test]
    async fn failed_service_calls_are_reported_per_entity() {
        let cache = HomeCache::new(home());
        let plan = turn_off_downstairs_except_hallway(&cache);
        let mut fake = fake(&cache, Some(("off", json!({}))));
        fake.failing_services = vec!["switch"];
        let report = execute(&plan, &fake, &cache).await;

        assert_eq!(
            report.summary(),
            ["Turned off kitchen and living room lamp.", "Couldn't turn off tv plug."]
        );
        assert!(report.for_model().contains("tv plug: failed"));
    }

    #[tokio::test(start_paused = true)]
    async fn moving_covers_are_in_progress() {
        let cache = HomeCache::new(home());
        let plan = plan(&cache.read(), &single(Action::Open, "cover.garage_door", None)).unwrap();
        let opening = fake(&cache, Some(("opening", json!({"device_class": "garage"}))));
        let report = execute(&plan, &opening, &cache).await;
        assert_eq!(report.summary(), ["Garage door is opening."]);
    }

    #[tokio::test]
    async fn brightness_is_verified_against_the_attribute() {
        let cache = HomeCache::new(home());
        let request = single(Action::SetBrightness, "light.hallway", Some(40.0));
        let plan = plan(&cache.read(), &request).unwrap();
        let attributes = json!({"brightness": 102, "supported_color_modes": ["brightness"]});
        let report = execute(&plan, &fake(&cache, Some(("on", attributes))), &cache).await;
        assert_eq!(report.summary(), ["Set hallway to 40%."]);
    }

    #[test]
    fn long_lists_collapse_to_a_count() {
        assert_eq!(list(&["a", "b", "c", "d"]), "4 devices");
        assert_eq!(list(&["a", "b"]), "a and b");
    }
}
