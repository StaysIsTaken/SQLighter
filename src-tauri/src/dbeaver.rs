//! Imports connections from DBeaver (`data-sources*.json` + `credentials-config.json`).
//!
//! DBeaver stores credentials AES-128-CBC encrypted with a key that is the same in every
//! installation, i.e. the file is only obfuscated. Passwords read from it are never sent to the
//! webview: the preview only reports *whether* a password exists, and the import itself happens
//! in the backend, where secrets go straight into the system keychain.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::session::default_port;
use crate::model::{ConnectionConfig, ConnectionSecrets, DbType, Folder, SshAuth, SshConfig, TlsConfig, TlsMode};
use crate::store::Store;

/// DBeaver's built-in key for `credentials-config.json` (public, identical in every installation).
const CREDENTIALS_KEY: [u8; 16] = [0xba, 0xbb, 0x4a, 0x9f, 0x77, 0x4a, 0xb8, 0x53, 0xc9, 0x6c, 0x2d, 0x65, 0x3d, 0xfe, 0x54, 0x4a];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// The data-sources file this connection comes from.
    pub source: String,
    /// DBeaver's connection id.
    pub id: String,
    pub config: ConnectionConfig,
    /// Folder path (DBeaver project first, if it is not the default project).
    pub folder: Vec<String>,
    pub has_password: bool,
    pub has_ssh_secret: bool,
    /// Plain-text connection to a remote host (no TLS, no SSH tunnel).
    pub unencrypted: bool,
    /// A connection with the same type, host, port, user and database already exists.
    pub exists: bool,
    /// Codes, translated in the UI: tls-unverified, oracle-sid, ssh-jump, proxy, auth-model.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub source: String,
    pub name: String,
    pub driver: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub sources: Vec<String>,
    pub connections: Vec<Candidate>,
    /// Connections of database types SQLighter does not support.
    pub skipped: Vec<Skipped>,
    /// Files that could not be read (path: message).
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub source: String,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Import stored passwords into the system keychain.
    pub passwords: bool,
    /// Keep plain-text connections to remote hosts working (sets `tls.allowInsecure`).
    pub allow_insecure: bool,
}

pub struct Parsed {
    pub candidate: Candidate,
    pub secrets: ConnectionSecrets,
}

// --- discovery -------------------------------------------------------------------------------

/// DBeaver workspace directories on this machine (only those that exist).
pub fn workspaces() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::data_dir() {
        v.push(d.join("DBeaverData/workspace6")); // Linux ~/.local/share, Windows %APPDATA%
    }
    if let Some(h) = dirs::home_dir() {
        v.push(h.join("Library/DBeaverData/workspace6")); // macOS
        v.push(h.join(".var/app/io.dbeaver.DBeaverCommunity/data/DBeaverData/workspace6")); // Flatpak
        v.push(h.join("snap/dbeaver-ce/current/.local/share/DBeaverData/workspace6")); // Snap
    }
    v.retain(|p| p.is_dir());
    v.dedup();
    v
}

/// All `data-sources*.json` files of all projects in the given workspaces.
pub fn data_source_files(workspaces: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for ws in workspaces {
        let Ok(projects) = std::fs::read_dir(ws) else { continue };
        let mut projects: Vec<PathBuf> = projects.flatten().map(|e| e.path().join(".dbeaver")).filter(|p| p.is_dir()).collect();
        projects.sort();
        for dir in projects {
            let Ok(files) = std::fs::read_dir(&dir) else { continue };
            let mut files: Vec<PathBuf> = files
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("data-sources") && n.ends_with(".json")))
                .collect();
            files.sort();
            out.extend(files);
        }
    }
    out
}

/// Reads one data-sources file together with the credentials of its project.
pub fn read_file(path: &Path, store: &Store) -> Result<Vec<Parsed>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let ds: Value = serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let dir = path.parent().context("invalid path")?;
    let creds = match std::fs::read(dir.join("credentials-config.json")) {
        Ok(bytes) => decrypt_credentials(&bytes).unwrap_or_else(|e| {
            log::warn!("DBeaver credentials could not be read: {e:#}");
            Value::Null
        }),
        Err(_) => Value::Null,
    };
    // <workspace>/<project>/.dbeaver/data-sources.json
    let project = dir.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("General").to_string();
    let existing = store.tree().connections;
    let mut out = parse(&ds, &creds, &project, &path.to_string_lossy());
    for p in &mut out {
        let c = &p.candidate.config;
        p.candidate.exists = existing.iter().any(|e| same_target(&e.config, c));
    }
    Ok(out)
}

