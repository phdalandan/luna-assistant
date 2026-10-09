use serde_json::{Value, json};

use crate::actions::{Action, ControlRequest, Target};
use crate::inference::FunctionCall;

pub const GET_STATES: &str = "get_states";
pub const CONTROL: &str = "control";

const DOMAINS: [&str; 14] = [
    "light",
    "switch",
    "fan",
    "cover",
    "climate",
    "media_player",
    "lock",
    "scene",
    "script",
    "sensor",
    "binary_sensor",
    "input_boolean",
    "alarm_control_panel",
    "person",
];

#[derive(Debug, PartialEq)]
pub enum ToolRequest {
    GetStates(Target),
    Control(ControlRequest),
}

/// The only tools offered to the model. Both are interpreted and validated in Rust.
pub fn definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": GET_STATES,
                "description": "Read the current state of devices and sensors. Use it to answer questions and to find entity IDs.",
                "parameters": {
                    "type": "object",
                    "properties": { "target": target_schema() },
                    "required": ["target"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": CONTROL,
                "description": "Change devices. Luna validates the request, skips devices that cannot do the action, and reports verified results.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": Action::ALL.map(Action::name),
                            "description": "set_brightness takes a percentage, set_temperature a target temperature, activate runs scenes and scripts."
                        },
                        "target": target_schema(),
                        "value": {
                            "type": "number",
                            "description": "Only for set_brightness (0 to 100) and set_temperature."
                        }
                    },
                    "required": ["action", "target"]
                }
            }
        }
    ])
}

fn target_schema() -> Value {
    let ids = |description: &str| {
        json!({"type": "array", "items": {"type": "string"}, "description": description})
    };
    json!({
        "type": "object",
        "description": "Which devices. Combine fields as needed. Exclusions are removed before anything runs.",
        "properties": {
            "everywhere": {"type": "boolean", "description": "The whole home."},
            "floors": ids("Floor IDs from the home summary."),
            "areas": ids("Area IDs from the home summary."),
            "entities": ids("Entity IDs from the home summary or get_states results."),
            "domains": {"type": "array", "items": {"type": "string", "enum": DOMAINS}, "description": "Limit to these device types."},
            "device_classes": ids("Limit to device classes such as temperature, garage, or motion."),
            "exclude_areas": ids("Area IDs to leave out."),
            "exclude_entities": ids("Entity IDs to leave out.")
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
        other => Err(format!("Unknown tool {other}. Use {GET_STATES} or {CONTROL}.")),
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

    #[test]
    fn definitions_list_every_action() {
        let definitions = definitions();
        let actions = &definitions[1]["function"]["parameters"]["properties"]["action"]["enum"];
        assert_eq!(actions.as_array().unwrap().len(), Action::ALL.len());
    }
}
