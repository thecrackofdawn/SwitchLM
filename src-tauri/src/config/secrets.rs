use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("secret storage consent not granted")]
    PendingConsent,
}

/// 当前激活的后端种类（供 `get_secret_status` 派生 mode/consent_required，无需额外状态字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Keyring,
    File,
    Pending,
}

/// 启动期后端选择（纯函数，可单测）。`keyring_ok` 来自 `keyring_available()`，
/// `granted` 来自 `Settings.secret_store_fallback`（None=每次询问；Some(true)=已授权）。
pub fn select_backend(keyring_ok: bool, granted: Option<bool>) -> BackendKind {
    if keyring_ok {
        BackendKind::Keyring
    } else if matches!(granted, Some(true)) {
        BackendKind::File
    } else {
        BackendKind::Pending
    }
}

/// 探测系统密钥环是否可用：对临时条目做 set→get→delete 往返。任一步失败即视为不可用
/// （覆盖 `NoBackendAccess` 无后端、D-Bus 无守护进程、gnome-keyring 锁定态等情况）。
pub fn keyring_available() -> bool {
    let store = KeyringStore;
    const PROBE: &str = "switchlm_availability_probe";
    let ok = store.set_key(PROBE, "1").is_ok()
        && matches!(store.get_key(PROBE), Ok(Some(_)));
    let _ = store.delete_key(PROBE); // 清理
    ok
}

/// 按 `kind` 构造具体后端。`File` 需 `<dir>` 以定位 secrets.json。
pub fn make_store(kind: BackendKind, dir: &Path) -> Result<Arc<dyn SecretStore>, SecretError> {
    Ok(match kind {
        BackendKind::Keyring => Arc::new(KeyringStore),
        BackendKind::File => Arc::new(FileSecretStore::new(dir)?),
        BackendKind::Pending => Arc::new(PendingStore),
    })
}

/// 将遗留 `secrets.json` 中的 usage_cookie 条目迁移进当前密钥存储。仅当主后端为 keyring 时由
/// 启动逻辑调用——cookie 现分片存入 keyring（绕过单条目字节上限），文件回退已移除，故旧版本曾
/// 落到 secrets.json 的 cookie 需在此一次性迁回。全部成功 → 删除明文 secrets.json；部分失败 →
/// 保留文件（磁盘原文未动，下次启动重试）。无文件/空文件 → 空操作，返回 0。
pub fn migrate_cookie_file_to_store(dir: &Path, dest: &dyn SecretStore) -> Result<usize, SecretError> {
    let cookies = FileSecretStore::new(dir)?.take_usage_cookies();
    if cookies.is_empty() {
        return Ok(0);
    }
    let total = cookies.len();
    let mut migrated = 0;
    for (pid, cookie) in &cookies {
        match dest.set_usage_cookie(pid, cookie) {
            Ok(()) => migrated += 1,
            Err(e) => tracing::warn!("迁移 cookie 到密钥存储失败 provider={pid}：{e}"),
        }
    }
    if migrated == total {
        if let Err(e) = std::fs::remove_file(dir.join("secrets.json")) {
            tracing::warn!("迁移后删除 secrets.json 失败：{e}");
        }
    } else {
        tracing::warn!("cookie 文件迁移部分成功 ({migrated}/{total})，保留 secrets.json");
    }
    Ok(migrated)
}

