use std::collections::HashMap;

use serde_json::{json, Value};

/// Translates an OpenAI streaming response into Anthropic SSE events.
/// Pure: ingest one chunk at a time, get back complete SSE frame strings to emit.
pub struct StreamTranslator {
    model: String,
    message_id: String,
    started: bool,
    next_index: usize,
    open_text: bool,                // is a text content block currently open?
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
                if let Some(choice) =
                    c.get("choices").and_then(|v| v.as_array()).and_then(|a| a.first())
                {
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
                        self.stop_reason =
                            Some(crate::translate::response::map_stop_reason(fr).to_string());
                    }
                    if let Some(usage) = choice.get("usage") {
                        if let Some(ot) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                            self.output_tokens = ot;
                        }
                    }
                }
                // Some providers put usage at the top level of the final chunk
                if let Some(ot) = c
                    .get("usage")
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|v| v.as_u64())
                {
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
            out.push(self.frame(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ));
            self.open_text = false;
        }
    }

    fn close_tool(&mut self, out: &mut Vec<String>) {
        if let Some(idx) = self.open_tool_index.take() {
            out.push(self.frame(
                "content_block_stop",
                json!({"type":"content_block_stop","index":idx}),
            ));
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
                ToolState {
                    block_index,
                    id,
                    name,
                    started: false,
                },
            );
        }
        // Read all needed fields from `st` into locals up front so the mutable
        // borrow of `self.tools` ends before we borrow `self` for `frame`
        // (E0502 avoidance). Emitted frames are byte-identical to the brief.
        let st = self.tools.get_mut(&oai_index).unwrap();
        let block_index = st.block_index;
        let id = st.id.clone();
        let name = st.name.clone();
        let was_started = st.started;
        st.started = true;
        if !was_started {
            self.open_tool_index = Some(block_index);
            out.push(self.frame(
                "content_block_start",
                json!({"type":"content_block_start","index":block_index,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
            ));
        }
        // arguments fragment
        if let Some(args) = tc
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|v| v.as_str())
        {
            if !args.is_empty() {
                out.push(self.frame(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":block_index,"delta":{"type":"input_json_delta","partial_json":args}}),
                ));
            }
        }
    }

    fn frame(&self, event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }
}

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
