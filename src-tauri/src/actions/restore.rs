use std::collections::BTreeMap;

use serde_json::Value;

use super::{Action, ControlRequest, Target};
use crate::home_assistant::model::{Entity, Home};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RestoreError {
    #[error("restoring {0} is not supported")]
    Unsupported(String),
    #[error("{0} no longer exists")]
    Missing(String),
}

/// Requests that return each entity to its recorded earlier state.
/// Entities already in that state are skipped, so the result can be empty.
pub fn restore(home: &Home, previous: &[Entity]) -> Result<Vec<ControlRequest>, RestoreError> {
    let mut groups: BTreeMap<(&'static str, Option<u64>), ControlRequest> = BTreeMap::new();
    for before in previous {
        let now = home
            .entity(&before.id)
            .ok_or_else(|| RestoreError::Missing(before.name.clone()))?;
        let steps =
            steps(before, now).ok_or_else(|| RestoreError::Unsupported(before.name.clone()))?;
        for (action, value) in steps {
            groups
                .entry((action.name(), value.map(f64::to_bits)))
                .or_insert_with(|| ControlRequest {
                    action,
                    target: Target::default(),
                    value,
                })
                .target
                .entities
                .push(before.id.clone());
        }
    }
    Ok(groups.into_values().collect())
}

/// `None` when the earlier state cannot be restored safely through Luna's actions.
fn steps(before: &Entity, now: &Entity) -> Option<Vec<(Action, Option<f64>)>> {
    let state = before.state.as_str();
    let unchanged = now.state == before.state;
    let step = match (before.domain(), state) {
        ("light", "off") | ("switch" | "fan" | "input_boolean" | "media_player", "off") => {
            (!unchanged).then_some((Action::TurnOff, None))
        }
        ("light", "on") => match brightness_percent(before) {
            Some(percent) if !unchanged || brightness_percent(now) != Some(percent) => {
                Some((Action::SetBrightness, Some(percent)))
            }
            Some(_) => None,
            None => (!unchanged).then_some((Action::TurnOn, None)),
        },
        ("switch" | "fan" | "input_boolean" | "media_player", "on") => {
            (!unchanged).then_some((Action::TurnOn, None))
        }
        ("climate", "off") => (!unchanged).then_some((Action::TurnOff, None)),
        ("climate", _) if unchanged => match number(before, "temperature") {
            Some(target) if number(now, "temperature") != Some(target) => {
                Some((Action::SetTemperature, Some(target)))
            }
            _ => None,
        },
        ("cover", "open") => (!unchanged).then_some((Action::Open, None)),
        ("cover", "closed") => (!unchanged).then_some((Action::Close, None)),
        ("lock", "locked") => (!unchanged).then_some((Action::Lock, None)),
        ("lock", "unlocked") => (!unchanged).then_some((Action::Unlock, None)),
        _ => return None,
    };
    Some(step.into_iter().collect())
}

fn brightness_percent(entity: &Entity) -> Option<f64> {
    number(entity, "brightness").map(|brightness| (brightness * 100.0 / 255.0).round())
}

fn number(entity: &Entity, attribute: &str) -> Option<f64> {
    entity.attributes.get(attribute).and_then(Value::as_f64)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::home_assistant::model::fixtures::{home, state};

    fn snapshot(home: &Home, id: &str) -> Entity {
        home.entity(id).unwrap().clone()
    }

    #[test]
    fn restores_the_recorded_state_not_the_opposite_action() {
        let mut home = home();
        let before = snapshot(&home, "light.hallway");
        home.apply_state(
            "light.hallway",
            Some(state(
                "light.hallway",
                "on",
                json!({"brightness": 255, "supported_color_modes": ["brightness"]}),
            )),
        );
        let requests = restore(&home, &[before]).unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].action, Action::SetBrightness);
        assert_eq!(requests[0].value, Some(50.0));
    }

    #[test]
    fn entities_already_in_their_earlier_state_are_skipped() {
        let home = home();
        let before = snapshot(&home, "light.kitchen");
        assert_eq!(restore(&home, &[before]), Ok(vec![]));
    }

    #[test]
    fn groups_entities_with_the_same_step() {
        let mut home = home();
        let previous = vec![
            snapshot(&home, "light.kitchen"),
            snapshot(&home, "switch.tv_plug"),
        ];
        for id in ["light.kitchen", "switch.tv_plug"] {
            home.apply_state(id, Some(state(id, "off", json!({}))));
        }
        let requests = restore(&home, &previous).unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].action, Action::TurnOn);
        assert_eq!(
            requests[0].target.entities,
            ["light.kitchen", "switch.tv_plug"]
        );
    }

    #[test]
    fn unsupported_states_are_not_guessed() {
        let mut home = home();
        let before = snapshot(&home, "media_player.living_room_tv");
        home.apply_state(
            "media_player.living_room_tv",
            Some(state("media_player.living_room_tv", "off", json!({}))),
        );
        assert!(matches!(
            restore(&home, &[before]),
            Err(RestoreError::Unsupported(_))
        ));
    }

    #[test]
    fn restoring_an_unlocked_door_goes_through_unlock() {
        let mut home = home();
        home.apply_state(
            "lock.front_door",
            Some(state("lock.front_door", "unlocked", json!({}))),
        );
        let before = snapshot(&home, "lock.front_door");
        home.apply_state(
            "lock.front_door",
            Some(state("lock.front_door", "locked", json!({}))),
        );
        let requests = restore(&home, &[before]).unwrap();
        assert_eq!(requests[0].action, Action::Unlock);
    }
}
