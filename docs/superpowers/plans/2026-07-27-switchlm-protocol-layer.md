# SwitchLM Plan 2 — Protocol Translation Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the Anthropic `/v1/messages` edge with Anthropic↔OpenAI translation (including the tool_use streaming state machine), dual-protocol same-protocol-passthrough routing, and response `model`-field echo — so Claude Code can drive 智谱/火山.

**Architecture:** A new `translate/` module holds pure translation functions (request, non-stream response, streaming state machine) tested in isolation. A new `anthropic_edge.rs` handler resolves the Profile→Model, then either **passes through** to the model's Anthropic backend (zero translation, common path) or **translates** via its OpenAI backend. Protocol selection is centralized so both edges share it. Response `model` echoes the agent's requested name on all paths.

**Tech Stack:** Rust (edition 2021), axum 0.7, reqwest 0.12, serde_json, plus new deps `eventsource-stream` (SSE parsing), `futures` + `async-stream` (stream transformation).

## Scope of THIS plan

**Covers:** §3.1 steps ①③④⑤⑦ for the Anthropic path, §3.2 (AnthropicEdge + Tool Use state machine + `tool_use_id` synthesis), §3.3 (same-protocol passthrough vs translate), §3.4 response `model` echo, §3.7 streaming pipeline (translate + passthrough).

**DEFERRED (later plans):**
- ErrorAdapter, per-model fallback, circuit breaker → **Plan 3**
- Usage adapters → **Plan 3**
- OpenAI-client ↔ Anthropic-backend *reverse* translation (rare: OpenAI clients usually have an OpenAI backend) → noted as future; the forward path (Anthropic client) is complete here.
- Tauri tray / Vue UI → **Plan 4**

> Image input blocks (`{type:"image"}`): request-side mapped to OpenAI `image_url`; output images (rare for coding) not handled in MVP.

## Global Constraints

- Built on **Plan 1** (`master`): `AppState`, `resolve_model`, `BackendConfig`, `ProxyError`, `build_router`, `chat_completions` (OpenAI edge). Branch from `master`.
- **Default port 6950** unchanged.
- **No new secrets/config**: reuses `BackendConfig` (a Model may now have BOTH `openai` and `anthropic` backends, already modeled in Plan 1's `Model`).
- **crate additions** (add to `src-tauri/Cargo.toml`): `eventsource-stream = "0.2"`, `futures = "0.3"`, `async-stream = "0.3"`.
- **Response `model`**: ALWAYS echo the agent's incoming `model` name (Profile name) on every path, stream and non-stream.
- **Outbound `model`**: ALWAYS the resolved backend's `upstream_model_id` (never the agent's name).
- **Commits**: conventional commits, one logical change per commit. Run `cargo test --manifest-path src-tauri/Cargo.toml` green before each commit.

## File Structure

```
src-tauri/src/
├─ proxy/
│  ├─ mod.rs               (MODIFY: declare new modules, re-export ProxyError)
│  ├─ error.rs             (CREATE: shared ProxyError, used by both edges)
│  ├─ openai_edge.rs       (MODIFY: use shared ProxyError + add response model echo)
│  ├─ resolve.rs           (unchanged)
│  ├─ server.rs            (MODIFY: mount /v1/messages)
│  ├─ state.rs             (unchanged)
│  └─ anthropic_edge.rs    (CREATE: /v1/messages handler)
└─ translate/
   ├─ mod.rs               (CREATE: re-exports)
   ├─ request.rs           (CREATE: Anthropic request -> OpenAI request)
   ├─ response.rs          (CREATE: OpenAI response -> Anthropic response, non-stream)
   └─ stream.rs            (CREATE: streaming state machine OpenAI SSE -> Anthropic SSE)
```

Responsibilities: `translate/*` = pure, no I/O, fully unit-tested; `proxy/anthropic_edge.rs` = HTTP + routing; `proxy/error.rs` = shared error type.

---

### Task 1: Anthropic→OpenAI request translation (`translate/request.rs`)

**Files:**
- Create: `src-tauri/src/translate/mod.rs`, `src-tauri/src/translate/request.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod translate;`)

