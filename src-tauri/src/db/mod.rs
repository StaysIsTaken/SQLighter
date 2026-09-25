//! Database layer: a uniform connection type over all drivers plus session management.

pub mod metadata;
pub mod mssql;
pub mod mysql;
#[cfg(feature = "oracle")]
pub mod oracle;
pub mod postgres;
pub mod session;
pub mod sqlite;

use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::model::{ColumnMeta, ConnectionConfig, ConnectionSecrets, DbType, QueryResult, Row};
use crate::ssh::SshTunnel;

/// Streaming unit for export / backup / transfer.
pub enum Batch {
    Columns(Vec<ColumnMeta>),
    Rows(Vec<Row>),
}

pub enum Conn {
    Pg(postgres::PgConn),
    My(mysql::MyConn),
    Lite(sqlite::LiteConn),
    Ms(mssql::MsConn),
    #[cfg(feature = "oracle")]
    Ora(oracle::OraConn),
}

/// Out-of-band cancellation of a running statement.
#[derive(Clone)]
pub enum Canceller {
    Pg(postgres::PgCanceller),
    My(::mysql_async::OptsBuilder, u32),
    Lite(Arc<rusqlite::InterruptHandle>),
    #[cfg(feature = "oracle")]
    Ora(Arc<::oracle::Connection>),
    /// No protocol-level cancel: the running future is dropped and the connection re-opened.
    Abort,
}

impl Canceller {
    pub async fn cancel(&self) -> Result<()> {
        match self {
            Canceller::Pg(c) => c.cancel().await,
            Canceller::My(o, id) => mysql::kill_query(o.clone(), *id).await,
            Canceller::Lite(h) => {
                h.interrupt();
                Ok(())
            }
            #[cfg(feature = "oracle")]
            Canceller::Ora(c) => {
                let c = c.clone();
                tokio::task::spawn_blocking(move || c.break_execution()).await??;
                Ok(())
            }
            Canceller::Abort => Ok(()),
        }
    }
}

impl Conn {
    pub async fn open(cfg: &ConnectionConfig, secrets: &ConnectionSecrets, tunnel: Option<Arc<SshTunnel>>) -> Result<Conn> {
        crate::tls::ensure_transport_allowed(cfg)?;
        let mut conn = match cfg.db_type {
            DbType::Postgres | DbType::Cockroach => Conn::Pg(postgres::PgConn::connect(cfg, secrets, tunnel).await?),
            DbType::Mysql | DbType::Mariadb => Conn::My(mysql::MyConn::connect(cfg, secrets, tunnel).await?),
            DbType::Sqlite => Conn::Lite(sqlite::LiteConn::connect(cfg).await?),
            DbType::Mssql => Conn::Ms(mssql::MsConn::connect(cfg, secrets, tunnel).await?),
            #[cfg(feature = "oracle")]
            DbType::Oracle => Conn::Ora(oracle::OraConn::connect(cfg, secrets, tunnel).await?),
            #[cfg(not(feature = "oracle"))]
            DbType::Oracle => anyhow::bail!("This build of SQLighter was compiled without Oracle support"),
        };
        // Read-only connections are also enforced by the database where possible.
        if cfg.read_only {
            let stmt = match cfg.db_type {
                DbType::Postgres | DbType::Cockroach => Some("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY"),
                DbType::Mysql | DbType::Mariadb => Some("SET SESSION TRANSACTION READ ONLY"),
                _ => None,
            };
            if let Some(s) = stmt {
                conn.run(s, 0).await?;
            }
        }
        Ok(conn)
    }

    pub fn canceller(&self, cfg: &ConnectionConfig) -> Canceller {
        match self {
            Conn::Pg(c) => Canceller::Pg(c.canceller(if cfg.port == 0 { 5432 } else { cfg.port })),
            Conn::My(c) => Canceller::My(c.opts.clone(), c.id()),
            Conn::Lite(c) => Canceller::Lite(Arc::new(c.interrupt_handle())),
            Conn::Ms(_) => Canceller::Abort,
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => Canceller::Ora(c.conn.clone()),
        }
    }

