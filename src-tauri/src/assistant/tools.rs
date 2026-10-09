use serde_json::{Value, json};

use crate::actions::{Action, ControlRequest, Target};
use crate::inference::FunctionCall;

pub const GET_STATES: &str = "get_states";
pub const CONTROL: &str = "control";

/// Device types the model can read. Control only offers the ones it can change.
const READABLE_DOMAINS: [&str; 14] = [
    "light",
    "switch",
    "fan",
    "cover",
    "climate",
    "media_player",
    "lock",
    "scene",
    "script",
    "input_boolean",
    "sensor",
    "binary_sensor",
    "alarm_control_panel",
    "person",
];
const CONTROLLABLE_DOMAINS: usize = 10;

#[derive(Debug, PartialEq)]
pub enum ToolRequest {
    GetStates(Target),
    Control(ControlRequest),
}

/// The only tools offered to the model. Both are interpreted and validated in Rust.
/// Every token here is evaluated whenever the prompt cache is cold, so descriptions stay short.
pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": GET_STATES,
                "description": "Read states that are not in the request context.",
                "parameters": {
                    "type": "object",
                    "properties": { "target": target_schema(&READABLE_DOMAINS) },
                    "required": ["target"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": CONTROL,
                "description": "Change devices. Activate runs scenes and scripts.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": Action::ALL.map(Action::name)},
                        "target": target_schema(&READABLE_DOMAINS[..CONTROLLABLE_DOMAINS]),
                        "value": {"type": "number", "description": "Brightness percentage or target temperature."}
                    },
                    "required": ["action", "target"]
                }
            }
        }
    ])
}

fn target_schema(domains: &[&str]) -> Value {
    let ids = json!({"type": "array", "items": {"type": "string"}});
    json!({
        "type": "object",
        "properties": {
            "everywhere": {"type": "boolean"},
            "floors": ids,
            "areas": ids,
            "entities": ids,
            "domains": {"type": "array", "items": {"type": "string", "enum": domains}},
            "device_classes": {"type": "array", "items": {"type": "string"}, "description": "Such as temperature, garage, or motion."},
            "exclude_areas": ids,
            "exclude_entities": ids
        }
    })
}

pub fn parse(call: &FunctionCall) -> Result<ToolRequest, String> {
    let arguments = call.arguments.clone();
    match call.name.as_str() {
        GET_STATES => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Arguments {
                target: Target,
            }
            serde_json::from_value::<Arguments>(arguments)
                .map(|arguments| ToolRequest::GetStates(arguments.target))
                .map_err(|error| format!("Invalid arguments: {error}"))
        }
        CONTROL => serde_json::from_value(arguments)
            .map(ToolRequest::Control)
            .map_err(|error| format!("Invalid arguments: {error}")),
        other => Err(format!(
            "Unknown tool {other}. Use {GET_STATES} or {CONTROL}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> FunctionCall {
        FunctionCall {
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn parses_control_requests() {
        let request = parse(&call(
            CONTROL,
            json!({"action": "turn_off", "target": {"floors": ["downstairs"], "exclude_entities": ["light.hallway"]}}),
        ))
        .unwrap();
        let ToolRequest::Control(request) = request else {
            panic!("expected control");
        };
        assert_eq!(request.action, Action::TurnOff);
        assert_eq!(request.target.exclude_entities, ["light.hallway"]);
    }

    #[test]
    fn parses_state_queries() {
        let request = parse(&call(GET_STATES, json!({"target": {"everywhere": true}}))).unwrap();
        assert_eq!(
            request,
            ToolRequest::GetStates(Target {
                everywhere: true,
                ..Target::default()
            })
        );
    }

    #[test]
    fn rejects_unknown_tools_and_actions() {
        assert!(parse(&call("call_service", json!({}))).is_err());
        let disarm = json!({"action": "disarm", "target": {"everywhere": true}});
        assert!(parse(&call(CONTROL, disarm)).is_err());
    }

    #[test]
    fn rejects_unexpected_fields() {
        let result = parse(&call(
            CONTROL,
            json!({"action": "turn_on", "target": {"entities": ["light.kitchen"]}, "service": "light.toggle"}),
        ));
        assert!(result.is_err());
    }

    /// llama.cpp's tool grammar enforces property order, so locations must precede exclusions.
    #[test]
    fn schema_properties_keep_their_declared_order() {
        let text = definitions()[1]["function"]["parameters"].to_string();
        let position = |key: &str| text.find(&format!("\"{key}\":{{")).unwrap();
        assert!(position("floors") < position("exclude_entities"));
        assert!(position("action") < position("target"));
        assert!(position("target") < position("value"));
    }

    #[test]
    fn definitions_list_every_action() {
        let definitions = definitions();
        let actions = &definitions[1]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(actions.as_array().unwrap().len(), Action::ALL.len());
    }
}