fn same_target(a: &ConnectionConfig, b: &ConnectionConfig) -> bool {
    a.db_type == b.db_type
        && a.host.eq_ignore_ascii_case(&b.host)
        && a.port == b.port
        && a.user == b.user
        && a.database == b.database
        && a.file_path == b.file_path
}

pub fn decrypt_credentials(bytes: &[u8]) -> Result<Value> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    if bytes.len() < 32 || bytes.len() % 16 != 0 {
        bail!("unexpected file size");
    }
    let (iv, data) = bytes.split_at(16);
    let mut buf = data.to_vec();
    let plain = cbc::Decryptor::<aes::Aes128>::new(&CREDENTIALS_KEY.into(), iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| anyhow::anyhow!("decryption failed (project password or secure storage in use?)"))?;
    Ok(serde_json::from_slice(plain)?)
}

// --- parsing ---------------------------------------------------------------------------------

fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(x) if !x.trim().is_empty() => Some(x.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn first(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| s(v, k))
}

fn truthy(v: &Value, key: &str) -> bool {
    match v.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(x)) => x.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn db_type(provider: &str, driver: &str) -> Option<DbType> {
    let p = provider.to_ascii_lowercase();
    let d = driver.to_ascii_lowercase();
    Some(if d.contains("mariadb") {
        DbType::Mariadb
    } else if d.contains("cockroach") {
        DbType::Cockroach
    } else if p == "mysql" || d.starts_with("mysql") {
        DbType::Mysql
    } else if p == "postgresql" || d.starts_with("postgres") {
        DbType::Postgres
    } else if p == "sqlite" || d.contains("sqlite") {
        DbType::Sqlite
    } else if p == "sqlserver" || p == "mssql" || d.contains("sqlserver") || d.contains("mssql") {
        DbType::Mssql
    } else if p == "oracle" || d.starts_with("oracle") {
        DbType::Oracle
    } else {
        return None;
    })
}

/// Host, port and database from a JDBC URL (used when DBeaver was configured by URL).
fn parse_jdbc_url(url: &str) -> (Option<String>, Option<u16>, Option<String>) {
    let u = url.trim();
    if let Some(path) = u.strip_prefix("jdbc:sqlite:") {
        return (None, None, Some(path.to_string()));
    }
    if let Some(rest) = u.split_once("@").map(|x| x.1).filter(|_| u.starts_with("jdbc:oracle:")) {
        // jdbc:oracle:thin:@host:port/service  or  @host:port:SID  or  @//host:port/service
        let rest = rest.trim_start_matches("//");
        let (hp, db) = match rest.split_once('/') {
            Some((hp, db)) => (hp, Some(db.to_string())),
            None => match rest.rsplitn(2, ':').collect::<Vec<_>>().as_slice() {
                [sid, hp] if hp.contains(':') => (*hp, Some(sid.to_string())),
                _ => (rest, None),
            },
        };
        let (h, p) = split_host_port(hp);
        return (h, p, db);
    }
    // jdbc:<sub>://host[:port][/db][?…]   |   jdbc:sqlserver://host:port;databaseName=db
    let Some(after) = u.split_once("://").map(|x| x.1) else { return (None, None, None) };
    let (main, params) = after.split_once(';').unwrap_or((after, ""));
    let main = main.split(['?', '#']).next().unwrap_or("");
    let (hp, db) = main.split_once('/').map(|(a, b)| (a, Some(b.to_string()).filter(|x| !x.is_empty()))).unwrap_or((main, None));
    let db = db.or_else(|| {
        params.split(';').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            matches!(k.to_ascii_lowercase().as_str(), "databasename" | "database").then(|| v.to_string())
        })
    });
    let hp = hp.split(',').next().unwrap_or(hp); // first host of a list
    let (h, p) = split_host_port(hp);
    (h, p, db)
}