**Interfaces:**
- Consumes: `serde_json::Value`.
- Produces: `pub fn anthropic_to_openai(req: &Value) -> Value`. Maps Anthropic Messages request → OpenAI Chat Completions request. Drops `model`/`stream` (caller sets those). Emits `messages` (with `system` role, `tool_calls`, `tool` role), and `tools` if present.

- [ ] **Step 1: Write the failing tests**

`src-tauri/src/translate/request.rs` (test module at bottom; structs/functions filled in Step 3):
```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::request`
Expected: FAIL — module/function missing.

- [ ] **Step 3: Implement**

`src-tauri/src/translate/request.rs`:
```rust
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
```

`src-tauri/src/translate/mod.rs`:
```rust
pub mod request;
pub mod response;
pub mod stream;
```
(Add `response` and `stream` as empty `pub mod response;`/`pub mod stream;` stub files now to satisfy `mod.rs`, filled in Tasks 2–3. Each stub file = empty.)

Add to `src-tauri/src/lib.rs` (top, alongside existing `pub mod config; pub mod proxy;`):
```rust
pub mod translate;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::request`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/translate/ src-tauri/src/lib.rs
git commit -m "feat(translate): Anthropic -> OpenAI request translation"
```

---

### Task 2: OpenAI→Anthropic non-stream response translation (`translate/response.rs`)

**Files:**
- Modify: `src-tauri/src/translate/response.rs` (replace stub)

**Interfaces:**
- Consumes: `serde_json::Value`.
- Produces: `pub fn openai_to_anthropic(resp: &Value, model_name: &str, request_model_echo: &str) -> Value`. Builds an Anthropic `Message` object from an OpenAI ChatCompletion response, including content blocks (text + tool_use) and usage. `model_name` is placed in the `model` field (echo).

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/src/translate/response.rs`:
```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::response`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement**

