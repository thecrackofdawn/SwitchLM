use serde_json::{json, Value};

/// Translate an Anthropic Messages request into an OpenAI Chat Completions request.
/// Does NOT set `model` or `stream` — the caller controls those.
pub fn anthropic_to_openai(req: &Value) -> Value {
    let mut messages: Vec<Value> = Vec::new();

    if let Some(system) = req.get("system") {
        let text = system_to_text(system);
        if !text.is_empty() {
            messages.push(json!({"role":"system","content":text}));
        }
    }

    if let Some(msgs) = req.get("messages").and_then(|v| v.as_array()) {
        for m in msgs {
            for oai in convert_message(m) {
                messages.push(oai);
            }
        }
    }

    let mut out = json!({ "messages": messages });

    if let Some(tools) = req.get("tools").and_then(|v| v.as_array()) {
        let oai_tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                let name = t.get("name").cloned().unwrap_or(json!(""));
                let desc = t.get("description").cloned().unwrap_or(json!(""));
                let schema = t.get("input_schema").cloned().unwrap_or(json!({}));
                json!({"type":"function","function":{"name":name,"description":desc,"parameters":schema}})
            })
            .collect();
        out["tools"] = Value::Array(oai_tools);
    }
    if let Some(choice) = req.get("tool_choice") {
        out["tool_choice"] = choice.clone(); // best-effort pass-through
    }
    out
}

fn system_to_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                (b.get("type").and_then(|v| v.as_str()) == Some("text"))
                    .then(|| b.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Convert one Anthropic message into 1+ OpenAI messages.
fn convert_message(m: &Value) -> Vec<Value> {
    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
    match m.get("content") {
        Some(Value::String(s)) => vec![json!({"role":role,"content":s})],
        Some(Value::Array(blocks)) => convert_blocks(role, blocks),
        _ => vec![json!({"role":role,"content":""})],
    }
}

fn convert_blocks(role: &str, blocks: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    let text: Vec<String> = blocks
        .iter()
        .filter_map(|b| {
            (b.get("type").and_then(|v| v.as_str()) == Some("text"))
                .then(|| b.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string())
        })
        .collect();
    let tool_calls: Vec<Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
        .map(|b| {
            let id = b.get("id").cloned().unwrap_or(json!(""));
            let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = b
                .get("input")
                .map(|i| serde_json::to_string(i).unwrap_or_default())
                .unwrap_or_default();
            json!({"id":id,"type":"function","function":{"name":name,"arguments":args}})
        })
        .collect();
    let tool_results: Vec<&Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_result"))
        .collect();

    if !tool_calls.is_empty() {
        let mut msg = json!({"role":"assistant","tool_calls":tool_calls});
        msg["content"] = if text.is_empty() { Value::Null } else { Value::String(text.join("\n")) };
        out.push(msg);
    } else if !tool_results.is_empty() {
        for tr in tool_results {
            let id = tr.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or("");
            let content = tr
                .get("content")
                .map(result_content_to_string)
                .unwrap_or_default();
            out.push(json!({"role":"tool","tool_call_id":id,"content":content}));
        }
        if !text.is_empty() {
            out.push(json!({"role":"user","content":text.join("\n")}));
        }
    } else if !text.is_empty() {
        out.push(json!({"role":role,"content":text.join("\n")}));
    } else {
        out.push(json!({"role":role,"content":""}));
    }
    out
}

fn result_content_to_string(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|v| v.as_str()).map(String::from))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_system_and_text() {
        let req = json!({
            "system": "You are helpful.",
            "messages": [
                {"role":"user","content":[{"type":"text","text":"hi"}]}
            ]
        });
        let oai = anthropic_to_openai(&req);
        let msgs = oai["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "You are helpful.");
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hi");
    }

    #[test]
    fn maps_assistant_tool_use() {
        let req = json!({
            "messages": [
                {"role":"user","content":"w?"},
                {"role":"assistant","content":[
                    {"type":"text","text":"calling"},
                    {"type":"tool_use","id":"toolu_1","name":"get_weather","input":{"city":"X"}}
                ]}
            ]
        });
        let oai = anthropic_to_openai(&req);
        let msgs = oai["messages"].as_array().unwrap();
        let asst = &msgs[1];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], "calling");
        assert_eq!(asst["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(asst["tool_calls"][0]["function"]["arguments"], json!("{\"city\":\"X\"}"));
    }

    #[test]
    fn maps_tool_result_to_tool_role() {
        let req = json!({
            "messages": [
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"toolu_1","content":"sunny"}
                ]}
            ]
        });
        let oai = anthropic_to_openai(&req);
        let msgs = oai["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "toolu_1");
        assert_eq!(msgs[0]["content"], "sunny");
    }

    #[test]
    fn maps_tools_definitions() {
        let req = json!({
            "tools":[{"name":"get_weather","description":"d","input_schema":{"type":"object","properties":{"city":{"type":"string"}}}}]
        });
        let oai = anthropic_to_openai(&req);
        assert_eq!(oai["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(oai["tools"][0]["function"]["parameters"]["properties"]["city"]["type"], "string");
    }
}
