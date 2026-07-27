use serde_json::{json, Value};

/// Build an Anthropic Message from an OpenAI ChatCompletion (non-streaming).
/// `model_name` is echoed into the response's `model` field.
pub fn openai_to_anthropic(resp: &Value, model_name: &str) -> Value {
    let choice = resp.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first());
    let message = choice.and_then(|c| c.get("message")).cloned().unwrap_or(json!({}));
    let finish = choice
        .and_then(|c| c.get("finish_reason").and_then(|v| v.as_str()))
        .unwrap_or("stop");

    let mut content: Vec<Value> = Vec::new();
    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            content.push(json!({"type":"text","text":text}));
        }
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tool_calls {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let fn_ = tc.get("function").cloned().unwrap_or(json!({}));
            let name = fn_.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let input_str = fn_.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
            let input: Value = serde_json::from_str(input_str).unwrap_or(json!({}));
            content.push(json!({"type":"tool_use","id":id,"name":name,"input":input}));
        }
    }
    if content.is_empty() {
        content.push(json!({"type":"text","text":""}));
    }

    let usage = resp.get("usage").cloned().unwrap_or(json!({}));
    let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let id = resp.get("id").and_then(|v| v.as_str()).unwrap_or("msg").to_string();

    json!({
        "id": format!("msg_{id}"),
        "type": "message",
        "role": "assistant",
        "model": model_name,
        "content": content,
        "stop_reason": map_stop_reason(finish),
        "stop_sequence": null,
        "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
    })
}

pub fn map_stop_reason(finish: &str) -> &'static str {
    match finish {
        "stop" => "end_turn",
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "end_turn",
        _ => "end_turn",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_text_response() {
        let oai = json!({
            "id":"chatcmpl-1","choices":[{"message":{"role":"assistant","content":"hello"}}],
            "usage":{"prompt_tokens":3,"completion_tokens":2}
        });
        let an = openai_to_anthropic(&oai, "glm-5.2");
        assert_eq!(an["type"], "message");
        assert_eq!(an["role"], "assistant");
        assert_eq!(an["model"], "glm-5.2");
        assert_eq!(an["content"][0]["type"], "text");
        assert_eq!(an["content"][0]["text"], "hello");
        assert_eq!(an["stop_reason"], "end_turn");
        assert_eq!(an["usage"]["input_tokens"], 3);
        assert_eq!(an["usage"]["output_tokens"], 2);
    }

    #[test]
    fn maps_tool_calls_response() {
        let oai = json!({
            "id":"chatcmpl-2",
            "choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
                {"id":"call_9","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"X\"}"}}
            ]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":1,"completion_tokens":5}
        });
        let an = openai_to_anthropic(&oai, "glm-5.2");
        assert_eq!(an["content"][0]["type"], "tool_use");
        assert_eq!(an["content"][0]["id"], "call_9");
        assert_eq!(an["content"][0]["name"], "get_weather");
        assert_eq!(an["content"][0]["input"]["city"], "X");
        assert_eq!(an["stop_reason"], "tool_use");
    }
}
