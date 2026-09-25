//! An open connection: one connection for the SQL editor, one for metadata / table browsing,
//! optional SSH tunnel, transaction state and cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use tokio::sync::{Mutex, MutexGuard, Notify};

use super::{Canceller, Conn};
use crate::model::*;
use crate::sql;
use crate::ssh::{PromptFn, SshTunnel};

pub struct Session {
    pub cfg: ConnectionConfig,
    secrets: ConnectionSecrets,
    pub tunnel: Option<Arc<SshTunnel>>,
    main: Mutex<Option<Conn>>,
    meta: Mutex<Option<Conn>>,
    canceller: StdMutex<Option<Canceller>>,
    abort: Notify,
    running: AtomicBool,
    pub in_tx: AtomicBool,
    pub auto_commit: AtomicBool,
    pub server_version: String,
}

pub struct ExecSettings {
    pub confirm_destructive: bool,
    pub max_rows: usize,
    pub timeout: Option<Duration>,
}

impl Session {
    /// Opens the session. Returns the SSH host key fingerprint that the user newly trusted.
    pub async fn open(cfg: ConnectionConfig, secrets: ConnectionSecrets, prompt: PromptFn, auto_commit: bool) -> Result<(Session, Option<String>)> {
        let mut new_fp = None;
        let tunnel = if cfg.ssh.enabled && cfg.db_type != DbType::Sqlite {
            let port = if cfg.port == 0 { default_port(cfg.db_type) } else { cfg.port };
            let t = SshTunnel::open(&cfg.ssh, &secrets, &cfg.host, port, prompt).await?;
            if t.accepted_fingerprint.is_some() && t.accepted_fingerprint != cfg.ssh.host_key_fingerprint {
                new_fp = t.accepted_fingerprint.clone();
            }
            Some(Arc::new(t))
        } else {
            None
        };
        let mut main = Conn::open(&cfg, &secrets, tunnel.clone()).await?;
        let server_version = main.server_version().await;
        let canceller = main.canceller(&cfg);
        let s = Session {
            secrets,
            tunnel,
            main: Mutex::new(Some(main)),
            meta: Mutex::new(None),
            canceller: StdMutex::new(Some(canceller)),
            abort: Notify::new(),
            running: AtomicBool::new(false),
            in_tx: AtomicBool::new(false),
            auto_commit: AtomicBool::new(auto_commit),
            server_version,
            cfg,
        };
        Ok((s, new_fp))
    }

    pub fn dialect(&self) -> Dialect {
        self.cfg.dialect()
    }

    /// A fresh, independent connection (export, backup, transfer run beside the editor).
    pub async fn new_conn(&self) -> Result<Conn> {
        Conn::open(&self.cfg, &self.secrets, self.tunnel.clone()).await
    }

