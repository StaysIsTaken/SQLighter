//! PostgreSQL / CockroachDB driver (tokio-postgres + rustls with SQLighter's verifiers).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::{pin_mut, TryStreamExt};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::types::Type;
use tokio_postgres::{CancelToken, Client, NoTls, SimpleQueryMessage};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::{num_i64, Batch};
use crate::model::{ColumnMeta, ConnectionConfig, ConnectionSecrets, QueryResult, Row, TlsMode};
use crate::ssh::SshTunnel;

pub struct PgConn {
    client: Client,
    cancel: CancelToken,
    tls: Option<Arc<rustls::ClientConfig>>,
    host: String,
    tunnel: Option<Arc<SshTunnel>>,
}

#[derive(Clone)]
pub struct PgCanceller {
    token: CancelToken,
    tls: Option<Arc<rustls::ClientConfig>>,
    host: String,
    port: u16,
    tunnel: Option<Arc<SshTunnel>>,
}

impl PgCanceller {
    pub async fn cancel(&self) -> Result<()> {
        match (&self.tunnel, &self.tls) {
            (Some(t), Some(cfg)) => {
                let s = t.stream().await?;
                let mut mk = MakeRustlsConnect::new((**cfg).clone());
                let tls = <MakeRustlsConnect as MakeTlsConnect<russh::ChannelStream<russh::client::Msg>>>::make_tls_connect(&mut mk, &self.host)?;
                self.token.cancel_query_raw(s, tls).await?;
            }
            (Some(t), None) => {
                let s = t.stream().await?;
                self.token.cancel_query_raw(s, NoTls).await?;
            }
            (None, Some(cfg)) => {
                let s = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await?;
                let mut mk = MakeRustlsConnect::new((**cfg).clone());
                let tls = <MakeRustlsConnect as MakeTlsConnect<tokio::net::TcpStream>>::make_tls_connect(&mut mk, &self.host)?;
                self.token.cancel_query_raw(s, tls).await?;
            }
            (None, None) => {
                let s = tokio::net::TcpStream::connect((self.host.as_str(), self.port)).await?;
                self.token.cancel_query_raw(s, NoTls).await?;
            }
        }
        Ok(())
    }
}

