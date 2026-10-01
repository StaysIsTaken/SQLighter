//! An open connection: one connection for the SQL editor, one for metadata / table browsing,
//! optional SSH tunnel, transaction state and cancellation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use tokio::sync::{Mutex, MutexGuard, Notify};

use super::{is_connection_lost, Canceller, Conn};
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
    /// When `main` / `meta` were last used: idle connections are pinged before reuse, because a
    /// server timeout or a firewall may have dropped them in the meantime.
    main_used: StdMutex<Instant>,
    meta_used: StdMutex<Instant>,
    /// The connection was lost while a transaction was open. Reported once to the user.
    tx_lost: AtomicBool,
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
            main_used: StdMutex::new(Instant::now()),
            meta_used: StdMutex::new(Instant::now()),
            tx_lost: AtomicBool::new(false),
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
        if !alive(&mut g, &self.meta_used, false).await {
            *g = Some(self.new_conn().await.map_err(reconnect_failed)?);
        }
        *self.meta_used.lock().unwrap() = Instant::now();
        Ok(g)
    }

    async fn main(&self) -> Result<MutexGuard<'_, Option<Conn>>> {
        let mut g = self.main.lock().await;
        if !alive(&mut g, &self.main_used, false).await {
            self.reopen_main(&mut g).await?;
        }
        *self.main_used.lock().unwrap() = Instant::now();
        Ok(g)
    }

    /// Replaces the (lost) editor connection with a fresh one.
    async fn reopen_main(&self, g: &mut MutexGuard<'_, Option<Conn>>) -> Result<()> {
        **g = None;
        if self.in_tx.swap(false, Ordering::SeqCst) {
            log::warn!("connection lost inside a transaction; uncommitted changes are gone");
            self.tx_lost.store(true, Ordering::SeqCst);
        }
        let c = self.new_conn().await.map_err(reconnect_failed)?;
        *self.canceller.lock().unwrap() = Some(c.canceller(&self.cfg));
        **g = Some(c);
        Ok(())
    }

    /// Background health check: makes sure the editor connection still works and transparently
    /// re-opens it if the server dropped it. Fails only if the server cannot be reached.
    pub async fn health(&self) -> Result<bool> {
        // While a statement runs, the connection is evidently in use.
        if !self.running.load(Ordering::SeqCst) {
            let mut g = self.main.lock().await;
            if !alive(&mut g, &self.main_used, true).await {
                self.reopen_main(&mut g).await?;
            }
            *self.main_used.lock().unwrap() = Instant::now();
        }
        if let Ok(mut g) = self.meta.try_lock() {
            if g.is_some() && !alive(&mut g, &self.meta_used, true).await {
                // Re-opened on next use.
                *g = None;
            }
        }
        Ok(self.in_tx.load(Ordering::SeqCst))
    }

    /// Reports (once) that an open transaction was lost together with the connection.
    fn take_tx_lost(&self) -> Result<()> {
        if self.tx_lost.swap(false, Ordering::SeqCst) {
            bail!("{TX_LOST}");
        }
        Ok(())
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
        if let Err(e) = self.take_tx_lost() {
            // Running the statements now would silently execute them outside the transaction the
            // user believes is still open.
            results.push(QueryResult::failed(stmts.first().map(String::as_str).unwrap_or(""), e.to_string()));
            return Ok(ExecuteResponse { results, needs_confirmation: None, in_transaction: false });
        }
        let switch = opts.schema.as_deref().and_then(|s| use_schema_sql(self.dialect(), s));
        if let Some(switch) = &switch {
            // Issued on every run: several editor tabs share this connection and the user may
            // also have switched manually with USE / SET search_path.
            let mut r = match g.as_mut() {
                Some(conn) => conn.run(switch, 1).await.map(|_| ()),
                None => Ok(()),
            };
            if let Err(e) = &r {
                // The switch is the first thing sent: if the connection turns out to be dead, it
                // can safely be re-opened and the switch repeated.
                if is_connection_lost(e) && !self.in_tx.load(Ordering::SeqCst) {
                    log::info!("connection lost, reconnecting: {}", crate::error::chain_message(e));
                    r = match self.reopen_main(&mut g).await {
                        Ok(()) => g.as_mut().unwrap().run(switch, 1).await.map(|_| ()),
                        Err(e) => Err(e),
                    };
                }
            }
            if let Err(e) = r {
                let mut msg = crate::error::chain_message(&e);
                if is_connection_lost(&e) {
                    *g = None;
                    msg = lost_message(msg, self.in_tx.swap(false, Ordering::SeqCst));
                }
                results.push(QueryResult::failed(switch, msg));
                return Ok(ExecuteResponse { results, needs_confirmation: None, in_transaction: self.in_tx.load(Ordering::SeqCst) });
            }
        }
        self.running.store(true, Ordering::SeqCst);
        let mut retried = false;
        let mut i = 0;
        while let Some(stmt) = stmts.get(i) {
            let conn = match g.as_mut() {
                Some(c) => c,
                None => break,
            };
            let kw = sql::leading_keyword(stmt);
            if !self.auto_commit.load(Ordering::SeqCst) && !self.in_tx.load(Ordering::SeqCst) && !is_tx_control(stmt) {
                if let Err(e) = conn.begin().await {
                    let mut msg = crate::error::chain_message(&e);
                    if is_connection_lost(&e) {
                        *g = None;
                        msg = lost_message(msg, false);
                    }
                    results.push(QueryResult::failed(stmt, msg));
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
                    i += 1;
                }
                Err(e) => {
                    let lost = is_connection_lost(&e);
                    // A dead connection noticed by the first statement: a read-only statement
                    // outside a transaction is safe to repeat on a new connection.
                    if lost && i == 0 && !retried && !self.in_tx.load(Ordering::SeqCst) && !sql::analyze(stmt).modifies {
                        log::info!("connection lost, reconnecting: {}", crate::error::chain_message(&e));
                        retried = true;
                        if self.reopen_main(&mut g).await.is_ok() {
                            let c = g.as_mut().unwrap();
                            let switched = match &switch {
                                Some(sw) => c.run(sw, 1).await.is_ok(),
                                None => true,
                            };
                            if switched {
                                continue;
                            }
                        }
                    }
                    let mut msg = crate::error::chain_message(&e);
                    if lost {
                        // Whether the statement reached the server is unknown, so it is not
                        // repeated. The next run opens a new connection.
                        msg = lost_message(msg, self.in_tx.swap(false, Ordering::SeqCst));
                    }
                    let mut r = QueryResult::failed(stmt, msg.clone());
                    r.duration_ms = elapsed;
                    results.push(r);
                    if lost {
                        *g = None;
                    } else if msg == "Query cancelled" || msg.starts_with("Query timed out") {
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
        *self.main_used.lock().unwrap() = Instant::now();
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
        self.take_tx_lost()?;
        if let Some(c) = g.as_mut() {
            c.commit().await?;
        }
        self.in_tx.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub async fn rollback(&self) -> Result<()> {
        let mut g = self.main().await?;
        // Lost with the connection: already rolled back by the server.
        self.tx_lost.store(false, Ordering::SeqCst);
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

    /// Runs one read-only statement from an AI agent / MCP client in a READ ONLY transaction that
    /// is always rolled back. The SQL is untrusted: it must pass [`sql::check_agent_query`] and is
    /// executed as a single statement. `schema` sets the default for unqualified names.
    pub async fn read_only_query(&self, stmt: &str, max_rows: usize, schema: Option<&str>) -> Result<QueryResult> {
        let d = self.dialect();
        let stmts = sql::split_statements(stmt, d);
        if stmts.len() != 1 {
            bail!("Only a single read-only statement (SELECT/WITH/SHOW/EXPLAIN) is allowed");
        }
        if let Err(reason) = sql::check_agent_query(&stmts[0], d) {
            bail!("Query rejected: {reason}. Only a single read-only statement (SELECT/WITH/SHOW/EXPLAIN) is allowed.");
        }
        let switch = schema.and_then(|s| use_schema_sql(d, s));
        let mut g = self.meta().await?;
        let c = g.as_mut().unwrap();
        // PostgreSQL: inside the transaction, so the rollback restores the search_path.
        if let Some(sw) = switch.as_deref().filter(|_| d != Dialect::Postgres) {
            c.run(sw, 1).await?;
        }
        c.begin_read_only().await?;
        if let Some(sw) = switch.as_deref().filter(|_| d == Dialect::Postgres) {
            if let Err(e) = c.run(sw, 1).await {
                let _ = c.end_read_only().await;
                return Err(e);
            }
        }
        let start = Instant::now();
        let r = tokio::time::timeout(Duration::from_secs(60), c.run_single(&stmts[0], max_rows)).await.unwrap_or_else(|_| Err(anyhow!("query timed out")));
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

/// How long a connection may sit idle before it is pinged prior to reuse.
const IDLE_PING: Duration = Duration::from_secs(30);

const TX_LOST: &str = "The connection to the server was lost while a transaction was open. The server rolled back the uncommitted changes. The connection has been re-established - run the statements again.";

/// Checks the connection in a slot: missing, closed, or (idle for a while or `force`) and no
/// longer answering.
async fn alive(g: &mut MutexGuard<'_, Option<Conn>>, used: &StdMutex<Instant>, force: bool) -> bool {
    let Some(c) = g.as_mut() else { return false };
    if c.is_broken() {
        return false;
    }
    if !force && used.lock().unwrap().elapsed() < IDLE_PING {
        return true;
    }
    match tokio::time::timeout(Duration::from_secs(10), c.ping()).await {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            log::info!("idle connection is gone, reconnecting: {}", crate::error::chain_message(&e));
            false
        }
        Err(_) => {
            log::info!("idle connection does not answer, reconnecting");
            false
        }
    }
}

fn reconnect_failed(e: anyhow::Error) -> anyhow::Error {
    e.context("Connection to the server was lost and could not be re-established")
}

fn lost_message(msg: String, in_tx: bool) -> String {
    if in_tx {
        format!("{msg}\n\nThe connection to the server was lost while a transaction was open. The server rolled back the uncommitted changes. It is re-established automatically on the next run.")
    } else {
        format!("{msg}\n\nThe connection to the server was lost. It is re-established automatically on the next run.")
    }
}

/// Statement that makes `schema` the default for unqualified names, if the dialect has one.
pub fn use_schema_sql(d: Dialect, schema: &str) -> Option<String> {
    let schema = schema.trim();
    if schema.is_empty() {
        return None;
    }
    let q = sql::quote_ident(schema, d);
    match d {
        Dialect::Mysql => Some(format!("USE {q}")),
        Dialect::Postgres if schema == "public" => Some("SET search_path TO public".into()),
        Dialect::Postgres => Some(format!("SET search_path TO {q}, public")),
        Dialect::Oracle => Some(format!("ALTER SESSION SET CURRENT_SCHEMA = {q}")),
        // SQL Server: the default schema belongs to the login; SQLite resolves across attached databases.
        Dialect::Mssql | Dialect::Sqlite => None,
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