pub trait SecretStore: Send + Sync {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError>;
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError>;
    /// Volcengine usage-query Secret Access Key. Stored under a distinct keyring entry from
    /// the inference `api_key` so the two never collide for one provider.
    fn set_usage_sk(&self, provider_id: &str, sk: &str) -> Result<(), SecretError>;
    fn get_usage_sk(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_usage_sk(&self, provider_id: &str) -> Result<(), SecretError>;
    /// 千问 (Qianwen) (console) usage-session cookie. Distinct keyring entry from the inference
    /// `api_key` and the Volcengine `usage_sk`. Captured by the in-app login window
    /// (`qianwen_login`); the stored value is a serialized `Cookie:` header string.
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError>;
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError>;
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError>;
}

const SERVICE: &str = "SwitchLM";

/// Keyring "username" for a provider's usage-query SK. Distinct from the api_key entry (which
/// uses the bare `provider_id`) so both can coexist for one provider. Do NOT change the api_key
/// entry naming or already-stored inference keys would be orphaned.
fn usage_sk_entry(provider_id: &str) -> String {
    format!("{provider_id}::usage_sk")
}

fn usage_cookie_entry(provider_id: &str) -> String {
    format!("{provider_id}::usage_cookie")
}

/// Keyring "username" for part `i` of a chunked usage cookie (`<provider_id>::usage_cookie::p{i}`).
/// Cookie blobs can exceed the OS keyring's per-entry byte cap, so an oversized cookie is split
/// across `p0, p1, …` and reassembled on read. Distinct namespace from the legacy single entry.
fn usage_cookie_part_entry(provider_id: &str, i: usize) -> String {
    format!("{provider_id}::usage_cookie::p{i}")
}

/// Per-part byte budget for chunking an oversized usage cookie. Conservatively small so it fits
/// under every known OS-keyring per-entry cap (WinRT PasswordVault ~2560 B, legacy Windows
/// CredManager ~510 B) — chunking then works uniformly regardless of backend or Windows version.
const USAGE_COOKIE_PART_MAX_BYTES: usize = 500;

/// Split `s` into contiguous `&str` slices each at most `max_bytes` long, never breaking a UTF-8
/// character. Empty input → empty vec. If `max_bytes` is smaller than a character (or is 0),
/// that character is emitted whole in its own part exceeding the budget - char integrity is the
/// hard guarantee, the byte budget is the soft target - so the function always makes progress and
/// never hangs on a too-small budget.
fn chunk_str(s: &str, max_bytes: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < s.len() {
        let mut end = (start + max_bytes).min(s.len());
        while end > start && !s.is_char_boundary(end) {
            end -= 1;
        }
        // If the budget is smaller than the char at `start` (or is 0), `end` couldn't advance
        // past `start` - emitting an empty slice would hang. Place that whole char in its own
        // part (exceeding the budget) so we always make progress. Char integrity is the hard
        // guarantee; the byte budget is the soft target.
        if end == start {
            let char_len = s[start..]
                .chars()
                .next()
                .expect("start < s.len() guarantees a next char")
                .len_utf8();
            end = start + char_len;
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

// --- thin keyring helpers (one Entry::new each; NoEntry → None / Ok(false)) ---
fn kget(user: &str) -> Result<Option<String>, SecretError> {
    match keyring::Entry::new(SERVICE, user)
        .map_err(|e| SecretError::Keyring(e.to_string()))?
        .get_password()
    {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretError::Keyring(e.to_string())),
    }
}
fn kset(user: &str, val: &str) -> Result<(), SecretError> {
    keyring::Entry::new(SERVICE, user)
        .map_err(|e| SecretError::Keyring(e.to_string()))?
        .set_password(val)
        .map_err(|e| SecretError::Keyring(e.to_string()))
}
/// Delete one entry; `Ok(true)` if it existed, `Ok(false)` on NoEntry.
fn kdelete_exists(user: &str) -> Result<bool, SecretError> {
    match keyring::Entry::new(SERVICE, user)
        .map_err(|e| SecretError::Keyring(e.to_string()))?
        .delete_credential()
    {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(SecretError::Keyring(e.to_string())),
    }
}

pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .set_password(key)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .get_password()
        {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, provider_id)
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .delete_credential()
        {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn set_usage_sk(&self, provider_id: &str, sk: &str) -> Result<(), SecretError> {
        keyring::Entry::new(SERVICE, &usage_sk_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .set_password(sk)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }
    fn get_usage_sk(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        match keyring::Entry::new(SERVICE, &usage_sk_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .get_password()
        {
            Ok(k) => Ok(Some(k)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn delete_usage_sk(&self, provider_id: &str) -> Result<(), SecretError> {
        match keyring::Entry::new(SERVICE, &usage_sk_entry(provider_id))
            .map_err(|e| SecretError::Keyring(e.to_string()))?
            .delete_credential()
        {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        // Split across part entries to fit the keyring's per-entry byte cap.
        let parts = chunk_str(cookie, USAGE_COOKIE_PART_MAX_BYTES);
        for (i, part) in parts.iter().enumerate() {
            kset(&usage_cookie_part_entry(provider_id, i), part)?;
        }
        // Clear leftover higher-indexed parts from a previously larger cookie, then any legacy
        // single entry. Setting an empty cookie thus removes everything.
        let mut i = parts.len();
        while kdelete_exists(&usage_cookie_part_entry(provider_id, i))? {
            i += 1;
        }
        let _ = kdelete_exists(&usage_cookie_entry(provider_id));
        Ok(())
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        let mut parts: Vec<String> = Vec::new();
        let mut i = 0;
        while let Some(p) = kget(&usage_cookie_part_entry(provider_id, i))? {
            parts.push(p);
            i += 1;
        }
        if !parts.is_empty() {
            return Ok(Some(parts.concat()));
        }
        // Legacy single-entry cookie (written before chunking) — backward compat.
        kget(&usage_cookie_entry(provider_id))
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut i = 0;
        while kdelete_exists(&usage_cookie_part_entry(provider_id, i))? {
            i += 1;
        }
        let _ = kdelete_exists(&usage_cookie_entry(provider_id));
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemoryStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap()
            .insert(provider_id.into(), key.into());
        Ok(())
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(provider_id).cloned())
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().remove(provider_id);
        Ok(())
    }
    fn set_usage_sk(&self, provider_id: &str, sk: &str) -> Result<(), SecretError> {
        self.inner
            .lock()
            .unwrap()
            .insert(usage_sk_entry(provider_id), sk.into());
        Ok(())
    }
    fn get_usage_sk(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&usage_sk_entry(provider_id))
            .cloned())
    }
    fn delete_usage_sk(&self, provider_id: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().remove(&usage_sk_entry(provider_id));
        Ok(())
    }
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().insert(usage_cookie_entry(provider_id), cookie.into());
        Ok(())
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_cookie_entry(provider_id)).cloned())
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        self.inner.lock().unwrap().remove(&usage_cookie_entry(provider_id));
        Ok(())
    }
}

/// 明文文件密钥后端（无系统密钥环时的用户授权回退）。键命名与 `KeyringStore` 1:1
/// （`provider_id`、`provider_id::usage_sk`、`provider_id::usage_cookie`），故二者语义等价、可互换。
/// 文件：<dir>/secrets.json，原子写（.tmp + rename），Unix 下 0600。
pub struct FileSecretStore {
    path: PathBuf,
    inner: Mutex<HashMap<String, String>>,
}

impl FileSecretStore {
    /// 加载 `<dir>/secrets.json`。缺失/空文件 → 空存储；损坏 → 隔离为 `.bak` 后置空。
    pub fn new(dir: &Path) -> Result<Self, SecretError> {
        let path = dir.join("secrets.json");
        let mut inner = HashMap::new();
        // Remove any stale tmp from a prior crashed write (would otherwise linger as plaintext).
        let _ = std::fs::remove_file(dir.join("secrets.json.tmp"));
        match std::fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => {}
            Ok(s) => match serde_json::from_str::<HashMap<String, String>>(&s) {
                Ok(map) => inner = map,
                Err(_) => {
                    if let Err(e) = std::fs::rename(&path, dir.join("secrets.json.bak")) {
                        tracing::warn!("无法隔离损坏的 secrets.json 为 .bak（救援副本不可用）：{e}");
                    } else {
                        tracing::warn!("secrets.json 解析失败，已隔离原文为 secrets.json.bak 并重置为空");
                    }
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(SecretError::Io(e.to_string())),
        }
        Ok(Self { path, inner: Mutex::new(inner) })
    }

    /// 取出并移除所有 usage_cookie 条目（迁移到 keyring 时由 `migrate_cookie_file_to_store` 调用）。
    /// 返回 `(provider_id, cookie)` 列表。仅改动内存映射——不触碰磁盘：迁移成功后由调用方删除
    /// secrets.json；若迁移失败，磁盘原文仍然完好（下次启动重试）。
    pub fn take_usage_cookies(&self) -> Vec<(String, String)> {
        const SUFFIX: &str = "::usage_cookie";
        let mut map = self.inner.lock().unwrap();
        let keys: Vec<String> = map.keys().filter(|k| k.ends_with(SUFFIX)).cloned().collect();
        let mut out = Vec::with_capacity(keys.len());
        for k in keys {
            if let (Some(pid), Some(v)) = (k.strip_suffix(SUFFIX), map.remove(&k)) {
                out.push((pid.to_string(), v));
            }
        }
        out
    }

    fn flush(&self, map: &HashMap<String, String>) -> Result<(), SecretError> {
        let dir = self.path.parent().expect("secrets.json has a parent dir");
        let tmp = dir.join("secrets.json.tmp");
        let bytes = serde_json::to_vec(map).map_err(|e| SecretError::Io(e.to_string()))?;
        // Create tmp with 0600 from the start (Unix) — avoids a brief world-readable window
        // holding plaintext secrets that a write-then-set_permissions would create.
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true).create(true).truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| SecretError::Io(e.to_string()))?;
            f.write_all(&bytes).map_err(|e| SecretError::Io(e.to_string()))?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&tmp, bytes).map_err(|e| SecretError::Io(e.to_string()))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| SecretError::Io(e.to_string()))?;
        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn set_key(&self, provider_id: &str, key: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(provider_id.into(), key.into());
        self.flush(&map)
    }
    fn get_key(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(provider_id).cloned())
    }
    fn delete_key(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(provider_id);
        self.flush(&map)
    }
    fn set_usage_sk(&self, provider_id: &str, sk: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(usage_sk_entry(provider_id), sk.into());
        self.flush(&map)
    }
    fn get_usage_sk(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_sk_entry(provider_id)).cloned())
    }
    fn delete_usage_sk(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(&usage_sk_entry(provider_id));
        self.flush(&map)
    }
    fn set_usage_cookie(&self, provider_id: &str, cookie: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.insert(usage_cookie_entry(provider_id), cookie.into());
        self.flush(&map)
    }
    fn get_usage_cookie(&self, provider_id: &str) -> Result<Option<String>, SecretError> {
        Ok(self.inner.lock().unwrap().get(&usage_cookie_entry(provider_id)).cloned())
    }
    fn delete_usage_cookie(&self, provider_id: &str) -> Result<(), SecretError> {
        let mut map = self.inner.lock().unwrap();
        map.remove(&usage_cookie_entry(provider_id));
        self.flush(&map)
    }
}