impl PgConn {
    pub async fn connect(cfg: &ConnectionConfig, secrets: &ConnectionSecrets, tunnel: Option<Arc<SshTunnel>>) -> Result<PgConn> {
        let mut pc = tokio_postgres::Config::new();
        pc.user(if cfg.user.is_empty() { "postgres" } else { &cfg.user })
            .dbname(if cfg.database.is_empty() { "postgres" } else { &cfg.database })
            .application_name("SQLighter")
            .connect_timeout(Duration::from_secs(15))
            .keepalives(true);
        if let Some(pw) = secrets.password.as_deref().filter(|p| !p.is_empty()) {
            pc.password(pw);
        }
        let tls = if cfg.tls.mode == TlsMode::Disabled {
            pc.ssl_mode(tokio_postgres::config::SslMode::Disable);
            None
        } else {
            pc.ssl_mode(tokio_postgres::config::SslMode::Require);
            Some(crate::tls::client_config(&cfg.tls)?)
        };
        let host = cfg.host.clone();
        let port = if cfg.port == 0 { 5432 } else { cfg.port };

        let client = match (&tunnel, &tls) {
            (Some(t), Some(tc)) => {
                let s = t.stream().await?;
                let mut mk = MakeRustlsConnect::new((**tc).clone());
                let tlsc = <MakeRustlsConnect as MakeTlsConnect<russh::ChannelStream<russh::client::Msg>>>::make_tls_connect(&mut mk, &host)?;
                let (client, conn) = pc.connect_raw(s, tlsc).await.map_err(pg_err)?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                client
            }
            (Some(t), None) => {
                let s = t.stream().await?;
                let (client, conn) = pc.connect_raw(s, NoTls).await.map_err(pg_err)?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                client
            }
            (None, _) if host.starts_with('/') => {
                #[cfg(unix)]
                {
                    let path = format!("{}/.s.PGSQL.{}", host.trim_end_matches('/'), port);
                    let s = tokio::net::UnixStream::connect(&path).await.with_context(|| format!("connect {path}"))?;
                    let (client, conn) = pc.connect_raw(s, NoTls).await.map_err(pg_err)?;
                    tokio::spawn(async move {
                        let _ = conn.await;
                    });
                    client
                }
                #[cfg(not(unix))]
                anyhow::bail!("unix sockets are not supported on this platform")
            }
            (None, tc) => {
                let s = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect((host.as_str(), port)))
                    .await
                    .with_context(|| format!("connection to {host}:{port} timed out"))?
                    .with_context(|| format!("could not connect to {host}:{port}"))?;
                s.set_nodelay(true)?;
                match tc {
                    Some(tc) => {
                        let mut mk = MakeRustlsConnect::new((**tc).clone());
                        let tlsc = <MakeRustlsConnect as MakeTlsConnect<tokio::net::TcpStream>>::make_tls_connect(&mut mk, &host)?;
                        let (client, conn) = pc.connect_raw(s, tlsc).await.map_err(pg_err)?;
                        tokio::spawn(async move {
                            let _ = conn.await;
                        });
                        client
                    }
                    None => {
                        let (client, conn) = pc.connect_raw(s, NoTls).await.map_err(pg_err)?;
                        tokio::spawn(async move {
                            let _ = conn.await;
                        });
                        client
                    }
                }
            }
        };
        let cancel = client.cancel_token();
        let _ = port;
        Ok(PgConn { client, cancel, tls, host, tunnel })
    }

    pub fn canceller(&self, port: u16) -> PgCanceller {
        PgCanceller { token: self.cancel.clone(), tls: self.tls.clone(), host: self.host.clone(), port, tunnel: self.tunnel.clone() }
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Executes one statement. Result sets are fetched in text format, typed via a prepare step.
    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        let stmt = self.client.prepare(sql).await.map_err(pg_err)?;
        if stmt.columns().is_empty() {
            let msgs = self.client.simple_query(sql).await.map_err(pg_err)?;
            let mut affected = None;
            for m in msgs {
                if let SimpleQueryMessage::CommandComplete(n) = m {
                    affected = Some(n);
                }
            }
            return Ok(vec![QueryResult::command(sql, affected)]);
        }
        let types: Vec<Type> = stmt.columns().iter().map(|c| c.type_().clone()).collect();
        let columns: Vec<ColumnMeta> = stmt.columns().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(c.type_().name().to_string()) }).collect();
        let stream = self.client.simple_query_raw(sql).await.map_err(pg_err)?;
        pin_mut!(stream);
        let mut rows = Vec::new();
        let mut truncated = false;
        while let Some(msg) = stream.try_next().await.map_err(pg_err)? {
            if let SimpleQueryMessage::Row(r) = msg {
                if rows.len() >= max_rows {
                    truncated = true;
                    break;
                }
                rows.push(convert_row(&r, &types));
            }
        }
        Ok(vec![QueryResult::rows(sql, columns, rows, truncated)])
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        let stmt = self.client.prepare(sql).await.map_err(pg_err)?;
        let types: Vec<Type> = stmt.columns().iter().map(|c| c.type_().clone()).collect();
        let columns: Vec<ColumnMeta> = stmt.columns().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(c.type_().name().to_string()) }).collect();
        tx.send(Batch::Columns(columns)).await.ok();
        let stream = self.client.simple_query_raw(sql).await.map_err(pg_err)?;
        pin_mut!(stream);
        let mut buf = Vec::with_capacity(batch);
        while let Some(msg) = stream.try_next().await.map_err(pg_err)? {
            if let SimpleQueryMessage::Row(r) = msg {
                buf.push(convert_row(&r, &types));
                if buf.len() >= batch {
                    if tx.send(Batch::Rows(std::mem::take(&mut buf))).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
        if !buf.is_empty() {
            tx.send(Batch::Rows(buf)).await.ok();
        }
        Ok(())
    }

    pub async fn server_version(&mut self) -> String {
        match self.client.simple_query("SELECT version()").await {
            Ok(m) => m
                .into_iter()
                .find_map(|x| if let SimpleQueryMessage::Row(r) = x { r.get(0).map(|s| s.to_string()) } else { None })
                .unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

fn convert_row(r: &tokio_postgres::SimpleQueryRow, types: &[Type]) -> Row {
    (0..r.len())
        .map(|i| match r.get(i) {
            None => Value::Null,
            Some(s) => convert(s, types.get(i)),
        })
        .collect()
}

fn convert(s: &str, t: Option<&Type>) -> Value {
    let Some(t) = t else { return Value::String(s.to_string()) };
    match *t {
        Type::BOOL => Value::Bool(s == "t"),
        Type::INT2 | Type::INT4 | Type::OID => s.parse::<i64>().map(Value::from).unwrap_or_else(|_| Value::String(s.into())),
        Type::INT8 => num_i64(s),
        Type::FLOAT4 | Type::FLOAT8 => match s.parse::<f64>() {
            Ok(f) if f.is_finite() => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::String(s.into())),
            _ => Value::String(s.into()),
        },
        _ => Value::String(s.to_string()),
    }
}

/// Includes the server's detail/hint in error messages.
pub fn pg_err(e: tokio_postgres::Error) -> anyhow::Error {
    if let Some(db) = e.as_db_error() {
        let mut m = format!("{}: {}", db.severity(), db.message());
        if let Some(d) = db.detail() {
            m.push_str(&format!("\nDETAIL: {d}"));
        }
        if let Some(h) = db.hint() {
            m.push_str(&format!("\nHINT: {h}"));
        }
        if let Some(p) = db.position() {
            if let tokio_postgres::error::ErrorPosition::Original(p) = p {
                m.push_str(&format!("\nPosition: {p}"));
            }
        }
        return anyhow::anyhow!(m);
    }
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(&format!(": {s}"));
        src = s.source();
    }
    anyhow::anyhow!(msg)
}
