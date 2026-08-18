//! Claude Code 上下文声明自动同步。
//! 见 docs/superpowers/specs/2026-08-15-claude-context-sync-design.md。
//!
//! 纯函数核心（可测）+ 异步壳（sync_once / spawn_sync_task，Task 4 加入）。
//! 唯一写入目标：{home}/.claude/settings.json 中模型名的 [1m] 后缀与
//! env.CLAUDE_CODE_MAX_CONTEXT_TOKENS。凡读不出/解析不了/查不到尺寸，一律跳过不写。

use crate::config::catalog::ProviderCatalog;
use crate::config::AppConfig;
use crate::proxy::health::LocalNow;
use crate::proxy::strategies::profile_start_model;

pub const ENV_KEY: &str = "CLAUDE_CODE_MAX_CONTEXT_TOKENS";
pub const ONE_M: u32 = 1_000_000;

/// OpenCode limit.output 的保守回退值：catalog 未收录该模型的 output_size、而 limit
/// 又必须声明 output（OpenCode 配置校验要求）时使用。
pub const OC_FALLBACK_OUTPUT: u32 = 8192;

#[derive(Debug, Clone, PartialEq)]
pub enum EntryLocation { Model, Env(&'static str) }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeEntries { pub entries: Vec<(EntryLocation, String)> }

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyncPlan {
    pub rewrites: Vec<(EntryLocation, String, String)>,
    pub env_value: Option<u32>,
    pub notes: Vec<String>,
}

pub fn claude_settings_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude").join("settings.json")
}

/// ~/.config/opencode/opencode.jsonc — OpenCode 全局配置（Windows/Linux 同路径，
/// 已对照本机安装验证）。Task 2/3 的同步轮消费。
pub fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config").join("opencode").join("opencode.jsonc")
}

/// JSONC → JSON 文本：去掉 `//` 行注释、`/* */` 块注释（识别字符串字面量）与
/// `}` / `]` 前的尾随逗号。仅当块注释或字符串字面量未闭合时返回 None。
/// 回写是纯 JSON——用户注释会丢失（spec §4 已接受）。
/// 按 `char`（而非字节）推进：配置含中文时按字节 `as char` 逐拷会得到乱码。
fn strip_jsonc(text: &str) -> Option<String> {
    #[derive(PartialEq)]
    enum St {
        Code,
        Str,
        Line,
        Block,
    }
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    let mut it = text.chars().peekable();
    let mut st = St::Code;
    // `out` 中最近一个待定逗号的下标（尾随逗号候选）。注释与空白不清除它
    // （二者都会消失或保持惰性）；任何其他代码内容（字符串、数字、括号）
    // 才清除。遇到 `}` / `]` 时若仍待定，则它必然是尾随逗号。
    let mut pending_comma: Option<usize> = None;

    while let Some(c) = it.next() {
        match st {
            St::Code => match c {
                '"' => {
                    pending_comma = None;
                    st = St::Str;
                    out.push('"');
                }
                '/' => match it.peek() {
                    Some('/') => {
                        it.next();
                        st = St::Line;
                    }
                    Some('*') => {
                        it.next();
                        st = St::Block;
                    }
                    // 孤立 `/`：非注释起点，按普通代码内容透传
                    _ => {
                        pending_comma = None;
                        out.push('/');
                    }
                },
                ',' => {
                    pending_comma = Some(out.len());
                    out.push(',');
                }
                '}' | ']' => {
                    // 自上一个待定逗号以来，能合法出现在容器内的内容要么是
                    // 空白（透传）、要么是注释（删除）——注释不清除 pending——
                    // 所以此刻仍待定的逗号按构造即尾随逗号，删之。
                    if let Some(pos) = pending_comma.take() {
                        out.remove(pos);
                    }
                    out.push(c);
                }
                _ => {
                    if !c.is_whitespace() {
                        pending_comma = None;
                    }
                    out.push(c);
                }
            },
            St::Str => {
                out.push(c);
                if c == '\\' {
                    // 转义对原样拷贝（含 \" —— 字符串继续）
                    if let Some(c2) = it.next() {
                        out.push(c2);
                    }
                } else if c == '"' {
                    st = St::Code;
                }
            }
            St::Line => {
                if c == '\n' {
                    st = St::Code;
                    out.push('\n');
                }
            }
            St::Block => {
                if c == '*' && it.peek() == Some(&'/') {
                    it.next();
                    st = St::Code;
                } else if c == '\n' {
                    out.push('\n'); // 保持行结构
                }
            }
        }
    }
    if st == St::Block || st == St::Str {
        return None; // 未闭合
    }
    Some(out.into_iter().collect())
}

/// Strip a trailing `[1m]` (case-insensitive). `[2m]` and non-suffixed names pass through.
/// Mirrors Claude Code's own suffix semantics (`[1m]` = hard 1M declaration).
pub fn strip_1m(name: &str) -> &str {
    for suffix in ["[1m]", "[1M]"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped;
        }
    }
    name
}

/// Claude Code env keys that carry model names, in collection order.
const ENV_MODEL_KEYS: [&str; 4] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// Collect the model-name entries Claude Code is configured with. Source order:
/// top-level `model`, then the four env keys in ENV_MODEL_KEYS order. Deduplicated by
/// exact raw string (first occurrence wins for `EntryLocation`); non-string and empty
/// values ignored; a non-object `env` means env keys are skipped but `model` is kept.
pub fn collect_entries(json: &serde_json::Value) -> ClaudeEntries {
    let mut out: Vec<(EntryLocation, String)> = vec![];
    // HashSet<String> (not &str): a borrowed `&str` from the closure parameter cannot
    // escape into the captured set (E0521); owned keys dedupe identically.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |loc: EntryLocation, v: Option<&serde_json::Value>| {
        if let Some(serde_json::Value::String(s)) = v {
            if !s.is_empty() && seen.insert(s.clone()) {
                out.push((loc, s.clone()));
            }
        }
    };
    push(EntryLocation::Model, json.get("model"));
    if let Some(serde_json::Value::Object(env)) = json.get("env") {
        for key in ENV_MODEL_KEYS {
            push(EntryLocation::Env(key), env.get(key));
        }
    }
    ClaudeEntries { entries: out }
}

/// A resolved route's catalog limits.
struct RouteLimits {
    context: u32,
    /// catalog 未知 output 时为 None（OpenCode 轮退到保守默认/保留用户手写值）。
    output: Option<u32>,
}

/// Resolve one base model name (already `[1m]`-stripped) to its primary route model's
/// real catalog limits. Err(carries vendor+upstream) when the route resolves but the
/// catalog lacks a context size; Ok(None) when the name matches no profile (or dangling refs).
fn resolve_limits(
    cfg: &AppConfig,
    catalog: &ProviderCatalog,
    base: &str,
    now: &LocalNow,
) -> Result<Option<RouteLimits>, (String, String)> {
    let Some(prof) = cfg.profiles.iter().find(|p| {
        p.name == base || p.aliases.iter().any(|a| a == base)
    }) else {
        return Ok(None);
    };
    let (model_id, _via) = profile_start_model(prof, cfg, now);
    let Some(model) = cfg.models.iter().find(|m| m.id == model_id) else {
        return Ok(None);
    };
    let vendor = cfg
        .providers
        .iter()
        .find(|p| p.id == model.provider_id)
        .map(|p| p.vendor.clone())
        .unwrap_or_default();
    match catalog.context_size(&vendor, &model.upstream_model_id) {
        Some(context) => Ok(Some(RouteLimits {
            context,
            output: catalog.output_size(&vendor, &model.upstream_model_id),
        })),
        // Err 带上 vendor+upstream（日志惯例：可脱离本机配置识别厂商与模型）。
        None => Err((vendor, model.upstream_model_id.clone())),
    }
}

