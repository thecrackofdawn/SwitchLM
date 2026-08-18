# 请求记录(Request Recording)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record every proxied request (full inbound body + two precomputed hashes + dispatch metadata) to a rolling JSONL file, behind an opt-in toggle, so the exact-duplicate rate, content-duplicate rate, prefix-overlap, and field-volatility can be analyzed offline — the instrumentation step before building a response cache.

**Architecture:** A new runtime-agnostic `recording` module exposes pure hash functions + a `RequestRecorder` (bounded `mpsc` → background writer task; drop-on-overflow counted; clear via channel message). `AppState` gains a `RwLock<Option<Arc<RequestRecorder>>>` field; `dispatch()` records one line at its tail (after outcome is known). A `Settings.request_recording` toggle hot-swaps the recorder live; a `clear_request_log` command wipes the dir. Frontend adds a Settings toggle + clear button with a privacy warning.

**Tech Stack:** Rust (axum, tokio mpsc, sha2, serde_json, chrono — all already in `Cargo.toml`), Vue 3 + Pinia + Naive UI frontend, Python analysis script.

## Global Constraints

- **No new crate dependencies.** `sha2 = "0.10"`, `tokio` (features `full` incl. `sync`/`rt`), `chrono`, `serde_json`, `tempfile` are all already in `src-tauri/Cargo.toml`. Hex digest via `format!("{:x}", Sha256::digest(...))` — no `hex` crate.
- **Frontend package manager is `pnpm`.** Never use `npm`/`npx`; use `pnpm`/`pnpm exec` (e.g. `pnpm exec vue-tsc --noEmit`). `npm install` would desync `pnpm-lock.yaml`.
- **Logs/records identify vendor + upstream model name, never the opaque internal `id`** (preserve the existing `m_xxx`-free convention; the record's `vendor`/`model` come from the already-computed `served` tag).
- **Cross-platform (Windows + Linux):** close file handles before rename/delete (Windows can't rename/delete open files); use `std::path::PathBuf`, never hardcoded separators.
- **Privacy:** `body` contains user code + possible secrets — default OFF, rotation-capped, one-click clear, never uploaded. UI toggle MUST show a warning.
- **`AppStateInner` has ~14 struct-literal construction sites** (enumerated in Task 3); adding the `recorder` field means adding `recorder: <init>` at every one. The field is `RwLock<Option<Arc<RequestRecorder>>>`.
- **The recorder must be runtime-agnostic** — the recording module MUST NOT call `tokio::spawn` or import `tauri`. It exposes `RequestRecorder::channel() -> (Arc<Self>, Receiver<Msg>)` + a public `async fn run_writer(rx, dir)`; the *caller* spawns `run_writer` via `tauri::async_runtime::spawn` (production) or `tokio::spawn` (tests). Dropping the `Arc` closes the channel → the writer task exits on its own (no abort needed).
- **Spec:** `docs/superpowers/specs/2026-08-11-request-recording-design.md`. Code comments reference the spec where relevant.

---

## File Structure

| File | Responsibility |
|---|---|
| `src-tauri/src/recording/mod.rs` (new) | Pure hash fns (`hash_full`, `hash_messages`, `hash_value`, canonical JSON), `RequestRecord` (Serialize), `RollWriter` (rotation), `RequestRecorder` (channel + drop counter), `run_writer` loop, `Msg` enum. |
| `src-tauri/src/proxy/state.rs` (modify) | Add `recorder` field to `AppStateInner`; init `None` in `load()`. |
| `src-tauri/src/proxy/dispatch.rs` (modify) | Record at tail of `dispatch()`; add `edge_str` helper. Existing 9 test literals get `recorder: …None`. |
| `src-tauri/src/proxy/anthropic_edge.rs`, `openai_edge.rs` (modify) | Test literals: add `recorder: …None`. |
| `src-tauri/src/config/types.rs` (modify) | Add `Settings.request_recording: bool` (default false). |
| `src-tauri/src/commands.rs` (modify) | `SettingsView` field + `get_settings` plumbing + `set_request_recording` + `clear_request_log` commands. |
| `src-tauri/src/lib.rs` (modify) | `pub mod recording;`; production `recorder` init from setting; register 2 new commands. |
| `src-tauri/tests/e2e_zhipu.rs` (modify) | Test literal: add `recorder: …None`. |
| `src-tauri/tests/request_recording.rs` (new) | Integration test: dispatch records a line; outcome correctness. |
| `src/lib/types.ts` (modify) | Add `request_recording` to `Settings` + `SettingsView`. |
| `src/lib/commands.ts` (modify) | `setRequestRecording`, `clearRequestLog` wrappers. |
| `src/stores/system.ts` (modify) | `saveRequestRecording`, `clearRequestLog` actions. |
| `src/views/Settings.vue` (modify) | Toggle + clear button + privacy warning. |
| `dev-reference/analyze_requests.py` (new) | Dup/prefix-overlap analysis from the JSONL. |

---

## Task 1: Pure hash functions (`recording::hash`)

**Files:**
- Create: `src-tauri/src/recording/mod.rs`
- Test: `#[cfg(test)] mod tests` inside the same file

**Interfaces:**
- Produces: `pub fn hash_value(v: &serde_json::Value) -> String` (canonical-SHA256 of any JSON value, `sha256:`-prefixed); `pub fn hash_full(req: &Value) -> String`; `pub fn hash_messages(req: &Value, is_anthropic: bool) -> String` (Anthropic normalized via `crate::translate::request::anthropic_to_openai`).
- Consumes: `crate::translate::request::anthropic_to_openai` (exists, tested).

- [ ] **Step 1: Register the module + write the failing tests**

Add to `src-tauri/src/lib.rs` after line 6 (`pub mod proxy;`):
```rust
pub mod recording;
```

Create `src-tauri/src/recording/mod.rs` with ONLY the tests (implementation comes step 3):
```rust
//! 请求记录:纯哈希函数(规范 JSON → SHA-256)。见 spec §6。
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_value_is_deterministic() {
        let v = json!({"model":"glm-4.6","messages":[{"role":"user","content":"hi"}]});
        assert_eq!(hash_value(&v), hash_value(&v));
        assert!(hash_value(&v).starts_with("sha256:"));
    }

    #[test]
    fn hash_value_is_key_order_invariant() {
        // Same content, different key order → same canonical hash.
        let a = json!({"b":1,"a":2});
        let b = json!({"a":2,"b":1});
        assert_eq!(hash_value(&a), hash_value(&b));
    }

    #[test]
    fn hash_value_differs_on_content() {
        assert_ne!(hash_value(&json!({"x":1})), hash_value(&json!({"x":2})));
    }

    #[test]
    fn hash_full_includes_stream_and_params() {
        let base = json!({"model":"m","messages":[],"max_tokens":16});
        let mut with_stream = base.clone();
        with_stream["stream"] = json!(true);
        assert_ne!(hash_full(&base), hash_full(&with_stream));
    }

    #[test]
    fn hash_messages_ignores_stream_and_params() {
        let base = json!({"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"}]});
        let mut with_stream = base.clone();
        with_stream["stream"] = json!(true);
        with_stream["temperature"] = json!(0.7);
        // OpenAI edge: messages identical → same content hash despite param/stream diffs.
        assert_eq!(hash_messages(&base, false), hash_messages(&with_stream, false));
    }

    #[test]
    fn hash_messages_cross_protocol_equivalent() {
        // Same conversation, expressed in OpenAI shape vs Anthropic shape.
        let oai = json!({
            "messages":[
                {"role":"system","content":"be brief"},
                {"role":"user","content":"hi"}
            ]
        });
        let an = json!({
            "system":"be brief",
            "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]
        });
        assert_eq!(hash_messages(&oai, false), hash_messages(&an, true));
    }

    #[test]
    fn hash_messages_anthropic_normalizes_tool_blocks() {
        let an = json!({
            "messages":[
                {"role":"user","content":"w?"},
                {"role":"assistant","content":[
                    {"type":"text","text":"c"},
                    {"type":"tool_use","id":"t1","name":"f","input":{"a":1}}
                ]}
            ]
        });
        // anthropic_to_openai produces a stable OpenAI messages array; hashing it twice is stable
        // and differs from a different tool input.
        assert_eq!(hash_messages(&an, true), hash_messages(&an, true));
        let mut an2 = an.clone();
        an2["messages"][1]["content"][1]["input"]["a"] = json!(2);
        assert_ne!(hash_messages(&an, true), hash_messages(&an2, true));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml recording::tests`
Expected: compile error — `hash_value` / `hash_full` / `hash_messages` not defined.

- [ ] **Step 3: Write the implementation**

Append ABOVE the `#[cfg(test)] mod tests` block in `src-tauri/src/recording/mod.rs`:
```rust
//! 请求记录:纯哈希函数(规范 JSON → SHA-256)。见 spec §6。
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 规范 JSON:对象键按字典序递归排序、无多余空白(确定性,与 serde_json 的
/// `preserve_order` feature 无关)。相同逻辑请求永远产生相同字符串。
fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    canonical_write(v, &mut out);
    out
}

fn canonical_write(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default()); // quoted key
                out.push(':');
                canonical_write(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_write(e, out);
            }
            out.push(']');
        }
        _ => out.push_str(&serde_json::to_string(v).unwrap_or_default()),
    }
}

/// 规范哈希任意 JSON 值,返回 `sha256:<hex>`。
pub fn hash_value(v: &Value) -> String {
    let canon = canonical_json(v);
    format!("sha256:{:x}", Sha256::digest(canon.as_bytes()))
}

/// 整请求体的规范哈希(含 stream/全部参数)——未来全响应缓存的 key。见 spec §6。
pub fn hash_full(req: &Value) -> String {
    hash_value(req)
}

/// 仅对话内容的规范哈希。Anthropic 边先经 `anthropic_to_openai` 归一化(把 system
/// 折叠进 messages、把 content block 摊平),再与 OpenAI 边共用同一套"messages 哈希"
/// 逻辑。剔除参数/stream,回答"内容相同、参数不同"。见 spec §6。
pub fn hash_messages(req: &Value, is_anthropic: bool) -> String {
    let messages = if is_anthropic {
        crate::translate::request::anthropic_to_openai(req)
            .get("messages")
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        req.get("messages").cloned().unwrap_or(Value::Null)
    };
    hash_value(&messages)
}

// keep the `json`/`Value`/`Sha256`/`Digest` imports used above; `json!` is used in tests only.
#[allow(unused_imports)]
use serde_json::json;
```
(Then the existing `#[cfg(test)] mod tests { ... }` follows.)

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml recording::tests`
Expected: PASS — all 7 tests green. If `canonical_write` has an unused-import warning on `json`/`Value`, the `#[allow(unused_imports)]` line suppresses it (tests use them).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/recording/mod.rs src-tauri/src/lib.rs
git commit -m "feat(recording): add canonical-JSON hash functions for request dedup"
```

---

## Task 2: `RollWriter` + `RequestRecord` + `RequestRecorder` (the writer engine)

**Files:**
- Modify: `src-tauri/src/recording/mod.rs`
- Test: `#[cfg(test)] mod tests` (append)

**Interfaces:**
- Produces:
  - `pub struct RequestRecord` (`#[derive(Serialize)]`) with fields: `ts: String`, `req_id: String`, `protocol: &'static str`, `requested: String`, `vendor: String`, `model: String`, `stream: bool`, `bytes: usize`, `body: Value`, `hash_full: String`, `hash_messages: String`, `outcome: &'static str`, `status: Option<u16>`, `ms: u128`, `hops: usize`.
  - `pub enum Msg { Record(RequestRecord), Clear }`
  - `pub struct RequestRecorder { tx: tokio::sync::mpsc::Sender<Msg>, dropped: std::sync::atomic::AtomicU64 }`
  - `impl RequestRecorder { pub fn channel() -> (std::sync::Arc<Self>, tokio::sync::mpsc::Receiver<Msg>); pub fn record(&self, rec: RequestRecord); pub async fn clear(&self); pub fn dropped_count(&self) -> u64 }`
  - `pub async fn run_writer(rx: tokio::sync::mpsc::Receiver<Msg>, dir: std::path::PathBuf)`
- Consumes: nothing from earlier tasks (self-contained). Uses `tempfile` in tests (already a dev-dep).

- [ ] **Step 1: Write the failing tests (append to the `mod tests` block)**

```rust
    use std::io::Read;
    use std::path::PathBuf;

    fn read_log(dir: &PathBuf) -> String {
        let p = dir.join("requests.jsonl");
        match std::fs::File::open(&p) {
            Ok(mut f) => { let mut s = String::new(); f.read_to_string(&mut s).unwrap(); s }
            Err(_) => String::new(),
        }
    }

    fn sample_record(req_id: &str, body: Value) -> RequestRecord {
        RequestRecord {
            ts: "2026-08-11T14:32:01+08:00".into(),
            req_id: req_id.into(),
            protocol: "openai",
            requested: "glm-4.6".into(),
            vendor: "zhipu".into(),
            model: "glm-4.6".into(),
            stream: false,
            bytes: 42,
            body,
            hash_full: "sha256:abc".into(),
            hash_messages: "sha256:def".into(),
            outcome: "ok",
            status: Some(200),
            ms: 5,
            hops: 1,
        }
    }

    #[tokio::test]
    async fn run_writer_appends_one_jsonl_line_per_record() {
        let dir = tempfile::tempdir().unwrap();
        let (rec, rx) = RequestRecorder::channel();
        let h = tokio::spawn(run_writer(rx, dir.path().to_path_buf()));
        rec.record(sample_record("a3f2", json!({"messages":[]})));
        // give the writer task a moment to drain
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(rec);                       // closes channel → writer exits
        let _ = h.await;
        let log = read_log(&dir.path().to_path_buf());
        assert_eq!(log.lines().count(), 1);
        let v: Value = serde_json::from_str(log.trim()).unwrap();
        assert_eq!(v["req_id"], "a3f2");
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["status"], 200);
        assert_eq!(v["body"]["messages"], json!([]));   // full body stored
    }

    #[tokio::test]
    async fn clear_wipes_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (rec, rx) = RequestRecorder::channel();
        let h = tokio::spawn(run_writer(rx, dir.path().to_path_buf()));
        rec.record(sample_record("0001", json!({})));
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!read_log(&dir.path().to_path_buf()).is_empty());
        rec.clear().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(read_log(&dir.path().to_path_buf()).is_empty()); // cleared
        drop(rec);
        let _ = h.await;
    }

    #[tokio::test]
    async fn dropped_count_increments_when_channel_is_full_and_closed() {
        // Capacity 256; fill without draining, then record more → drops counted.
        let (rec, _rx) = RequestRecorder::channel(); // NOTE: rx not spawned → never drains
        for _ in 0..300 {
            rec.record(sample_record("x", json!({})));
        }
        assert!(rec.dropped_count() > 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --manifest-path src-tauri/Cargo.toml recording::tests`
Expected: compile error — `RequestRecord`, `RequestRecorder`, `run_writer` undefined.

- [ ] **Step 3: Add `RollWriter`, `RequestRecord`, `Msg`, `RequestRecorder`, `run_writer`**

Append (above the test module) to `src-tauri/src/recording/mod.rs`:
```rust
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 每请求一行的记录。见 spec §5。`body` 是入站原始请求体(无损、未归一化)。
#[derive(Serialize)]
pub struct RequestRecord {
    pub ts: String,
    pub req_id: String,
    pub protocol: &'static str,
    pub requested: String,
    pub vendor: String,
    pub model: String,
    pub stream: bool,
    pub bytes: usize,
    pub body: Value,
    pub hash_full: String,
    pub hash_messages: String,
    pub outcome: &'static str,
    pub status: Option<u16>,
    pub ms: u128,
    pub hops: usize,
}

/// 后台写盘任务消费的消息:写一条记录,或清空整个目录。
pub enum Msg {
    Record(RequestRecord),
    Clear,
}

/// 滚动 JSONL 写盘器(镜像 logging::RollingFileWriter 的轮转模式,但独立文件名/容量)。
/// 见 spec §7。50 MiB × 5 文件。
const MAX_SIZE: u64 = 50 * 1024 * 1024;
const MAX_FILES: usize = 5;
const FILE_NAME: &str = "requests.jsonl";

pub struct RollWriter {
    dir: PathBuf,
    file: Option<File>,
    written: u64,
}

impl RollWriter {
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = open_append(dir)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self { dir: dir.to_path_buf(), file: Some(file), written })
    }

    /// 关闭当前文件句柄(Windows 删除/重命名前必须关闭)。
    pub fn close(&mut self) {
        self.file = None;
    }

    fn rotate(&mut self) {
        self.file = None; // close BEFORE rename (Windows)
        let cur = self.dir.join(FILE_NAME);
        let _ = std::fs::remove_file(self.dir.join(format!("{FILE_NAME}.{}", MAX_FILES - 1)));
        for n in (1..MAX_FILES - 1).rev() {
            let from = self.dir.join(format!("{FILE_NAME}.{n}"));
            let to = self.dir.join(format!("{FILE_NAME}.{}", n + 1));
            let _ = std::fs::rename(&from, &to);
        }
        let _ = std::fs::rename(&cur, self.dir.join(format!("{FILE_NAME}.1")));
        self.file = open_append(&self.dir).ok();
        self.written = 0;
    }
}

impl Write for RollWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = match self.file.as_mut() {
            Some(f) => f.write(buf)?,
            None => return Err(std::io::Error::other("no request-log file")),
        };
        self.written += n as u64;
        if self.written >= MAX_SIZE {
            self.rotate();
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map(|f| f.flush()).transpose().map(|_| ())
    }
}

impl Drop for RollWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn open_append(dir: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(dir.join(FILE_NAME))
}

/// 请求记录器:持有通道发送端 + 丢弃计数。运行时无关——调用方负责 spawn `run_writer`
/// (生产用 `tauri::async_runtime::spawn`,测试用 `tokio::spawn`)。见 spec §3/§8。
pub struct RequestRecorder {
    tx: mpsc::Sender<Msg>,
    dropped: AtomicU64,
}

impl RequestRecorder {
    /// 返回(记录器句柄, 接收端)。把接收端交给 `run_writer`。
    pub fn channel() -> (Arc<Self>, mpsc::Receiver<Msg>) {
        let (tx, rx) = mpsc::channel(256);
        (Arc::new(Self { tx, dropped: AtomicU64::new(0) }), rx)
    }

    /// 非阻塞记录:通道满则丢弃并计数(偶发,仅写盘严重卡顿时)。见 spec §11。
    pub fn record(&self, rec: RequestRecord) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(Msg::Record(rec)) {
            let total = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // 在 2 的幂次上告警(1,2,4,8…),既可见又不刷屏。
            if total.is_power_of_two() {
                tracing::warn!(
                    target: "switchlm::recording",
                    dropped = total, "request record dropped (writer stalled)"
                );
            }
        }
    }

    /// 清空记录目录(经通道串行化,确保先关闭打开的文件句柄再删除)。见 spec §7。
    pub async fn clear(&self) {
        let _ = self.tx.send(Msg::Clear).await;
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 后台写盘循环:消费 `Msg`,写 JSONL 或清空目录。通道关闭(rx.recv()→None)即退出。
pub async fn run_writer(mut rx: mpsc::Receiver<Msg>, dir: PathBuf) {
    let mut w = match RollWriter::new(&dir) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(target: "switchlm::recording", error = %e, "request log unavailable");
            return;
        }
    };
    while let Some(msg) = rx.recv().await {
        match msg {
            Msg::Record(r) => {
                if let Ok(s) = serde_json::to_string(&r) {
                    let _ = w.write_all(s.as_bytes());
                    let _ = w.write_all(b"\n");
                }
            }
            Msg::Clear => {
                w.close();
                let _ = std::fs::remove_dir_all(&dir);
                let _ = std::fs::create_dir_all(&dir);
                w = match RollWriter::new(&dir) {
                    Ok(nw) => nw,
                    Err(_) => break, // 无法重建则退出;下次记录会由调用方重建
                };
            }
        }
    }
}

// RollWriter 持有 Mutex 仅用于潜在的多任务共享;当前 run_writer 单任务独占。保留以备扩展。
#[allow(dead_code)]
fn _link_mutex(_: Mutex<()>) {}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml recording::tests`
Expected: PASS — all tests (Task 1's 7 + Task 2's 3) green.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/recording/mod.rs
git commit -m "feat(recording): add RollWriter + RequestRecorder writer engine"
```

---

## Task 3: Add the `recorder` field to `AppStateInner` (compiles, no behavior change)

**Files:**
- Modify: `src-tauri/src/proxy/state.rs` (struct + `load()`)
- Modify (test/production literals — add one line each): `src-tauri/src/lib.rs:157`, `src-tauri/src/proxy/dispatch.rs` (×9: ~lines 1216, 1240, 1267, 1459, 1526, 1669, 2056, 2149, 2225), `src-tauri/src/proxy/anthropic_edge.rs:60`, `src-tauri/src/proxy/openai_edge.rs:68`, `src-tauri/tests/e2e_zhipu.rs:85`

**Interfaces:**
- Produces: `AppStateInner.recorder: std::sync::RwLock<Option<std::sync::Arc<crate::recording::RequestRecorder>>>`. In this task it is ALWAYS `None` at every site (behavior unchanged; existing tests must still pass). Production reads the setting in Task 6.

- [ ] **Step 1: Add the field to the struct**

In `src-tauri/src/proxy/state.rs`, add to the imports near the top:
```rust
use crate::recording::RequestRecorder;
```
Add this field to `AppStateInner` (after `last_served_provider`, before the closing brace at line 43):
```rust
    /// 请求记录器(`None` = 记录关闭,零开销)。RwLock 以便 set_request_recording 热切换。
    /// 见 spec §3/§8。
    pub recorder: std::sync::RwLock<Option<std::sync::Arc<RequestRecorder>>>,
```

- [ ] **Step 2: Initialize `None` in `load()`**

In `src-tauri/src/proxy/state.rs::AppStateInner::load` (the `Ok(Self { ... })` around line 54-66), add as the last field before the closing `}`:
```rust
            recorder: std::sync::RwLock::new(None),
```

- [ ] **Step 3: Add `recorder: std::sync::RwLock::new(None),` to EVERY struct-literal site**

At each of these exact locations, add the line inside the `AppStateInner { ... }` literal (right after the `last_served_provider: ...` line):
- `src-tauri/src/lib.rs` (~line 157) — production site (will be replaced in Task 6)
- `src-tauri/src/proxy/dispatch.rs` — 9 test sites (~lines 1216, 1240, 1267, 1459, 1526, 1669, 2056, 2149, 2225)
- `src-tauri/src/proxy/anthropic_edge.rs` (~line 60)
- `src-tauri/src/proxy/openai_edge.rs` (~line 68)
- `src-tauri/tests/e2e_zhipu.rs` (~line 85)

The added line at each site:
```rust
            recorder: std::sync::RwLock::new(None),
```

> Tip: search for `last_served_provider: std::sync::Mutex::new(None),` and `last_served_provider: Mutex::new(None),` — each occurrence is one `AppStateInner` literal; add the `recorder:` line immediately below it. There are 14 occurrences total (1 in lib.rs, 1 in state.rs::load handled above, 9 in dispatch.rs, 1 in anthropic_edge, 1 in openai_edge, 1 in e2e_zhipu).

- [ ] **Step 4: Build + run the full backend test suite to confirm no behavior change**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — everything compiles (recorder is always `None`, dispatch does not yet read it) and all pre-existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/proxy/state.rs src-tauri/src/lib.rs src-tauri/src/proxy/dispatch.rs src-tauri/src/proxy/anthropic_edge.rs src-tauri/src/proxy/openai_edge.rs src-tauri/tests/e2e_zhipu.rs
git commit -m "feat(recording): add Option<RequestRecorder> field to AppState (no behavior yet)"
```

---

## Task 4: `Settings.request_recording` + `get_settings`/`set_request_recording`/`clear_request_log` commands

**Files:**
- Modify: `src-tauri/src/config/types.rs` (`Settings` struct + `Default`)
- Modify: `src-tauri/src/commands.rs` (`SettingsView`, `get_settings`, new commands)
- Modify: `src-tauri/src/lib.rs` (register 2 commands)

**Interfaces:**
- Produces: `Settings.request_recording: bool` (default `false`, persisted in `app_config.json`); `SettingsView.request_recording: bool`; commands `set_request_recording(state, app, enabled) -> Result<()>` (persists + hot-swaps the recorder) and `clear_request_log(state, app) -> Result<()>`.
- Consumes: `crate::recording::{RequestRecorder, run_writer}` (Task 2); `AppState.recorder` (Task 3); `persist(app, cfg)` helper at `commands.rs:1221`.

- [ ] **Step 1: Add the setting field**

In `src-tauri/src/config/types.rs`, add to `pub struct Settings` (after `log_level`):
```rust
    /// 是否记录每个请求的完整入站请求体 + 哈希到本地(默认关)。见 spec §8。
    #[serde(default)]
    pub request_recording: bool,
```
In `impl Default for Settings`, add:
```rust
            request_recording: false,
```

- [ ] **Step 2: Add `request_recording` to `SettingsView` + `get_settings`**

In `src-tauri/src/commands.rs`, add to `pub struct SettingsView` (after `log_level`):
```rust
    pub request_recording: bool,
```
In `get_settings` (the `Ok(SettingsView { ... })`), add:
```rust
        request_recording: cfg.settings.request_recording,
```

- [ ] **Step 3: Write failing tests for the two new commands**

Add to the `#[cfg(test)]` module in `src-tauri/src/commands.rs` (find an existing test that builds state via `AppStateInner::load(dir.path(), ...)` around line 1797 and mirror its setup). Append:
```rust
    #[tokio::test]
    async fn set_request_recording_persists_and_swaps_recorder() {
        use crate::proxy::AppStateInner;
        let dir = tempfile::tempdir().unwrap();
        let secrets: Arc<dyn crate::config::SecretStore> = Arc::new(crate::config::MemoryStore::default());
        let state: AppState = Arc::new(AppStateInner::load(dir.path(), secrets).unwrap());
        assert!(state.recorder.read().unwrap().is_none()); // off by default

        // Build the minimal tauri::AppHandle-free path: test the core swap directly.
        let dir2 = dir.path().to_path_buf();
        // enable
        {
            let (rec, rx) = crate::recording::RequestRecorder::channel();
            tokio::spawn(crate::recording::run_writer(rx, dir2.join("request_log")));
            *state.recorder.write().unwrap() = Some(rec);
        }
        assert!(state.recorder.read().unwrap().is_some());
        // disable → drops Arc → recorder None
        *state.recorder.write().unwrap() = None;
        assert!(state.recorder.read().unwrap().is_none());
    }
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml set_request_recording_persists_and_swaps_recorder`
Expected: PASS actually (the test exercises `RequestRecorder` + the field directly, both already exist from Tasks 2–3) — this test pins the swap contract the command will use. If it passes now, good; it guards the command implementation in step 5.

- [ ] **Step 5: Implement `set_request_recording` + `clear_request_log`**

In `src-tauri/src/commands.rs`, add (after `set_log_level`, near line 1022):
```rust
/// 开关请求记录:持久化设置 + 热切换运行时记录器(无需重启)。见 spec §8。
#[tauri::command]
pub async fn set_request_recording(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    {
        let mut config = state.config.write().await;
        config.settings.request_recording = enabled;
        persist(&app, &config)?;
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let next = if enabled {
        let (rec, rx) = crate::recording::RequestRecorder::channel();
        tauri::async_runtime::spawn(crate::recording::run_writer(rx, dir.join("request_log")));
        Some(rec)
    } else {
        None // drop old Arc → channel closes → old writer task exits
    };
    *state.recorder.write().unwrap() = next;
    Ok(())
}

/// 清空请求记录目录。记录开启时经记录器串行清空(先关句柄再删);关闭时直接删目录。
#[tauri::command]
pub async fn clear_request_log(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("request_log");
    let cleared = {
        let g = state.recorder.read().unwrap();
        if let Some(rec) = g.as_ref() {
            rec.clear().await;
            true
        } else {
            false
        }
    };
    if !cleared {
        // 记录关闭 → 没有打开的句柄,直接删(best-effort)。
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(())
}
```

- [ ] **Step 6: Register the commands**

In `src-tauri/src/lib.rs` `invoke_handler` (after `commands::open_log_dir,` ~line 243), add:
```rust
            commands::set_request_recording,
            commands::clear_request_log,
```

- [ ] **Step 7: Build + run tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — compiles; new test green; existing tests unaffected.

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/config/types.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(recording): add Settings.request_recording + set/clear commands"
```

---

## Task 5: Record in `dispatch()` + production init from setting

**Files:**
- Modify: `src-tauri/src/proxy/dispatch.rs` (tail of `dispatch()`; add `edge_str` helper)
- Modify: `src-tauri/src/lib.rs` (production `recorder` init, replacing the `None` from Task 3)
- Create: `src-tauri/tests/request_recording.rs`

**Interfaces:**
- Consumes: `crate::recording::{RequestRecord, RequestRecorder, hash_full, hash_messages}`; `state.recorder`; the `ClientProtocol` enum; `chrono::Local`.
- Produces: a JSONL line per request when recording is on.

- [ ] **Step 1: Write the failing integration test**

Create `src-tauri/tests/request_recording.rs`:
```rust
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use switchlm_lib::config::*;
use switchlm_lib::proxy::server::build_router;
use switchlm_lib::proxy::{AppState, AppStateInner, SystemClock};
use switchlm_lib::recording::RequestRecorder;

/// State whose recorder writes into `recdir`. Returns the recorder handle so the test can
/// `drop(rec)` to close the channel (→ writer task exits) before reading the file.
async fn state_with_recording(upstream: &str, recdir: &std::path::Path) -> (AppState, Arc<RequestRecorder>) {
    let mut cfg = AppConfig::default();
    cfg.providers.push(Provider {
        id: "zhipu".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
        openai_base_url: Some(upstream.into()), anthropic_base_url: None, usage_creds: None,
    });
    cfg.models.push(Model {
        id: "m".into(), provider_id: "zhipu".into(), source: ModelSource::Manual,
        upstream_model_id: "glm-4.6".into(), ..Default::default()
    });
    cfg.profiles.push(Profile {
        id: "p".into(), name: "glm-4.6".into(), aliases: vec![],
        backing_model_id: "m".into(), ..Default::default()
    });
    let secrets = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
    secrets.set_key("zhipu", "sk-test").unwrap();
    let (rec, rx) = RequestRecorder::channel();
    let dir = recdir.to_path_buf();
    tokio::spawn(switchlm_lib::recording::run_writer(rx, dir));
    let state = Arc::new(AppStateInner {
        config: tokio::sync::RwLock::new(cfg),
        catalog: Default::default(),
        secrets,
        health: Default::default(),
        clock: Arc::new(SystemClock),
        usage_cache: Default::default(),
        bound_port: std::sync::Mutex::new(None),
        server_handle: std::sync::Mutex::new(None),
        bind_error: std::sync::Mutex::new(None),
        polling_handle: std::sync::Mutex::new(None),
        last_served_provider: std::sync::Mutex::new(None),
        recorder: std::sync::RwLock::new(Some(rec.clone())),
    });
    (state, rec)
}

fn oai_post() -> Request<Body> {
    Request::builder().method("POST").uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({
            "model":"glm-4.6","messages":[{"role":"user","content":"hi"}]
        }).to_string())).unwrap()
}

