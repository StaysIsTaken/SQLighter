//! MySQL / MariaDB driver (mysql_async with rustls).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::*;
use mysql_async::{Column, Conn, OptsBuilder, SslOpts, Value as MyValue};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{hex_value, num_i64, Batch};
use crate::model::{ColumnMeta, ConnectionConfig, ConnectionSecrets, QueryResult, Row, TlsMode};
use crate::ssh::SshTunnel;

pub struct MyConn {
    pub conn: Conn,
    pub opts: OptsBuilder,
}

impl MyConn {
    pub async fn connect(cfg: &ConnectionConfig, secrets: &ConnectionSecrets, tunnel: Option<Arc<SshTunnel>>) -> Result<MyConn> {
        let (host, port) = match &tunnel {
            Some(t) => ("127.0.0.1".to_string(), t.local_port().await?),
            None => (cfg.host.clone(), if cfg.port == 0 { 3306 } else { cfg.port }),
        };
        let mut o = OptsBuilder::default()
            .ip_or_hostname(host)
            .tcp_port(port)
            .user(Some(cfg.user.clone()))
            .pass(secrets.password.clone().filter(|p| !p.is_empty()))
            .db_name(Some(cfg.database.clone()).filter(|d| !d.is_empty()))
            .prefer_socket(false)
            .conn_ttl(None)
            .wait_timeout(Some(28_800))
            .client_found_rows(false);
        match cfg.tls.mode {
            TlsMode::Disabled => {}
            TlsMode::Pinned => bail!("Certificate pinning is not supported for MySQL/MariaDB. Use 'Verify CA' with the server's CA certificate file instead."),
            mode => {
                let mut ssl = SslOpts::default();
                if let Some(ca) = cfg.tls.ca_file.as_deref().filter(|s| !s.is_empty()) {
                    ssl = ssl.with_root_certs(vec![std::path::PathBuf::from(ca).into()]).with_disable_built_in_roots(true);
                }
                if mode == TlsMode::VerifyCa {
                    ssl = ssl.with_danger_skip_domain_validation(true);
                }
                if tunnel.is_some() {
                    // Verify the certificate against the real server name, not 127.0.0.1.
                    ssl = ssl.with_danger_tls_hostname_override(Some(cfg.host.clone()));
                }
                if let (Some(c), Some(k)) = (cfg.tls.cert_file.as_deref().filter(|s| !s.is_empty()), cfg.tls.key_file.as_deref().filter(|s| !s.is_empty())) {
                    ssl = ssl.with_client_identity(Some(mysql_async::ClientIdentity::new(
                        std::path::PathBuf::from(c).into(),
                        std::path::PathBuf::from(k).into(),
                    )));
                }
                o = o.ssl_opts(Some(ssl));
            }
        }
        let conn = tokio::time::timeout(Duration::from_secs(20), Conn::new(o.clone())).await.map_err(|_| anyhow::anyhow!("connection timed out"))??;
        Ok(MyConn { conn, opts: o })
    }

    pub fn id(&self) -> u32 {
        self.conn.id()
    }

    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        let mut out = Vec::new();
        let mut result = self.conn.query_iter(sql).await?;
        loop {
            let cols: Vec<Column> = result.columns_ref().to_vec();
            if cols.is_empty() {
                let affected = result.affected_rows();
                // advance past this (empty) set
                let _ = result.collect::<mysql_async::Row>().await?;
                out.push(QueryResult::command(sql, Some(affected)));
            } else {
                let metas = columns(&cols);
                let mut rows = Vec::new();
                let mut truncated = false;
                result
                    .for_each(|r| {
                        if rows.len() < max_rows {
                            rows.push(convert_row(r, &cols));
                        } else {
                            truncated = true;
                        }
                    })
                    .await?;
                out.push(QueryResult::rows(sql, metas, rows, truncated));
            }
            if result.is_empty() {
                break;
            }
        }
        if out.is_empty() {
            out.push(QueryResult::command(sql, None));
        }
        Ok(out)
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        let mut result = self.conn.query_iter(sql).await?;
        let cols: Vec<Column> = result.columns_ref().to_vec();
        tx.send(Batch::Columns(columns(&cols))).await.ok();
        let mut buf = Vec::with_capacity(batch);
        while let Some(r) = result.next().await? {
            buf.push(convert_row(r, &cols));
            if buf.len() >= batch && tx.send(Batch::Rows(std::mem::take(&mut buf))).await.is_err() {
                result.drop_result().await.ok();
                return Ok(());
            }
        }
        if !buf.is_empty() {
            tx.send(Batch::Rows(buf)).await.ok();
        }
        Ok(())
    }

    pub async fn server_version(&mut self) -> String {
        self.conn.query_first::<String, _>("SELECT VERSION()").await.ok().flatten().unwrap_or_default()
    }
}