/// Compute the full round plan. Spec §5 steps 3-5.
pub fn compute_plan(
    cfg: &AppConfig,
    catalog: &ProviderCatalog,
    entries: &ClaudeEntries,
    now: &LocalNow,
) -> SyncPlan {
    let mut plan = SyncPlan::default();
    let mut env_candidates: Vec<u32> = vec![];

    for (loc, raw) in &entries.entries {
        let base = strip_1m(raw).to_string();
        let size = match resolve_limits(cfg, catalog, &base, now) {
            Ok(Some(l)) => l.context,
            Ok(None) => {
                plan.notes.push(format!("skip '{raw}'（未匹配任何路由）"));
                continue;
            }
            Err((vendor, upstream)) => {
                plan.notes.push(format!(
                    "skip '{raw}'（{vendor}/{upstream} 上下文大小未知）"
                ));
                continue;
            }
        };
        let has_1m = base.len() < raw.len();
        match (has_1m, size >= ONE_M) {
            (true, true) | (false, false) => {} // declaration already correct
            (true, false) => {
                plan.rewrites.push((loc.clone(), raw.clone(), base.clone()));
                env_candidates.push(size);
            }
            (false, true) => {
                plan.rewrites.push((loc.clone(), raw.clone(), format!("{base}[1m]")));
            }
        }
        // env 候选 = 改写后仍无 [1m] 的入口。(false,true) 分支刚被改写为带 [1m]，
        // 改写后不再是 [1m]-less，不参与 env 计算（SyncPlan.env_value 语义：改写后）。
        if !has_1m && size < ONE_M {
            env_candidates.push(size);
        }
    }

    plan.env_value = env_candidates.iter().copied().min();
    plan
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("settings.json 解析失败，跳过本轮")]
    Malformed,
    #[error("写入失败: {0}")]
    Io(#[from] std::io::Error),
}

/// True when `json[loc]` currently equals `raw`.
fn location_holds(json: &serde_json::Value, loc: &EntryLocation, raw: &str) -> bool {
    match loc {
        EntryLocation::Model => json.get("model").and_then(|v| v.as_str()) == Some(raw),
        EntryLocation::Env(key) => json
            .get("env")
            .and_then(|e| e.get(*key))
            .and_then(|v| v.as_str())
            == Some(raw),
    }
}

fn set_at_location(json: &mut serde_json::Value, loc: &EntryLocation, new_name: &str) {
    match loc {
        EntryLocation::Model => {
            if json.get("model").map(|v| v.is_string()).unwrap_or(false) {
                json["model"] = serde_json::Value::String(new_name.to_string());
            }
        }
        EntryLocation::Env(key) => {
            if let Some(serde_json::Value::Object(env)) = json.get_mut("env") {
                if env.get(*key).map(|v| v.is_string()).unwrap_or(false) {
                    env.insert(
                        key.to_string(),
                        serde_json::Value::String(new_name.to_string()),
                    );
                }
            }
        }
    }
}

/// All five model-name locations, in `collect_entries` order. A plan rewrite tuple
/// carries only the FIRST location that held the raw name (`collect_entries` dedups
/// by raw string), so application must scan every location and rewrite each one that
/// still holds the raw name — the same name may live in several places at once.
fn all_model_locations() -> [EntryLocation; 5] {
    [
        EntryLocation::Model,
        EntryLocation::Env(ENV_MODEL_KEYS[0]),
        EntryLocation::Env(ENV_MODEL_KEYS[1]),
        EntryLocation::Env(ENV_MODEL_KEYS[2]),
        EntryLocation::Env(ENV_MODEL_KEYS[3]),
    ]
}

/// Spec §5 step 6: read-modify-write on the parsed value. Rewrites every location
/// currently holding a raw name named by `plan.rewrites` (not just the tuple's first
/// location) and the single `ENV_KEY` entry. Returns whether anything changed.
/// `last_written_env`: what THIS FEATURE last wrote to ENV_KEY (None = never wrote).
/// Managed-delete: when `plan.env_value` is None and the on-disk value equals
/// `last_written_env`, the key is removed; a user-authored value is never touched.
pub fn apply_plan(
    json: &mut serde_json::Value,
    plan: &SyncPlan,
    last_written_env: Option<&str>,
) -> Result<bool, SyncError> {
    let mut changed = false;

    for (_loc, raw, new_name) in &plan.rewrites {
        for loc in all_model_locations() {
            if location_holds(json, &loc, raw) {
                set_at_location(json, &loc, new_name);
                changed = true;
            }
        }
    }

    let want = plan.env_value.map(|v| v.to_string());
    match want {
        Some(want) => {
            let current = json
                .get("env")
                .and_then(|e| e.get(ENV_KEY))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if current.as_deref() != Some(want.as_str()) {
                let env = match json.get_mut("env") {
                    Some(serde_json::Value::Object(env)) => env,
                    _ => {
                        // absent or non-object: only create when absent
                        if json.get("env").is_some() {
                            return Ok(changed); // non-object env: never touch
                        }
                        json.as_object_mut()
                            .ok_or(SyncError::Malformed)?
                            .insert("env".into(), serde_json::Value::Object(Default::default()));
                        json.get_mut("env").unwrap().as_object_mut().unwrap()
                    }
                };
                env.insert(ENV_KEY.into(), serde_json::Value::String(want));
                changed = true;
            }
        }
        None => {
            // Managed-delete: only a value this feature wrote may be removed.
            if let Some(last) = last_written_env {
                let current = json
                    .get("env")
                    .and_then(|e| e.get(ENV_KEY))
                    .and_then(|v| v.as_str());
                if current == Some(last) {
                    if let Some(serde_json::Value::Object(env)) = json.get_mut("env") {
                        env.remove(ENV_KEY);
                        changed = true;
                    }
                }
            }
        }
    }

    Ok(changed)
}

// ---- 状态记录（仅内存，重启即忘，by design；Task 5 经 tauri app.manage 暴露）----

#[derive(Debug, Clone, serde::Serialize)]
pub struct LastRound {
    pub at: String,              // RFC3339 本地时间
    pub action: String,          // "written" | "no_change" | "skipped"
    pub detail: String,          // 人类可读摘要（notes / 写入集 / 跳过原因）
}

#[derive(Default)]
pub struct SyncBookkeeping {
    inner: std::sync::Mutex<BookkeepingInner>,
}

#[derive(Default)]
struct BookkeepingInner {
    /// Claude 目标专用：本功能最后一次写入的 ENV_KEY 值（受管删除的判定依据）。
    last_written_env: Option<String>,
    /// Claude 目标专用：上一轮跳过原因集合（拼接串），"只在变化时 warn"去重。
    last_notes: Option<String>,
    /// OpenCode 目标专用：上一轮跳过原因集合（拼接串），与 Claude 的 last_notes 独立。
    oc_notes: Option<String>,
    claude_round: Option<LastRound>,
    opencode_round: Option<LastRound>,
}