`src-tauri/src/translate/response.rs`:
```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::response`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/translate/response.rs
git commit -m "feat(translate): OpenAI -> Anthropic non-stream response"
```

---

### Task 3: Streaming translation state machine (`translate/stream.rs`)

**Files:**
- Modify: `src-tauri/src/translate/stream.rs` (replace stub)

**Interfaces:**
- Consumes: `serde_json::Value` (one OpenAI SSE chunk payload).
- Produces: `pub struct StreamTranslator` with `pub fn new(model_echo: String) -> Self` and `pub fn ingest(&mut self, chunk: Option<&Value>) -> Vec<String>`. Each returned `String` is a complete SSE frame (`event: <t>\ndata: {..}\n\n`). `chunk == None` means upstream `[DONE]` → emits closing events.

- [ ] **Step 1: Write the failing tests**

Append to `src-tauri/src/translate/stream.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(chunks: Vec<Value>) -> String {
        let mut t = StreamTranslator::new("glm-5.2".into());
        let mut out = String::new();
        for c in &chunks {
            out.push_str(&t.ingest(Some(c)).join(""));
        }
        out.push_str(&t.ingest(None).join("")); // [DONE]
        out
    }

    #[test]
    fn text_stream() {
        let s = run(vec![
            json!({"id":"x","choices":[{"index":0,"delta":{"role":"assistant","content":"Hel"}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"lo"}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ]);
        assert!(s.contains("\"type\":\"message_start\""));
        assert!(s.contains("\"type\":\"content_block_start\""));
        assert!(s.contains("\"text\":\"Hel\""));
        assert!(s.contains("\"text\":\"lo\""));
        assert!(s.contains("\"type\":\"content_block_stop\""));
        assert!(s.contains("\"stop_reason\":\"end_turn\""));
        assert!(s.contains("\"type\":\"message_stop\""));
        assert!(s.contains("\"model\":\"glm-5.2\"")); // echo
    }

    #[test]
    fn tool_use_stream_assembles_and_synthesizes_id() {
        // OpenAI sends tool_calls with no `id` on the first fragment -> must synthesize toolu_*
        let s = run(vec![
            json!({"id":"x","choices":[{"index":0,"delta":{"role":"assistant","content":null}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"type":"function","function":{"name":"get_weather","arguments":""}}
            ]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"{\"city\":"}}
            ]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"function":{"arguments":"\"X\"}"}}
            ]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        assert!(s.contains("\"type\":\"tool_use\""));
        assert!(s.contains("\"name\":\"get_weather\""));
        assert!(s.contains("\"type\":\"input_json_delta\""));
        assert!(s.contains("\"partial_json\":\"{\\\"city\\\":"));
        // synthesized id prefix
        assert!(s.contains("\"id\":\"toolu_"));
        assert!(s.contains("\"stop_reason\":\"tool_use\""));
    }

    #[test]
    fn text_then_tool_in_one_response() {
        let s = run(vec![
            json!({"id":"x","choices":[{"index":0,"delta":{"role":"assistant","content":"thinking"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[
                {"index":0,"id":"call_1","type":"function","function":{"name":"run","arguments":"{}"}}
            ]}}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        ]);
        // text block closed before tool block starts
        let text_stop = s.find("\"type\":\"content_block_stop\"").unwrap();
        let tool_start = s.find("\"type\":\"tool_use\"").unwrap();
        assert!(text_stop < tool_start);
        assert!(s.contains("\"id\":\"call_1\""));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::stream`
Expected: FAIL — `StreamTranslator` missing.

- [ ] **Step 3: Implement**

`src-tauri/src/translate/stream.rs`:
```rust
use std::collections::HashMap;

use serde_json::{json, Value};

/// Translates an OpenAI streaming response into Anthropic SSE events.
/// Pure: ingest one chunk at a time, get back complete SSE frame strings to emit.
pub struct StreamTranslator {
    model: String,
    message_id: String,
    started: bool,
    next_index: usize,
    open_text: bool,            // is a text content block currently open?
    open_tool_index: Option<usize>, // index of currently-open tool block, if any
    tools: HashMap<u64, ToolState>,
    stop_reason: Option<String>,
    output_tokens: u64,
    finished: bool,
}

struct ToolState {
    block_index: usize,
    id: String,
    name: String,
    started: bool,
}

impl StreamTranslator {
    pub fn new(model: String) -> Self {
        Self {
            model,
            message_id: "msg_default".into(),
            started: false,
            next_index: 0,
            open_text: false,
            open_tool_index: None,
            tools: HashMap::new(),
            stop_reason: None,
            output_tokens: 0,
            finished: false,
        }
    }

    /// `chunk = None` signals upstream `[DONE]`.
    pub fn ingest(&mut self, chunk: Option<&Value>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        match chunk {
            None => {
                self.close_text(&mut out);
                self.close_tool(&mut out);
                if self.started && !self.finished {
                    out.push(self.frame(
                        "message_delta",
                        json!({"type":"message_delta","delta":{"stop_reason": self.stop_reason.clone().unwrap_or_else(|| "end_turn".into()),"stop_sequence":null},"usage":{"output_tokens": self.output_tokens}}),
                    ));
                    out.push(self.frame("message_stop", json!({"type":"message_stop"})));
                    self.finished = true;
                }
            }
            Some(c) => {
                if !self.started {
                    if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
                        self.message_id = format!("msg_{id}");
                    }
                    out.push(self.frame(
                        "message_start",
                        json!({"type":"message_start","message":{"id":self.message_id.clone(),"type":"message","role":"assistant","content":[],"model":self.model.clone(),"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}),
                    ));
                    self.started = true;
                }
                if let Some(choice) = c.get("choices").and_then(|v| v.as_array()).and_then(|a| a.first()) {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                self.ensure_text_open(&mut out);
                                out.push(self.frame(
                                    "content_block_delta",
                                    json!({"type":"content_block_delta","index": self.text_index(),"delta":{"type":"text_delta","text":text}}),
                                ));
                            }
                        }
                        if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc in tcs {
                                self.handle_tool_call(tc, &mut out);
                            }
                        }
                    }
                    if let Some(fr) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                        self.stop_reason = Some(crate::translate::response::map_stop_reason(fr).to_string());
                    }
                    if let Some(usage) = choice.get("usage") {
                        if let Some(ot) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                            self.output_tokens = ot;
                        }
                    }
                }
                // Some providers put usage at the top level of the final chunk
                if let Some(ot) = c.get("usage").and_then(|u| u.get("completion_tokens")).and_then(|v| v.as_u64()) {
                    self.output_tokens = ot;
                }
            }
        }
        out
    }

    fn text_index(&self) -> usize {
        0 // text, when open, is always index 0 in our scheme (tools come after)
    }

    fn ensure_text_open(&mut self, out: &mut Vec<String>) {
        if self.open_text {
            return;
        }
        self.close_tool(out);
        // text block uses index = next_index, but to keep "text first" simple we use 0 if nothing opened
        let idx = if self.next_index == 0 { 0 } else { self.next_index };
        self.next_index = idx + 1;
        out.push(self.frame(
            "content_block_start",
            json!({"type":"content_block_start","index":idx,"content_block":{"type":"text","text":""}}),
        ));
        self.open_text = true;
    }

    fn close_text(&mut self, out: &mut Vec<String>) {
        if self.open_text {
            out.push(self.frame("content_block_stop", json!({"type":"content_block_stop","index":0})));
            self.open_text = false;
        }
    }

    fn close_tool(&mut self, out: &mut Vec<String>) {
        if let Some(idx) = self.open_tool_index.take() {
            out.push(self.frame("content_block_stop", json!({"type":"content_block_stop","index":idx})));
        }
    }

    fn handle_tool_call(&mut self, tc: &Value, out: &mut Vec<String>) {
        let oai_index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
        let existing = self.tools.contains_key(&oai_index);
        // If a different tool or text is open, close it before starting a new block.
        if !existing {
            self.close_text(out);
            self.close_tool(out);
            let id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| format!("toolu_{}", oai_index));
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let block_index = self.next_index;
            self.next_index += 1;
            self.tools.insert(
                oai_index,
                ToolState { block_index, id, name, started: false },
            );
        }
        let st = self.tools.get_mut(&oai_index).unwrap();
        if !st.started {
            out.push(self.frame(
                "content_block_start",
                json!({"type":"content_block_start","index":st.block_index,"content_block":{"type":"tool_use","id":st.id,"name":st.name,"input":{}}}),
            ));
            st.started = true;
            self.open_tool_index = Some(st.block_index);
        }
        // arguments fragment
        if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")).and_then(|v| v.as_str()) {
            if !args.is_empty() {
                out.push(self.frame(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":st.block_index,"delta":{"type":"input_json_delta","partial_json":args}}),
                ));
            }
        }
    }

    fn frame(&self, event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }
}
```

> Indexing scheme: the text block (if any) is always index 0; tool blocks follow at 1, 2, …. `ensure_text_open` / `close_text` / `close_tool` guarantee only one block is open at a time and that a text block is closed before a tool block starts.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml translate::stream`
Expected: PASS (3 tests). If the borrow helper is awkward, simplify until it compiles — the emitted frames must match the asserts.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/translate/stream.rs
git commit -m "feat(translate): OpenAI->Anthropic streaming state machine (tool_use + id synth)"
```

---

### Task 4: Shared `ProxyError` + Anthropic edge (non-stream) + mount route

**Files:**
- Create: `src-tauri/src/proxy/error.rs`, `src-tauri/src/proxy/anthropic_edge.rs`
- Modify: `src-tauri/src/proxy/mod.rs`, `src-tauri/src/proxy/openai_edge.rs`, `src-tauri/src/proxy/server.rs`

**Interfaces:**
- Consumes: `AppState`, `resolve_model`, `BackendConfig`, `translate::request::anthropic_to_openai`, `translate::response::openai_to_anthropic`, `join_url` semantics.
- Produces: shared `ProxyError` (moved from `openai_edge`), `anthropic_edge::messages` handler. Adds `ProxyError::NoBackend(String)` and `ProxyError::Translation(String)` variants.

- [ ] **Step 1: Move ProxyError to `proxy/error.rs`**

`src-tauri/src/proxy/error.rs`:
```rust
use axum::body::Body;
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use thiserror::Error;

