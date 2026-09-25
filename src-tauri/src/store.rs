//! Persistent storage: settings, connections, folders and query history live in JSON files in the
//! app data directory (written atomically, owner-only permissions). Secrets (passwords, API keys)
//! are NEVER written to these files; they go to the OS credential store (macOS Keychain, Windows
//! Credential Manager, Secret Service on Linux). When no credential store is available, secrets
//! are kept in memory for the running session only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::model::*;

const KEYRING_SERVICE: &str = "SQLighter";
const MAX_HISTORY: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConnectionsFile {
    folders: Vec<Folder>,
    connections: Vec<ConnectionConfig>,
}

pub struct Store {
    dir: PathBuf,
    settings: Mutex<Settings>,
    conns: Mutex<ConnectionsFile>,
    history: Mutex<Vec<HistoryEntry>>,
    secrets: SecretStore,
}

impl Store {
    pub fn open(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        restrict_dir(&dir);
        let settings: Settings = read_json(&dir.join("settings.json")).unwrap_or_default();
        let conns: ConnectionsFile = read_json(&dir.join("connections.json")).unwrap_or_default();
        let history: Vec<HistoryEntry> = read_json(&dir.join("history.json")).unwrap_or_default();
        Ok(Store {
            dir,
            settings: Mutex::new(settings),
            conns: Mutex::new(conns),
            history: Mutex::new(history),
            secrets: SecretStore::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    // --- settings --------------------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn save_settings(&self, s: Settings) -> Result<()> {
        write_json(&self.dir.join("settings.json"), &s)?;
        *self.settings.lock().unwrap() = s;
        Ok(())
    }

    pub fn settings_view(&self) -> SettingsView {
        let s = self.settings();
        SettingsView {
            ai_providers: s
                .ai_providers
                .iter()
                .map(|p| AiProviderView { has_api_key: self.secrets.get(&format!("ai:{}:apikey", p.id)).is_some(), config: p.clone() })
                .collect(),
            theme: s.theme,
            language: s.language,
            editor_font_size: s.editor_font_size,
            max_rows: s.max_rows,
            confirm_destructive: s.confirm_destructive,
            auto_commit: s.auto_commit,
            ai_access: s.ai_access,
            ai_default_provider_id: s.ai_default_provider_id,
            mcp_enabled: s.mcp_enabled,
            mcp_port: s.mcp_port,
            mcp_allow_write: s.mcp_allow_write,
            query_timeout_sec: s.query_timeout_sec,
            secure_storage: SecureStorageInfo { available: self.secrets.persistent, backend: self.secrets.backend.clone() },
        }
    }

    pub fn ai_api_key(&self, provider_id: &str) -> Option<String> {
        self.secrets.get(&format!("ai:{provider_id}:apikey"))
    }

    pub fn set_ai_api_key(&self, provider_id: &str, key: Option<&str>) {
        let k = format!("ai:{provider_id}:apikey");
        match key {
            Some(v) if !v.is_empty() => self.secrets.set(&k, v),
            _ => self.secrets.delete(&k),
        }
    }

    // --- connections -----------------------------------------------------------------------

    pub fn tree(&self) -> ConnectionTree {
        let c = self.conns.lock().unwrap();
        ConnectionTree { folders: c.folders.clone(), connections: c.connections.iter().map(|x| self.view(x)).collect() }
    }

    pub fn view(&self, c: &ConnectionConfig) -> ConnectionView {
        ConnectionView {
            has_password: self.secrets.get(&secret_key(&c.id, "password")).is_some(),
            has_ssh_password: self.secrets.get(&secret_key(&c.id, "sshPassword")).is_some(),
            has_ssh_passphrase: self.secrets.get(&secret_key(&c.id, "sshPassphrase")).is_some(),
            config: c.clone(),
        }
    }

    pub fn connection(&self, id: &str) -> Option<ConnectionConfig> {
        self.conns.lock().unwrap().connections.iter().find(|c| c.id == id).cloned()
    }

    pub fn secrets_for(&self, id: &str) -> ConnectionSecrets {
        ConnectionSecrets {
            password: self.secrets.get(&secret_key(id, "password")),
            ssh_password: self.secrets.get(&secret_key(id, "sshPassword")),
            ssh_passphrase: self.secrets.get(&secret_key(id, "sshPassphrase")),
        }
    }

    /// Stores a connection. `secrets` fields: None = keep existing, Some("") = delete, Some(x) = set.
    pub fn save_connection(&self, mut cfg: ConnectionConfig, secrets: Option<ConnectionSecrets>) -> Result<ConnectionView> {
        if cfg.id.is_empty() {
            cfg.id = uuid::Uuid::new_v4().to_string();
        }
        if cfg.created_at == 0 {
            cfg.created_at = chrono::Utc::now().timestamp_millis();
        }
        if let Some(s) = secrets {
            let persist = cfg.save_password;
            for (name, val) in [("password", s.password), ("sshPassword", s.ssh_password), ("sshPassphrase", s.ssh_passphrase)] {
                let key = secret_key(&cfg.id, name);
                match val {
                    Some(v) if v.is_empty() => self.secrets.delete(&key),
                    Some(v) if persist => self.secrets.set(&key, &v),
                    Some(v) => self.secrets.set_session(&key, &v),
                    None => {}
                }
            }
        }
        if !cfg.save_password {
            // Keep secrets only for this session.
            for name in ["password", "sshPassword", "sshPassphrase"] {
                self.secrets.forget_persistent(&secret_key(&cfg.id, name));
            }
        }
        {
            let mut c = self.conns.lock().unwrap();
            match c.connections.iter_mut().find(|x| x.id == cfg.id) {
                Some(existing) => *existing = cfg.clone(),
                None => c.connections.push(cfg.clone()),
            }
        }
        self.flush_conns()?;
        Ok(self.view(&cfg))
    }

    /// Updates a connection without touching secrets (e.g. after trusting a host key).
    pub fn update_connection(&self, f: impl FnOnce(&mut ConnectionConfig), id: &str) -> Result<()> {
        {
            let mut c = self.conns.lock().unwrap();
            if let Some(x) = c.connections.iter_mut().find(|x| x.id == id) {
                f(x);
            }
        }
        self.flush_conns()
    }

    pub fn delete_connection(&self, id: &str) -> Result<()> {
        self.conns.lock().unwrap().connections.retain(|c| c.id != id);
        for name in ["password", "sshPassword", "sshPassphrase"] {
            self.secrets.delete(&secret_key(id, name));
        }
        self.flush_conns()
    }

    pub fn save_folder(&self, mut f: Folder) -> Result<Folder> {
        if f.id.is_empty() {
            f.id = uuid::Uuid::new_v4().to_string();
        }
        {
            let mut c = self.conns.lock().unwrap();
            // Prevent cycles: a folder may not become its own descendant.
            if let Some(parent) = &f.parent_id {
                let mut cur = Some(parent.clone());
                while let Some(p) = cur {
                    if p == f.id {
                        anyhow::bail!("A folder cannot be moved into itself");
                    }
                    cur = c.folders.iter().find(|x| x.id == p).and_then(|x| x.parent_id.clone());
                }
            }
            match c.folders.iter_mut().find(|x| x.id == f.id) {
                Some(existing) => *existing = f.clone(),
                None => c.folders.push(f.clone()),
            }
        }
        self.flush_conns()?;
        Ok(f)
    }

    /// Deletes a folder; its children move to the parent folder.
    pub fn delete_folder(&self, id: &str) -> Result<()> {
        {
            let mut c = self.conns.lock().unwrap();
            let parent = c.folders.iter().find(|f| f.id == id).and_then(|f| f.parent_id.clone());
            for f in c.folders.iter_mut() {
                if f.parent_id.as_deref() == Some(id) {
                    f.parent_id = parent.clone();
                }
            }
            for x in c.connections.iter_mut() {
                if x.folder_id.as_deref() == Some(id) {
                    x.folder_id = parent.clone();
                }
            }
            c.folders.retain(|f| f.id != id);
        }
        self.flush_conns()
    }

    /// Moves a connection or folder into `folder_id` (None = root) at the given order.
    pub fn move_item(&self, kind: &str, id: &str, folder_id: Option<String>, order: Option<f64>) -> Result<()> {
        if kind == "folder" {
            let mut f = self.conns.lock().unwrap().folders.iter().find(|f| f.id == id).cloned().context("folder not found")?;
            f.parent_id = folder_id;
            f.order = order.or(f.order);
            self.save_folder(f)?;
            return Ok(());
        }
        {
            let mut c = self.conns.lock().unwrap();
            let x = c.connections.iter_mut().find(|x| x.id == id).context("connection not found")?;
            x.folder_id = folder_id;
            if order.is_some() {
                x.order = order;
            }
        }
        self.flush_conns()
    }

    fn flush_conns(&self) -> Result<()> {
        let c = self.conns.lock().unwrap().clone();
        write_json(&self.dir.join("connections.json"), &c)
    }

    // --- history ---------------------------------------------------------------------------

    pub fn add_history(&self, e: HistoryEntry) {
        let snapshot = {
            let mut h = self.history.lock().unwrap();
            h.push(e);
            let len = h.len();
            if len > MAX_HISTORY {
                h.drain(0..len - MAX_HISTORY);
            }
            h.clone()
        };
        let _ = write_json(&self.dir.join("history.json"), &snapshot);
    }

    pub fn history(&self, connection_id: Option<&str>) -> Vec<HistoryEntry> {
        let h = self.history.lock().unwrap();
        h.iter().rev().filter(|e| connection_id.is_none_or(|c| e.connection_id == c)).take(500).cloned().collect()
    }

    pub fn clear_history(&self) -> Result<()> {
        self.history.lock().unwrap().clear();
        write_json(&self.dir.join("history.json"), &Vec::<HistoryEntry>::new())
    }

    // --- generic small files ----------------------------------------------------------------

    pub fn read_file<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        read_json(&self.dir.join(name)).ok()
    }

    pub fn write_file<T: Serialize>(&self, name: &str, v: &T) -> Result<()> {
        write_json(&self.dir.join(name), v)
    }

    pub fn secret(&self, key: &str) -> Option<String> {
        self.secrets.get(key)
    }

    pub fn set_secret(&self, key: &str, value: &str) {
        self.secrets.set(key, value)
    }
}

fn secret_key(id: &str, name: &str) -> String {
    format!("conn:{id}:{name}")
}

fn read_json<T: DeserializeOwned>(p: &Path) -> Result<T> {
    let s = std::fs::read_to_string(p)?;
    Ok(serde_json::from_str(&s)?)
}

/// Atomic write (temp file + rename) with owner-only permissions.
pub fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<()> {
    let data = serde_json::to_vec_pretty(v)?;
    write_private(p, &data)
}

pub fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, p).with_context(|| format!("rename to {}", p.display()))?;
    Ok(())
}

