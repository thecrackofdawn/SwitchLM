//! 请求记录:纯哈希函数(规范 JSON → SHA-256)。见 spec §6。
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

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

// 顶部已导入 `Value`/`Sha256`/`Digest` 供实现使用;`json!` 仅测试用到(经 `use super::*` 可见),
// 非 test 构建下会触发未使用导入,此处以 `#[allow(unused_imports)]` 静默。
#[allow(unused_imports)]
use serde_json::json;

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
}
