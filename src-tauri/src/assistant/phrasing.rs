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
