//! SSH tunnels with strict host key verification.
//!
//! A host key is accepted only if it matches the fingerprint stored for the connection, the
//! user's ~/.ssh/known_hosts, or the user explicitly confirms it (trust on first use). A CHANGED
//! host key is treated as a possible man-in-the-middle attack and requires an explicit decision.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, AuthResult, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::model::{ConnectionSecrets, HostKeyPrompt, SshAuth, SshConfig};

pub type PromptFn = Arc<dyn Fn(HostKeyPrompt) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

struct Verifier {
    host: String,
    port: u16,
    expected: Option<String>,
    prompt: PromptFn,
    accepted: Arc<Mutex<Option<String>>>,
}

impl client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key: &PublicKey = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key,
            // Certificates would need a CA configuration; not supported -> reject.
            russh::keys::PublicKeyOrCertificate::Certificate(_) => return Ok(false),
        };
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let algorithm = key.algorithm().to_string();
        if let Some(expected) = &self.expected {
            if expected == &fp {
                *self.accepted.lock().unwrap() = Some(fp);
                return Ok(true);
            }
            // Key changed -> possible MITM. Only continue if the user explicitly re-trusts it.
            let ok = (self.prompt)(HostKeyPrompt {
                prompt_id: uuid::Uuid::new_v4().to_string(),
                host: self.host.clone(),
                port: self.port,
                fingerprint: fp.clone(),
                algorithm,
                previous: Some(expected.clone()),
            })
            .await;
            if ok {
                *self.accepted.lock().unwrap() = Some(fp);
            }
            return Ok(ok);
        }
        match russh::keys::check_known_hosts(&self.host, self.port, key) {
            Ok(true) => {
                *self.accepted.lock().unwrap() = Some(fp);
                return Ok(true);
            }
            Err(russh::keys::Error::KeyChanged { .. }) => {
                let ok = (self.prompt)(HostKeyPrompt {
                    prompt_id: uuid::Uuid::new_v4().to_string(),
                    host: self.host.clone(),
                    port: self.port,
                    fingerprint: fp.clone(),
                    algorithm,
                    previous: Some("(different key in ~/.ssh/known_hosts)".into()),
                })
                .await;
                if ok {
                    *self.accepted.lock().unwrap() = Some(fp);
                }
                return Ok(ok);
            }
            _ => {}
        }
        let ok = (self.prompt)(HostKeyPrompt {
            prompt_id: uuid::Uuid::new_v4().to_string(),
            host: self.host.clone(),
            port: self.port,
            fingerprint: fp.clone(),
            algorithm,
            previous: None,
        })
        .await;
        if ok {
            *self.accepted.lock().unwrap() = Some(fp);
        }
        Ok(ok)
    }
}

pub struct SshTunnel {
    handle: Arc<Handle<Verifier>>,
    target_host: String,
    target_port: u16,
    listener: Mutex<Option<(u16, JoinHandle<()>)>>,
    /// Fingerprint the user trusted (store it in the connection config).
    pub accepted_fingerprint: Option<String>,
}

