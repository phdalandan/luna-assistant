use std::collections::BTreeMap;

use serde_json::{Map, json};

use super::resolve::{ResolveError, select};
use super::{Action, ControlRequest};
use crate::home_assistant::ServiceCall;
use crate::home_assistant::model::{Entity, Home};

const DEFAULT_MIN_TEMP: f64 = 7.0;
const DEFAULT_MAX_TEMP: f64 = 35.0;

// Home Assistant entity feature flags.
const COVER_OPEN: u64 = 1;
const COVER_CLOSE: u64 = 2;
const CLIMATE_TARGET_TEMPERATURE: u64 = 1;
const CLIMATE_TURN_OFF: u64 = 128;
const CLIMATE_TURN_ON: u64 = 256;
const MEDIA_PLAYER_TURN_ON: u64 = 128;
const MEDIA_PLAYER_TURN_OFF: u64 = 256;

/// Errors are returned to the model as tool results so it can correct itself or ask the user.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValidationError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error("{name} does not support {action}")]
    Unsupported { name: String, action: &'static str },
    #[error("{name} is unavailable")]
    Unavailable { name: String },
    #[error("{action} requires value: {expected}")]
    MissingValue {
        action: &'static str,
        expected: &'static str,
    },
    #[error("{action} does not take a value")]
    UnexpectedValue { action: &'static str },
    #[error("{value} is outside the allowed range for {name} ({min} to {max})")]
    OutOfRange {
        name: String,
        value: f64,
        min: f64,
        max: f64,
    },
    #[error("nothing in the target supports {action}{}", left_out_note(.left_out))]
    NothingToDo {
        action: &'static str,
        left_out: Vec<String>,
    },
}

fn left_out_note(left_out: &[String]) -> String {
    if left_out.is_empty() {
        String::new()
    } else {
        format!("; left out: {}", left_out.join(", "))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlannedEntity {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub action: Action,
    pub value: Option<f64>,
    pub calls: Vec<ServiceCall>,
    pub entities: Vec<PlannedEntity>,
    /// Matched broadly but skipped because they are protected, unavailable, or out of range.
    pub left_out: Vec<String>,
    pub requires_confirmation: bool,
}

pub fn plan(home: &Home, request: &ControlRequest) -> Result<Plan, ValidationError> {
    let action = request.action;
    let value = checked_value(action, request.value)?;
    let selection = select(home, &request.target)?;

    let mut groups: BTreeMap<(&'static str, &'static str), Vec<String>> = BTreeMap::new();
    let mut entities = Vec::new();
    let mut left_out = Vec::new();
    let mut requires_confirmation = false;

    for selected in selection.entities {
        let entity = selected.entity;
        let Some(service) = service_for(action, entity.domain()) else {
            if selected.explicit {
                return Err(unsupported(entity, action));
            }
            continue;
        };
        if !selected.explicit && is_protected(entity) {
            left_out.push(entity.name.clone());
            continue;
        }
        match check_entity(entity, action, value) {
            Ok(()) => {}
            Err(error) if selected.explicit => return Err(error),
            // Broadly matched devices that lack the feature are not part of the request.
            Err(ValidationError::Unsupported { .. }) => continue,
            Err(_) => {
                left_out.push(entity.name.clone());
                continue;
            }
        }

        requires_confirmation |= needs_confirmation(entity, action);
        groups.entry(service).or_default().push(entity.id.clone());
        entities.push(PlannedEntity {
            id: entity.id.clone(),
            name: entity.name.clone(),
        });
    }

    if entities.is_empty() {
        return Err(ValidationError::NothingToDo {
            action: action.name(),
            left_out,
        });
    }
    let data = service_data(action, value);
    let calls = groups
        .into_iter()
        .map(|((domain, service), entity_ids)| ServiceCall {
            domain,
            service,
            entity_ids,
            data: data.clone(),
        })
        .collect();
    Ok(Plan {
        action,
        value,
        calls,
        entities,
        left_out,
        requires_confirmation,
    })
}

/// Locks, alarms, and doors are only acted on when named directly.
pub fn is_protected(entity: &Entity) -> bool {
    match entity.domain() {
        "lock" | "alarm_control_panel" => true,
        "cover" => matches!(entity.device_class(), Some("garage" | "door" | "gate")),
        _ => false,
    }
}

fn needs_confirmation(entity: &Entity, action: Action) -> bool {
    match action {
        Action::Unlock => true,
        Action::Open => is_protected(entity),
        _ => false,
    }
}

fn service_for(action: Action, domain: &str) -> Option<(&'static str, &'static str)> {
    const ON_OFF: [&str; 6] = [
        "light",
        "switch",
        "fan",
        "media_player",
        "climate",
        "input_boolean",
    ];
    let on_off_domain = ON_OFF.into_iter().find(|candidate| *candidate == domain);
    match (action, domain) {
        (Action::TurnOn, _) => on_off_domain.map(|domain| (domain, "turn_on")),
        (Action::TurnOff, _) => on_off_domain.map(|domain| (domain, "turn_off")),
        (Action::SetBrightness, "light") => Some(("light", "turn_on")),
        (Action::SetTemperature, "climate") => Some(("climate", "set_temperature")),
        (Action::Open, "cover") => Some(("cover", "open_cover")),
        (Action::Close, "cover") => Some(("cover", "close_cover")),
        (Action::Lock, "lock") => Some(("lock", "lock")),
        (Action::Unlock, "lock") => Some(("lock", "unlock")),
        (Action::Activate, "scene") => Some(("scene", "turn_on")),
        (Action::Activate, "script") => Some(("script", "turn_on")),
        _ => None,
    }
}

fn supports(entity: &Entity, action: Action) -> bool {
    let features = entity.supported_features();
    match (action, entity.domain()) {
        (Action::SetBrightness, "light") => supports_brightness(entity),
        (Action::SetTemperature, "climate") => features & CLIMATE_TARGET_TEMPERATURE != 0,
        (Action::TurnOn, "climate") => features & CLIMATE_TURN_ON != 0,
        (Action::TurnOff, "climate") => features & CLIMATE_TURN_OFF != 0,
        (Action::TurnOn, "media_player") => features & MEDIA_PLAYER_TURN_ON != 0,
        (Action::TurnOff, "media_player") => features & MEDIA_PLAYER_TURN_OFF != 0,
        (Action::Open, "cover") => features & COVER_OPEN != 0,
        (Action::Close, "cover") => features & COVER_CLOSE != 0,
        _ => true,
    }
}

fn supports_brightness(entity: &Entity) -> bool {
    entity
        .attributes
        .get("supported_color_modes")
        .and_then(|modes| modes.as_array())
        .is_some_and(|modes| modes.iter().any(|mode| mode.as_str() != Some("onoff")))
}

fn checked_value(action: Action, value: Option<f64>) -> Result<Option<f64>, ValidationError> {
    let expected = match action {
        Action::SetBrightness => "brightness percentage from 0 to 100",
        Action::SetTemperature => "target temperature",
        _ => {
            return match value {
                Some(_) => Err(ValidationError::UnexpectedValue {
                    action: action.name(),
                }),
                None => Ok(None),
            };
        }
    };
    let value = value
        .filter(|value| value.is_finite())
        .ok_or(ValidationError::MissingValue {
            action: action.name(),
            expected,
        })?;
    if action == Action::SetBrightness && !(0.0..=100.0).contains(&value) {
        return Err(ValidationError::OutOfRange {
            name: "brightness".into(),
            value,
            min: 0.0,
            max: 100.0,
        });
    }
    Ok(Some(value))
}

fn check_entity(
    entity: &Entity,
    action: Action,
    value: Option<f64>,
) -> Result<(), ValidationError> {
    if !entity.is_available() {
        return Err(ValidationError::Unavailable {
            name: entity.name.clone(),
        });
    }
    if !supports(entity, action) {
        return Err(unsupported(entity, action));
    }
    check_range(entity, action, value)
}

fn check_range(entity: &Entity, action: Action, value: Option<f64>) -> Result<(), ValidationError> {
    let (Action::SetTemperature, Some(value)) = (action, value) else {
        return Ok(());
    };
    let attribute = |key: &str, default: f64| {
        entity
            .attributes
            .get(key)
            .and_then(|value| value.as_f64())
            .unwrap_or(default)
    };
    let (min, max) = (
        attribute("min_temp", DEFAULT_MIN_TEMP),
        attribute("max_temp", DEFAULT_MAX_TEMP),
    );
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(ValidationError::OutOfRange {
            name: entity.name.clone(),
            value,
            min,
            max,
        })
    }
}

fn service_data(action: Action, value: Option<f64>) -> Map<String, serde_json::Value> {
    let mut data = Map::new();
    match (action, value) {
        (Action::SetBrightness, Some(value)) => {
            data.insert("brightness_pct".into(), json!(value.round() as u8));
        }
        (Action::SetTemperature, Some(value)) => {
            data.insert("temperature".into(), json!(value));
        }
        _ => {}
    }
    data
}

fn unsupported(entity: &Entity, action: Action) -> ValidationError {
    ValidationError::Unsupported {
        name: entity.name.clone(),
        action: action.name(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::actions::Target;
    use crate::home_assistant::model::fixtures::{home, state};

    fn request(action: Action, target: Target, value: Option<f64>) -> ControlRequest {
        ControlRequest {
            action,
            target,
            value,
        }
    }

    fn entities(values: &[&str]) -> Target {
        Target {
            entities: values.iter().map(|value| value.to_string()).collect(),
            ..Target::default()
        }
    }

    fn floor(id: &str) -> Target {
        Target {
            floors: vec![id.into()],
            ..Target::default()
        }
    }

    fn planned_ids(plan: &Plan) -> Vec<&str> {
        plan.entities
            .iter()
            .map(|entity| entity.id.as_str())
            .collect()
    }

    #[test]
    fn everything_downstairs_except_hallway_light() {
        let target = Target {
            exclude_entities: vec!["light.hallway".into()],
            ..floor("downstairs")
        };
        let plan = plan(&home(), &request(Action::TurnOff, target, None)).unwrap();
        assert_eq!(
            planned_ids(&plan),
            ["light.kitchen", "light.living_room_lamp", "switch.tv_plug"]
        );
        assert!(!plan.requires_confirmation);
        let services: Vec<_> = plan
            .calls
            .iter()
            .map(|call| (call.domain, call.service, call.entity_ids.len()))
            .collect();
        assert_eq!(
            services,
            [("light", "turn_off", 2), ("switch", "turn_off", 1)]
        );
    }

    #[test]
    fn broad_selection_never_includes_protected_entities() {
        let plan = plan(&home(), &request(Action::Open, floor("downstairs"), None)).unwrap();
        assert_eq!(planned_ids(&plan), ["cover.living_room_blinds"]);
        assert_eq!(plan.left_out, ["garage door"]);
        assert!(!plan.requires_confirmation);
    }

    #[test]
    fn broad_unlock_is_never_planned() {
        let everywhere = Target {
            everywhere: true,
            ..Target::default()
        };
        let result = plan(&home(), &request(Action::Unlock, everywhere, None));
        assert!(matches!(result, Err(ValidationError::NothingToDo { .. })));
    }

    #[test]
    fn sensitive_actions_require_confirmation() {
        let home = home();
        let unlock = plan(
            &home,
            &request(Action::Unlock, entities(&["lock.front_door"]), None),
        );
        assert!(unlock.unwrap().requires_confirmation);
        let garage = plan(
            &home,
            &request(Action::Open, entities(&["cover.garage_door"]), None),
        );
        assert!(garage.unwrap().requires_confirmation);
        let lock = plan(
            &home,
            &request(Action::Lock, entities(&["lock.front_door"]), None),
        );
        assert!(!lock.unwrap().requires_confirmation);
    }

    #[test]
    fn unsupported_action_on_named_entity_is_rejected() {
        let result = plan(
            &home(),
            &request(
                Action::TurnOn,
                entities(&["sensor.bedroom_temperature"]),
                None,
            ),
        );
        assert!(matches!(result, Err(ValidationError::Unsupported { .. })));
    }

    #[test]
    fn missing_feature_is_rejected_for_named_entities_and_skipped_otherwise() {
        let named = plan(
            &home(),
            &request(
                Action::SetBrightness,
                entities(&["light.kitchen"]),
                Some(50.0),
            ),
        );
        assert!(matches!(named, Err(ValidationError::Unsupported { .. })));

        let areas = Target {
            areas: vec!["kitchen".into(), "hallway".into()],
            ..Target::default()
        };
        let broad = plan(&home(), &request(Action::SetBrightness, areas, Some(50.0))).unwrap();
        assert_eq!(planned_ids(&broad), ["light.hallway"]);
        assert!(broad.left_out.is_empty());
        assert_eq!(broad.calls[0].data["brightness_pct"], json!(50));
    }

    #[test]
    fn media_player_without_power_features_is_rejected() {
        let result = plan(
            &home(),
            &request(
                Action::TurnOff,
                entities(&["media_player.living_room_tv"]),
                None,
            ),
        );
        assert!(matches!(result, Err(ValidationError::Unsupported { .. })));
    }

    #[test]
    fn values_are_validated() {
        let home = home();
        let hallway = || entities(&["light.hallway"]);
        let thermostat = || entities(&["climate.thermostat"]);

        let missing = plan(&home, &request(Action::SetBrightness, hallway(), None));
        assert!(matches!(missing, Err(ValidationError::MissingValue { .. })));
        let too_bright = plan(
            &home,
            &request(Action::SetBrightness, hallway(), Some(150.0)),
        );
        assert!(matches!(
            too_bright,
            Err(ValidationError::OutOfRange { .. })
        ));
        let unexpected = plan(&home, &request(Action::TurnOn, hallway(), Some(1.0)));
        assert!(matches!(
            unexpected,
            Err(ValidationError::UnexpectedValue { .. })
        ));
        let too_hot = plan(
            &home,
            &request(Action::SetTemperature, thermostat(), Some(40.0)),
        );
        assert!(matches!(too_hot, Err(ValidationError::OutOfRange { .. })));

        let ok = plan(
            &home,
            &request(Action::SetTemperature, thermostat(), Some(21.5)),
        )
        .unwrap();
        assert_eq!(ok.calls[0].service, "set_temperature");
        assert_eq!(ok.calls[0].data["temperature"], json!(21.5));
    }

    #[test]
    fn unavailable_entities_are_reported() {
        let mut home = home();
        home.apply_state(
            "light.bedroom",
            Some(state("light.bedroom", "unavailable", json!({}))),
        );
        let named = plan(
            &home,
            &request(Action::TurnOn, entities(&["light.bedroom"]), None),
        );
        assert!(matches!(named, Err(ValidationError::Unavailable { .. })));

        let broad = plan(&home, &request(Action::TurnOn, floor("upstairs"), None));
        assert!(
            matches!(broad, Err(ValidationError::NothingToDo { left_out, .. }) if left_out == ["bedroom"])
        );
    }

    #[test]
    fn scenes_are_activated() {
        let plan = plan(
            &home(),
            &request(Action::Activate, entities(&["scene.movie_night"]), None),
        )
        .unwrap();
        assert_eq!(
            (plan.calls[0].domain, plan.calls[0].service),
            ("scene", "turn_on")
        );
    }
}