fn restrict_dir(_dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(_dir, std::fs::Permissions::from_mode(0o700));
    }
}

// ------------------------------------------------------------------------------------------

/// OS keychain with in-memory cache / fallback.
struct SecretStore {
    persistent: bool,
    backend: String,
    cache: Mutex<HashMap<String, String>>,
    /// Keys that exist only for this session (save_password = false).
    session_only: Mutex<std::collections::HashSet<String>>,
}

impl SecretStore {
    fn new() -> Self {
        let backend = if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service"
        };
        let persistent = std::env::var("SQLIGHTER_NO_KEYRING").is_err() && probe_keyring();
        if !persistent {
            log::warn!("No OS credential store available - secrets are kept in memory only");
        }
        SecretStore {
            persistent,
            backend: if persistent { backend.to_string() } else { "memory (session only)".to_string() },
            cache: Mutex::new(HashMap::new()),
            session_only: Mutex::new(Default::default()),
        }
    }

    fn get(&self, key: &str) -> Option<String> {
        if let Some(v) = self.cache.lock().unwrap().get(key) {
            return Some(v.clone());
        }
        if !self.persistent {
            return None;
        }
        let v = keyring::Entry::new(KEYRING_SERVICE, key).ok()?.get_password().ok()?;
        self.cache.lock().unwrap().insert(key.to_string(), v.clone());
        Some(v)
    }

    fn set(&self, key: &str, value: &str) {
        self.cache.lock().unwrap().insert(key.to_string(), value.to_string());
        self.session_only.lock().unwrap().remove(key);
        if self.persistent {
            if let Err(e) = keyring::Entry::new(KEYRING_SERVICE, key).and_then(|e| e.set_password(value)) {
                log::error!("keyring write failed: {e}");
            }
        }
    }

    fn set_session(&self, key: &str, value: &str) {
        self.cache.lock().unwrap().insert(key.to_string(), value.to_string());
        self.session_only.lock().unwrap().insert(key.to_string());
        self.forget_persistent(key);
    }

    fn forget_persistent(&self, key: &str) {
        if self.persistent {
            if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, key) {
                let _ = e.delete_credential();
            }
        }
    }

    fn delete(&self, key: &str) {
        self.cache.lock().unwrap().remove(key);
        self.session_only.lock().unwrap().remove(key);
        self.forget_persistent(key);
    }
}

fn probe_keyring() -> bool {
    let probe = || -> keyring::Result<bool> {
        let e = keyring::Entry::new(KEYRING_SERVICE, "__probe__")?;
        e.set_password("ok")?;
        let ok = e.get_password()? == "ok";
        let _ = e.delete_credential();
        Ok(ok)
    };
    // Some Secret Service implementations block when no session bus exists; guard with a thread.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe().unwrap_or(false));
    });
    rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap_or(false)
}