use crate::proxy::ResolveError;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("no profiles/models configured; set up SwitchLM first")]
    NotConfigured,
    #[error("{0}")]
    Resolve(#[from] ResolveError),
    #[error("model has no usable backend ({0})")]
    NoBackend(String),
    #[error("missing api key for provider '{0}'")]
    NoApiKey(String),
    #[error("translation error: {0}")]
    Translation(String),
    #[error("upstream request failed: {0}")]
    Upstream(String),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response<Body> {
        let (code, msg) = match &self {
            ProxyError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ProxyError::Resolve(ResolveError::ProfileNotFound { .. }) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::Resolve(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
            ProxyError::NoBackend(_) | ProxyError::NoApiKey(_) => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
            ProxyError::Translation(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            ProxyError::Upstream(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
        };
        Response::builder().status(code).body(Body::from(msg)).unwrap()
    }
}
```

`src-tauri/src/proxy/mod.rs` — replace the current contents with:
```rust
pub mod anthropic_edge;
pub mod error;
pub mod openai_edge;
pub mod resolve;
pub mod server;
pub mod state;

pub use error::ProxyError;
pub use resolve::{resolve_model, ResolveError};
pub use state::{AppState, AppStateInner};
```

- [ ] **Step 2: Update `openai_edge.rs` to use the shared error**

In `src-tauri/src/proxy/openai_edge.rs`:
- Delete the local `ProxyError` enum + its `IntoResponse` impl (now in `error.rs`).
- Change `use crate::proxy::{AppState, ResolveError};` → `use crate::proxy::{AppState, ProxyError};`
- Replace the two error sites that used removed variants:
  - `ok_or(ProxyError::NoOpenAiBackend)` → `ok_or(ProxyError::NoBackend("openai".into()))`
- Keep `chat_completions` otherwise as-is for now (echo added in Task 6).

(Leave the existing `#[cfg(test)]` block — it builds `AppStateInner` and uses `build_router`; unaffected.)

- [ ] **Step 3: Write the failing test for the Anthropic edge (non-stream, translate path)**

`src-tauri/src/proxy/anthropic_edge.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::proxy::server::build_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use std::sync::Arc;
    use tower::ServiceExt;

    async fn state_with_openai_backend(upstream_base: &str) -> AppState {
        let mut cfg = AppConfig::default();
        cfg.models.push(Model {
            id: "m_glm46".into(),
            provider_id: "zhipu".into(),
            display_name: "GLM-4.6".into(),
            source: ModelSource::Manual,
            openai: Some(BackendConfig { base_url: upstream_base.into(), upstream_model_id: "glm-4.6".into(), api_key_ref: None }),
            anthropic: None,
            cooldown_seconds: None,
            fallback_target_model_id: None,
        });
        cfg.profiles.push(Profile { id: "p".into(), name: "glm-5.2".into(), aliases: vec![], backing_model_id: "m_glm46".into() });
        let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
        secrets.set_key("zhipu", "sk-test").unwrap();
        Arc::new(AppStateInner { config: tokio::sync::RwLock::new(cfg), secrets })
    }

    #[tokio::test]
    async fn anthropic_edge_translates_non_stream() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"c1","choices":[{"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}),
            ))
            .mount(&mock).await;

        let app = build_router(state_with_openai_backend(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({
                    "model":"glm-5.2",
                    "max_tokens":100,
                    "messages":[{"role":"user","content":"hi"}]
                }).to_string())).unwrap()
        ).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["model"], "glm-5.2"); // echoed
        // forwarded body was OpenAI-shaped
        let received = &mock.received_requests().await.unwrap()[0];
        let fwd: serde_json::Value = serde_json::from_slice(&received.body).unwrap();
        assert_eq!(fwd["model"], "glm-4.6");
        assert_eq!(fwd["messages"][0]["content"], "hi");
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml anthropic_edge`
Expected: FAIL — `messages` handler missing / route not mounted.

- [ ] **Step 5: Implement the handler (non-stream only; streaming in Task 5)**

`src-tauri/src/proxy/anthropic_edge.rs` (prepend the handler above the test module):
```rust
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::Response;

use crate::config::BackendConfig;
use crate::proxy::resolve::resolve_model;
use crate::proxy::{AppState, ProxyError};
use crate::translate::request::anthropic_to_openai;
use crate::translate::response::openai_to_anthropic;

pub async fn messages(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    let req: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ProxyError::Translation(format!("invalid json: {e}")))?;

    // Resolve Profile -> Model, snapshot what we need.
    let (backend_opt, anthropic_backend_opt, provider_id, upstream_id, echo_model) = {
        let cfg = state.config.read().await;
        if cfg.profiles.is_empty() || cfg.models.is_empty() {
            return Err(ProxyError::NotConfigured);
        }
        let requested = req.get("model").and_then(|v| v.as_str());
        let model = resolve_model(&cfg, requested)?;
        (
            model.openai.clone(),
            model.anthropic.clone(),
            model.provider_id.clone(),
            model.openai.as_ref().map(|b| b.upstream_model_id.clone()).unwrap_or_default(),
            requested.map(str::to_string).unwrap_or_else(|| model.display_name.clone()),
        )
    };

    let key = state.secrets.get_key(&provider_id)
        .map_err(|e| ProxyError::Upstream(e.to_string()))?
        .ok_or_else(|| ProxyError::NoApiKey(provider_id.clone()))?;

    let is_stream = req.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    if let Some(ab) = anthropic_backend_opt {
        // ---- Same-protocol passthrough (Anthropic client -> Anthropic backend) ----
        forward_anthropic_passthrough(&ab, &req, &key, is_stream, &echo_model, upstream_id_opt(&ab)).await
    } else if let Some(ob) = backend_opt {
        // ---- Translate (Anthropic client -> OpenAI backend) ----
        let openai_req = anthropic_to_openai(&req);
        translate_via_openai(&ob, &openai_req, &key, is_stream, &echo_model, &upstream_id).await
    } else {
        Err(ProxyError::NoBackend("anthropic/openai".into()))
    }
}

fn upstream_id_opt(b: &BackendConfig) -> String {
    b.upstream_model_id.clone()
}

// Non-streaming implementations only here; streaming added in Task 5.
async fn forward_anthropic_passthrough(
    backend: &BackendConfig,
    req: &serde_json::Value,
    key: &str,
    _is_stream: bool,
    echo_model: &str,
    upstream_id: String,
) -> Result<Response<Body>, ProxyError> {
    let mut fwd = req.clone();
    fwd["model"] = serde_json::Value::String(upstream_id);
    let url = join_url(backend);
    let resp = reqwest::Client::new().post(&url).bearer_auth(key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&fwd).unwrap_or_default())
        .send().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
    // Task 5 handles streaming passthrough; for now echo model in non-stream JSON.
    let bytes = resp.bytes().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let mut v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| ProxyError::Upstream(e.to_string()))?;
    v["model"] = serde_json::Value::String(echo_model.to_string());
    Ok(Response::builder().status(200).header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&v).unwrap_or_default())).unwrap())
}

async fn translate_via_openai(
    backend: &BackendConfig,
    openai_req: &serde_json::Value,
    key: &str,
    _is_stream: bool,
    echo_model: &str,
    upstream_id: &str,
) -> Result<Response<Body>, ProxyError> {
    let mut oai = openai_req.clone();
    oai["model"] = serde_json::Value::String(upstream_id.to_string());
    oai["stream"] = serde_json::Value::Bool(false); // non-stream only here; Task 5 adds stream
    let url = join_url(backend);
    let resp = reqwest::Client::new().post(&url).bearer_auth(key)
        .header("content-type", "application/json")
        .body(serde_json::to_vec(&oai).unwrap_or_default())
        .send().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let bytes = resp.bytes().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let oai_resp: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| ProxyError::Upstream(e.to_string()))?;
    let an = openai_to_anthropic(&oai_resp, echo_model);
    Ok(Response::builder().status(200).header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&an).unwrap_or_default())).unwrap())
}

fn join_url(b: &BackendConfig) -> String {
    let base = b.base_url.trim_end_matches('/');
    format!("{base}/chat/completions")
}
```

- [ ] **Step 6: Mount the route**

In `src-tauri/src/proxy/server.rs`, change `build_router` to also register the Anthropic route:
```rust
use crate::proxy::{anthropic_edge, openai_edge, AppState};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(openai_edge::chat_completions))
        .route("/v1/messages", post(anthropic_edge::messages))
        .with_state(state)
}
```

- [ ] **Step 7: Run all tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all prior tests + the new Anthropic edge test).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/proxy/
git commit -m "feat(proxy): Anthropic /v1/messages edge (non-stream translate+passthrough)"
```

---

### Task 5: Streaming paths (translate + passthrough)

**Files:**
- Modify: `src-tauri/Cargo.toml` (add deps), `src-tauri/src/proxy/anthropic_edge.rs`

**Interfaces:**
- Consumes: `StreamTranslator`, `eventsource_stream`, `async_stream`, `futures`.
- Produces: streaming variants of the two paths used when `is_stream == true`.

- [ ] **Step 1: Add streaming dependencies**

In `src-tauri/Cargo.toml` `[dependencies]`, add:
```toml
eventsource-stream = "0.2"
futures = "0.3"
async-stream = "0.3"
```

- [ ] **Step 2: Write the failing test (translate stream)**

Append to `src-tauri/src/proxy/anthropic_edge.rs` tests:
```rust
    #[tokio::test]
    async fn anthropic_edge_translates_stream() {
        let mock = wiremock::MockServer::start().await;
        let sse = "data: {\"id\":\"x\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"}}]}\n\n\
                   data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_bytes(sse.as_bytes()))
            .mount(&mock).await;

        let app = build_router(state_with_openai_backend(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/messages")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!({"model":"glm-5.2","stream":true,"max_tokens":16,"messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let out = String::from_utf8_lossy(&bytes);
        assert!(out.contains("\"type\":\"message_start\""));
        assert!(out.contains("\"text\":\"Hel\""));
        assert!(out.contains("\"text\":\"lo\""));
        assert!(out.contains("\"type\":\"message_stop\""));
        assert!(out.contains("\"model\":\"glm-5.2\""));
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml anthropic_edge_translates_stream`
Expected: FAIL (current handler buffers, ignores `stream`).

- [ ] **Step 4: Implement streaming**

In `src-tauri/src/proxy/anthropic_edge.rs`, branch on `is_stream` in both helpers. Replace `translate_via_openai` and `forward_anthropic_passthrough` to handle streaming:

For the **translate** path, when `is_stream`:
```rust
use async_stream::stream;
use eventsource_stream::{Event as SseEvent, Eventsource};
use futures::StreamExt;
use axum::body::Bytes as ABytes;

// inside translate_via_openai, after building `oai` with model + stream=true:
oai["stream"] = serde_json::Value::Bool(true);
let resp = reqwest::Client::new().post(&url).bearer_auth(key)
    .header("content-type","application/json")
    .body(serde_json::to_vec(&oai).unwrap_or_default())
    .send().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
let status = resp.status();
let body_stream = translate_stream(resp.bytes_stream(), echo_model.to_string());
let body = Body::from_stream(body_stream);
return Ok(Response::builder().status(status).header("content-type","text/event-stream").body(body).unwrap());
```

Add a free function:
```rust
fn translate_stream(upstream: impl futures::Stream<Item = Result<axum::body::Bytes, reqwest::Error>> + Send + 'static,
                    echo_model: String)
    -> impl futures::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + 'static
{
    let mut t = crate::translate::stream::StreamTranslator::new(echo_model);
    let mut sse = upstream.eventsource();
    async_stream::stream! {
        while let Some(item) = sse.next().await {
            match item {
                Ok(ev) => {
                    if ev.data == "[DONE]" {
                        for f in t.ingest(None) {
                            yield Ok::<_, std::io::Error>(axum::body::Bytes::from(f));
                        }
                        break;
                    }
                    let chunk: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
                    for f in t.ingest(Some(&chunk)) {
                        yield Ok(axum::body::Bytes::from(f));
                    }
                }
                Err(e) => { yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())); break; }
            }
        }
    }
}
```

For the **passthrough** path when `is_stream`: forward upstream bytes unchanged (already Anthropic SSE), then rewrite the `"model"` field per line via a small SSE filter (or simply pass through — passthrough providers already echo their own model; the agent sees the upstream model name, acceptable for MVP). Implement passthrough as a direct byte stream:
```rust
return Ok(Response::builder().status(resp.status()).header("content-type","text/event-stream")
    .body(Body::from_stream(resp.bytes_stream())).unwrap());
