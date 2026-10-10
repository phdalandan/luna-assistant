//! Handles recognised requests without the language model, and shared execution helpers.
use super::route::{self, Asked, Intent, Resolution, Subject};
use super::session::{Choice, Memory, Offer, Pending, Turn};
use super::{Home, Metrics, Reply, clock, phrasing};
use crate::actions::{
    self, Action, ControlRequest, ExecutionReport, Plan, Target, ValidationError,
};
use crate::home_assistant::model::{Entity, Home as HomeModel};
use crate::home_assistant::{HomeApi, HomeCache};

pub const NOT_CONNECTED: &str = "Home Assistant isn't connected right now.";

/// Returns `None` when the request needs the model.
pub async fn handle<A: HomeApi>(
    home: &Home<'_, A>,
    memory: &mut Memory,
    request: &str,
) -> Option<Reply> {
    let previous = memory.last_request.take();
    if let Some(offer) = memory.offer.take() {
        match super::relevance::answer(request) {
            Some(true) if !home.connected => return Some(Reply::direct(NOT_CONNECTED)),
            Some(true) => {
                let pending = Pending::Control(offer.action);
                return perform(home, memory, pending, None, offer.ids).await;
            }
            Some(false) => return Some(Reply::direct("Okay.")),
            None => {}
        }
    }
    if let Some(choice) = memory.choice.take() {
        let picked = route::pick(&home.cache.read(), &choice.ids, request);
        if let Some(ids) = picked {
            if !home.connected {
                return Some(Reply::direct(NOT_CONNECTED));
            }
            return perform(home, memory, choice.pending, None, ids).await;
        }
    }
    let intent = route::parse(request)?;
    if !home.connected {
        return Some(Reply::direct(NOT_CONNECTED));
    }
    let intent_domains = route::domains(&intent);
    let asked = match &intent {
        Intent::Query { asked, .. } => Some(*asked),
        _ => None,
    };
    let (subject, pending) = match intent {
        Intent::Thanks => return Some(Reply::direct(phrasing::thanks(memory.next_variant()))),
        Intent::Time => return Some(Reply::direct(clock::read(&home.cache.read()).reply())),
        Intent::FollowUp { subject } => (subject, previous?),
        Intent::Undo => return Some(undo(home, memory).await),
        Intent::Recheck => return Some(recheck(home, memory).await),
        Intent::Control { action, subject } => (subject, Pending::Control(action)),
        Intent::Set { value, subject, .. } => (subject, Pending::Set(value)),
        Intent::Query { subject, .. } => (subject, Pending::Query(intent_domains)),
    };
    let domains = pending_domains(&pending);
    let ids = {
        let snapshot = home.cache.read();
        match route::resolve(&snapshot, memory, &subject, &domains) {
            Resolution::Exact(ids) => ids,
            // Reading a few devices is harmless, so a question answers for each of them.
            Resolution::Choices(ids) if matches!(pending, Pending::Query(_)) => ids,
            Resolution::Choices(ids) => match just_mentioned(memory, &ids) {
                Some(id) => vec![id],
                None => return Some(ask_which(&snapshot, memory, ids, pending)),
            },
            Resolution::Unknown => return None,
        }
    };
    perform(home, memory, pending, asked, ids).await
}

/// The one choice the conversation was just about, so after "is the guest Wi-Fi on?",
/// "turn on the 5G" means the guest network's 5G rather than another 5G switch.
fn just_mentioned(memory: &Memory, ids: &[String]) -> Option<String> {
    let mut mentioned = ids.iter().filter(|id| {
        memory
            .referenced
            .iter()
            .any(|reference| &reference.id == *id)
    });
    match (mentioned.next(), mentioned.next()) {
        (Some(id), None) => Some(id.clone()),
        _ => None,
    }
}