/// "等待授权"占位后端：写一律返回 `PendingConsent`，读一律返回 `Ok(None)`。
pub struct PendingStore;

impl SecretStore for PendingStore {
    fn set_key(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_key(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_key(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn set_usage_sk(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_usage_sk(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_usage_sk(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn set_usage_cookie(&self, _: &str, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
    fn get_usage_cookie(&self, _: &str) -> Result<Option<String>, SecretError> { Ok(None) }
    fn delete_usage_cookie(&self, _: &str) -> Result<(), SecretError> { Err(SecretError::PendingConsent) }
}

/// 可在运行期切换后端的句柄：持有 `(当前后端, 种类)`。impl `SecretStore`（委托给当前后端），
/// 故可透明替换 `AppStateInner.secrets` 字段（所有 `state.secrets.set_key(...)` 调用点零改动）。
/// 授权后 `swap` 从 PendingStore 切到 FileSecretStore，无需重启。
pub struct SecretStoreHandle {
    inner: Mutex<(Arc<dyn SecretStore>, BackendKind)>,
}

impl SecretStoreHandle {
    pub fn new(store: Arc<dyn SecretStore>, kind: BackendKind) -> Self {
        Self { inner: Mutex::new((store, kind)) }
    }
    fn current(&self) -> Arc<dyn SecretStore> {
        self.inner.lock().unwrap().0.clone()
    }
    pub fn swap(&self, store: Arc<dyn SecretStore>, kind: BackendKind) {
        *self.inner.lock().unwrap() = (store, kind);
    }
    pub fn kind(&self) -> BackendKind {
        self.inner.lock().unwrap().1
    }
}

impl SecretStore for SecretStoreHandle {
    fn set_key(&self, id: &str, k: &str) -> Result<(), SecretError> { self.current().set_key(id, k) }
    fn get_key(&self, id: &str) -> Result<Option<String>, SecretError> { self.current().get_key(id) }
    fn delete_key(&self, id: &str) -> Result<(), SecretError> { self.current().delete_key(id) }
    fn set_usage_sk(&self, id: &str, k: &str) -> Result<(), SecretError> { self.current().set_usage_sk(id, k) }
    fn get_usage_sk(&self, id: &str) -> Result<Option<String>, SecretError> { self.current().get_usage_sk(id) }
    fn delete_usage_sk(&self, id: &str) -> Result<(), SecretError> { self.current().delete_usage_sk(id) }
    fn set_usage_cookie(&self, id: &str, k: &str) -> Result<(), SecretError> {
        self.current().set_usage_cookie(id, k)
    }
    fn get_usage_cookie(&self, id: &str) -> Result<Option<String>, SecretError> {
        // Mask transient secret-store errors as None so the 千问 登录态 presence tag degrades to
        // "未登录" instead of surfacing a hard error to the UI.
        match self.current().get_usage_cookie(id) {
            Ok(v) => Ok(v),
            Err(e) => {
                tracing::debug!("get_usage_cookie: masked transient secret-store error: {e}");
                Ok(None)
            }
        }
    }
    fn delete_usage_cookie(&self, id: &str) -> Result<(), SecretError> {
        self.current().delete_usage_cookie(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::default();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
        s.set_key("zhipu", "sk-abc").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), Some("sk-abc".into()));
        s.delete_key("zhipu").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
    }

    #[test]
    fn memory_store_usage_sk_distinct_from_api_key() {
        // One provider, two secrets: the inference api_key and the usage SK must not collide.
        let s = MemoryStore::default();
        s.set_key("volcengine-coding", "sk-inference").unwrap();
        s.set_usage_sk("volcengine-coding", "secret-access-key").unwrap();
        assert_eq!(s.get_key("volcengine-coding").unwrap(), Some("sk-inference".into()));
        assert_eq!(s.get_usage_sk("volcengine-coding").unwrap(), Some("secret-access-key".into()));
        // Deleting the api_key must not touch the usage SK (and vice versa).
        s.delete_key("volcengine-coding").unwrap();
        assert_eq!(s.get_key("volcengine-coding").unwrap(), None);
        assert_eq!(s.get_usage_sk("volcengine-coding").unwrap(), Some("secret-access-key".into()));
        s.delete_usage_sk("volcengine-coding").unwrap();
        assert_eq!(s.get_usage_sk("volcengine-coding").unwrap(), None);
    }

    /// Exercises the REAL OS keyring (not MemoryStore) so a missing/broken backend surfaces
    /// here at test time instead of silently at runtime. If this fails with NoBackendAccess,
    /// enable the keyring platform feature in Cargo.toml (windows-native / apple-native /
    /// linux-native-sync-persistent). Cleans up before + after.
    #[ignore]
    #[test]
    fn keyring_store_roundtrip_real() {
        let store = KeyringStore;
        let id = "switchlm_keyring_test";
        let _ = store.delete_key(id); // clean any leftover from a prior run
        let got = store
            .get_key(id)
            .expect("keyring backend unavailable - enable the platform feature (e.g. windows-native) in Cargo.toml");
        assert_eq!(got, None);
        store
            .set_key(id, "sk-test-value")
            .expect("set_key failed - keyring backend unavailable");
        assert_eq!(store.get_key(id).unwrap(), Some("sk-test-value".to_string()));
        store.delete_key(id).unwrap();
        assert_eq!(store.get_key(id).unwrap(), None);

        // usage SK lives in a distinct keyring entry alongside the api_key.
        let _ = store.delete_usage_sk(id); // clean any leftover from a prior run
        assert_eq!(store.get_usage_sk(id).unwrap(), None);
        store
            .set_usage_sk(id, "sk-usage-value")
            .expect("set_usage_sk failed - keyring backend unavailable");
        assert_eq!(store.get_usage_sk(id).unwrap(), Some("sk-usage-value".to_string()));
        // api_key was deleted above; setting usage_sk must not have revived it.
        assert_eq!(store.get_key(id).unwrap(), None);
        store.delete_usage_sk(id).unwrap();
        assert_eq!(store.get_usage_sk(id).unwrap(), None);
    }

    #[test]
    fn secret_error_variants_format() {
        assert_eq!(SecretError::Io("denied".into()).to_string(), "io error: denied");
        assert_eq!(
            SecretError::PendingConsent.to_string(),
            "secret storage consent not granted"
        );
    }

    use std::fs;

    fn tmp_store() -> (tempfile::TempDir, FileSecretStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn file_store_roundtrip() {
        let (_dir, s) = tmp_store();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
        s.set_key("zhipu", "sk-abc").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), Some("sk-abc".into()));
        s.delete_key("zhipu").unwrap();
        assert_eq!(s.get_key("zhipu").unwrap(), None);
    }

    #[test]
    fn file_store_usage_sk_distinct_from_api_key() {
        let (_dir, s) = tmp_store();
        s.set_key("volc", "sk-inference").unwrap();
        s.set_usage_sk("volc", "secret-access-key").unwrap();
        assert_eq!(s.get_key("volc").unwrap(), Some("sk-inference".into()));
        assert_eq!(s.get_usage_sk("volc").unwrap(), Some("secret-access-key".into()));
        s.delete_key("volc").unwrap();
        assert_eq!(s.get_key("volc").unwrap(), None);
        assert_eq!(s.get_usage_sk("volc").unwrap(), Some("secret-access-key".into()));
    }

    #[test]
    fn file_store_missing_or_empty_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s.get_key("x").unwrap(), None);
        fs::write(dir.path().join("secrets.json"), "").unwrap();
        let s2 = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s2.get_key("x").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn file_store_creates_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path()).unwrap();
        s.set_key("a", "b").unwrap();
        let perms = fs::metadata(dir.path().join("secrets.json")).unwrap().permissions().mode() & 0o777;
        assert_eq!(perms, 0o600, "secrets.json must be 0600 on unix");
    }

    #[test]
    fn file_store_corrupt_file_quarantined_to_bak() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("secrets.json"), "{ not valid json").unwrap();
        let s = FileSecretStore::new(dir.path()).unwrap();
        assert_eq!(s.get_key("x").unwrap(), None);
        assert!(dir.path().join("secrets.json.bak").exists(), "corrupt file quarantined");
    }

