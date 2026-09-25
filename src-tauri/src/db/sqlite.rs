//! SQLite driver (rusqlite, bundled SQLite). Blocking calls run on the blocking thread pool.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, InterruptHandle, OpenFlags};
use serde_json::Value;
use tokio::sync::mpsc;

use super::{hex_value, Batch};
use crate::model::{ColumnMeta, ConnectionConfig, QueryResult, Row};

pub struct LiteConn {
    conn: Arc<Mutex<Connection>>,
    pub path: String,
}

impl LiteConn {
    pub async fn connect(cfg: &ConnectionConfig) -> Result<LiteConn> {
        let path = cfg.file_path.clone().filter(|p| !p.is_empty()).context("SQLite database file is required")?;
        let path = crate::ssh::expand_home(&path);
        let read_only = cfg.read_only;
        let p = path.clone();
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let flags = if read_only {
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
            } else {
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
            };
            let c = Connection::open_with_flags(&p, flags).with_context(|| format!("open {p}"))?;
            c.busy_timeout(std::time::Duration::from_secs(10))?;
            c.execute_batch("PRAGMA foreign_keys = ON;")?;
            Ok(c)
        })
        .await??;
        Ok(LiteConn { conn: Arc::new(Mutex::new(conn)), path })
    }

    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.conn.lock().unwrap().get_interrupt_handle()
    }

    pub fn raw(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || -> Result<Vec<QueryResult>> {
            let c = conn.lock().unwrap();
            let mut stmt = c.prepare(&sql)?;
            let ncol = stmt.column_count();
            if ncol == 0 {
                let n = stmt.execute([])?;
                let changes = if stmt.readonly() { None } else { Some(n as u64) };
                return Ok(vec![QueryResult::command(&sql, changes)]);
            }
            let cols = columns(&stmt);
            let mut rows = Vec::new();
            let mut truncated = false;
            let mut rs = stmt.query([])?;
            while let Some(r) = rs.next()? {
                if rows.len() >= max_rows {
                    truncated = true;
                    break;
                }
                rows.push(convert_row(r, ncol));
            }
            Ok(vec![QueryResult::rows(&sql, cols, rows, truncated)])
        })
        .await?
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let c = conn.lock().unwrap();
            let mut stmt = c.prepare(&sql)?;
            let ncol = stmt.column_count();
            if tx.blocking_send(Batch::Columns(columns(&stmt))).is_err() {
                return Ok(());
            }
            let mut rs = stmt.query([])?;
            let mut buf = Vec::with_capacity(batch);
            while let Some(r) = rs.next()? {
                buf.push(convert_row(r, ncol));
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

    /// Online, consistent backup using SQLite's backup API.
    pub async fn backup_to(&self, dest: &str) -> Result<()> {
        let conn = self.conn.clone();
        let dest = dest.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            if std::path::Path::new(&dest).exists() {
                std::fs::remove_file(&dest)?;
            }
            let c = conn.lock().unwrap();
            let mut target = Connection::open(&dest)?;
            let b = rusqlite::backup::Backup::new(&c, &mut target)?;
            b.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
            Ok(())
        })
        .await?
    }

    pub async fn server_version(&mut self) -> String {
        format!("SQLite {}", rusqlite::version())
    }
}

fn columns(stmt: &rusqlite::Statement<'_>) -> Vec<ColumnMeta> {
    stmt.columns()
        .iter()
        .map(|c| ColumnMeta { name: c.name().to_string(), type_name: c.decl_type().map(|s| s.to_string()) })
        .collect()
}

fn convert_row(r: &rusqlite::Row<'_>, ncol: usize) -> Row {
    (0..ncol)
        .map(|i| match r.get_ref(i) {
            Ok(ValueRef::Null) | Err(_) => Value::Null,
            Ok(ValueRef::Integer(n)) => super::num_i64(&n.to_string()),
            Ok(ValueRef::Real(f)) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
            Ok(ValueRef::Text(t)) => Value::String(String::from_utf8_lossy(t).into_owned()),
            Ok(ValueRef::Blob(b)) => hex_value(b),
        })
        .collect()
}