    pub async fn meta(&self) -> Result<MutexGuard<'_, Option<Conn>>> {
        let mut g = self.meta.lock().await;
        if g.as_ref().is_none_or(|c| c.is_broken()) {
            *g = Some(self.new_conn().await?);
        }
        Ok(g)
    }

    async fn main(&self) -> Result<MutexGuard<'_, Option<Conn>>> {
        let mut g = self.main.lock().await;
        if g.as_ref().is_none_or(|c| c.is_broken()) {
            if self.in_tx.swap(false, Ordering::SeqCst) {
                log::warn!("connection lost inside a transaction; uncommitted changes are gone");
            }
            let c = self.new_conn().await?;
            *self.canceller.lock().unwrap() = Some(c.canceller(&self.cfg));
            *g = Some(c);
        }
        Ok(g)
    }

    /// Checks statements against read-only / production / destructive rules.
    pub fn check(&self, stmts: &[String], confirmed: bool, confirm_destructive: bool) -> Result<Option<NeedsConfirmation>> {
        let mut destructive = vec![];
        let mut modifying = vec![];
        for s in stmts {
            let d = sql::analyze(s);
            if self.cfg.read_only && d.modifies && !is_tx_control(s) {
                bail!("This connection is read-only. The statement was not executed:\n{}", truncate(s, 200));
            }
            if d.destructive {
                destructive.push((s.clone(), d.reason.unwrap_or("")));
            } else if d.modifies && !is_tx_control(s) {
                modifying.push(s.clone());
            }
        }
        if confirmed {
            return Ok(None);
        }
        if confirm_destructive && !destructive.is_empty() {
            let reasons: Vec<&str> = destructive.iter().map(|x| x.1).collect();
            return Ok(Some(NeedsConfirmation { reason: reasons.join(", "), statements: destructive.into_iter().map(|x| x.0).collect() }));
        }
        if self.cfg.production && (!modifying.is_empty() || !destructive.is_empty()) {
            let mut all: Vec<String> = destructive.into_iter().map(|x| x.0).collect();
            all.extend(modifying);
            return Ok(Some(NeedsConfirmation { reason: "production".into(), statements: all }));
        }
        Ok(None)
    }

    pub async fn execute(&self, script: &str, opts: &ExecuteOptions, es: &ExecSettings) -> Result<ExecuteResponse> {
        let stmts = sql::split_statements(script, self.dialect());
        if let Some(nc) = self.check(&stmts, opts.confirmed, es.confirm_destructive)? {
            return Ok(ExecuteResponse { results: vec![], needs_confirmation: Some(nc), in_transaction: self.in_tx.load(Ordering::SeqCst) });
        }
        let max_rows = opts.max_rows.unwrap_or(es.max_rows).max(1);
        let mut g = self.main().await?;
        let mut results = Vec::new();
        self.running.store(true, Ordering::SeqCst);
        for stmt in &stmts {
            let conn = match g.as_mut() {
                Some(c) => c,
                None => break,
            };
            let kw = sql::leading_keyword(stmt);
            if !self.auto_commit.load(Ordering::SeqCst) && !self.in_tx.load(Ordering::SeqCst) && !is_tx_control(stmt) {
                if let Err(e) = conn.begin().await {
                    results.push(QueryResult::failed(stmt, format!("{e:#}")));
                    break;
                }
                self.in_tx.store(true, Ordering::SeqCst);
            }
            let start = Instant::now();
            let fut = conn.run(stmt, max_rows);
            let outcome = tokio::select! {
                r = async {
                    match es.timeout {
                        Some(t) => tokio::time::timeout(t, fut).await.unwrap_or_else(|_| Err(anyhow!("Query timed out after {}s", t.as_secs()))),
                        None => fut.await,
                    }
                } => r,
                _ = self.abort.notified() => Err(anyhow!("Query cancelled")),
            };
            let elapsed = start.elapsed().as_millis() as u64;
            match outcome {
                Ok(mut rs) => {
                    for r in rs.iter_mut() {
                        r.duration_ms = elapsed;
                    }
                    results.extend(rs);
                    match kw.as_str() {
                        "BEGIN" | "START" => self.in_tx.store(true, Ordering::SeqCst),
                        "COMMIT" | "ROLLBACK" | "END" => self.in_tx.store(false, Ordering::SeqCst),
                        _ => {}
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let mut r = QueryResult::failed(stmt, msg.clone());
                    r.duration_ms = elapsed;
                    results.push(r);
                    if msg == "Query cancelled" || msg.starts_with("Query timed out") {
                        // The protocol state is unknown after dropping the future: reconnect lazily.
                        if matches!(*self.canceller.lock().unwrap(), Some(Canceller::Abort)) {
                            *g = None;
                            self.in_tx.store(false, Ordering::SeqCst);
                        }
                    }
                    break;
                }
            }
        }
        self.running.store(false, Ordering::SeqCst);
        Ok(ExecuteResponse { results, needs_confirmation: None, in_transaction: self.in_tx.load(Ordering::SeqCst) })
    }

    pub async fn cancel(&self) -> Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }
        let c = self.canceller.lock().unwrap().clone();
        match c {
            Some(Canceller::Abort) | None => self.abort.notify_waiters(),
            Some(c) => c.cancel().await?,
        }
        Ok(())
    }

    pub async fn commit(&self) -> Result<()> {
        let mut g = self.main().await?;
        if let Some(c) = g.as_mut() {
            c.commit().await?;
        }
        self.in_tx.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub async fn rollback(&self) -> Result<()> {
        let mut g = self.main().await?;
        if let Some(c) = g.as_mut() {
            c.rollback().await?;
        }
        self.in_tx.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub async fn set_auto_commit(&self, on: bool) -> Result<()> {
        if on && self.in_tx.load(Ordering::SeqCst) {
            self.commit().await?;
        }
        self.auto_commit.store(on, Ordering::SeqCst);
        Ok(())
    }

    /// Runs statements atomically on the metadata connection (used for saving grid edits).
    pub async fn apply_in_transaction(&self, stmts: &[String]) -> Result<u64> {
        if self.cfg.read_only {
            bail!("This connection is read-only");
        }
        let mut g = self.meta().await?;
        let c = g.as_mut().unwrap();
        c.begin().await?;
        let mut total = 0;
        for s in stmts {
            match c.run(s, 0).await {
                Ok(r) => {
                    let n = r.iter().filter_map(|x| x.affected_rows).sum::<u64>();
                    total += n;
                }
                Err(e) => {
                    let _ = c.rollback().await;
                    return Err(e.context(format!("statement failed, all changes were rolled back:\n{}", truncate(s, 300))));
                }
            }
        }
        c.commit().await?;
        Ok(total)
    }

    /// Runs a single read-only query inside a READ ONLY transaction that is always rolled back.
    pub async fn read_only_query(&self, stmt: &str, max_rows: usize) -> Result<QueryResult> {
        let stmts = sql::split_statements(stmt, self.dialect());
        if stmts.len() != 1 || !sql::is_read_only(&stmts[0]) {
            bail!("Only a single read-only statement (SELECT/WITH/SHOW/EXPLAIN) is allowed");
        }
        let mut g = self.meta().await?;
        let c = g.as_mut().unwrap();
        c.begin_read_only().await?;
        let start = Instant::now();
        let r = tokio::time::timeout(Duration::from_secs(60), c.run(&stmts[0], max_rows)).await.unwrap_or_else(|_| Err(anyhow!("query timed out")));
        let _ = c.end_read_only().await;
        let mut r = r?.into_iter().find(|x| x.is_result_set).ok_or_else(|| anyhow!("statement returned no rows"))?;
        r.duration_ms = start.elapsed().as_millis() as u64;
        Ok(r)
    }

    pub async fn close(&self) {
        *self.main.lock().await = None;
        *self.meta.lock().await = None;
        if let Some(t) = &self.tunnel {
            t.close().await;
        }
    }

    pub fn is_sqlite(&self) -> bool {
        self.cfg.db_type == DbType::Sqlite
    }

    pub fn secrets(&self) -> &ConnectionSecrets {
        &self.secrets
    }
}

pub fn default_port(t: DbType) -> u16 {
    match t {
        DbType::Postgres => 5432,
        DbType::Cockroach => 26257,
        DbType::Mysql | DbType::Mariadb => 3306,
        DbType::Mssql => 1433,
        DbType::Oracle => 1521,
        DbType::Sqlite => 0,
    }
}

fn is_tx_control(s: &str) -> bool {
    let kw = sql::leading_keyword(s);
    matches!(kw.as_str(), "BEGIN" | "COMMIT" | "ROLLBACK" | "END" | "SAVEPOINT" | "RELEASE")
        || (kw == "START" && s.to_ascii_uppercase().contains("TRANSACTION"))
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}