fn split_host_port(hp: &str) -> (Option<String>, Option<u16>) {
    let hp = hp.trim();
    if hp.is_empty() {
        return (None, None);
    }
    if let Some(v6) = hp.strip_prefix('[') {
        let (h, rest) = v6.split_once(']').unwrap_or((v6, ""));
        return (Some(h.to_string()), rest.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match hp.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (Some(h.to_string()), p.parse().ok()),
        _ => (Some(hp.to_string()), None),
    }
}

/// Converts DBeaver's JSON into connection candidates. Pure function; `creds` is the decrypted
/// credentials JSON (or Null).
pub fn parse(ds: &Value, creds: &Value, project: &str, source: &str) -> Vec<Parsed> {
    let mut out = Vec::new();
    let Some(conns) = ds.get("connections").and_then(|c| c.as_object()) else { return out };
    for (id, c) in conns {
        let provider = s(c, "provider").unwrap_or_default();
        let driver = s(c, "driver").unwrap_or_default();
        let name = s(c, "name").unwrap_or_else(|| id.clone());
        let Some(t) = db_type(&provider, &driver) else {
            out.push(Parsed {
                candidate: Candidate {
                    source: source.into(),
                    id: id.clone(),
                    config: ConnectionConfig { name, ..blank(DbType::Postgres) },
                    folder: vec![],
                    has_password: false,
                    has_ssh_secret: false,
                    unencrypted: false,
                    exists: false,
                    warnings: vec![format!("unsupported:{provider}/{driver}")],
                },
                secrets: ConnectionSecrets::default(),
            });
            continue;
        };
        let conf = c.get("configuration").cloned().unwrap_or(Value::Null);
        let cred = creds.get(id).cloned().unwrap_or(Value::Null);
        let main_cred = cred.get("#connection").cloned().unwrap_or(Value::Null);
        let mut warnings = Vec::new();

        let mut cfg = blank(t);
        cfg.name = name;
        let (url_host, url_port, url_db) = s(&conf, "url").map(|u| parse_jdbc_url(&u)).unwrap_or((None, None, None));
        cfg.host = s(&conf, "host").or(url_host).unwrap_or_default();
        cfg.port = s(&conf, "port").and_then(|p| p.parse().ok()).or(url_port).unwrap_or(default_port(t));
        let database = s(&conf, "database").or(url_db).unwrap_or_default();
        cfg.user = s(&main_cred, "user").or_else(|| s(&conf, "user")).unwrap_or_default();
        cfg.read_only = truthy(c, "read-only") || truthy(&conf, "read-only");
        cfg.production = s(&conf, "type").is_some_and(|x| x.eq_ignore_ascii_case("prod"));
        match t {
            DbType::Sqlite => {
                cfg.file_path = Some(database).filter(|d| !d.is_empty());
                cfg.host.clear();
                cfg.port = 0;
            }
            DbType::Oracle => {
                let sid = c.pointer("/configuration/provider-properties/@dbeaver-sid-service@").and_then(|v| v.as_str()).is_some_and(|x| x.eq_ignore_ascii_case("SID"));
                if sid {
                    warnings.push("oracle-sid".into());
                }
                cfg.service_name = Some(database).filter(|d| !d.is_empty());
            }
            DbType::Mssql => {
                if let Some((h, inst)) = cfg.host.clone().split_once('\\') {
                    cfg.host = h.to_string();
                    cfg.instance_name = Some(inst.to_string());
                }
                cfg.database = database;
            }
            _ => cfg.database = database,
        }
        if let Some(model) = s(&conf, "auth-model") {
            if !model.eq_ignore_ascii_case("native") {
                warnings.push("auth-model".into());
            }
        }

        let password = s(&main_cred, "password").or_else(|| s(&conf, "password"));
        let handlers = conf.get("handlers").and_then(|h| h.as_object()).cloned().unwrap_or_default();

        // SSH tunnel
        let mut secrets = ConnectionSecrets { password: password.clone(), ..Default::default() };
        let mut has_ssh_secret = false;
        if let Some(h) = handlers.get("ssh_tunnel").filter(|h| truthy(h, "enabled")) {
            let props = h.get("properties").cloned().unwrap_or(Value::Null);
            let ssh_cred = cred.get("network/ssh_tunnel").cloned().unwrap_or(Value::Null);
            let auth = match s(&props, "authType").unwrap_or_default().to_ascii_uppercase().as_str() {
                "PUBLIC_KEY" => SshAuth::Key,
                "AGENT" => SshAuth::Agent,
                _ => SshAuth::Password,
            };
            cfg.ssh = SshConfig {
                enabled: true,
                host: s(&props, "host").unwrap_or_default(),
                port: s(&props, "port").and_then(|p| p.parse().ok()).unwrap_or(22),
                user: s(&ssh_cred, "user").or_else(|| s(&props, "user")).unwrap_or_default(),
                auth,
                key_file: s(&props, "keyPath").filter(|_| auth == SshAuth::Key),
                host_key_fingerprint: None,
            };
            let secret = s(&ssh_cred, "password").or_else(|| s(&props, "password"));
            has_ssh_secret = secret.is_some();
            match auth {
                SshAuth::Key => secrets.ssh_passphrase = secret,
                SshAuth::Password => secrets.ssh_password = secret,
                SshAuth::Agent => {}
            }
            if props.as_object().is_some_and(|o| o.keys().any(|k| k.to_ascii_lowercase().contains("jump"))) {
                warnings.push("ssh-jump".into());
            }
        }
        if handlers.iter().any(|(k, h)| k.contains("proxy") && truthy(h, "enabled")) {
            warnings.push("proxy".into());
        }

        // TLS
        let ssl = handlers.iter().find(|(k, h)| k.contains("ssl") && truthy(h, "enabled")).map(|(_, h)| h.get("properties").cloned().unwrap_or(Value::Null));
        let ssl_mode = ssl.as_ref().and_then(|p| first(p, &["ssl.mode", "sslMode", "sslmode"])).map(|m| m.to_ascii_lowercase());
        let local = t == DbType::Sqlite || crate::tls::is_loopback(&cfg.host);
        cfg.tls = match (&ssl, ssl_mode.as_deref()) {
            (Some(_), Some("disable")) | (None, _) => TlsConfig { mode: TlsMode::Disabled, ..Default::default() },
            (Some(p), mode) => {
                let mode = match mode {
                    Some("verify-full") => TlsMode::VerifyFull,
                    Some("verify-ca") => TlsMode::VerifyCa,
                    _ if truthy(p, "ssl.verify.server") => TlsMode::VerifyFull,
                    _ => {
                        // DBeaver encrypted without checking the certificate; SQLighter always checks.
                        warnings.push("tls-unverified".into());
                        TlsMode::VerifyFull
                    }
                };
                TlsConfig {
                    mode,
                    ca_file: first(p, &["ssl.ca.cert", "sslRootCert", "sslrootcert", "ssl.ca"]),
                    cert_file: first(p, &["ssl.client.cert", "sslCert", "sslcert"]),
                    key_file: first(p, &["ssl.client.key", "sslKey", "sslkey"]),
                    ..Default::default()
                }
            }
        };
        let unencrypted = cfg.tls.mode == TlsMode::Disabled && !cfg.ssh.enabled && !local;

        cfg.save_password = truthy(c, "save-password") || password.is_some();
        let mut folder = Vec::new();
        if !project.eq_ignore_ascii_case("General") {
            folder.push(project.to_string());
        }
        if let Some(f) = s(c, "folder") {
            folder.extend(f.split('/').filter(|x| !x.trim().is_empty()).map(|x| x.trim().to_string()));
        }

        out.push(Parsed {
            candidate: Candidate {
                source: source.into(),
                id: id.clone(),
                config: cfg,
                folder,
                has_password: password.is_some(),
                has_ssh_secret,
                unencrypted,
                exists: false,
                warnings,
            },
            secrets,
        });
    }
    out.sort_by(|a, b| (&a.candidate.folder, a.candidate.config.name.to_lowercase()).cmp(&(&b.candidate.folder, b.candidate.config.name.to_lowercase())));
    out
}

fn blank(t: DbType) -> ConnectionConfig {
    ConnectionConfig {
        id: String::new(),
        name: String::new(),
        db_type: t,
        folder_id: None,
        color: None,
        host: String::new(),
        port: default_port(t),
        database: String::new(),
        user: String::new(),
        file_path: None,
        service_name: None,
        instance_name: None,
        save_password: true,
        read_only: false,
        production: false,
        tls: TlsConfig::default(),
        ssh: SshConfig::default(),
        order: None,
        created_at: 0,
    }
}

/// Splits parsed entries into supported candidates and skipped (unsupported) ones.
pub fn split_supported(parsed: Vec<Parsed>) -> (Vec<Parsed>, Vec<Skipped>) {
    let mut ok = Vec::new();
    let mut skipped = Vec::new();
    for p in parsed {
        match p.candidate.warnings.iter().find_map(|w| w.strip_prefix("unsupported:")) {
            Some(driver) => skipped.push(Skipped { source: p.candidate.source.clone(), name: p.candidate.config.name.clone(), driver: driver.to_string() }),
            None => ok.push(p),
        }
    }
    (ok, skipped)
}

// --- import ----------------------------------------------------------------------------------

/// Returns the id of the folder at `path`, creating missing folders (existing ones with the
/// same name and parent are reused).
fn ensure_folder(store: &Store, path: &[String], cache: &mut HashMap<Vec<String>, String>) -> Result<Option<String>> {
    let mut parent: Option<String> = None;
    for i in 0..path.len() {
        let key = path[..=i].to_vec();
        if let Some(id) = cache.get(&key) {
            parent = Some(id.clone());
            continue;
        }
        let existing = store.tree().folders.into_iter().find(|f| f.parent_id == parent && f.name == path[i]);
        let id = match existing {
            Some(f) => f.id,
            None => store.save_folder(Folder { id: String::new(), name: path[i].clone(), parent_id: parent.clone(), order: None, collapsed: false })?.id,
        };
        cache.insert(key, id.clone());
        parent = Some(id);
    }
    Ok(parent)
}

pub fn import(store: &Store, parsed: Vec<Parsed>, opts: &ImportOptions) -> Result<usize> {
    let mut cache = HashMap::new();
    let mut n = 0;
    for p in parsed {
        let mut cfg = p.candidate.config;
        cfg.folder_id = ensure_folder(store, &p.candidate.folder, &mut cache)?;
        if p.candidate.unencrypted && opts.allow_insecure {
            cfg.tls.allow_insecure = true;
        }
        let secrets = if opts.passwords {
            p.secrets
        } else {
            cfg.save_password = false;
            ConnectionSecrets::default()
        };
        store.save_connection(cfg, Some(secrets))?;
        n += 1;
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn encrypt(v: &Value) -> Vec<u8> {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        let iv = [7u8; 16];
        let plain = serde_json::to_vec(v).unwrap();
        let mut buf = vec![0u8; plain.len() + 16];
        buf[..plain.len()].copy_from_slice(&plain);
        let ct = cbc::Encryptor::<aes::Aes128>::new(&CREDENTIALS_KEY.into(), &iv.into()).encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len()).unwrap().to_vec();
        [iv.to_vec(), ct].concat()
    }

    fn sample() -> (Value, Value) {
        let ds = json!({
            "folders": {"Prod": {}, "Prod/Shop": {"parent": "Prod"}},
            "connections": {
                "mariaDB-1": {
                    "provider": "mysql", "driver": "mariaDB", "name": "DB", "save-password": true, "folder": "Prod/Shop",
                    "configuration": {"host": "100.99.21.10", "port": "3307", "database": "", "url": "jdbc:mariadb://100.99.21.10:3307/", "type": "prod", "auth-model": "native"}
                },
                "postgres-jdbc-2": {
                    "provider": "postgresql", "driver": "postgres-jdbc", "name": "PG", "read-only": true,
                    "configuration": {"host": "db.example.com", "port": "5432", "database": "app",
                        "handlers": {
                            "postgre_ssl": {"type": "CONFIG", "enabled": true, "properties": {"sslMode": "require", "ssl.ca.cert": "/ca.pem"}},
                            "ssh_tunnel": {"type": "TUNNEL", "enabled": true, "save-password": true, "properties": {"host": "bastion", "port": 2222, "authType": "PUBLIC_KEY", "keyPath": "/k"}}
                        }}
                },
                "mssql-3": {"provider": "sqlserver", "driver": "microsoft", "name": "MS", "configuration": {"url": "jdbc:sqlserver://sql1\\inst:1444;databaseName=erp"}},
                "sqlite-4": {"provider": "sqlite", "driver": "sqlite_jdbc", "name": "Local", "configuration": {"database": "/data/a.db"}},
                "mongo-5": {"provider": "mongodb", "driver": "mongo", "name": "Mongo", "configuration": {}}
            }
        });
        let creds = json!({
            "mariaDB-1": {"#connection": {"user": "root", "password": "p@ss:w#rd"}},
            "postgres-jdbc-2": {"#connection": {"user": "app"}, "network/ssh_tunnel": {"user": "jump", "password": "keypass"}}
        });
        (ds, creds)
    }

    #[test]
    fn decrypts_credentials() {
        let (_, creds) = sample();
        assert_eq!(decrypt_credentials(&encrypt(&creds)).unwrap(), creds);
        assert!(decrypt_credentials(&[0u8; 48]).is_err());
    }

    #[test]
    fn parses_connections() {
        let (ds, creds) = sample();
        let (ok, skipped) = split_supported(parse(&ds, &creds, "General", "f"));
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].driver, "mongodb/mongo");
        let get = |n: &str| ok.iter().find(|p| p.candidate.config.name == n).unwrap();

        let m = get("DB");
        assert_eq!(m.candidate.config.db_type, DbType::Mariadb);
        assert_eq!((m.candidate.config.host.as_str(), m.candidate.config.port), ("100.99.21.10", 3307));
        assert_eq!(m.candidate.config.user, "root");
        assert_eq!(m.secrets.password.as_deref(), Some("p@ss:w#rd"));
        assert!(m.candidate.config.production && m.candidate.unencrypted);
        assert_eq!(m.candidate.folder, vec!["Prod", "Shop"]);

        let p = get("PG");
        assert!(p.candidate.config.read_only);
        assert_eq!(p.candidate.config.tls.mode, TlsMode::VerifyFull);
        assert_eq!(p.candidate.config.tls.ca_file.as_deref(), Some("/ca.pem"));
        assert!(p.candidate.warnings.contains(&"tls-unverified".to_string()));
        assert!(p.candidate.config.ssh.enabled && p.candidate.config.ssh.auth == SshAuth::Key);
        assert_eq!((p.candidate.config.ssh.host.as_str(), p.candidate.config.ssh.port, p.candidate.config.ssh.user.as_str()), ("bastion", 2222, "jump"));
        assert_eq!(p.secrets.ssh_passphrase.as_deref(), Some("keypass"));
        assert!(!p.candidate.has_password && !p.candidate.unencrypted);

        let ms = get("MS");
        assert_eq!((ms.candidate.config.host.as_str(), ms.candidate.config.port), ("sql1", 1444));
        assert_eq!(ms.candidate.config.instance_name.as_deref(), Some("inst"));
        assert_eq!(ms.candidate.config.database, "erp");

        let l = get("Local");
        assert_eq!(l.candidate.config.file_path.as_deref(), Some("/data/a.db"));
        assert!(!l.candidate.unencrypted);
    }

    #[test]
    fn jdbc_urls() {
        assert_eq!(parse_jdbc_url("jdbc:postgresql://h:5433/db?ssl=true"), (Some("h".into()), Some(5433), Some("db".into())));
        assert_eq!(parse_jdbc_url("jdbc:oracle:thin:@h:1521/ORCL"), (Some("h".into()), Some(1521), Some("ORCL".into())));
        assert_eq!(parse_jdbc_url("jdbc:oracle:thin:@h:1521:XE"), (Some("h".into()), Some(1521), Some("XE".into())));
        assert_eq!(parse_jdbc_url("jdbc:mysql://[::1]:3306/x"), (Some("::1".into()), Some(3306), Some("x".into())));
    }

    #[test]
    fn imports_into_folders() {
        std::env::set_var("SQLIGHTER_NO_KEYRING", "1");
        let dir = std::env::temp_dir().join(format!("sqlighter-dbeaver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.clone()).unwrap();
        let (ds, creds) = sample();
        let (ok, _) = split_supported(parse(&ds, &creds, "Work", "f"));
        let opts = ImportOptions { passwords: true, allow_insecure: false };
        assert_eq!(import(&store, ok, &opts).unwrap(), 4);
        let tree = store.tree();
        let work = tree.folders.iter().find(|f| f.name == "Work" && f.parent_id.is_none()).unwrap();
        let prod = tree.folders.iter().find(|f| f.name == "Prod").unwrap();
        assert_eq!(prod.parent_id.as_deref(), Some(work.id.as_str()));
        let db = tree.connections.iter().find(|c| c.config.name == "DB").unwrap();
        assert!(db.has_password && !db.config.tls.allow_insecure);
        assert_eq!(store.secrets_for(&db.config.id).password.as_deref(), Some("p@ss:w#rd"));
        // Importing again reuses the folders.
        let (ok, _) = split_supported(parse(&ds, &creds, "Work", "f"));
        import(&store, ok, &opts).unwrap();
        assert_eq!(store.tree().folders.len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