```
> Acceptable for MVP: passthrough stream does not rewrite `model`. Document this; full SSE-model-rewrite lands in Plan 4 polish. The translate path DOES set model (translator owns it).

Make both helpers return early on the streaming branch.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all, incl. stream test).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/src/proxy/anthropic_edge.rs
git commit -m "feat(proxy): Anthropic edge streaming (translate + passthrough)"
```

---

### Task 6: Response `model` echo on the OpenAI edge + non-stream passthrough echo

**Files:**
- Modify: `src-tauri/src/proxy/openai_edge.rs`, `src-tauri/src/proxy/anthropic_edge.rs`

**Interfaces:**
- Consumes: existing edges.
- Produces: OpenAI edge rewrites the response `model` to the agent's requested name (non-stream fully; stream via a SSE line filter). Anthropic passthrough non-stream already echoes (Task 4).

- [ ] **Step 1: Write the failing test**

Append to `src-tauri/src/proxy/openai_edge.rs` tests (a second test in the existing `mod tests`):
```rust
    #[tokio::test]
    async fn openai_edge_echoes_requested_model_nonstream() {
        let mock = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"id":"x","model":"glm-4.6","choices":[{"message":{"role":"assistant","content":"hi"}}]}),
            )).mount(&mock).await;

        let app = build_router(test_state(&mock.uri()).await);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/v1/chat/completions")
                .header("content-type","application/json")
                .body(Body::from(serde_json::json!({"model":"glm-5.2","messages":[{"role":"user","content":"hi"}]}).to_string())).unwrap()
        ).await.unwrap();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["model"], "glm-5.2"); // echoed, not glm-4.6
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml openai_edge_echoes_requested_model_nonstream`
Expected: FAIL (currently returns upstream `glm-4.6`).