impl SshTunnel {
    pub async fn open(cfg: &SshConfig, secrets: &ConnectionSecrets, target_host: &str, target_port: u16, prompt: PromptFn) -> Result<SshTunnel> {
        if cfg.host.trim().is_empty() || cfg.user.trim().is_empty() {
            bail!("SSH host and user are required");
        }
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 4,
            ..Default::default()
        });
        let accepted = Arc::new(Mutex::new(None));
        let verifier = Verifier {
            host: cfg.host.clone(),
            port: cfg.port,
            expected: cfg.host_key_fingerprint.clone().filter(|s| !s.is_empty()),
            prompt,
            accepted: accepted.clone(),
        };
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let mut handle = tokio::time::timeout(Duration::from_secs(20), client::connect(config, addr.as_str(), verifier))
            .await
            .map_err(|_| anyhow!("SSH connection to {addr} timed out"))?
            .map_err(|e| match e {
                russh::Error::UnknownKey => anyhow!("SSH host key was rejected - connection aborted to protect against a man-in-the-middle attack"),
                e => anyhow!("SSH connection to {addr} failed: {e}"),
            })?;

        let ok = match cfg.auth {
            SshAuth::Password => {
                let pw = secrets.ssh_password.clone().unwrap_or_default();
                matches!(handle.authenticate_password(&cfg.user, pw).await?, AuthResult::Success)
            }
            SshAuth::Key => {
                let path = cfg.key_file.as_deref().filter(|s| !s.is_empty()).context("SSH private key file is required")?;
                let path = expand_home(path);
                let key = russh::keys::load_secret_key(&path, secrets.ssh_passphrase.as_deref().filter(|s| !s.is_empty()))
                    .with_context(|| format!("load SSH key {path}"))?;
                let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
                let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
                matches!(handle.authenticate_publickey(&cfg.user, key).await?, AuthResult::Success)
            }
            SshAuth::Agent => agent_auth(&mut handle, &cfg.user).await?,
        };
        if !ok {
            bail!("SSH authentication failed for {}@{}", cfg.user, cfg.host);
        }
        let accepted_fingerprint = accepted.lock().unwrap().clone();
        Ok(SshTunnel {
            handle: Arc::new(handle),
            target_host: target_host.to_string(),
            target_port,
            listener: Mutex::new(None),
            accepted_fingerprint,
        })
    }

    /// A direct-tcpip stream to the database through the tunnel (no local port involved).
    pub async fn stream(&self) -> Result<russh::ChannelStream<russh::client::Msg>> {
        let ch = self
            .handle
            .channel_open_direct_tcpip(self.target_host.clone(), self.target_port as u32, "127.0.0.1", 0)
            .await
            .with_context(|| format!("SSH port forwarding to {}:{} failed", self.target_host, self.target_port))?;
        Ok(ch.into_stream())
    }

    /// For drivers that can only connect to host:port: forwards a random loopback port.
    pub async fn local_port(&self) -> Result<u16> {
        if let Some((port, _)) = &*self.listener.lock().unwrap() {
            return Ok(*port);
        }
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let handle = self.handle.clone();
        let (host, tport) = (self.target_host.clone(), self.target_port);
        let task = tokio::spawn(async move {
            while let Ok((mut sock, peer)) = listener.accept().await {
                // Only loopback peers can reach this socket anyway (bound to 127.0.0.1).
                if !peer.ip().is_loopback() {
                    continue;
                }
                let handle = handle.clone();
                let host = host.clone();
                tokio::spawn(async move {
                    if let Ok(ch) = handle.channel_open_direct_tcpip(host, tport as u32, "127.0.0.1", peer.port() as u32).await {
                        let mut s = ch.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut sock, &mut s).await;
                    }
                });
            }
        });
        *self.listener.lock().unwrap() = Some((port, task));
        Ok(port)
    }

    pub async fn close(&self) {
        if let Some((_, t)) = self.listener.lock().unwrap().take() {
            t.abort();
        }
        let _ = self.handle.disconnect(russh::Disconnect::ByApplication, "bye", "en").await;
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        if let Some((_, t)) = self.listener.lock().unwrap().take() {
            t.abort();
        }
    }
}

async fn agent_auth(handle: &mut Handle<Verifier>, user: &str) -> Result<bool> {
    #[cfg(unix)]
    let mut agent = russh::keys::agent::client::AgentClient::connect_env().await.context("SSH agent not available (SSH_AUTH_SOCK)")?;
    #[cfg(windows)]
    let mut agent = russh::keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await.context("OpenSSH agent not available")?;
    let ids = agent.request_identities().await?;
    let hash = handle.best_supported_rsa_hash().await.ok().flatten().flatten();
    for id in ids {
        let key = match &id {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key.clone(),
            _ => continue,
        };
        if let Ok(AuthResult::Success) = handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn expand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return std::path::Path::new(&home).join(rest).to_string_lossy().into_owned();
        }
    }
    p.to_string()
}