    #[test]
    fn file_store_multiple_writes_leave_consistent_file() {
        // NOTE: this only verifies two sequential writes leave a consistent file — it would
        // also pass for a non-atomic impl. True atomicity is provided by the .tmp+rename in
        // flush(), which can't be proven without fault injection (out of unit-test scope).
        let (_dir, s) = tmp_store();
        s.set_key("a", "1").unwrap();
        s.set_key("b", "2").unwrap();
        let raw = fs::read_to_string(tmp_store_path(&_dir)).unwrap();
        assert!(raw.contains("\"a\"") && raw.contains("\"b\""));
    }

    fn tmp_store_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("secrets.json")
    }

    #[test]
    fn pending_store_reads_none_writes_err() {
        let s = PendingStore;
        assert_eq!(s.get_key("a").unwrap(), None);
        assert_eq!(s.get_usage_sk("a").unwrap(), None);
        assert!(matches!(s.set_key("a", "x"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.set_usage_sk("a", "x"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_key("a"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_usage_sk("a"), Err(SecretError::PendingConsent)));
        assert_eq!(s.get_usage_cookie("a").unwrap(), None);
        assert!(matches!(s.set_usage_cookie("a", "x"), Err(SecretError::PendingConsent)));
        assert!(matches!(s.delete_usage_cookie("a"), Err(SecretError::PendingConsent)));
    }

    #[test]
    fn select_backend_pure_logic() {
        use BackendKind::*;
        // keyring 可用 → 永远 Keyring（无视授权标记）
        assert_eq!(select_backend(true, None), Keyring);
        assert_eq!(select_backend(true, Some(true)), Keyring);
        // keyring 不可用 + 已授权 → File
        assert_eq!(select_backend(false, Some(true)), File);
        // keyring 不可用 + 未授权（含 None 与 Some(false)）→ Pending
        assert_eq!(select_backend(false, None), Pending);
        assert_eq!(select_backend(false, Some(false)), Pending);
    }

    #[test]
    fn memory_store_usage_cookie_distinct_from_api_key_and_sk() {
        let s = MemoryStore::default();
        s.set_key("qianwen", "sk-inference").unwrap();
        s.set_usage_sk("qianwen", "volc-sk").unwrap();
        s.set_usage_cookie("qianwen", "cna=x; ticket=y").unwrap();
        assert_eq!(s.get_key("qianwen").unwrap(), Some("sk-inference".into()));
        assert_eq!(s.get_usage_sk("qianwen").unwrap(), Some("volc-sk".into()));
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_key("qianwen").unwrap();
        assert_eq!(s.get_key("qianwen").unwrap(), None);
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_usage_cookie("qianwen").unwrap();
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), None);
        assert_eq!(s.get_usage_sk("qianwen").unwrap(), Some("volc-sk".into()));
    }

    #[test]
    fn file_store_usage_cookie_roundtrip() {
        let (_dir, s) = tmp_store();
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), None);
        s.set_usage_cookie("qianwen", "cna=x; ticket=y").unwrap();
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), Some("cna=x; ticket=y".into()));
        s.delete_usage_cookie("qianwen").unwrap();
        assert_eq!(s.get_usage_cookie("qianwen").unwrap(), None);
    }

    #[test]
    fn handle_delegates_usage_cookie() {
        let h = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        assert_eq!(h.get_usage_cookie("a").unwrap(), None);
        h.set_usage_cookie("a", "c=1").unwrap();
        assert_eq!(h.get_usage_cookie("a").unwrap(), Some("c=1".into()));
        h.delete_usage_cookie("a").unwrap();
        assert_eq!(h.get_usage_cookie("a").unwrap(), None);
    }

    #[test]
    fn handle_delegates_and_swaps_and_reports_kind() {
        let h = SecretStoreHandle::new(Arc::new(MemoryStore::default()), BackendKind::Keyring);
        assert_eq!(h.kind(), BackendKind::Keyring);
        // 委托给当前后端（MemoryStore 读 None）
        assert_eq!(h.get_key("a").unwrap(), None);
        h.set_key("a", "v").unwrap(); // MemoryStore 写成功
        assert_eq!(h.get_key("a").unwrap(), Some("v".into()));
        // swap 到另一个后端，kind 随之更新（真正跨 kind：Keyring→Pending）
        h.swap(Arc::new(PendingStore), BackendKind::Pending);
        assert_eq!(h.kind(), BackendKind::Pending);
        assert_eq!(h.get_key("a").unwrap(), None); // PendingStore 读 None（不再持有旧值）
        assert!(matches!(h.set_key("a", "x"), Err(SecretError::PendingConsent)));
    }

    // ----- cookie chunking (works around the OS keyring's per-entry byte cap) -----

    #[test]
    fn chunk_str_empty() {
        assert!(chunk_str("", 500).is_empty());
    }

    #[test]
    fn chunk_str_under_limit_is_single_part() {
        assert_eq!(chunk_str("hello", 500), vec!["hello"]);
    }

    #[test]
    fn chunk_str_splits_on_byte_budget() {
        let s = "a".repeat(1100);
        let parts = chunk_str(&s, 500);
        assert_eq!(parts.len(), 3, "500 + 500 + 100");
        assert!(parts.iter().all(|p| p.len() <= 500));
        assert_eq!(parts.concat(), s);
    }

    #[test]
    fn chunk_str_exact_multiple() {
        let s = "a".repeat(1000);
        let parts = chunk_str(&s, 500);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts.concat(), s);
    }

    #[test]
    fn chunk_str_never_splits_a_multibyte_char() {
        // 'é' is 2 bytes; a 3-byte budget must hold one whole char (2B), never a lone byte.
        let s = "éééé"; // 8 bytes
        let parts = chunk_str(s, 3);
        assert!(parts.iter().all(|p| p.len() <= 3));
        assert_eq!(parts.concat(), s);
        assert!(parts.iter().all(|p| p.len() % 2 == 0), "no part may split a 2-byte char");
    }

    #[test]
    fn file_store_take_usage_cookies_extracts_only_cookies() {
        let (_dir, s) = tmp_store();
        s.set_usage_cookie("prov_a", "cna=x; ticket=y").unwrap();
        s.set_key("prov_a", "sk-inference").unwrap(); // bare api_key — must NOT be taken
        let taken = s.take_usage_cookies();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, "prov_a");
        assert_eq!(taken[0].1, "cna=x; ticket=y");
        assert_eq!(s.get_usage_cookie("prov_a").unwrap(), None);
    }

    #[test]
    fn migrate_cookie_file_moves_cookies_and_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let f = FileSecretStore::new(dir.path()).unwrap();
            f.set_usage_cookie("prov_a", "big-cookie-value").unwrap();
        }
        assert!(dir.path().join("secrets.json").exists());
        let dest = MemoryStore::default();
        let n = migrate_cookie_file_to_store(dir.path(), &dest).unwrap();
        assert_eq!(n, 1);
        assert_eq!(dest.get_usage_cookie("prov_a").unwrap(), Some("big-cookie-value".into()));
        assert!(!dir.path().join("secrets.json").exists(), "plaintext file deleted after migration");
    }

    #[test]
    fn migrate_cookie_file_no_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let dest = MemoryStore::default();
        let n = migrate_cookie_file_to_store(dir.path(), &dest).unwrap();
        assert_eq!(n, 0);
    }

    /// Exercises the REAL OS keyring: an oversized cookie must transparently round-trip via
    /// chunked part entries. If this fails with NoBackendAccess, enable the platform feature
    /// (windows-native / apple-native / linux-native-sync-persistent) in Cargo.toml.
    #[ignore]
    #[test]
    fn keyring_store_oversized_cookie_chunked_real() {
        let store = KeyringStore;
        let id = "switchlm_cookie_chunk_test";
        let _ = store.delete_usage_cookie(id); // clean leftovers from a prior run
        let big = "x".repeat(USAGE_COOKIE_PART_MAX_BYTES * 4); // forces >= 4 parts
        store
            .set_usage_cookie(id, &big)
            .expect("keyring backend unavailable - enable the platform feature in Cargo.toml");
        assert_eq!(store.get_usage_cookie(id).unwrap(), Some(big));
        // Confirm it was actually split: part index 1 must exist.
        let part1_exists = keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, 1))
            .map(|e| e.get_password().is_ok())
            .unwrap_or(false);
        assert!(part1_exists, "oversized cookie must be stored as multiple keyring parts");
        store.delete_usage_cookie(id).unwrap();
        assert_eq!(store.get_usage_cookie(id).unwrap(), None);
    }

    #[test]
    fn chunk_str_budget_one_splits_every_ascii_byte() {
        // Degenerate min budget: each ASCII char becomes its own part. Guards the lower bound of
        // the byte-budget loop (start..end must always make progress on 1-byte chars).
        let parts = chunk_str("abcd", 1);
        assert_eq!(parts, vec!["a", "b", "c", "d"]);
        assert_eq!(parts.concat(), "abcd");
    }

    #[test]
    fn chunk_str_cjk_3byte_char_never_split() {
        // '中' is 3 bytes. Budgets >= the largest char size must hold whole chars, never split
        // one, and respect the byte budget. Budgets below the char size are covered by the
        // hardening tests below (whole char emitted, budget exceeded).
        let s = "中文测试"; // 4 × 3 = 12 bytes
        for budget in [3usize, 4, 5, 6, 12] {
            let parts = chunk_str(s, budget);
            assert!(
                parts.iter().all(|p| p.len() <= budget),
                "budget {budget}: every part must respect the byte budget"
            );
            assert_eq!(parts.concat(), s, "budget {budget}: concat must equal the original");
        }
    }

    #[test]
    fn chunk_str_budget_below_char_size_emits_whole_char() {
        // Regression: a budget smaller than the char at `start` previously infinite-looped (the
        // walk-back left `end == start`, emitting an empty slice with no forward progress). Now
        // the whole char is emitted in its own part, exceeding the budget. '中' is 3 bytes.
        let s = "中文"; // 6 bytes, two 3-byte chars
        for budget in [0usize, 1, 2] {
            let parts = chunk_str(s, budget);
            assert_eq!(parts.len(), 2, "budget {budget}: one whole char per part");
            assert!(
                parts.iter().all(|p| p.len() == 3),
                "budget {budget}: each part is one 3-byte char"
            );
            assert_eq!(parts.concat(), s, "budget {budget}: concat must equal the original");
        }
    }

    #[test]
    fn chunk_str_zero_budget_ascii_one_char_per_part() {
        // Zero budget must still make progress: each ASCII char (1 byte) becomes its own part,
        // exceeding the 0 budget rather than hanging.
        let parts = chunk_str("abc", 0);
        assert_eq!(parts, vec!["a", "b", "c"]);
        assert_eq!(parts.concat(), "abc");
    }

    #[test]
    fn chunk_str_budget_below_char_size_preserves_concat_identity() {
        // Mixed 1- and 3-byte chars with a budget (2) below the 3-byte char: the 3-byte char is
        // emitted whole; 1-byte chars still pack within budget. Concat must round-trip exactly and
        // no part may split a char.
        let s = "a中b文c"; // a(1) 中(3) b(1) 文(3) c(1) = 9 bytes
        let parts = chunk_str(s, 2);
        assert_eq!(parts.concat(), s);
        assert!(
            parts.iter().all(|p| std::str::from_utf8(p.as_bytes()).is_ok()),
            "no part may split a multibyte char"
        );
    }

    // ----- Linux-only real-keyring regression tests -----
    //
    // These exercise the REAL Linux keyring backend (linux-native-sync-persistent / D-Bus Secret
    // Service). They are `#[cfg(target_os = "linux")]`-gated so they only compile on Linux, and
    // `#[ignore]`-d so they run only as an opt-in regression pass (`cargo test --ignored`) on a
    // Linux box with a working secret service (gnome-keyring / KWallet + libdbus). They cover the
    // KeyringStore chunking *orchestration* - part cleanup on shrink/empty, legacy single-entry
    // backward-compat read, and the file->keyring cookie migration - none of which the pure
    // chunk_str tests or the MemoryStore/FileStore tests can reach, since only KeyringStore splits.
    // A NoBackendAccess failure means no secret-service daemon is running.

    #[cfg(target_os = "linux")]
    #[ignore]
    #[test]
    fn keyring_store_cookie_shrink_clears_leftover_parts_real() {
        // Writing a small cookie after a large one must delete the leftover higher-indexed parts
        // (the cleanup loop in set_usage_cookie). Stale tails would otherwise corrupt the
        // reassembled cookie on the next read.
        let store = KeyringStore;
        let id = "switchlm_cookie_shrink_test";
        let _ = store.delete_usage_cookie(id); // clean leftovers from a prior run
        let big = "x".repeat(USAGE_COOKIE_PART_MAX_BYTES * 4); // forces >= 4 parts (p0..p3)
        store
            .set_usage_cookie(id, &big)
            .expect("Linux keyring backend unavailable - is a secret service daemon running?");
        assert!(
            keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, 3))
                .map(|e| e.get_password().is_ok())
                .unwrap_or(false),
            "large cookie must have produced a part at index 3"
        );
        // Shrink to a single-part cookie.
        let small = "tiny";
        store.set_usage_cookie(id, small).expect("set small cookie failed");
        assert_eq!(store.get_usage_cookie(id).unwrap(), Some(small.into()));
        for i in 1..4 {
            let still = keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, i))
                .map(|e| e.get_password().is_ok())
                .unwrap_or(false);
            assert!(!still, "leftover part p{i} must be cleared after shrinking to a small cookie");
        }
        store.delete_usage_cookie(id).unwrap();
        assert_eq!(store.get_usage_cookie(id).unwrap(), None);
    }

    #[cfg(target_os = "linux")]
    #[ignore]
    #[test]
    fn keyring_store_cookie_legacy_single_entry_read_real() {
        // A cookie written by an OLD SwitchLM build (before chunking) lives in the single legacy
        // entry `<id>::usage_cookie`. The current reader must still return it (backward-compat
        // fallback), and a subsequent chunked write must delete that legacy entry so the stale
        // value can't resurrect on read.
        let store = KeyringStore;
        let id = "switchlm_cookie_legacy_test";
        let _ = store.delete_usage_cookie(id);
        let legacy = "cna=legacy; ticket=old";
        keyring::Entry::new(SERVICE, &usage_cookie_entry(id))
            .expect("entry construction failed")
            .set_password(legacy)
            .expect("Linux keyring backend unavailable - is a secret service daemon running?");
        // Current reader falls back to the legacy single entry.
        assert_eq!(store.get_usage_cookie(id).unwrap(), Some(legacy.into()));
        // A chunked write must clear the legacy entry.
        store
            .set_usage_cookie(id, "cna=new; ticket=fresh")
            .expect("set failed");
        let legacy_still = keyring::Entry::new(SERVICE, &usage_cookie_entry(id))
            .map(|e| e.get_password().is_ok())
            .unwrap_or(false);
        assert!(!legacy_still, "legacy single entry must be deleted after a chunked write");
        assert_eq!(
            store.get_usage_cookie(id).unwrap(),
            Some("cna=new; ticket=fresh".into())
        );
        store.delete_usage_cookie(id).unwrap();
        assert_eq!(store.get_usage_cookie(id).unwrap(), None);
    }

    #[cfg(target_os = "linux")]
    #[ignore]
    #[test]
    fn keyring_store_cookie_empty_clears_all_parts_real() {
        // Setting an empty cookie must remove every part (the "empty removes everything" contract
        // in set_usage_cookie), so a subsequent read returns None.
        let store = KeyringStore;
        let id = "switchlm_cookie_empty_test";
        let _ = store.delete_usage_cookie(id);
        let big = "y".repeat(USAGE_COOKIE_PART_MAX_BYTES * 3); // forces 3 parts (p0..p2)
        store
            .set_usage_cookie(id, &big)
            .expect("Linux keyring backend unavailable - is a secret service daemon running?");
        assert!(
            keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, 1))
                .map(|e| e.get_password().is_ok())
                .unwrap_or(false),
            "large cookie must have produced a part at index 1"
        );
        store.set_usage_cookie(id, "").expect("set empty failed");
        assert_eq!(store.get_usage_cookie(id).unwrap(), None);
        for i in 0..3 {
            let still = keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, i))
                .map(|e| e.get_password().is_ok())
                .unwrap_or(false);
            assert!(!still, "part p{i} must be cleared after setting an empty cookie");
        }
        store.delete_usage_cookie(id).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[ignore]
    #[test]
    fn migrate_cookie_file_to_real_keyring_linux() {
        // End-to-end: a legacy plaintext secrets.json cookie is migrated into the REAL keyring as
        // chunked part entries, and the plaintext file is deleted. Guards the startup migration
        // path against the real backend, not just MemoryStore.
        let dir = tempfile::tempdir().unwrap();
        let id = "switchlm_migrate_real_test";
        let big = "z".repeat(USAGE_COOKIE_PART_MAX_BYTES * 2 + 50); // forces >= 3 parts
        {
            let f = FileSecretStore::new(dir.path()).unwrap();
            f.set_usage_cookie(id, &big).unwrap();
        }
        assert!(dir.path().join("secrets.json").exists());
        let store = KeyringStore;
        let _ = store.delete_usage_cookie(id); // clean leftovers from a prior run
        let n = migrate_cookie_file_to_store(dir.path(), &store)
            .expect("Linux keyring backend unavailable - is a secret service daemon running?");
        assert_eq!(n, 1, "one cookie migrated");
        assert_eq!(store.get_usage_cookie(id).unwrap(), Some(big));
        // It really landed as chunked parts (not the legacy single entry).
        assert!(
            keyring::Entry::new(SERVICE, &usage_cookie_part_entry(id, 1))
                .map(|e| e.get_password().is_ok())
                .unwrap_or(false),
            "migrated cookie must be stored as chunked parts in the keyring"
        );
        assert!(
            !dir.path().join("secrets.json").exists(),
            "plaintext secrets.json must be deleted after a fully successful migration"
        );
        store.delete_usage_cookie(id).unwrap();
    }
}