/// Carries out a recognised request on resolved entities. `asked` is set for direct questions,
/// so "is the garage open?" can be answered yes or no.
async fn perform<A: HomeApi>(
    home: &Home<'_, A>,
    memory: &mut Memory,
    pending: Pending,
    asked: Option<Asked>,
    ids: Vec<String>,
) -> Option<Reply> {
    let (action, value) = match &pending {
        Pending::Query(domains) => {
            let snapshot = home.cache.read();
            let entities: Vec<&Entity> = ids.iter().filter_map(|id| snapshot.entity(id)).collect();
            let mut turn = Turn::default();
            let answer = match (asked, entities.as_slice()) {
                (Some(asked), [entity]) => {
                    let phrase = state_phrase(entity);
                    let answer = phrasing::yes_or_no(entity, asked, &phrase, memory.next_variant());
                    if answer.is_some() {
                        turn.refer(&entity.id, phrase);
                    }
                    answer
                }
                (Some(Asked::Power(on)), [_, _, ..]) => {
                    let variant = memory.next_variant();
                    phrasing::power_of_several(&entities, on, variant).map(|answer| {
                        for entity in &entities {
                            turn.refer(&entity.id, state_phrase(entity));
                        }
                        if !answer.offer.is_empty() {
                            memory.offer = Some(Offer {
                                action: if on { Action::TurnOn } else { Action::TurnOff },
                                ids: answer.offer,
                            });
                        }
                        answer.text
                    })
                }
                _ => None,
            };
            let text = match answer {
                Some(answer) => answer,
                None if domains.as_slice() == route::TEMPERATURE_DOMAINS => {
                    describe_temperatures(&entities, &mut turn)
                }
                None => describe(&entities, &mut turn),
            };
            memory.record(turn);
            memory.last_request = Some(pending.clone());
            return Some(Reply::direct(text));
        }
        Pending::Control(action) => (*action, None),
        Pending::Set(value) => (setting_action(&home.cache.read(), &ids)?, Some(*value)),
    };
    memory.last_request = Some(pending);
    let request = ControlRequest {
        action,
        target: Target {
            entities: ids,
            ..Target::default()
        },
        value,
    };
    Some(run(home, memory, vec![request], None, true).await)
}

/// The domains a request applies to, so a follow-up resolves its new subject the same way.
fn pending_domains(pending: &Pending) -> Vec<&'static str> {
    match pending {
        Pending::Query(domains) => domains.clone(),
        Pending::Control(action) => route::domains(&Intent::Control {
            action: *action,
            subject: Subject::Pronoun,
        }),
        Pending::Set(_) => route::domains(&Intent::Set {
            kind: None,
            value: 0.0,
            subject: Subject::Pronoun,
        }),
    }
}