impl SyncBookkeeping {
    pub fn record_written_env(&self, v: Option<String>) {
        self.inner.lock().unwrap().last_written_env = v;
    }
    pub fn record_notes(&self, v: Option<String>) {
        self.inner.lock().unwrap().last_notes = v;
    }
    pub fn record_oc_notes(&self, v: Option<String>) {
        self.inner.lock().unwrap().oc_notes = v;
    }
    pub fn last_written_env(&self) -> Option<String> {
        self.inner.lock().unwrap().last_written_env.clone()
    }
    pub fn last_notes(&self) -> Option<String> {
        self.inner.lock().unwrap().last_notes.clone()
    }
    pub fn oc_notes(&self) -> Option<String> {
        self.inner.lock().unwrap().oc_notes.clone()
    }
    pub fn record_claude_round(&self, r: LastRound) {
        self.inner.lock().unwrap().claude_round = Some(r);
    }
    pub fn record_opencode_round(&self, r: LastRound) {
        self.inner.lock().unwrap().opencode_round = Some(r);
    }
    pub fn snapshot_claude(&self) -> Option<LastRound> {
        self.inner.lock().unwrap().claude_round.clone()
    }
    pub fn snapshot_opencode(&self) -> Option<LastRound> {
        self.inner.lock().unwrap().opencode_round.clone()
    }
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

/// Resolve the user's home dir (cross-platform, no new dep). None if neither env var set.
pub fn home_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("HOME").map(std::path::PathBuf::from)
    }
}

/// One round, parameterized by the settings path (tests pass a tempdir path).
/// Every failure path records a skipped round and returns quietly — the background
/// loop must never die on an error.
pub async fn sync_round(
    state: &crate::proxy::AppState,
    status: &SyncBookkeeping,
    path: &std::path::Path,
) {
    let (cfg, catalog, now) = {
        let cfg = state.config.read().await;
        if !cfg.settings.sync_claude_context {
            return; // 开关关 → 什么都不做，什么都不碰
        }
        let catalog = state.catalog.read().await;
        (cfg.clone(), catalog.clone(), state.clock.now_local())
    };

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 未安装 Claude Code / 文件不存在：正常情况，静默跳过（debug 级日志）。
            tracing::debug!(target: "switchlm::sync", "settings.json 不存在，跳过同步");
            return;
        }
        Err(e) => {
            // 权限等真实读错误：与"未安装"区分开，让设置页能看到（记录 skipped 轮）。
            tracing::warn!(target: "switchlm::sync", "settings.json 读取失败（文件未改动）: {e}");
            status.record_claude_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("settings.json 读取失败: {e}"),
            });
            return;
        }
    };
    let mut json: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "settings.json 解析失败，跳过本轮（文件未改动）: {e}");
            status.record_claude_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "settings.json 解析失败".into(),
            });
            return;
        }
    };

    // FORWARD requirement (Task 2 review, spec §5 step 1): `env` present but not an
    // object → skip the whole round (file untouched). We can only manage a key inside
    // a real env object; any other shape means we don't understand this file.
    if let Some(env) = json.get("env") {
        if !env.is_object() {
            tracing::warn!(target: "switchlm::sync", "settings.json 的 env 字段不是对象，跳过本轮（文件未改动）");
            status.record_claude_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "settings.json 的 env 字段不是对象".into(),
            });
            return;
        }
    }

    let entries = collect_entries(&json);
    let plan = compute_plan(&cfg, &catalog, &entries, &now);
    // 跳过原因只在集合变化时 warn（否则 30s 一轮会对同一问题刷 2880 条/天）；
    // 集合本身总是并入本轮 detail，让设置页能看到"为什么没同步某个入口"。
    {
        let note_set = plan.notes.join(" | ");
        let notes_changed = status.last_notes().as_deref() != Some(note_set.as_str());
        if notes_changed && !plan.notes.is_empty() {
            for note in &plan.notes {
                tracing::warn!(target: "switchlm::sync", "{note}");
            }
        }
        status.record_notes(if plan.notes.is_empty() { None } else { Some(note_set) });
    }
    let notes_detail = |base: &str| -> String {
        match status.last_notes() {
            Some(n) if !n.is_empty() => format!("{base}（{n}）"),
            _ => base.to_string(),
        }
    };
    let last_env = status.last_written_env();
    match apply_plan(&mut json, &plan, last_env.as_deref()) {
        Ok(false) => {
            status.record_claude_round(LastRound {
                at: now_rfc3339(), action: "no_change".into(),
                detail: notes_detail("无变化"),
            });
        }
        Ok(true) => {
            // Value→String 序列化实际不可失败；万一失败也绝不写空文件（skip 而非清空）。
            let body = match serde_json::to_string_pretty(&json) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(target: "switchlm::sync", "settings.json 序列化失败（文件未改动）: {e}");
                    status.record_claude_round(LastRound {
                        at: now_rfc3339(), action: "skipped".into(),
                        detail: format!("settings.json 序列化失败: {e}"),
                    });
                    return;
                }
            };
            match std::fs::write(path, body) {
                Ok(()) => {
                    tracing::info!(target: "switchlm::sync", path = %path.display(), "已同步 Claude Code 上下文声明");
                    status.record_written_env(plan.env_value.map(|v| v.to_string()));
                    status.record_claude_round(LastRound {
                        at: now_rfc3339(), action: "written".into(),
                        detail: notes_detail(&format!(
                            "重写 {} 项, env={:?}",
                            plan.rewrites.len(),
                            plan.env_value.map(|v| v.to_string())
                        )),
                    });
                }
                Err(e) => {
                    tracing::warn!(target: "switchlm::sync", "写入失败（下轮重试）: {e}");
                    status.record_claude_round(LastRound {
                        at: now_rfc3339(), action: "skipped".into(),
                        detail: format!("写入失败: {e}"),
                    });
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "应用同步计划失败: {e}");
        }
    }
}

/// Run one round against the real user path.
pub async fn sync_once(state: &crate::proxy::AppState, status: &SyncBookkeeping) {
    if let Some(home) = home_dir() {
        sync_round(state, status, &claude_settings_path(&home)).await;
    }
}

/// Provider ids in an OpenCode config whose `options.baseURL` targets the local
/// proxy. Matching: baseURL == `http://127.0.0.1:{port}/v1` OR starts with
/// `http://127.0.0.1:{port}` (covers a bare root or deeper path). `None` = the
/// `provider` section exists but isn't an object (caller skips the round);
/// `Some(vec![])` = no provider matches (normal — nothing to do).
fn switchlm_providers(json: &serde_json::Value, port: u16) -> Option<Vec<String>> {
    let providers = match json.get("provider") {
        None => return Some(vec![]),
        Some(serde_json::Value::Object(p)) => p,
        Some(_) => return None,
    };
    let exact = format!("http://127.0.0.1:{port}/v1");
    let prefix = format!("http://127.0.0.1:{port}");
    let mut out = vec![];
    for (id, v) in providers {
        let base = v
            .get("options")
            .and_then(|o| o.get("baseURL"))
            .and_then(|b| b.as_str());
        if let Some(b) = base {
            if b == exact || b.starts_with(&prefix) {
                out.push(id.clone());
            }
        }
    }
    Some(out)
}

