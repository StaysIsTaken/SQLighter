//! Oracle driver (ODPI-C). Requires the Oracle Instant Client at runtime; it is loaded lazily so
//! the rest of SQLighter works without it.

use std::sync::Arc;

use anyhow::{Context, Result};
use oracle::sql_type::OracleType;
use oracle::{Connection, Connector, SqlValue};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{hex_value, Batch};
use crate::model::{ColumnMeta, ConnectionConfig, ConnectionSecrets, QueryResult, Row, TlsMode};
use crate::ssh::SshTunnel;

pub struct OraConn {
    pub conn: Arc<Connection>,
    /// Commit after each data-modifying statement (false inside an explicit transaction).
    pub autocommit: bool,
}

impl OraConn {
    pub async fn connect(cfg: &ConnectionConfig, secrets: &ConnectionSecrets, tunnel: Option<Arc<SshTunnel>>) -> Result<OraConn> {
        let (host, port) = match &tunnel {
            Some(t) => ("127.0.0.1".to_string(), t.local_port().await?),
            None => (cfg.host.clone(), if cfg.port == 0 { 1521 } else { cfg.port }),
        };
        let service = cfg.service_name.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| cfg.database.clone());
        let (protocol, security) = match cfg.tls.mode {
            TlsMode::Disabled => ("TCP", String::new()),
            TlsMode::VerifyFull => (
                "TCPS",
                format!(
                    "(SECURITY=(SSL_SERVER_DN_MATCH=YES){})",
                    cfg.tls.ca_file.as_deref().filter(|s| !s.is_empty()).map(|w| format!("(MY_WALLET_DIRECTORY={w})")).unwrap_or_default()
                ),
            ),
            TlsMode::VerifyCa => (
                "TCPS",
                format!(
                    "(SECURITY=(SSL_SERVER_DN_MATCH=NO){})",
                    cfg.tls.ca_file.as_deref().filter(|s| !s.is_empty()).map(|w| format!("(MY_WALLET_DIRECTORY={w})")).unwrap_or_default()
                ),
            ),
            TlsMode::Pinned => anyhow::bail!("Certificate pinning is not supported for Oracle. Use a wallet with the server certificate (Verify CA)."),
        };
        let connect = format!(
            "(DESCRIPTION=(ADDRESS=(PROTOCOL={protocol})(HOST={host})(PORT={port}))(CONNECT_DATA=(SERVICE_NAME={service})){security})"
        );
        let user = cfg.user.clone();
        let pw = secrets.password.clone().unwrap_or_default();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let c = Connector::new(user, pw, connect).connect().map_err(|e| {
                let m = e.to_string();
                if m.contains("DPI-1047") {
                    anyhow::anyhow!("Oracle Instant Client not found. Install it (https://www.oracle.com/database/technologies/instant-client.html) and make sure it is on the library path.\n{m}")
                } else {
                    anyhow::anyhow!(m)
                }
            })?;
            Ok(c)
        })
        .await??;
        Ok(OraConn { conn: Arc::new(conn), autocommit: true })
    }

    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        let conn = self.conn.clone();
        let sql = sql.trim_end_matches(';').to_string();
        // PL/SQL blocks keep their trailing semicolon.
        let sql = if is_plsql(&sql) { format!("{sql};") } else { sql };
        let autocommit = self.autocommit;
        tokio::task::spawn_blocking(move || -> Result<Vec<QueryResult>> {
            let mut stmt = conn.statement(&sql).build()?;
            if stmt.is_query() {
                let rs = stmt.query(&[])?;
                let cols: Vec<ColumnMeta> = rs.column_info().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(c.oracle_type().to_string()) }).collect();
                let mut rows = Vec::new();
                let mut truncated = false;
                for r in rs {
                    let r = r?;
                    if rows.len() >= max_rows {
                        truncated = true;
                        break;
                    }
                    rows.push(convert_row(r.sql_values()));
                }
                Ok(vec![QueryResult::rows(&sql, cols, rows, truncated)])
            } else {
                stmt.execute(&[])?;
                let n = stmt.row_count().ok();
                if autocommit {
                    conn.commit()?;
                }
                Ok(vec![QueryResult::command(&sql, n)])
            }
        })
        .await?
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        let conn = self.conn.clone();
        let sql = sql.trim_end_matches(';').to_string();
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut stmt = conn.statement(&sql).fetch_array_size(batch.min(1000) as u32).build()?;
            let rs = stmt.query(&[])?;
            let cols = rs.column_info().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(c.oracle_type().to_string()) }).collect();
            if tx.blocking_send(Batch::Columns(cols)).is_err() {
                return Ok(());
            }
            let mut buf = Vec::with_capacity(batch);
            for r in rs {
                buf.push(convert_row(r?.sql_values()));
                if buf.len() >= batch && tx.blocking_send(Batch::Rows(std::mem::take(&mut buf))).is_err() {
                    return Ok(());
                }
            }
            if !buf.is_empty() {
                let _ = tx.blocking_send(Batch::Rows(buf));
            }
            Ok(())
        })
        .await?
    }

    pub async fn commit(&mut self) -> Result<()> {
        self.autocommit = true;
        let c = self.conn.clone();
        tokio::task::spawn_blocking(move || c.commit().context("commit")).await?
    }

    pub async fn rollback(&mut self) -> Result<()> {
        self.autocommit = true;
        let c = self.conn.clone();
        tokio::task::spawn_blocking(move || c.rollback().context("rollback")).await?
    }

    pub async fn server_version(&mut self) -> String {
        let c = self.conn.clone();
        tokio::task::spawn_blocking(move || c.server_version().map(|(_, banner)| banner).unwrap_or_default()).await.unwrap_or_default()
    }
}

fn is_plsql(sql: &str) -> bool {
    let kw = crate::sql::leading_keyword(sql);
    if kw == "BEGIN" || kw == "DECLARE" {
        return true;
    }
    let up = sql.to_ascii_uppercase();
    kw == "CREATE" && ["PROCEDURE", "FUNCTION", "PACKAGE", "TRIGGER", "TYPE BODY"].iter().any(|k| up.contains(k))
}

fn convert_row(vals: &[SqlValue]) -> Row {
    vals.iter().map(convert).collect()
}

fn convert(v: &SqlValue) -> Value {
    if v.is_null().unwrap_or(true) {
        return Value::Null;
    }
    match v.oracle_type() {
        Ok(OracleType::Number(_, scale)) if *scale <= 0 => v.get::<String>().map(|s| super::num_i64(&s)).unwrap_or(Value::Null),
        Ok(OracleType::BinaryFloat) | Ok(OracleType::BinaryDouble) => {
            v.get::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null)
        }
        Ok(OracleType::Raw(_)) | Ok(OracleType::BLOB) | Ok(OracleType::LongRaw) => v.get::<Vec<u8>>().map(|b| hex_value(&b)).unwrap_or(Value::Null),
        _ => v.get::<String>().map(Value::String).unwrap_or(Value::Null),
    }
}