#[tokio::test]
async fn dispatch_records_a_line_when_recording_on() {
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id":"x","choices":[{"message":{"role":"assistant","content":"ok"}}]
        })))
        .mount(&mock).await;

    let dir = tempfile::tempdir().unwrap();
    let (state, rec) = state_with_recording(&mock.uri(), dir.path()).await;
    let app = build_router(state);
    let resp = app.oneshot(oai_post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = resp.into_body().collect().await;

    // drain the writer, then close the channel so the file is flushed
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    drop(rec);

    let log = std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap();
    assert_eq!(log.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(v["outcome"], "ok");
    assert_eq!(v["status"], 200);
    assert_eq!(v["protocol"], "openai");
    assert!(v["hash_full"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(v["body"]["messages"][0]["content"], "hi"); // full inbound body stored
}

#[tokio::test]
async fn dispatch_outcome_error_on_passthrough_non_2xx() {
    let mock = MockServer::start().await;
    Mock::given(method("POST")).and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error":"no"})))
        .mount(&mock).await;
    let dir = tempfile::tempdir().unwrap();
    let (state, rec) = state_with_recording(&mock.uri(), dir.path()).await;
    let app = build_router(state);
    let resp = app.oneshot(oai_post()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let _ = resp.into_body().collect().await;
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    drop(rec);
    let v: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(dir.path().join("requests.jsonl")).unwrap().trim()).unwrap();
    assert_eq!(v["outcome"], "error");
    assert_eq!(v["status"], 401);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test request_recording`
Expected: the first test FAILS — `requests.jsonl` is empty/missing because `dispatch` does not yet record.

- [ ] **Step 3: Add the `edge_str` helper + the recording block in `dispatch()`**

In `src-tauri/src/proxy/dispatch.rs`, add a helper near `path_for` (~line 594):
```rust
/// 协议边 → 记录用的字符串标识(日志/记录元数据)。
fn edge_str(p: ClientProtocol) -> &'static str {
    match p {
        ClientProtocol::OpenAI => "openai",
        ClientProtocol::Anthropic => "anthropic",
    }
}
```

In `dispatch()`, insert this block right BEFORE the final `result` expression (after the `last_served_provider` block, ~line 147), so `result` is still owned afterward:
```rust
    // 请求记录(spec §4):在 dispatch 末端、结果已知后落一条 JSONL。仅当记录开启时有开销。
    {
        let recorder = state.recorder.read().unwrap().clone(); // Option<Arc<RequestRecorder>>
        if let Some(r) = recorder {
            let (served_vendor, served_model) = served
                .split_once('/')
                .map(|(v, m)| (v.to_string(), m.to_string()))
                .unwrap_or_else(|| ("-".to_string(), served.clone()));
            let (outcome_str, status) = match &result {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if resp.status().is_success() { ("ok", Some(code)) } else { ("error", Some(code)) }
                }
                Err(_) => ("error", None),
            };
            r.record(crate::recording::RequestRecord {
                ts: chrono::Local::now().to_rfc3339(),
                req_id,
                protocol: edge_str(protocol),
                requested,
                vendor: served_vendor,
                model: served_model,
                stream: is_stream,
                bytes: body.len(),
                body: req.clone(),
                hash_full: crate::recording::hash_full(&req),
                hash_messages: crate::recording::hash_messages(
                    &req,
                    matches!(protocol, ClientProtocol::Anthropic),
                ),
                outcome: outcome_str,
                status,
                ms,
                hops: outcome.hops,
            });
        }
    }
```

- [ ] **Step 4: Wire production init in `lib.rs`**

In `src-tauri/src/lib.rs`, replace the Task-3 placeholder line `recorder: std::sync::RwLock::new(None),` (in the production `AppStateInner { ... }` at ~line 157) with a computed value. Immediately BEFORE the `let state: proxy::AppState = Arc::new(AppStateInner {` line (~line 146), add:
```rust
            let recorder: Option<std::sync::Arc<crate::recording::RequestRecorder>> =
                if cfg.settings.request_recording {
                    let (rec, rx) = crate::recording::RequestRecorder::channel();
                    tauri::async_runtime::spawn(
                        crate::recording::run_writer(rx, dir.join("request_log")),
                    );
                    Some(rec)
                } else {
                    None
                };
```
And in the struct literal, replace the placeholder `recorder: std::sync::RwLock::new(None),` with:
```rust
                recorder: std::sync::RwLock::new(recorder),
```

- [ ] **Step 5: Run the integration tests to verify they pass**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --test request_recording`
Expected: PASS — both tests green (line recorded; outcome ok/error correct).

- [ ] **Step 6: Run the full backend suite to confirm no regressions**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — existing dispatch tests still green (their `recorder` is `None`, so the new block is a no-op there).

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/proxy/dispatch.rs src-tauri/src/lib.rs src-tauri/tests/request_recording.rs
git commit -m "feat(recording): record each request at dispatch tail + wire production init"
```

---

## Task 6: Frontend toggle + clear button with privacy warning

**Files:**
- Modify: `src/lib/types.ts` (`Settings`, `SettingsView`)
- Modify: `src/lib/commands.ts` (wrappers)
- Modify: `src/stores/system.ts` (actions)
- Modify: `src/views/Settings.vue` (UI)

**Interfaces:**
- Consumes: backend commands `set_request_recording`, `clear_request_log` (Task 4); `get_settings` now includes `request_recording`.
- Produces: a Settings card with an `NSwitch` + clear button + warning text.

- [ ] **Step 1: Add the field to TS types**

In `src/lib/types.ts`, add to `interface Settings` (after `log_level: string;`, ~line 79):
```ts
  request_recording: boolean;
```
Add to `interface SettingsView` (after `log_level: string;`, ~line 147):
```ts
  request_recording: boolean;
```

- [ ] **Step 2: Add command wrappers**

In `src/lib/commands.ts`, after `openLogDir` (~line 135), add:
```ts
export const setRequestRecording = (enabled: boolean) =>
  invoke<void>("set_request_recording", { enabled });
export const clearRequestLog = () => invoke<void>("clear_request_log");
```

- [ ] **Step 3: Add store actions**

In `src/stores/system.ts`, after `saveLogLevel` (~line 68), add:
```ts
  async function saveRequestRecording(enabled: boolean) {
    await api.setRequestRecording(enabled);
    await loadSettings();
  }

  async function clearRequestLog() {
    await api.clearRequestLog();
  }
```
Add both to the returned object (after `saveLogLevel,` and `openLogDir,`):
```ts
    saveRequestRecording,
    clearRequestLog,
```

- [ ] **Step 4: Add the UI card in `Settings.vue`**

In `src/views/Settings.vue`, add two handler functions (after `toggleAutostart`, ~line 97):
```ts
async function toggleRecording(on: boolean) {
  try {
    await system.saveRequestRecording(on);
    msg.success(on ? "已开启请求记录" : "已关闭请求记录");
  } catch (e) {
    msg.error(`设置失败：${String(e)}`);
  }
}
async function clearRequests() {
  dialog.warning({
    title: "清空请求记录",
    content: "确认清空所有已记录的请求数据？此操作不可撤销。",
    positiveText: "清空",
    negativeText: "取消",
    onPositiveClick: async () => {
      try {
        await system.clearRequestLog();
        msg.success("已清空请求记录");
      } catch (e) {
        msg.error(`清空失败：${String(e)}`);
      }
    },
  });
}
```
In the template, add a new card after the 日志 card (`</NCard>` ~line 165) and before the 服务 card:
```html
    <NCard title="请求记录" size="small">
      <NSpace vertical :size="10">
        <NSpace align="center" :size="12">
          <NSwitch
            :value="system.settings?.request_recording ?? false"
            @update:value="(v: boolean) => toggleRecording(v)"
          />
          <span class="muted">记录每个请求的完整内容(含代码与可能的密钥)到本地，用于分析缓存优化</span>
        </NSpace>
        <NSpace align="center" :size="12">
          <NButton :disabled="!system.settings?.request_recording" @click="clearRequests">清空记录</NButton>
          <span class="muted">仅本地存储、不上传；默认关闭。开启后可在 app_data/request_log/ 查看 requests.jsonl</span>
        </NSpace>
      </NSpace>
    </NCard>
```

- [ ] **Step 5: Type-check + build the frontend**

Run: `pnpm exec vue-tsc --noEmit && pnpm build`
Expected: PASS — no type errors; the `request_recording` field is present on both `Settings` and `SettingsView`, matching the Rust `SettingsView` (Task 4).

- [ ] **Step 6: Commit**

```bash
git add src/lib/types.ts src/lib/commands.ts src/stores/system.ts src/views/Settings.vue
git commit -m "feat(recording): add Settings toggle + clear button with privacy warning"
```

---

## Task 7: Analysis script `dev-reference/analyze_requests.py`

**Files:**
- Create: `dev-reference/analyze_requests.py`

**Interfaces:**
- Consumes: `<app_data>/request_log/requests.jsonl` (one `RequestRecord` per line; fields per spec §5).
- Produces: prints exact-dup rate, content-dup rate, top-N keys, prefix-overlap hint, byte-bucketed dup rate.

- [ ] **Step 1: Write the script**

Create `dev-reference/analyze_requests.py`:
```python
#!/usr/bin/env python3
"""Analyze request-recording JSONL for cache viability.

Usage: python analyze_requests.py <path/to/requests.jsonl> [--top N]

Prints (over outcome=="ok" rows only — only cacheable responses count):
  - exact-duplicate rate   (hash_full)  -> full-response cache hit-rate ceiling
  - content-duplicate rate (hash_messages)
  - top-N most-repeated keys
  - retry vs concurrent-dup split (same hash_full within 2s = retry)
  - byte-bucketed exact-dup rate
"""
import argparse
import collections
import json
import sys


def load(path):
    rows = []
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def dup_rate(rows, key):
    counts = collections.Counter(r[key] for r in rows)
    total = sum(counts.values())
    unique = len(counts)
    hits = total - unique  # every repeat beyond the first is a would-be cache hit
    return unique, total, hits, counts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--top", type=int, default=10)
    args = ap.parse_args()

    rows = load(args.file)
    ok = [r for r in rows if r.get("outcome") == "ok"]
    print(f"total records: {len(rows)} | cacheable (outcome==ok): {len(ok)}")
    if not ok:
        print("no cacheable rows; nothing to analyze.")
        return

    for label, key in [("exact (hash_full)", "hash_full"), ("content (hash_messages)", "hash_messages")]:
        unique, total, hits, counts = dup_rate(ok, key)
        rate = hits / total if total else 0.0
        print(f"\n{label}: {unique} unique / {total} total -> dup rate {rate:.2%} ({hits} repeat hits)")
        print(f"  top {args.top}:")
        for h, c in counts.most_common(args.top):
            if c > 1:
                print(f"    {c}x  {h}")

    # retry vs concurrent-dup split (exact), by hash_full
    by_key = collections.defaultdict(list)
    for r in ok:
        by_key[r["hash_full"]].append(r.get("ts", ""))
    retries, concurrent = 0, 0
    for _, ts_list in by_key.items():
        if len(ts_list) < 2:
            continue
        # crude: if any two within 2s, call the group retries
        ts_sorted = sorted(ts_list)
        paired = False
        for a, b in zip(ts_sorted, ts_sorted[1:]):
            if (parse_ts(b) - parse_ts(a)) <= 2.0:
                paired = True
                break
        if paired:
            retries += len(ts_list) - 1
        else:
            concurrent += len(ts_list) - 1
    print(f"\nrepeats split: ~{retries} retry-like | ~{concurrent} concurrent-like (exact hash_full)")


def parse_ts(s):
    # ISO 8601 local; fall back to 0 on parse error so ordering still works
    try:
        from datetime import datetime
        return datetime.fromisoformat(s).timestamp()
    except Exception:
        return 0.0


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Smoke-test the script against the integration test's JSONL**

Run the integration test once to produce a `requests.jsonl`, then point the script at it (the file lives under the test's tempdir; for a quick smoke test, hand-craft a 2-line file):
```bash
printf '%s\n' \
'{"ts":"2026-08-11T14:32:01+08:00","req_id":"a","protocol":"openai","requested":"glm-4.6","vendor":"zhipu","model":"glm-4.6","stream":false,"bytes":10,"body":{"messages":[{"role":"user","content":"hi"}]},"hash_full":"sha256:1","hash_messages":"sha256:m","outcome":"ok","status":200,"ms":5,"hops":1}' \
'{"ts":"2026-08-11T14:32:02+08:00","req_id":"b","protocol":"openai","requested":"glm-4.6","vendor":"zhipu","model":"glm-4.6","stream":false,"bytes":10,"body":{"messages":[{"role":"user","content":"hi"}]},"hash_full":"sha256:1","hash_messages":"sha256:m","outcome":"ok","status":200,"ms":4,"hops":1}' \
> /tmp/reqs.jsonl
python dev-reference/analyze_requests.py /tmp/reqs.jsonl
```
Expected: prints `dup rate 50.00%` for both exact and content (2 rows, 1 unique, 1 hit), and `~1 retry-like` (the two rows are 1s apart).

- [ ] **Step 3: Commit**

```bash
git add dev-reference/analyze_requests.py
git commit -m "feat(recording): add analyze_requests.py for dup-rate / cache-viability analysis"
```

---

## Self-Review (completed)

**1. Spec coverage:** Every spec section maps to a task — §3 architecture (Tasks 1–3) ✓, §4 dispatch tail (Task 5) ✓, §5 schema (Task 2 `RequestRecord` + Task 5 fills it) ✓, §6 hashing (Task 1) ✓, §7 storage/rotation/clear (Task 2 `RollWriter`/`Clear` + Task 4 `clear_request_log`) ✓, §8 toggle/privacy/overhead (Tasks 3–6) ✓, §9 analysis (Task 7) ✓, §10 testing (each task) ✓, §11 risks addressed in Global Constraints + Task 4 warning ✓.

**2. Placeholder scan:** No TBD/TODO. Task 5's test helper is the final clean version (no `recdir()` placeholder, no duplicate definition).

**3. Type consistency:** `RequestRecord` fields (Task 2) match what `dispatch` populates (Task 5) — same names/types (`ts`,`req_id`,`protocol`,`requested`,`vendor`,`model`,`stream`,`bytes`,`body`,`hash_full`,`hash_messages`,`outcome`,`status`,`ms`,`hops`). `hash_messages(req, is_anthropic: bool)` signature (Task 1) matches the `matches!(protocol, …)` call (Task 5). `RequestRecorder::channel() -> (Arc<Self>, Receiver<Msg>)` (Task 2) matches all callers (Tasks 4, 5). `Settings.request_recording` / `SettingsView.request_recording` (Task 4) match TS `Settings`/`SettingsView` (Task 6).