/// Kills the running query of connection `id` using a fresh connection.
pub async fn kill_query(opts: OptsBuilder, id: u32) -> Result<()> {
    let mut c = Conn::new(opts).await?;
    c.query_drop(format!("KILL QUERY {id}")).await?;
    c.disconnect().await.ok();
    Ok(())
}

fn type_name(c: &Column) -> String {
    use ColumnType::*;
    let binary = c.character_set() == 63;
    let t = match c.column_type() {
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_STRING => {
            if binary {
                "binary"
            } else {
                "char"
            }
        }
        _ => "unknown",
    };
    t.to_string()
}

fn columns(cols: &[Column]) -> Vec<ColumnMeta> {
    cols.iter().map(|c| ColumnMeta { name: c.name_str().to_string(), type_name: Some(type_name(c)) }).collect()
}

fn convert_row(r: mysql_async::Row, cols: &[Column]) -> Row {
    let vals = r.unwrap();
    vals.into_iter().enumerate().map(|(i, v)| convert(v, cols.get(i))).collect()
}

fn convert(v: MyValue, c: Option<&Column>) -> Value {
    match v {
        MyValue::NULL => Value::Null,
        MyValue::Int(i) => Value::from(i),
        MyValue::UInt(u) => num_i64(&u.to_string()),
        MyValue::Float(f) => serde_json::Number::from_f64(f as f64).map(Value::Number).unwrap_or(Value::Null),
        MyValue::Double(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
        MyValue::Date(y, m, d, h, mi, s, us) => {
            let date = format!("{y:04}-{m:02}-{d:02}");
            let is_date = c.map(|c| matches!(c.column_type(), ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE)).unwrap_or(false);
            if is_date {
                Value::String(date)
            } else if us > 0 {
                Value::String(format!("{date} {h:02}:{mi:02}:{s:02}.{us:06}"))
            } else {
                Value::String(format!("{date} {h:02}:{mi:02}:{s:02}"))
            }
        }
        MyValue::Time(neg, d, h, m, s, us) => {
            let hours = d * 24 + h as u32;
            let sign = if neg { "-" } else { "" };
            if us > 0 {
                Value::String(format!("{sign}{hours:02}:{m:02}:{s:02}.{us:06}"))
            } else {
                Value::String(format!("{sign}{hours:02}:{m:02}:{s:02}"))
            }
        }
        MyValue::Bytes(b) => {
            let Some(c) = c else { return Value::String(String::from_utf8_lossy(&b).into_owned()) };
            use ColumnType::*;
            match c.column_type() {
                MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG | MYSQL_TYPE_LONGLONG | MYSQL_TYPE_YEAR => {
                    let s = String::from_utf8_lossy(&b);
                    if c.column_type() == MYSQL_TYPE_TINY && c.column_length() == 1 && !c.flags().contains(ColumnFlags::UNSIGNED_FLAG) {
                        // TINYINT(1) is conventionally a boolean, keep the number but it maps fine.
                        return num_i64(&s);
                    }
                    num_i64(&s)
                }
                MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => {
                    let s = String::from_utf8_lossy(&b);
                    s.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::String(s.into_owned()))
                }
                MYSQL_TYPE_BIT => {
                    if b.len() == 1 && c.column_length() == 1 {
                        Value::Bool(b[0] != 0)
                    } else {
                        let mut n: u64 = 0;
                        for x in &b {
                            n = (n << 8) | *x as u64;
                        }
                        Value::from(n)
                    }
                }
                MYSQL_TYPE_GEOMETRY => hex_value(&b),
                _ if c.character_set() == 63 && !matches!(c.column_type(), MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL | MYSQL_TYPE_DATE | MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIME | MYSQL_TYPE_JSON) => hex_value(&b),
                _ => Value::String(String::from_utf8_lossy(&b).into_owned()),
            }
        }
    }
}
