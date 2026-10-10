//! Short, conversational replies built from verified results. Wording varies in a fixed rotation,
//! so replies never depend on randomness and execution is never affected by phrasing.
use serde_json::Value;

use super::route::Asked;
use crate::actions::{Action, ExecutionReport};
use crate::home_assistant::model::Entity;

const ACKNOWLEDGEMENTS: &[&str] = &["Done.", "You got it.", "All set."];
const THANKS: &[&str] = &["You're welcome.", "Anytime.", "No problem."];
const UNDONE: &[&str] = &["Changed it back.", "Reverted."];
/// Above this many devices, a reply gives a count rather than names.
const MAX_NAMED: usize = 3;

fn pick<'a>(options: &[&'a str], variant: usize) -> &'a str {
    options[variant % options.len()]
}

pub fn thanks(variant: usize) -> &'static str {
    pick(THANKS, variant)
}

pub fn undone(variant: usize) -> &'static str {
    pick(UNDONE, variant)
}

/// The reply to verified actions. `named` means the user said which device, by name or "it",
/// so a single device needs no name in the reply.
pub fn acknowledge(reports: &[ExecutionReport], named: bool, variant: usize) -> String {
    match reports {
        [report] if named && report.outcomes.len() == 1 && fully_done(report) => {
            brief(report, variant)
        }
        _ => reports
            .iter()
            .flat_map(|report| result_lines(report, variant))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Result lines for one report. Partial results always name what did not work.
pub fn result_lines(report: &ExecutionReport, variant: usize) -> Vec<String> {
    let state = report.done_state();
    match state {
        Some(state) if report.outcomes.len() > MAX_NAMED && fully_done(report) => {
            let count = report.outcomes.len();
            vec![format!(
                "{} All {count} devices are {state}.",
                pick(&ACKNOWLEDGEMENTS[..2], variant)
            )]
        }
        _ => report.summary(),
    }
}

fn fully_done(report: &ExecutionReport) -> bool {
    report.all_done() && report.left_out.is_empty()
}

/// "Done.", "You got it.", "Turned off.", "Dimmed to 20%.", "Set to 26°."
fn brief(report: &ExecutionReport, variant: usize) -> String {
    match (report.action, report.value) {
        (Action::SetBrightness, Some(value)) if value > 0.0 => {
            let percent = value.round();
            let change = match brightness_direction(report, percent) {
                Some(true) => "Brightened",
                Some(false) => "Dimmed",
                None => "Set",
            };
            let options = [
                format!("{change} to {percent}%."),
                format!("You got it, {percent}%."),
            ];
            options[variant % options.len()].clone()
        }
        (Action::SetTemperature, Some(value)) => {
            let options = [
                format!("Set to {value}\u{b0}."),
                format!("You got it, {value}\u{b0}."),
            ];
            options[variant % options.len()].clone()
        }
        (action, _) => {
            let options = [
                ACKNOWLEDGEMENTS[0],
                ACKNOWLEDGEMENTS[1],
                past_tense(action),
                ACKNOWLEDGEMENTS[2],
            ];
            pick(&options, variant).to_owned()
        }
    }
}

fn past_tense(action: Action) -> &'static str {
    match action {
        Action::TurnOn => "Turned on.",
        Action::TurnOff | Action::SetBrightness => "Turned off.",
        Action::SetTemperature => "Changed.",
        Action::Open => "Opened.",
        Action::Close => "Closed.",
        Action::Lock => "Locked.",
        Action::Unlock => "Unlocked.",
        Action::Activate => "Started.",
    }
}

/// Whether every light got brighter (`true`) or dimmer (`false`), from its state before.
fn brightness_direction(report: &ExecutionReport, percent: f64) -> Option<bool> {
    let before: Vec<f64> = report.previous.iter().map(brightness_percent).collect();
    if before.is_empty() {
        None
    } else if before.iter().all(|before| *before < percent) {
        Some(true)
    } else if before.iter().all(|before| *before > percent) {
        Some(false)
    } else {
        None
    }
}

fn brightness_percent(entity: &Entity) -> f64 {
    if entity.state == "off" {
        return 0.0;
    }
    entity
        .attributes
        .get("brightness")
        .and_then(Value::as_f64)
        .map_or(100.0, |brightness| (brightness * 100.0 / 255.0).round())
}

/// "Nope, it's closed." for a yes-or-no question about one device. `None` when the state does
/// not answer it, such as a door that is still opening or a device that is unavailable.
pub fn yes_or_no(entity: &Entity, asked: Asked, phrase: &str, variant: usize) -> Option<String> {
    if !entity.is_available() {
        return None;
    }
    let matches = match asked {
        Asked::Power(on) => {
            let is_on = !matches!(entity.state.as_str(), "off" | "standby");
            is_on == on
        }
        Asked::Opening(open) => match phrase {
            "open" => open,
            "closed" => !open,
            _ => return None,
        },
        Asked::Locking(locked) => match phrase {
            "locked" => locked,
            "unlocked" => !locked,
            _ => return None,
        },
        Asked::Status | Asked::Temperature => return None,
    };
    let answer = if matches {
        pick(&["Yes", "Yep"], variant)
    } else {
        pick(&["Nope", "No"], variant)
    };
    Some(format!("{answer}, it's {phrase}."))
}

/// A reply about several devices, and the ones Luna offers to switch.
pub struct GroupAnswer {
    pub text: String,
    pub offer: Vec<String>,
}

/// "Yep, but only the 2.4G. Want me to turn on the 5G too?" for "is the guest Wi-Fi on?" about
/// several devices. Unavailable ones are left out; `None` when none is available.
pub fn power_of_several(entities: &[&Entity], on: bool, variant: usize) -> Option<GroupAnswer> {
    let names = short_names(entities);
    let available: Vec<(&Entity, &str)> = entities
        .iter()
        .zip(&names)
        .filter(|(entity, _)| entity.is_available())
        .map(|(entity, name)| (*entity, name.as_str()))
        .collect();
    if available.is_empty() {
        return None;
    }
    let (matching, others): (Vec<_>, Vec<_>) = available
        .iter()
        .partition(|(entity, _)| !matches!(entity.state.as_str(), "off" | "standby") == on);
    let matching: Vec<&str> = matching.iter().map(|(_, name)| *name).collect();
    let other_names: Vec<&str> = others.iter().map(|(_, name)| *name).collect();
    let (state, opposite) = if on { ("on", "off") } else { ("off", "on") };
    let yes = pick(&["Yes", "Yep"], variant);
    let no = pick(&["Nope", "No"], variant);
    let text = match (matching.len(), other_names.len()) {
        (_, 0) => format!("{yes}, {} {state}.", all_of(&matching)),
        (0, 1) => format!(
            "{no}, {} {opposite}. Want me to turn it {state}?",
            all_of(&other_names)
        ),
        (0, _) => format!(
            "{no}, {} {opposite}. Want me to turn them {state}?",
            all_of(&other_names)
        ),
        _ => format!(
            "{yes}, but only the {}. Want me to turn {state} the {} too?",
            list(&matching),
            list(&other_names)
        ),
    };
    Some(GroupAnswer {
        text,
        offer: others.iter().map(|(entity, _)| entity.id.clone()).collect(),
    })
}

/// "the 2.4G is", "both are", "all three are".
fn all_of(names: &[&str]) -> String {
    match names {
        [name] => format!("the {name} is"),
        [_, _] => "both are".into(),
        [_, _, _] => "all three are".into(),
        [_, _, _, _] => "all four are".into(),
        _ => format!("all {} are", names.len()),
    }
}

/// "2.4G", "2.4G and 5G", "2.4G, 5G, and 6G".
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// Names without the words they all start with, so "TP-Link Router Guest WIFI 2.4G" and
/// "TP-Link Router Guest WIFI 5G" become "2.4G" and "5G".
fn short_names(entities: &[&Entity]) -> Vec<String> {
    let words: Vec<Vec<&str>> = entities
        .iter()
        .map(|entity| entity.name.split_whitespace().collect())
        .collect();
    let shortest = words.iter().map(Vec::len).min().unwrap_or(0);
    let shared = (0..shortest)
        .take_while(|&index| words.iter().all(|name| name[index] == words[0][index]))
        .count();
    if entities.len() < 2 || shared == 0 || shared == shortest {
        return entities.iter().map(|entity| entity.name.clone()).collect();
    }
    words.iter().map(|name| name[shared..].join(" ")).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::actions::{EntityOutcome, Outcome};
    use crate::home_assistant::model::fixtures::{home, state};

    fn report(action: Action, value: Option<f64>, outcomes: &[Outcome]) -> ExecutionReport {
        ExecutionReport {
            action,
            value,
            outcomes: outcomes
                .iter()
                .enumerate()
                .map(|(index, outcome)| EntityOutcome {
                    id: format!("light.l{index}"),
                    name: format!("Light {index}"),
                    outcome: *outcome,
                })
                .collect(),
            left_out: Vec::new(),
            previous: Vec::new(),
            service_time: Default::default(),
            verify_time: Default::default(),
        }
    }

    #[test]
    fn acknowledgements_rotate_without_repeating_back_to_back() {
        let done = report(Action::TurnOff, None, &[Outcome::Done]);
        let replies: Vec<String> = (0..4)
            .map(|variant| acknowledge(std::slice::from_ref(&done), true, variant))
            .collect();
        assert_eq!(replies, ["Done.", "You got it.", "Turned off.", "All set."]);
        assert_eq!(acknowledge(&[done], true, 4), "Done.");
    }

    #[test]
    fn devices_are_named_when_the_user_did_not_name_them() {
        let done = report(Action::TurnOff, None, &[Outcome::Done]);
        assert_eq!(acknowledge(&[done], false, 0), "Light 0 is off.");
    }

    #[test]
    fn many_devices_get_a_count() {
        let done = report(Action::TurnOff, None, &[Outcome::Done; 6]);
        assert_eq!(
            acknowledge(&[done], false, 0),
            "Done. All 6 devices are off."
        );
    }

    #[test]
    fn partial_failures_name_what_did_not_work() {
        let mixed = report(
            Action::TurnOff,
            None,
            &[
                Outcome::Done,
                Outcome::Failed,
                Outcome::Done,
                Outcome::Done,
                Outcome::Done,
            ],
        );
        assert_eq!(
            acknowledge(&[mixed], true, 0),
            "4 devices are off. Couldn't turn off Light 1."
        );
        let failed = report(Action::TurnOn, None, &[Outcome::Failed]);
        assert_eq!(acknowledge(&[failed], true, 0), "Couldn't turn on Light 0.");
    }

    #[test]
    fn settings_mention_the_value_and_direction() {
        let mut dimmed = report(Action::SetBrightness, Some(20.0), &[Outcome::Done]);
        let home = home();
        let mut before = home.entity("light.hallway").unwrap().clone();
        before.attributes.insert("brightness".into(), json!(128));
        dimmed.previous = vec![before];
        assert_eq!(acknowledge(&[dimmed.clone()], true, 0), "Dimmed to 20%.");
        assert_eq!(acknowledge(&[dimmed], true, 1), "You got it, 20%.");

        let warmer = report(Action::SetTemperature, Some(26.0), &[Outcome::Done]);
        assert_eq!(acknowledge(&[warmer], true, 0), "Set to 26\u{b0}.");
    }

    fn router(states: &[(&str, &str)]) -> crate::home_assistant::model::Home {
        let mut home = home();
        for (band, value) in states {
            let id = format!("switch.guest_wifi_{band}");
            let name = format!("TP-Link Router Guest WIFI {}", band.to_uppercase());
            home.apply_state(&id, Some(state(&id, value, json!({"friendly_name": name}))));
        }
        home
    }

    fn ask_power(states: &[(&str, &str)], on: bool) -> (String, Vec<String>) {
        let home = router(states);
        let entities: Vec<&Entity> = states
            .iter()
            .map(|(band, _)| home.entity(&format!("switch.guest_wifi_{band}")).unwrap())
            .collect();
        let answer = power_of_several(&entities, on, 0).unwrap();
        (answer.text, answer.offer)
    }

    #[test]
    fn questions_about_several_devices_answer_casually_and_offer_the_rest() {
        let (text, offer) = ask_power(&[("2g", "on"), ("5g", "off"), ("6g", "unavailable")], true);
        assert_eq!(text, "Yes, but only the 2G. Want me to turn on the 5G too?");
        assert_eq!(offer, ["switch.guest_wifi_5g"]);

        let (text, offer) = ask_power(&[("2g", "on"), ("5g", "on")], true);
        assert_eq!(text, "Yes, both are on.");
        assert!(offer.is_empty());

        let (text, offer) = ask_power(&[("2g", "off"), ("5g", "off")], true);
        assert_eq!(text, "Nope, both are off. Want me to turn them on?");
        assert_eq!(offer.len(), 2);

        let (text, _) = ask_power(&[("2g", "off"), ("6g", "unavailable")], true);
        assert_eq!(text, "Nope, the 2G is off. Want me to turn it on?");
    }

    #[test]
    fn yes_or_no_answers_follow_the_live_state() {
        let mut home = home();
        home.apply_state(
            "cover.garage_door",
            Some(state("cover.garage_door", "closed", json!({}))),
        );
        let garage = home.entity("cover.garage_door").unwrap();
        assert_eq!(
            yes_or_no(garage, Asked::Opening(true), "closed", 0).as_deref(),
            Some("Nope, it's closed.")
        );
        assert_eq!(
            yes_or_no(garage, Asked::Opening(false), "closed", 1).as_deref(),
            Some("Yep, it's closed.")
        );
        assert_eq!(yes_or_no(garage, Asked::Opening(true), "opening", 0), None);
    }
}