/// "Bacsil AC is 27.5°, set to 26°." Sensors read as they are.
fn describe_temperatures(entities: &[&Entity], turn: &mut Turn) -> String {
    entities
        .iter()
        .map(|entity| {
            let phrase = state_phrase(entity);
            turn.refer(&entity.id, phrase.clone());
            let number = |key: &str| {
                entity
                    .attributes
                    .get(key)
                    .and_then(serde_json::Value::as_f64)
            };
            let reading = match (
                entity.domain(),
                number("current_temperature"),
                number("temperature"),
            ) {
                ("climate", Some(current), Some(target)) => {
                    format!("{current}\u{b0}, set to {target}\u{b0}")
                }
                ("climate", Some(current), None) => format!("{current}\u{b0}"),
                _ => phrase,
            };
            format!("{} is {reading}.", actions::capitalize(&entity.name))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Which one: Office AC or Bedroom AC?" The answer completes `pending`.
fn ask_which(home: &HomeModel, memory: &mut Memory, ids: Vec<String>, pending: Pending) -> Reply {
    let entities: Vec<&Entity> = ids.iter().filter_map(|id| home.entity(id)).collect();
    let mut turn = Turn::default();
    let labels: Vec<String> = entities
        .iter()
        .map(|entity| {
            turn.refer(&entity.id, state_phrase(entity));
            let shared = entities
                .iter()
                .any(|other| other.id != entity.id && other.name == entity.name);
            let area = entity.area_id.as_deref().and_then(|id| home.area(id));
            match area {
                Some(area) if shared => format!("{} in {}", entity.name, area.name),
                _ => entity.name.clone(),
            }
        })
        .collect();
    memory.record(turn);
    memory.choice = Some(Choice {
        ids: entities.iter().map(|entity| entity.id.clone()).collect(),
        pending,
    });
    let text = match labels.as_slice() {
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
        [] => String::new(),
    };
    Reply::direct(format!("Which one: {text}?"))
}

/// Brightness for lights, temperature for climate devices. Mixed targets need the model.
fn setting_action(home: &HomeModel, ids: &[String]) -> Option<Action> {
    let domains: Vec<&str> = ids
        .iter()
        .filter_map(|id| home.entity(id))
        .map(Entity::domain)
        .collect();
    if domains.iter().all(|domain| *domain == "light") {
        Some(Action::SetBrightness)
    } else if domains.iter().all(|domain| *domain == "climate") {
        Some(Action::SetTemperature)
    } else {
        None
    }
}

/// Validates, asks for confirmation if needed, then executes. `done` replaces verified results;
/// `named` means the user said which device, so a single one is not named again.
pub async fn run<A: HomeApi>(
    home: &Home<'_, A>,
    memory: &mut Memory,
    requests: Vec<ControlRequest>,
    done: Option<&str>,
    named: bool,
) -> Reply {
    let plans = match plan_all(home.cache, &requests) {
        Ok(plans) => plans,
        Err(error) => {
            log::info!("request rejected: {error}");
            return Reply::direct(rejection(&error));
        }
    };
    if plans.iter().any(|plan| plan.requires_confirmation) {
        return Reply {
            confirmation: requests,
            ..Reply::direct(confirmation_prompt(&plans))
        };
    }
    let mut reply = Reply::direct("");
    let mut turn = Turn::default();
    let reports = execute_all(home, &plans, &mut turn, &mut reply.metrics).await;
    memory.record(turn);
    let variant = memory.next_variant();
    reply.text = match done {
        Some(done) if reports.iter().all(ExecutionReport::all_done) => done.to_owned(),
        _ => phrasing::acknowledge(&reports, named, variant),
    };
    reply
}

pub fn plan_all(
    cache: &HomeCache,
    requests: &[ControlRequest],
) -> Result<Vec<Plan>, ValidationError> {
    let home = cache.read();
    requests
        .iter()
        .map(|request| actions::plan(&home, request))
        .collect()
}

pub async fn execute_all<A: HomeApi>(
    home: &Home<'_, A>,
    plans: &[Plan],
    turn: &mut Turn,
    metrics: &mut Metrics,
) -> Vec<ExecutionReport> {
    let mut reports = Vec::new();
    for plan in plans {
        let report = actions::execute(plan, home.api, home.cache).await;
        record_execution(&report, home.cache, turn);
        metrics.record(&report);
        reports.push(report);
    }
    reports
}

/// Remembers what was acted on and, for entities that changed, their earlier state.
pub fn record_execution(report: &ExecutionReport, cache: &HomeCache, turn: &mut Turn) {
    let home = cache.read();
    for outcome in &report.outcomes {
        if let Some(entity) = home.entity(&outcome.id) {
            turn.refer(&entity.id, state_phrase(entity));
        }
    }
    for before in &report.previous {
        let changed = home
            .entity(&before.id)
            .is_some_and(|now| now.state != before.state || now.attributes != before.attributes);
        if changed {
            turn.previous.push(before.clone());
        }
    }
}

pub fn confirmation_prompt(plans: &[Plan]) -> String {
    plans
        .iter()
        .filter(|plan| plan.requires_confirmation)
        .map(|plan| {
            let names: Vec<&str> = plan
                .entities
                .iter()
                .map(|entity| entity.name.as_str())
                .collect();
            let verb = actions::verb(plan.action);
            actions::capitalize(&format!("{verb} {}?", actions::list(&names)))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn undo<A: HomeApi>(home: &Home<'_, A>, memory: &mut Memory) -> Reply {
    if memory.last_action.is_empty() {
        return Reply::direct("There's nothing to undo.");
    }
    let requests = match actions::restore(&home.cache.read(), &memory.last_action) {
        Ok(requests) => requests,
        Err(error) => {
            log::info!("cannot undo: {error}");
            return Reply::direct("I can't undo that.");
        }
    };
    if requests.is_empty() {
        return Reply::direct("It's already back to how it was.");
    }
    let done = phrasing::undone(memory.next_variant());
    run(home, memory, requests, Some(done), true).await
}

async fn recheck<A: HomeApi>(home: &Home<'_, A>, memory: &mut Memory) -> Reply {
    if memory.referenced.is_empty() {
        return Reply::direct("What should I check?");
    }
    if let Err(error) = home.api.refresh_states().await {
        log::warn!("could not refresh states: {error}");
        return Reply::direct("I couldn't reach Home Assistant to check.");
    }
    let snapshot = home.cache.read();
    let entities: Vec<&Entity> = memory
        .referenced
        .iter()
        .filter_map(|reference| snapshot.entity(&reference.id))
        .collect();
    if entities.is_empty() {
        return Reply::direct("I can't find that in Home Assistant anymore.");
    }
    let unchanged = memory.referenced.iter().all(|reference| {
        snapshot
            .entity(&reference.id)
            .is_some_and(|entity| state_phrase(entity) == reference.reported)
    });
    let mut turn = Turn::default();
    let description = describe(&entities, &mut turn);
    let text = match (unchanged, entities.as_slice()) {
        (true, [entity]) => format!("Yes, I checked. It's {}.", state_phrase(entity)),
        (true, _) => format!("Yes, I checked. {description}"),
        (false, _) => format!("I checked again. {description}"),
    };
    drop(snapshot);
    memory.record(turn);
    Reply::direct(text)
}

/// "Garage door is closed." Entities sharing a state are grouped into one sentence.
fn describe(entities: &[&Entity], turn: &mut Turn) -> String {
    let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
    for entity in entities {
        let phrase = state_phrase(entity);
        turn.refer(&entity.id, phrase.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == phrase) {
            Some((_, names)) => names.push(&entity.name),
            None => groups.push((phrase, vec![&entity.name])),
        }
    }
    groups
        .iter()
        .map(|(phrase, names)| format!("{} {phrase}.", actions::subject(names)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn state_phrase(entity: &Entity) -> String {
    if !entity.is_available() {
        return "unavailable".into();
    }
    let state = entity.state.as_str();
    match (entity.domain(), entity.device_class()) {
        ("binary_sensor", Some("door" | "garage_door" | "window" | "opening")) => match state {
            "on" => "open".into(),
            "off" => "closed".into(),
            other => other.into(),
        },
        ("sensor", _) => {
            let unit = entity
                .attributes
                .get("unit_of_measurement")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let space = if unit.starts_with(char::is_alphabetic) {
                " "
            } else {
                ""
            };
            format!("{state}{space}{unit}")
        }
        ("climate", _) if state != "off" => format!("set to {}", state.replace('_', " ")),
        _ => state.replace('_', " "),
    }
}

fn rejection(error: &ValidationError) -> String {
    device_limit(error).unwrap_or_else(|| "That can't be done right now.".into())
}

/// A user-facing reason when a device itself cannot do what was asked.
pub fn device_limit(error: &ValidationError) -> Option<String> {
    match error {
        ValidationError::Unsupported { name, action } => Some(format!(
            "{} can't {}.",
            actions::capitalize(name),
            action.replace('_', " ")
        )),
        ValidationError::Unavailable { name } => Some(format!(
            "{} isn't available right now.",
            actions::capitalize(name)
        )),
        _ => None,
    }
}