    pub fn is_broken(&self) -> bool {
        match self {
            Conn::Pg(c) => c.is_closed(),
            _ => false,
        }
    }

    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        match self {
            Conn::Pg(c) => c.run(sql, max_rows).await,
            Conn::My(c) => c.run(sql, max_rows).await,
            Conn::Lite(c) => c.run(sql, max_rows).await,
            Conn::Ms(c) => c.run(sql, max_rows).await,
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => c.run(sql, max_rows).await,
        }
    }

    /// Runs a query and returns all rows (for metadata queries).
    pub async fn rows(&mut self, sql: &str) -> Result<Vec<Row>> {
        let r = self.run(sql, usize::MAX).await?;
        Ok(r.into_iter().find(|r| r.is_result_set).map(|r| r.rows).unwrap_or_default())
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        match self {
            Conn::Pg(c) => c.stream(sql, batch, tx).await,
            Conn::My(c) => c.stream(sql, batch, tx).await,
            Conn::Lite(c) => c.stream(sql, batch, tx).await,
            Conn::Ms(c) => c.stream(sql, batch, tx).await,
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => c.stream(sql, batch, tx).await,
        }
    }

    pub async fn server_version(&mut self) -> String {
        match self {
            Conn::Pg(c) => c.server_version().await,
            Conn::My(c) => c.server_version().await,
            Conn::Lite(c) => c.server_version().await,
            Conn::Ms(c) => c.server_version().await,
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => c.server_version().await,
        }
    }

    pub async fn begin(&mut self) -> Result<()> {
        let sql = match self {
            Conn::Pg(_) | Conn::Lite(_) => "BEGIN",
            Conn::My(_) => "START TRANSACTION",
            Conn::Ms(_) => "BEGIN TRANSACTION",
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => {
                c.autocommit = false;
                return Ok(());
            }
        };
        self.run(sql, 0).await.map(|_| ())
    }

    /// Starts a READ ONLY transaction (used for AI agents / MCP queries).
    pub async fn begin_read_only(&mut self) -> Result<()> {
        let sql = match self {
            Conn::Pg(_) => "BEGIN READ ONLY",
            Conn::My(_) => "START TRANSACTION READ ONLY",
            Conn::Lite(_) => "BEGIN",
            Conn::Ms(_) => "BEGIN TRANSACTION",
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => {
                c.autocommit = false;
                "SET TRANSACTION READ ONLY"
            }
        };
        self.run(sql, 0).await?;
        if let Conn::Lite(_) = self {
            self.run("PRAGMA query_only = ON", 0).await?;
        }
        Ok(())
    }

    pub async fn end_read_only(&mut self) -> Result<()> {
        let r = self.rollback().await;
        if let Conn::Lite(_) = self {
            self.run("PRAGMA query_only = OFF", 0).await?;
        }
        r
    }

    pub async fn commit(&mut self) -> Result<()> {
        match self {
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => c.commit().await,
            Conn::Ms(_) => self.run("IF @@TRANCOUNT > 0 COMMIT TRANSACTION", 0).await.map(|_| ()),
            _ => self.run("COMMIT", 0).await.map(|_| ()),
        }
    }

    pub async fn rollback(&mut self) -> Result<()> {
        match self {
            #[cfg(feature = "oracle")]
            Conn::Ora(c) => c.rollback().await,
            Conn::Ms(_) => self.run("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", 0).await.map(|_| ()),
            _ => self.run("ROLLBACK", 0).await.map(|_| ()),
        }
    }
}

/// Integers beyond JavaScript's safe range are transported as strings.
pub fn num_i64(s: &str) -> Value {
    match s.trim().parse::<i64>() {
        Ok(n) if n.unsigned_abs() <= (1u64 << 53) => Value::from(n),
        Ok(_) => Value::String(s.trim().to_string()),
        Err(_) => match s.trim().parse::<u64>() {
            Ok(n) if n <= (1u64 << 53) => Value::from(n),
            _ => Value::String(s.trim().to_string()),
        },
    }
}

/// Binary values are transported as "\x<hex>" strings (same as PostgreSQL's text format).
pub fn hex_value(b: &[u8]) -> Value {
    Value::String(format!("\\x{}", hex::encode(b)))
}

pub fn cell_str(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn cell_opt(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        other => Some(cell_str(other)),
    }
}

pub fn cell_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
        Value::String(s) => matches!(s.to_ascii_uppercase().as_str(), "YES" | "Y" | "T" | "TRUE" | "1"),
        _ => false,
    }
}

pub fn cell_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}