/// One OpenCode round. Same contract family as `sync_round` (Claude target): every
/// failure path records a skipped round and returns quietly.
pub async fn sync_opencode_round(
    state: &crate::proxy::AppState,
    status: &SyncBookkeeping,
    path: &std::path::Path,
) {
    let (cfg, catalog, now, port) = {
        let cfg = state.config.read().await;
        if !cfg.settings.sync_claude_context {
            return; // 单一总开关：关 → 两个目标都不碰
        }
        let port = state.bound_port();
        if port.is_none() {
            // 代理未绑定端口：无法识别 SwitchLM provider，本轮跳过（Claude 轮不受影响）。
            tracing::debug!(target: "switchlm::sync", "代理端口未就绪，跳过 OpenCode 同步");
            return;
        }
        let catalog = state.catalog.read().await;
        (cfg.clone(), catalog.clone(), state.clock.now_local(), port.unwrap())
    };

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(target: "switchlm::sync", "opencode.jsonc 不存在，跳过同步");
            return;
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 读取失败（文件未改动）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("opencode.jsonc 读取失败: {e}"),
            });
            return;
        }
    };

    let stripped = match strip_jsonc(&text).and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        Some(v) => v,
        None => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 解析失败，跳过本轮（文件未改动）");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "opencode.jsonc 解析失败".into(),
            });
            return;
        }
    };
    let mut json = stripped;

    let targets = match switchlm_providers(&json, port) {
        Some(t) => t,
        None => {
            tracing::warn!(target: "switchlm::sync", "opencode 配置的 provider 字段不是对象，跳过本轮（文件未改动）");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: "provider 字段不是对象".into(),
            });
            return;
        }
    };

    // 计划：对每个 SwitchLM provider 的每个模型条目求目标 limit.context / limit.output。
    struct EntryEdit { provider: String, model: String, size: u32, output: Option<u32> }
    let mut edits: Vec<EntryEdit> = vec![];
    let mut notes: Vec<String> = vec![];
    for pid in &targets {
        let models = json
            .get("provider").and_then(|p| p.get(pid))
            .and_then(|p| p.get("models"))
            .and_then(|m| m.as_object());
        let Some(models) = models else {
            continue; // models 非对象/缺失 → 该 provider 静默跳过（正常形态）
        };
        for key in models.keys() {
            match resolve_limits(&cfg, &catalog, key, &now) {
                Ok(Some(l)) => edits.push(EntryEdit {
                    provider: pid.clone(), model: key.clone(), size: l.context, output: l.output,
                }),
                Ok(None) => notes.push(format!("skip '{key}'（未匹配任何路由）")),
                Err((vendor, upstream)) => notes.push(format!(
                    "skip '{key}'（{vendor}/{upstream} 上下文大小未知）"
                )),
            }
        }
    }

    // 应用（改写 limit.context 并保证 limit.output 同时声明；不动现有的 input）。
    let mut changed = false;
    for e in &edits {
        let limit = json
            .get_mut("provider").and_then(|p| p.get_mut(&e.provider))
            .and_then(|p| p.get_mut("models"))
            .and_then(|m| m.get_mut(&e.model))
            .and_then(|m| m.get_mut("limit"));
        match limit {
            Some(serde_json::Value::Object(l)) => {
                if l.get("context").and_then(|c| c.as_u64()) != Some(e.size as u64) {
                    l.insert("context".into(), serde_json::Value::from(e.size));
                    changed = true;
                }
                // OpenCode 配置校验要求 limit 必须同时声明 output。catalog 已知时写
                // catalog 值；未知时只补缺（回退 OC_FALLBACK_OUTPUT），不覆盖用户手写值。
                let cur_output = l.get("output").and_then(|o| o.as_u64());
                let want_output = match e.output {
                    Some(w) => Some(w as u64),
                    None if cur_output.is_none() => Some(OC_FALLBACK_OUTPUT as u64),
                    None => None, // catalog 未知且用户已手写 -> 保留
                };
                if want_output.is_some() && cur_output != want_output {
                    l.insert("output".into(), serde_json::Value::from(want_output.unwrap()));
                    changed = true;
                }
            }
            _ => {
                // limit 缺失（或非对象——非对象不动，视为用户手写异形）：
                // OpenCode 配置验证要求 limit 必须包含 output 字段
                if let Some(m) = json
                    .get_mut("provider").and_then(|p| p.get_mut(&e.provider))
                    .and_then(|p| p.get_mut("models"))
                    .and_then(|m| m.get_mut(&e.model))
                    .and_then(|m| m.as_object_mut())
                {
                    if !m.contains_key("limit") {
                        // 创建包含 context 和 output 的 limit 对象；output 未知时回退保守默认值
                        m.insert("limit".into(), serde_json::json!({
                            "context": e.size,
                            "output": e.output.unwrap_or(OC_FALLBACK_OUTPUT)
                        }));
                        changed = true;
                    }
                }
            }
        }
    }

    // notes 去重 warn + 并入 detail（与 Claude 轮相同机制；OpenCode 轮单独记）。
    {
        let note_set = notes.join(" | ");
        let changed_notes = status.oc_notes().as_deref() != Some(note_set.as_str());
        if changed_notes && !notes.is_empty() {
            for n in &notes {
                tracing::warn!(target: "switchlm::sync", "{n}");
            }
        }
        status.record_oc_notes(if notes.is_empty() { None } else { Some(note_set) });
    }
    let notes_detail = |base: &str| -> String {
        match status.oc_notes() {
            Some(n) if !n.is_empty() => format!("{base}（{n}）"),
            _ => base.to_string(),
        }
    };

    if !changed {
        status.record_opencode_round(LastRound {
            at: now_rfc3339(), action: "no_change".into(), detail: notes_detail("无变化"),
        });
        return;
    }

    // 写回：纯 JSON pretty（注释丢失，spec §4 已接受）。序列化不可失败；万一失败
    // 也绝不写空文件。
    let body = match serde_json::to_string_pretty(&json) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "opencode.jsonc 序列化失败（文件未改动）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("opencode.jsonc 序列化失败: {e}"),
            });
            return;
        }
    };
    match std::fs::write(path, body) {
        Ok(()) => {
            tracing::info!(target: "switchlm::sync", path = %path.display(), "已同步 OpenCode 上下文声明");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "written".into(),
                detail: notes_detail(&format!("写入 {} 项 limit.context/output", edits.len())),
            });
        }
        Err(e) => {
            tracing::warn!(target: "switchlm::sync", "写入失败（下轮重试）: {e}");
            status.record_opencode_round(LastRound {
                at: now_rfc3339(), action: "skipped".into(),
                detail: format!("写入失败: {e}"),
            });
        }
    }
}

/// Run one OpenCode round against the real user path.
pub async fn sync_opencode_once(state: &crate::proxy::AppState, status: &SyncBookkeeping) {
    if let Some(home) = home_dir() {
        sync_opencode_round(state, status, &opencode_config_path(&home)).await;
    }
}