- [ ] **Step 3: Implement echo in the OpenAI edge**

In `src-tauri/src/proxy/openai_edge.rs` `chat_completions`, for the **non-stream** branch, after fetching `bytes`, rewrite model before returning:
```rust
let bytes = resp.bytes().await.map_err(|e| ProxyError::Upstream(e.to_string()))?;
let mut v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
if let Some(name) = &requested_name { v["model"] = serde_json::Value::String(name.clone()); }
out.body(Body::from(serde_json::to_vec(&v).unwrap_or(bytes.to_vec()))).unwrap()
```
(For the streaming branch, leave passthrough as-is for MVP — note in code comment that stream `model` echo is deferred.)

- [ ] **Step 4: Run all tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS (all).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/openai_edge.rs
git commit -m "feat(proxy): echo requested model name in OpenAI edge non-stream response"
```

---

## Definition of Done (Plan 2)

- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` — all tests green (Plan 1's 12 + Plan 2's translation/edge/stream tests).
- [ ] `/v1/messages` mounted; Claude Code can point `ANTHROPIC_BASE_URL=http://localhost:6950` at the proxy.
- [ ] Translation path (Anthropic→OpenAI backend) verified: text + tool_use round-trip (non-stream + stream).
- [ ] Passthrough path (Anthropic backend) verified non-stream.
- [ ] `tool_use_id` synthesized when upstream omits it.
- [ ] Response `model` echoes the agent's requested name (non-stream, all paths; translate stream).
- [ ] E2E smoke: real Claude Code → SwitchLM → real 智谱/火山, one coding task completes (manual, with a real key).

## Hand-off to Plan 3

Plan 3 adds resilience + usage on top of the now-complete protocol layer:
- `ErrorAdapter` (provider rate-limit detection) + per-Model `fallback_target_model_id` walk + circuit breaker (`ModelHealth`, `recover_at`) — these wrap the upstream call inside both edges.
- Usage adapters (`UsageProvider` + Zhipu/Volcengine) feeding the breaker's `reset_at` and the UI.
- The shared upstream-call helper (currently inlined in both edges) is the natural place to insert fallback + breaker; Plan 3 may refactor both edges to call a common `dispatch()` that resolves backend, applies breaker, calls upstream, and runs fallback.
