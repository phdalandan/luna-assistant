//! OpenAI Chat Completions. Luna's messages and tools already use this format.
use serde_json::{Value, json};

use super::{Usage, add_request_fields, count};
use crate::inference::{ChatMessage, MAX_RESPONSE_TOKENS};
use crate::models::CloudModel;

pub fn body(model: &CloudModel, messages: &[ChatMessage], tools: &Value) -> Value {
    let mut body = json!({
        "model": model.id,
        "messages": messages.iter().map(ChatMessage::to_wire).collect::<Vec<_>>(),
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "max_completion_tokens": MAX_RESPONSE_TOKENS,
        "store": false,
    });
    add_request_fields(&mut body, &model.request);
    body
}

pub fn parse(body: &Value) -> Result<(ChatMessage, Usage), String> {
    let message = ChatMessage::from_wire(&body["choices"][0]["message"])
        .map_err(|error| error.to_string())?;
    let usage = &body["usage"];
    Ok((
        message,
        Usage {
            input: count(&usage["prompt_tokens"]),
            cached: count(&usage["prompt_tokens_details"]["cached_tokens"]),
            output: count(&usage["completion_tokens"]),
        },
    ))
}