/// Background loop: immediate run + every 30s. Toggle checked inside each round
/// (a flip is honored within one tick). Errors never escape `sync_once`.
pub fn spawn_sync_task(state: crate::proxy::AppState, status: std::sync::Arc<SyncBookkeeping>) {
    tauri::async_runtime::spawn(async move {
        loop {
            sync_once(&state, &status).await;
            sync_opencode_once(&state, &status).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;
    use serde_json::json;

    // ---- strip_1m ----

    #[test]
    fn strip_1m_variants() {
        assert_eq!(strip_1m("pro[1m]"), "pro");
        assert_eq!(strip_1m("pro[1M]"), "pro");
        assert_eq!(strip_1m("pro"), "pro");
        assert_eq!(strip_1m("pro[2m]"), "pro[2m]"); // 2m NOT stripped
        assert_eq!(strip_1m("[1m]"), "");           // degenerate but well-defined
        assert_eq!(strip_1m(""), "");
    }

    // ---- claude_settings_path ----

    #[test]
    fn settings_path_joins_dot_claude() {
        use std::path::Path;
        let p = claude_settings_path(Path::new("/home/u"));
        assert!(p.ends_with(".claude/settings.json") || p.to_string_lossy().contains(".claude"));
    }

    // ---- strip_jsonc / opencode_config_path ----

    #[test]
    fn opencode_path_joins_config_dir() {
        use std::path::Path;
        let p = opencode_config_path(Path::new("/home/u"));
        assert!(p.ends_with(".config/opencode/opencode.jsonc")
            || p.to_string_lossy().contains("opencode.jsonc"));
    }

    #[test]
    fn strip_jsonc_passthrough_plain_json() {
        let s = r#"{"a":1,"b":[1,2],"c":{"d":"x"}}"#;
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }

    #[test]
    fn strip_jsonc_line_and_block_comments() {
        let s = "{\n// line\n\"a\": 1, /* block */ \"b\": 2\n}";
        assert_eq!(strip_jsonc(s).unwrap(), "{\n\n\"a\": 1,  \"b\": 2\n}");
    }

    #[test]
    fn strip_jsonc_preserves_slashes_inside_strings() {
        let s = r#"{"url":"http://x//y","re":"a/*b*/c"}"#;
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }

    #[test]
    fn strip_jsonc_trailing_commas() {
        assert_eq!(strip_jsonc(r#"{"a":1,}"#).unwrap(), r#"{"a":1}"#);
        assert_eq!(strip_jsonc(r#"{"a":[1,2,3,],}"#).unwrap(), r#"{"a":[1,2,3]}"#);
        // comma before a comment that precedes } still trailing — exact whitespace
        // here is incidental, so assert parse-equivalence instead (brief note).
        let stripped = strip_jsonc("{\"a\":1, // c\n}").unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stripped).unwrap(),
            json!({"a": 1})
        );
    }

    #[test]
    fn strip_jsonc_unterminated_is_none() {
        assert!(strip_jsonc("{\"a\": /* never closed").is_none());
        assert!(strip_jsonc("{\"a\": \"never closed").is_none());
    }

    #[test]
    fn strip_jsonc_comment_char_inside_string_not_comment() {
        // "//" inside a string must not start a comment; string content kept verbatim.
        let s = "{\"k\": \"va//lue\"}";
        assert_eq!(strip_jsonc(s).unwrap(), s);
    }

    #[test]
    fn strip_jsonc_semantics_via_parse() {
        for (input, expect) in [
            ("{\n// line\n\"a\": 1, /* block */ \"b\": 2\n}", json!({"a":1,"b":2})),
            ("{\"a\":1, // c\n}", json!({"a":1})),
            ("{\"a\":[1,2,3,],}", json!({"a":[1,2,3]})),
        ] {
            let stripped = strip_jsonc(input).unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&stripped).unwrap(), expect);
        }
    }

    #[test]
    fn strip_jsonc_preserves_multibyte_string_content() {
        // CJK string content must round-trip byte-exact: byte-at-a-time `as char`
        // copying would emit mojibake (U+0080..U+00FF per byte).
        let s = "{\"名字\":\"模型\",\"c\":\"é🎉\"}";
        assert_eq!(strip_jsonc(s).unwrap(), s);
        let commented = "{\n  // 中文注释\n  \"名字\": \"模型\"\n}";
        let stripped = strip_jsonc(commented).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stripped).unwrap(),
            json!({"名字": "模型"})
        );
    }

    // ---- collect_entries ----

    #[test]
    fn collect_all_five_sources_deduped() {
        let j = json!({
            "model": "opus",
            "env": {
                "ANTHROPIC_MODEL": "pro[1m]",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "pro[1m]",   // dup raw string → one entry
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "flash",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "",         // empty → ignored
                "ANTHROPIC_BASE_URL": "http://x",            // not a model key → ignored
                "ANTHROPIC_AUTH_TOKEN": 42,                  // non-string → ignored
            },
            "other": "noise"
        });
        let c = collect_entries(&j);
        let names: Vec<&str> = c.entries.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["opus", "pro[1m]", "flash"]); // source order: model first
        assert_eq!(c.entries[0].0, EntryLocation::Model);
        assert!(matches!(c.entries[1].0, EntryLocation::Env("ANTHROPIC_MODEL")));
    }

    #[test]
    fn collect_tolerates_missing_keys_and_env() {
        let c = collect_entries(&json!({}));
        assert!(c.entries.is_empty());
        let c2 = collect_entries(&json!({"model": "opus"})); // no env object at all
        assert_eq!(c2.entries.len(), 1);
        let c3 = collect_entries(&json!({"model": "a", "env": "not-an-object"}));
        assert_eq!(c3.entries.len(), 1, "env present but not an object: env keys skipped, model kept");
    }

    // ---- compute_plan ----
    // Shared fixture: two profiles across context tiers.
    //   Profile "pro"  (alias "opus")  → m_small (zhipu/glm-4.6, 200k)
    //   Profile "flash"               → m_big (volcengine-coding/glm-5.2, 1M)

    fn plan_cfg() -> AppConfig {
        AppConfig {
            providers: vec![
                Provider { id: "p1".into(), vendor: "zhipu".into(), display_name: "智谱".into(),
                           openai_base_url: None, anthropic_base_url: None, usage_creds: None },
                Provider { id: "p2".into(), vendor: "volcengine-coding".into(), display_name: "火山".into(),
                           openai_base_url: None, anthropic_base_url: None, usage_creds: None },
            ],
            models: vec![
                Model { id: "m_small".into(), provider_id: "p1".into(),
                        upstream_model_id: "glm-4.6".into(), ..Default::default() },
                Model { id: "m_big".into(), provider_id: "p2".into(),
                        upstream_model_id: "glm-5.2".into(), ..Default::default() },
            ],
            profiles: vec![
                Profile { id: "pf1".into(), name: "pro".into(), aliases: vec!["opus".into()],
                          backing_model_id: "m_small".into(), ..Default::default() },
                Profile { id: "pf2".into(), name: "flash".into(), aliases: vec![],
                          backing_model_id: "m_big".into(), ..Default::default() },
            ],
            ..Default::default()
        }
    }

    fn plan_catalog() -> ProviderCatalog {
        serde_json::from_str(r#"{"version":1,"providers":[
            {"provider_id":"zhipu","models":[{"upstream_model_id":"glm-4.6","context_size":200000,"output_size":128000}]},
            {"provider_id":"volcengine-coding","models":[{"upstream_model_id":"glm-5.2","context_size":1000000,"output_size":131072}]}
        ]}"#).unwrap()
    }

    const NOW: LocalNow = LocalNow { weekday: 1, minute: 0 };

    #[test]
    fn plan_four_rewrite_branches() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "opus".into()),          // alias → 200k, no [1m] → keep, env candidate 200k
            (EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"), "pro[1m]".into()), // 200k → strip
            (EntryLocation::Env("ANTHROPIC_DEFAULT_HAIKU_MODEL"), "flash".into()),    // 1M → append
            (EntryLocation::Env("ANTHROPIC_DEFAULT_OPUS_MODEL"), "flash[1m]".into()), // 1M → keep
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        // rewrite set: "pro[1m]"→"pro", "flash"→"flash[1m]"; kept: "opus", "flash[1m]"
        let mut rw: Vec<_> = plan.rewrites.iter().map(|(_loc, from, to)| (from.clone(), to.clone())).collect();
        rw.sort();
        assert_eq!(rw, vec![("flash".to_string(), "flash[1m]".to_string()),
                            ("pro[1m]".to_string(), "pro".to_string())]);
        // [1m]-less after rewrite: "opus" (200k), "pro" (200k) → min = 200k
        assert_eq!(plan.env_value, Some(200_000));
    }

    #[test]
    fn plan_all_1m_entries_env_none() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "flash[1m]".into()),
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        assert!(plan.rewrites.is_empty());
        assert_eq!(plan.env_value, None, "no [1m]-less entry → no env write");
    }

    #[test]
    fn plan_unresolvable_and_unknown_size_skip() {
        let cfg = plan_cfg();
        let cat = plan_catalog(); // lacks glm-9.9 size
        // add a model with no catalog entry, backing a profile
        let mut cfg2 = cfg.clone();
        cfg2.models.push(Model { id: "m_unknown".into(), provider_id: "p1".into(),
                                 upstream_model_id: "glm-9.9".into(), ..Default::default() });
        cfg2.profiles.push(Profile { id: "pf3".into(), name: "weird".into(), aliases: vec![],
                                     backing_model_id: "m_unknown".into(), ..Default::default() });
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "no-such-profile".into()), // Skip: unresolvable
            (EntryLocation::Env("ANTHROPIC_MODEL"), "weird".into()), // Skip: no catalog size
            (EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"), "opus".into()), // normal
        ]};
        let plan = compute_plan(&cfg2, &cat, &entries, &NOW);
        assert!(plan.rewrites.is_empty(), "skipped entries never rewritten");
        assert_eq!(plan.env_value, Some(200_000), "skips excluded from env calc");
        assert_eq!(plan.notes.len(), 2, "one note per skip, with reason");
    }

    #[test]
    fn plan_env_takes_min_across_mixed() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![
            (EntryLocation::Model, "opus".into()),   // 200k, no suffix
            (EntryLocation::Env("ANTHROPIC_MODEL"), "flash".into()), // → flash[1m], excluded
        ]};
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        assert_eq!(plan.env_value, Some(200_000));
    }

    #[test]
    fn plan_follows_time_strategy_flip() {
        // "pro" has a time strategy routing to m_big on Tuesday; strategy wins over backing.
        let mut cfg = plan_cfg();
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s1".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_big".into(),
            }),
        }];
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![(EntryLocation::Model, "pro".into())] };

        // Monday: backing (200k) → no suffix, env 200k
        let p1 = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 1, minute: 0 });
        assert!(p1.rewrites.is_empty());
        assert_eq!(p1.env_value, Some(200_000));
        // Tuesday: strategy → m_big (1M) → append [1m], env None
        let p2 = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 2, minute: 0 });
        assert_eq!(p2.rewrites, vec![(EntryLocation::Model, "pro".into(), "pro[1m]".into())]);
        assert_eq!(p2.env_value, None);
    }

    #[test]
    fn plan_exact_1m_boundary_keeps_suffix() {
        let cfg = plan_cfg();
        let cat = plan_catalog();
        let entries = ClaudeEntries { entries: vec![(EntryLocation::Model, "flash".into())] };
        let plan = compute_plan(&cfg, &cat, &entries, &NOW);
        // 1_000_000 >= ONE_M → append
        assert_eq!(plan.rewrites, vec![(EntryLocation::Model, "flash".into(), "flash[1m]".into())]);
    }

    // ---- apply_plan ----

    fn settings_doc() -> serde_json::Value {
        serde_json::json!({
            "model": "opus",
            "includeCoAuthoredBy": false,
            "env": {
                "DISABLE_AUTOUPDATER": "1",
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:6950",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro[1m]",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "flash",
                "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "999999"
            },
            "permissions": { "allow": ["Bash(git *)"] }
        })
    }

    fn simple_plan(rewrites: Vec<(EntryLocation, String, String)>, env: Option<u32>) -> SyncPlan {
        SyncPlan { rewrites, env_value: env, notes: vec![] }
    }

    #[test]
    fn apply_rewrites_every_location_holding_the_raw_name() {
        // Two locations hold "pro[1m]": model + one env key (dedup gave one plan entry,
        // but apply must fix both).
        let mut j = serde_json::json!({
            "model": "pro[1m]",
            "env": { "ANTHROPIC_DEFAULT_SONNET_MODEL": "pro[1m]" }
        });
        let plan = simple_plan(
            vec![(EntryLocation::Model, "pro[1m]".into(), "pro".into())],
            Some(200_000),
        );
        let changed = apply_plan(&mut j, &plan, None).unwrap();
        assert!(changed);
        assert_eq!(j["model"], "pro");
        assert_eq!(j["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "pro");
        assert_eq!(j["env"][ENV_KEY], "200000"); // env written as string
    }

    #[test]
    fn apply_preserves_unrelated_keys_and_env_entries() {
        let mut j = settings_doc();
        let plan = simple_plan(
            vec![(EntryLocation::Env("ANTHROPIC_DEFAULT_SONNET_MODEL"),
                  "pro[1m]".into(), "pro".into())],
            Some(200_000),
        );
        apply_plan(&mut j, &plan, None).unwrap();
        assert_eq!(j["includeCoAuthoredBy"], false);
        assert_eq!(j["permissions"]["allow"][0], "Bash(git *)");
        assert_eq!(j["env"]["DISABLE_AUTOUPDATER"], "1");
        assert_eq!(j["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:6950");
        assert_eq!(j["env"]["CLAUDE_CODE_MAX_CONTEXT_TOKENS"], "200000"); // overwritten by plan
    }

    #[test]
    fn apply_creates_env_object_when_absent() {
        let mut j = serde_json::json!({ "model": "opus" });
        let plan = simple_plan(vec![], Some(200_000));
        let changed = apply_plan(&mut j, &plan, None).unwrap();
        assert!(changed);
        assert_eq!(j["env"][ENV_KEY], "200000");
    }

    #[test]
    fn apply_no_change_returns_false() {
        let mut j = serde_json::json!({
            "model": "opus",
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000" }
        });
        // Plan with no rewrites and env already equal → no change.
        let plan = simple_plan(vec![], Some(200_000));
        assert!(!apply_plan(&mut j, &plan, None).unwrap());
    }

    #[test]
    fn apply_managed_delete_only_our_last_value() {
        // env_value None + on-disk == last_written → delete
        let mut j = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "200000", "KEEP": "1" }
        });
        let plan = simple_plan(vec![], None);
        assert!(apply_plan(&mut j, &plan, Some("200000")).unwrap());
        assert!(j["env"].get(ENV_KEY).is_none());
        assert_eq!(j["env"]["KEEP"], "1");
        // env_value None + on-disk == user-authored → untouched
        let mut j2 = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "123456" }
        });
        assert!(!apply_plan(&mut j2, &plan, Some("200000")).unwrap());
        assert_eq!(j2["env"][ENV_KEY], "123456");
        // env_value None + we never wrote → untouched
        let mut j3 = serde_json::json!({
            "env": { "CLAUDE_CODE_MAX_CONTEXT_TOKENS": "123456" }
        });
        assert!(!apply_plan(&mut j3, &plan, None).unwrap());
        assert_eq!(j3["env"][ENV_KEY], "123456");
    }

    #[test]
    fn apply_env_write_also_counts_as_change() {
        // No rewrites, but env key absent and plan wants it → changed.
        let mut j = serde_json::json!({ "model": "opus" });
        let plan = simple_plan(vec![], Some(200_000));
        assert!(apply_plan(&mut j, &plan, None).unwrap());
    }

    #[test]
    fn apply_malformed_shapes_do_not_panic() {
        // "model" not a string, env scalar, root not an object: apply must not panic;
        // rewrites targeting these silently miss, env handling guards.
        let mut j = serde_json::json!({ "model": 42, "env": "oops" });
        let plan = simple_plan(
            vec![(EntryLocation::Model, "42".into(), "x".into())],
            None,
        );
        // No panic; nothing matched → unchanged.
        assert!(!apply_plan(&mut j, &plan, None).unwrap());
    }

    // ---- sync round (异步壳核心，真实文件落在 tempdir) ----
    // sync_round 以 settings 路径为参数注入（真实用户目录只在 sync_once 里解析）。

    use crate::config::{BackendKind, MemoryStore, SecretStoreHandle};
    use crate::proxy::state::{AppState, AppStateInner};
    use std::sync::Arc;

    async fn test_state(cfg: AppConfig) -> AppState {
        Arc::new(AppStateInner {
            config: tokio::sync::RwLock::new(cfg),
            catalog: tokio::sync::RwLock::new(plan_catalog()),
            secrets: SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring),
            health: Default::default(),
            clock: Arc::new(crate::proxy::SystemClock),
            usage_cache: Default::default(),
            bound_port: Default::default(),
            server_handle: Default::default(),
            bind_error: Default::default(),
            polling_handle: Default::default(),
            last_served_provider: Default::default(),
            recorder: Default::default(),
            statistics: None,
        })
    }

    fn round_cfg() -> AppConfig {
        let mut cfg = plan_cfg();
        cfg.settings.sync_claude_context = true;
        cfg
    }

    #[tokio::test]
    async fn round_writes_then_settles() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "model": "pro[1m]",
            "env": { "ANTHROPIC_DEFAULT_SONNET_MODEL": "flash" }
        }).to_string()).unwrap();

        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;

        let j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["model"], "pro", "[1m] stripped (200k)");
        assert_eq!(j["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "flash[1m]");
        assert_eq!(j["env"][ENV_KEY], "200000");
        assert_eq!(status.last_written_env().as_deref(), Some("200000"));
        assert_eq!(status.snapshot_claude().unwrap().action, "written");

        // Second round: nothing changes → file content byte-identical.
        let before = std::fs::read_to_string(&path).unwrap();
        sync_round(&state, &status, &path).await;
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "no-change round must not rewrite");
        assert_eq!(status.snapshot_claude().unwrap().action, "no_change");
    }

    #[tokio::test]
    async fn round_toggle_off_never_touches_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::json!({"model": "pro[1m]"}).to_string();
        std::fs::write(&path, &original).unwrap();

        let mut cfg = round_cfg();
        cfg.settings.sync_claude_context = false;
        let state = test_state(cfg).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(status.snapshot_claude().is_none(), "toggle off → no round recorded");
    }

    #[tokio::test]
    async fn round_corrupt_json_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "{ not json".to_string();
        std::fs::write(&path, &original).unwrap();

        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(status.snapshot_claude().is_some(), "skipped round still recorded");
        assert_eq!(status.snapshot_claude().unwrap().action, "skipped");
    }

    #[tokio::test]
    async fn round_env_not_object_skips_round() {
        // FORWARD requirement (Task 2 review, spec §5 step 1): env present but not an
        // object → skip the whole round, file untouched, recorded as skipped.
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = serde_json::json!({"model": "pro[1m]", "env": "oops"}).to_string();
        std::fs::write(&path, &original).unwrap();

        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(status.snapshot_claude().unwrap().action, "skipped");
    }

    #[tokio::test]
    async fn round_missing_file_is_quiet_skip() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        let state = test_state(round_cfg()).await;
        let status = SyncBookkeeping::default();
        sync_round(&state, &status, &path).await;
        assert!(!path.exists(), "missing file → no file created");
        assert!(status.snapshot_claude().is_none(), "missing file → no round recorded");
    }

    #[test]
    fn bookkeeping_rounds_are_independent() {
        let bk = SyncBookkeeping::default();
        assert!(bk.snapshot_claude().is_none());
        assert!(bk.snapshot_opencode().is_none());
        bk.record_claude_round(LastRound { at: "t1".into(), action: "written".into(), detail: "c".into() });
        bk.record_opencode_round(LastRound { at: "t2".into(), action: "skipped".into(), detail: "o".into() });
        assert_eq!(bk.snapshot_claude().unwrap().action, "written");
        assert_eq!(bk.snapshot_opencode().unwrap().action, "skipped");
    }

    // Direct variant (recommended by the brief): the managed-delete path verified via
    // compute_plan + apply_plan + SyncBookkeeping with two pinned LocalNow values,
    // avoiding clock-injection plumbing through AppState.
    #[test]
    fn managed_delete_after_all_1m_direct() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "model": "pro", "env": { "KEEP": "1" }
        }).to_string()).unwrap();

        let mut cfg = round_cfg();
        cfg.profiles[0].strategies = vec![Strategy {
            id: "s1".into(), priority: 1, enabled: true,
            kind: StrategyKind::Time(TimeStrategy {
                days_of_week: vec![2], time_start: 0, time_end: 1439, model_id: "m_big".into(),
            }),
        }];
        let cat = plan_catalog();
        let status = SyncBookkeeping::default();

        // Monday: strategy off → backing (200k) → write env.
        let mut j: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries = collect_entries(&j);
        let plan = compute_plan(&cfg, &cat, &entries, &LocalNow { weekday: 1, minute: 0 });
        let changed = apply_plan(&mut j, &plan, status.last_written_env().as_deref()).unwrap();
        assert!(changed);
        std::fs::write(&path, serde_json::to_string_pretty(&j).unwrap()).unwrap();
        status.record_written_env(plan.env_value.map(|v| v.to_string()));

        // Tuesday: strategy flips "pro" to a 1M model → env_value None → our value
        // deleted (managed delete); user keys kept.
        let mut j2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entries2 = collect_entries(&j2);
        let plan2 = compute_plan(&cfg, &cat, &entries2, &LocalNow { weekday: 2, minute: 0 });
        assert!(apply_plan(&mut j2, &plan2, status.last_written_env().as_deref()).unwrap());
        assert!(j2["env"].get(ENV_KEY).is_none());
        assert_eq!(j2["env"]["KEEP"], "1");
        assert_eq!(j2["model"], "pro[1m]");
    }

    // ---- oc_notes independence ----

    #[test]
    fn oc_notes_and_claude_notes_are_independent() {
        let bk = SyncBookkeeping::default();
        assert!(bk.last_notes().is_none());
        assert!(bk.oc_notes().is_none());
        bk.record_notes(Some("claude skip set".into()));
        bk.record_oc_notes(Some("opencode skip set".into()));
        assert_eq!(bk.last_notes().as_deref(), Some("claude skip set"));
        assert_eq!(bk.oc_notes().as_deref(), Some("opencode skip set"));
        // Mutating one does not affect the other.
        bk.record_oc_notes(None);
        assert_eq!(bk.last_notes().as_deref(), Some("claude skip set"));
        assert!(bk.oc_notes().is_none());
        bk.record_notes(None);
        assert!(bk.last_notes().is_none());
    }

    // ---- switchlm_providers ----

    fn oc_doc(base: Option<&str>) -> serde_json::Value {
        let mut prov = serde_json::Map::new();
        if let Some(b) = base {
            prov.insert("options".into(), json!({ "baseURL": b }));
        }
        json!({ "provider": { "switchlm": prov } })
    }

    #[test]
    fn switchlm_providers_matches_exact_and_prefix() {
        let p = 6950u16;
        assert_eq!(
            switchlm_providers(&oc_doc(Some("http://127.0.0.1:6950/v1")), p),
            Some(vec!["switchlm".to_string()])
        );
        assert_eq!(
            switchlm_providers(&oc_doc(Some("http://127.0.0.1:6950")), p),
            Some(vec!["switchlm".to_string()])
        );
    }

    #[test]
    fn switchlm_providers_rejects_foreign() {
        let p = 6950u16;
        assert_eq!(switchlm_providers(&oc_doc(Some("http://127.0.0.1:6951/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(Some("https://api.deepseek.com/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(Some("http://localhost:6950/v1")), p), Some(vec![]));
        assert_eq!(switchlm_providers(&oc_doc(None), p), Some(vec![])); // no baseURL
    }

    #[test]
    fn switchlm_providers_shape_errors() {
        // provider present but not an object → None (caller skips round)
        assert_eq!(switchlm_providers(&json!({ "provider": "oops" }), 6950), None);
        // absent provider → empty vec, NOT an error
        assert_eq!(switchlm_providers(&json!({}), 6950), Some(vec![]));
    }

    // ---- sync_opencode_round ----

    fn with_port(state: &crate::proxy::AppState, port: Option<u16>) {
        match port {
            Some(p) => state.set_bound_port(p),
            None => state.clear_bound_port(),
        }
    }

    #[tokio::test]
    async fn oc_round_writes_limit_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::json!({
            "provider": {
                "switchlm": {
                    "options": { "baseURL": "http://127.0.0.1:6950/v1" },
                    "models": {
                        "pro":   { "options": { "reasoningEffort": "high" },
                                   "limit": { "context": 1000000 } },
                        "flash": { "limit": { "context": 200000 } },
                        "weird": {}
                    }
                },
                "direct": {
                    "options": { "baseURL": "https://api.deepseek.com/v1" },
                    "models": { "deepseek-v4-pro": {} }
                }
            }
        }).to_string()).unwrap();

        let cfg = round_cfg();
        // "pro" → m_small (200k), "flash" → m_big (1M), "weird" → no profile
        let state = test_state(cfg).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();

        sync_opencode_round(&state, &status, &path).await;

        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        // 已存在的 limit 对象：output 一并写入（OpenCode 校验要求 limit 同时声明 output）
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["output"], 128_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["options"]["reasoningEffort"], "high", "sibling options kept");
        assert_eq!(j["provider"]["switchlm"]["models"]["flash"]["limit"]["context"], 1_000_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["flash"]["limit"]["output"], 131_072);
        assert!(j["provider"]["switchlm"]["models"]["weird"].as_object().unwrap().is_empty(), "unmatched entry untouched (no limit created)");
        assert_eq!(j["provider"]["direct"]["models"]["deepseek-v4-pro"].as_object().unwrap().len(), 0, "foreign provider untouched");
        let r = status.snapshot_opencode().unwrap();
        assert_eq!(r.action, "written");
        assert!(r.detail.contains("weird"), "skip note surfaced in detail");

        // Settle: second round, no change → byte-identical file.
        let before = std::fs::read_to_string(&path).unwrap();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(status.snapshot_opencode().unwrap().action, "no_change");
    }

    #[tokio::test]
    async fn oc_round_creates_limit_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, json!({
            "provider": { "switchlm": {
                "options": { "baseURL": "http://127.0.0.1:6950/v1" },
                "models": { "pro": { "options": { "textVerbosity": "low" } } }
            }}
        }).to_string()).unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["options"]["textVerbosity"], "low");
        // output 字段必须存在以满足 OpenCode 配置验证（catalog 已知 -> 写 catalog 值）
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["output"], 128_000);
        // input 字段不创建
        assert!(j["provider"]["switchlm"]["models"]["pro"]["limit"].get("input").is_none());
    }

    #[tokio::test]
    async fn oc_round_unknown_output_keeps_user_value_fills_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // "kept"：用户手写 output；"filled"：缺 output -- catalog 未知时才分别保留/回退
        std::fs::write(&path, json!({
            "provider": { "switchlm": {
                "options": { "baseURL": "http://127.0.0.1:6950/v1" },
                "models": {
                    "pro":   { "limit": { "context": 1, "output": 55555 } },
                    "flash": { "limit": { "context": 1 } }
                }
            }}
        }).to_string()).unwrap();
        let state = test_state(round_cfg()).await;
        // 换成不含 output_size 的 catalog（模拟自定义模型未收录 output）
        *state.catalog.write().await = serde_json::from_str(r#"{"version":1,"providers":[
            {"provider_id":"zhipu","models":[{"upstream_model_id":"glm-4.6","context_size":200000}]},
            {"provider_id":"volcengine-coding","models":[{"upstream_model_id":"glm-5.2","context_size":1000000}]}
        ]}"#).unwrap();
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // context 仍被改写
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        assert_eq!(j["provider"]["switchlm"]["models"]["flash"]["limit"]["context"], 1_000_000);
        // catalog 未知 output：用户手写值保留，缺失的补保守回退
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["output"], 55_555);
        assert_eq!(j["provider"]["switchlm"]["models"]["flash"]["limit"]["output"], 8_192);
    }

    #[tokio::test]
    async fn oc_round_jsonc_comments_survive_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\n  // my note\n  \"provider\": { \"switchlm\": {\n    \"options\": { \"baseURL\": \"http://127.0.0.1:6950/v1\", },\n    \"models\": { \"pro\": {}, }\n  } }\n}\n").unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(j["provider"]["switchlm"]["models"]["pro"]["limit"]["context"], 200_000);
        // write-back is plain JSON: the original line-comment marker is gone
        // (note: `//` also appears inside URL strings like `http://`, so we check
        // for the specific comment text rather than bare `//`).
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(!written.contains("// my note"), "line comment stripped from output");
        assert!(!written.contains("/*"), "no block-comment marker in output");
    }

    #[tokio::test]
    async fn oc_round_no_port_skips() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = json!({ "provider": { "switchlm": {
            "options": { "baseURL": "http://127.0.0.1:6950/v1" },
            "models": { "pro": {} }
        }}}).to_string();
        std::fs::write(&path, &original).unwrap();
        let state = test_state(round_cfg()).await;
        with_port(&state, None); // proxy not bound
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original, "untouched");
    }

    #[tokio::test]
    async fn oc_round_toggle_off_and_corrupt_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = opencode_config_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // toggle off
        let original = json!({ "provider": { "switchlm": {
            "options": { "baseURL": "http://127.0.0.1:6950/v1" }, "models": { "pro": {} }
        }}}).to_string();
        std::fs::write(&path, &original).unwrap();
        let mut cfg = round_cfg();
        cfg.settings.sync_claude_context = false;
        let state = test_state(cfg).await;
        with_port(&state, Some(6950));
        let status = SyncBookkeeping::default();
        sync_opencode_round(&state, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        // corrupt
        std::fs::write(&path, "{ not json").unwrap();
        let state2 = test_state(round_cfg()).await;
        with_port(&state2, Some(6950));
        sync_opencode_round(&state2, &status, &path).await;
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        assert_eq!(status.snapshot_opencode().unwrap().action, "skipped");
    }
}
