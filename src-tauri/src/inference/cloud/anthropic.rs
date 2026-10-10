//! Anthropic Messages API. Luna's messages and tools are translated to content blocks, and
//! `tool_use` blocks come back as Luna's tool calls.
use serde_json::{Value, json};

use super::{Usage, add_request_fields, count};
use crate::inference::{ChatMessage, FunctionCall, MAX_RESPONSE_TOKENS, Role, ToolCall};
use crate::models::CloudModel;

pub const VERSION: &str = "2023-06-01";

pub fn body(model: &CloudModel, messages: &[ChatMessage], tools: &Value) -> Value {
    let mut body = json!({
        "model": model.id,
        "max_tokens": MAX_RESPONSE_TOKENS,
        "messages": conversation(messages),
        "tools": tool_definitions(tools),
    });
    let system: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == Role::System)
        .map(|message| message.content.as_str())
        .collect();
    if !system.is_empty() {
        // Caches the tools and instructions, which change only with the home layout.
        body["system"] = json!([{
            "type": "text",
            "text": system.join("\n\n"),
            "cache_control": {"type": "ephemeral"},
        }]);
    }
    add_request_fields(&mut body, &model.request);
    body
}

/// Consecutive messages with the same role are merged, so every tool result for one assistant
/// turn arrives in a single user message, as the API requires.
fn conversation(messages: &[ChatMessage]) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    for message in messages {
        let (role, blocks) = match message.role {
            Role::System => continue,
            Role::User => ("user", text_block(&message.content)),
            Role::Assistant => {
                let mut blocks = text_block(&message.content);
                blocks.extend(message.tool_calls.iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.function.name,
                        "input": call.function.arguments,
                    })
                }));
                ("assistant", blocks)
            }
            Role::Tool => (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.as_deref().unwrap_or_default(),
                    "content": message.content,
                })],
            ),
        };
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some(last) if last["role"] == role => {
                if let Some(content) = last["content"].as_array_mut() {
                    content.extend(blocks);
                }
            }
            _ => turns.push(json!({"role": role, "content": blocks})),
        }
    }
    turns
}

/// The API rejects empty text blocks.
fn text_block(text: &str) -> Vec<Value> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![json!({"type": "text", "text": text})]
    }
}

fn tool_definitions(tools: &Value) -> Vec<Value> {
    tools
        .as_array()
        .into_iter()
        .flatten()
        .map(|tool| {
            let function = &tool["function"];
            json!({
                "name": function["name"],
                "description": function["description"],
                "input_schema": function["parameters"],
            })
        })
        .collect()
}

pub fn parse(body: &Value) -> Result<(ChatMessage, Usage), String> {
    let blocks = body["content"].as_array().ok_or("missing content")?;
    let mut content = String::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => content.push_str(block["text"].as_str().unwrap_or_default()),
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                function: FunctionCall {
                    name: block["name"].as_str().unwrap_or_default().to_owned(),
                    arguments: block["input"].clone(),
                },
            }),
            _ => {}
        }
    }
    let usage = &body["usage"];
    let cached = count(&usage["cache_read_input_tokens"]);
    Ok((
        ChatMessage {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        },
        Usage {
            input: count(&usage["input_tokens"])
                + cached
                + count(&usage["cache_creation_input_tokens"]),
            cached,
            output: count(&usage["output_tokens"]),
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, action: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: "control".into(),
                arguments: json!({"action": action}),
            },
        }
    }

    #[test]
    fn tool_turns_become_content_blocks() {
        let first = call("toolu_1", "turn_off");
        let second = call("toolu_2", "turn_on");
        let messages = [
            ChatMessage::new(Role::System, "Instructions"),
            ChatMessage::new(Role::User, "Earlier request"),
            ChatMessage::new(Role::Assistant, "Earlier reply"),
            ChatMessage::new(Role::User, "Turn off the porch and on the hallway"),
            ChatMessage {
                tool_calls: vec![first.clone(), second.clone()],
                ..ChatMessage::new(Role::Assistant, "")
            },
            ChatMessage::tool_result(&first, "Porch is off."),
            ChatMessage::tool_result(&second, "Rejected: unknown entity."),
        ];

        let turns = conversation(&messages);

        let roles: Vec<_> = turns.iter().map(|turn| turn["role"].clone()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
        assert_eq!(
            turns[3]["content"],
            json!([
                {"type": "tool_use", "id": "toolu_1", "name": "control", "input": {"action": "turn_off"}},
                {"type": "tool_use", "id": "toolu_2", "name": "control", "input": {"action": "turn_on"}},
            ])
        );
        assert_eq!(
            turns[4]["content"],
            json!([
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Porch is off."},
                {"type": "tool_result", "tool_use_id": "toolu_2", "content": "Rejected: unknown entity."},
            ])
        );
    }

    #[test]
    fn tools_use_input_schemas() {
        let tools = json!([{"type": "function", "function": {
            "name": "get_states", "description": "Read states.",
            "parameters": {"type": "object", "properties": {}}
        }}]);
        assert_eq!(
            tool_definitions(&tools),
            [json!({"name": "get_states", "description": "Read states.",
                "input_schema": {"type": "object", "properties": {}}})]
        );
    }

    #[test]
    fn replies_keep_text_and_skip_thinking() {
        let body = json!({"content": [
            {"type": "thinking", "thinking": "internal"},
            {"type": "text", "text": "The porch light is off."}
        ], "usage": {"input_tokens": 10, "output_tokens": 5}});
        let (message, usage) = parse(&body).unwrap();
        assert_eq!(message.content, "The porch light is off.");
        assert!(message.tool_calls.is_empty());
        assert_eq!(
            usage,
            Usage {
                input: 10,
                cached: 0,
                output: 5
            }
        );
    }
}
