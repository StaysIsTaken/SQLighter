//! Import, export, DB-to-DB transfer, backup and restore. Long running work runs as background
//! tasks reporting "progress" events; commands return the task id immediately.

pub mod backup;
pub mod formats;

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tauri::State;
use tokio::sync::mpsc;

use crate::app_err;
use crate::db::{metadata, Batch, Conn};
use crate::error::AppResult;
use crate::model::*;
use crate::sql::{qualified, quote_ident};
use crate::sqlgen::{self, category, Cat};
use crate::state::AppState;

type S<'a> = State<'a, Arc<AppState>>;

/// Spawns a background task with progress reporting and cancellation.
pub fn spawn_task<F>(state: Arc<AppState>, label: String, f: F) -> String
where
    F: FnOnce(Progress) -> futures::future::BoxFuture<'static, Result<String>> + Send + 'static,
{
    let task_id = uuid::Uuid::new_v4().to_string();
    let p = Progress { state: state.clone(), task_id: task_id.clone(), label: label.clone() };
    let st = state.clone();
    let tid = task_id.clone();
    let handle = tokio::spawn(async move {
        let p2 = p.clone();
        let r = f(p).await;
        match r {
            Ok(msg) => p2.finish(None, Some(msg)),
            Err(e) => p2.finish(Some(format!("{e:#}")), None),
        }
        st.tasks.lock().unwrap().remove(&tid);
    });
    state.tasks.lock().unwrap().insert(task_id.clone(), handle.abort_handle());
    task_id
}

#[derive(Clone)]
pub struct Progress {
    state: Arc<AppState>,
    task_id: String,
    label: String,
}

impl Progress {
    pub fn update(&self, done: u64, total: Option<u64>, message: Option<String>) {
        self.state.emit_progress(&ProgressEvent { task_id: self.task_id.clone(), label: self.label.clone(), done, total, finished: false, error: None, message });
    }
    fn finish(&self, error: Option<String>, message: Option<String>) {
        self.state.emit_progress(&ProgressEvent { task_id: self.task_id.clone(), label: self.label.clone(), done: 0, total: None, finished: true, error, message });
    }
}

#[tauri::command]
pub fn cancel_task(state: S<'_>, task_id: String) {
    if let Some(h) = state.tasks.lock().unwrap().remove(&task_id) {
        h.abort();
        state.emit_progress(&ProgressEvent { task_id, label: String::new(), done: 0, total: None, finished: true, error: Some("Cancelled".into()), message: None });
    }
}

// ------------------------------------------------------------------------------------------
// Export

#[tauri::command]
pub async fn export_data(state: S<'_>, source: ExportSource, options: ExportOptions) -> AppResult<String> {
    state.check_path(&options.file_path)?;
    let st = state.inner().clone();
    let label = format!("Export {}", std::path::Path::new(&options.file_path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(spawn_task(st.clone(), label, move |p| Box::pin(async move { export(st, source, options, p).await })))
}

async fn export(state: Arc<AppState>, source: ExportSource, opts: ExportOptions, p: Progress) -> Result<String> {
    match source {
        ExportSource::Rows { columns, rows, table_name, dialect } => {
            let table_name = table_name.unwrap_or_else(|| "export".into());
            let dialect = dialect.unwrap_or(Dialect::Postgres);
            let n = rows.len() as u64;
            tokio::task::spawn_blocking(move || -> Result<()> {
                let ctx = formats::ExportCtx { opts: &opts, table_name, dialect };
                let mut w = formats::Writer::create(&ctx, &columns)?;
                w.write_rows(&rows)?;
                w.finish()
            })
            .await??;
            Ok(format!("{n} rows exported"))
        }
        ExportSource::Query { connection_id, sql, table_name } => {
            let s = state.session(&connection_id).await?;
            let d = s.dialect();
            let stmts = crate::sql::split_statements(&sql, d);
            if stmts.len() != 1 {
                bail!("Export needs exactly one query");
            }
            let conn = s.new_conn().await?;
            stream_export(conn, stmts[0].clone(), opts, table_name.unwrap_or_else(|| "query".into()), d, p).await
        }
        ExportSource::Table { connection_id, table } => {
            let s = state.session(&connection_id).await?;
            let d = s.dialect();
            let conn = s.new_conn().await?;
            let sql = format!("SELECT * FROM {}", qualified(&table.schema, &table.name, d));
            let name = if opts.format == ExportFormat::Sql { qualified(&table.schema, &table.name, d) } else { table.name.clone() };
            stream_export(conn, sql, opts, name, d, p).await
        }
    }
}

async fn stream_export(mut conn: Conn, sql: String, opts: ExportOptions, table_name: String, d: Dialect, p: Progress) -> Result<String> {
    let (tx, mut rx) = mpsc::channel::<Batch>(4);
    let producer = async move {
        let r = conn.stream(&sql, 2000, &tx).await;
        drop(tx);
        r
    };
    let consumer = async move {
        let mut writer: Option<formats::Writer> = None;
        let mut n = 0u64;
        while let Some(b) = rx.recv().await {
            match b {
                Batch::Columns(c) => {
                    let ctx = formats::ExportCtx { opts: &opts, table_name: table_name.clone(), dialect: d };
                    writer = Some(formats::Writer::create(&ctx, &c)?);
                }
                Batch::Rows(rows) => {
                    let w = writer.as_mut().context("no columns")?;
                    // File writes are fast and buffered; keep them on this task.
                    w.write_rows(&rows)?;
                    n += rows.len() as u64;
                    p.update(n, None, None);
                }
            }
        }
        if let Some(w) = writer {
            tokio::task::spawn_blocking(move || w.finish()).await??;
        }
        Ok::<u64, anyhow::Error>(n)
    };
    let (a, b) = tokio::join!(producer, consumer);
    a?;
    Ok(format!("{} rows exported", b?))
}

// ------------------------------------------------------------------------------------------
// Import

#[tauri::command]
pub async fn import_preview(state: S<'_>, source: ImportSourceOptions) -> AppResult<ImportPreview> {
    state.check_path(&source.file_path)?;
    let r = tokio::task::spawn_blocking(move || -> Result<ImportPreview> {
        let t = formats::read_table(&source, None)?;
        Ok(ImportPreview { columns: t.columns, total_rows: t.rows.len(), rows: t.rows.into_iter().take(100).collect(), sheets: t.sheets })
    })
    .await
    .map_err(|e| app_err!("{e}"))??;
    Ok(r)
}

#[tauri::command]
pub async fn import_data(state: S<'_>, options: ImportOptions) -> AppResult<String> {
    state.check_path(&options.source.file_path)?;
    let st = state.inner().clone();
    let label = format!("Import into {}", options.table.name);
    Ok(spawn_task(st.clone(), label, move |p| Box::pin(async move { import(st, options, p).await })))
}

async fn import(state: Arc<AppState>, o: ImportOptions, p: Progress) -> Result<String> {
    let s = state.session(&o.connection_id).await?;
    if s.cfg.read_only {
        bail!("This connection is read-only");
    }
    let d = s.dialect();
    let src = o.source.clone();
    let table = tokio::task::spawn_blocking(move || formats::read_table(&src, None)).await??;
    // Mapping: source column index -> target column name
    let mapped: Vec<(usize, String)> = table
        .columns
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let t = o.mapping.get(c).cloned().unwrap_or_else(|| c.clone());
            (!t.is_empty()).then_some((i, t))
        })
        .collect();
    if mapped.is_empty() {
        bail!("No columns mapped");
    }
    let target_cols: Vec<String> = mapped.iter().map(|m| m.1.clone()).collect();
    let rows: Vec<Row> = table.rows.into_iter().map(|r| mapped.iter().map(|(i, _)| r.get(*i).cloned().unwrap_or(Value::Null)).collect()).collect();
    let total = rows.len() as u64;
    let mut conn = s.new_conn().await?;
    let target = qualified(&o.table.schema, &o.table.name, d);
    if o.create_table {
        let cols = sqlgen::infer_columns(&target_cols, &rows[..rows.len().min(5000)]);
        let info = TableInfo { schema: o.table.schema.clone(), name: o.table.name.clone(), kind: ObjectKind::Table, columns: cols, primary_key: vec![], indexes: vec![], foreign_keys: vec![], comment: None, row_estimate: None };
        for stmt in sqlgen::create_table(&info, Dialect::Postgres, &sqlgen::CreateOpts { to: d, schema: &o.table.schema, name: &o.table.name, foreign_keys: false, indexes: false }) {
            conn.run(&stmt, 0).await.with_context(|| format!("create table {target}"))?;
        }
    }
    let info = metadata::describe(&mut conn, d, &o.table.schema, &o.table.name, ObjectKind::Table).await.context("target table not found")?;
    let types: Vec<Option<String>> = target_cols.iter().map(|c| info.columns.iter().find(|x| x.name == *c).map(|x| x.data_type.clone())).collect();
    for c in &target_cols {
        if !info.columns.iter().any(|x| x.name == *c) {
            bail!("Column '{c}' does not exist in {target}");
        }
    }
    let rows: Vec<Row> = rows.into_iter().map(|r| coerce_row(r, &types)).collect();
    conn.begin().await?;
    let res = async {
        if o.truncate {
            conn.run(&format!("DELETE FROM {target}"), 0).await?;
        }
        let batch = o.batch_size.unwrap_or(500).max(1);
        let mut done = 0u64;
        for chunk in rows.chunks(batch) {
            for stmt in sqlgen::inserts(&target, &target_cols, &types, chunk, d, batch) {
                conn.run(&stmt, 0).await?;
            }
            done += chunk.len() as u64;
            p.update(done, Some(total), None);
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    match res {
        Ok(()) => {
            conn.commit().await?;
            Ok(format!("{total} rows imported into {}", o.table.name))
        }
        Err(e) => {
            let _ = conn.rollback().await;
            Err(e.context("import failed, all changes were rolled back"))
        }
    }
}

/// Converts imported text values to the target column's type category.
pub fn coerce_row(r: Row, types: &[Option<String>]) -> Row {
    r.into_iter()
        .enumerate()
        .map(|(i, v)| {
            let cat = types.get(i).and_then(|t| t.as_deref()).map(category);
            match (v, cat) {
                (Value::String(s), Some(Cat::Bool)) => match s.to_ascii_lowercase().as_str() {
                    "true" | "t" | "yes" | "y" | "1" => Value::Bool(true),
                    "false" | "f" | "no" | "n" | "0" => Value::Bool(false),
                    _ => Value::String(s),
                },
                (Value::String(s), Some(Cat::SmallInt | Cat::Int | Cat::BigInt)) if s.trim().parse::<i64>().is_ok() => Value::from(s.trim().parse::<i64>().unwrap()),
                (Value::String(s), Some(Cat::Float)) if s.trim().parse::<f64>().is_ok() => {
                    serde_json::Number::from_f64(s.trim().parse::<f64>().unwrap()).map(Value::Number).unwrap_or(Value::String(s))
                }
                (Value::Bool(b), Some(Cat::SmallInt | Cat::Int | Cat::BigInt)) => Value::from(b as i64),
                (v, _) => v,
            }
        })
        .collect()
}

// ------------------------------------------------------------------------------------------
// Transfer between connections

#[tauri::command]
pub async fn transfer_data(state: S<'_>, options: TransferOptions) -> AppResult<String> {
    if options.tables.is_empty() {
        return Err(app_err!("No tables selected"));
    }
    let st = state.inner().clone();
    let label = format!("Transfer {} table(s)", options.tables.len());
    Ok(spawn_task(st.clone(), label, move |p| Box::pin(async move { transfer(st, options, p).await })))
}

async fn transfer(state: Arc<AppState>, o: TransferOptions, p: Progress) -> Result<String> {
    let src = state.session(&o.source_connection_id).await?;
    let dst = state.session(&o.target_connection_id).await?;
    if dst.cfg.read_only {
        bail!("The target connection is read-only");
    }
    let (sd, td) = (src.dialect(), dst.dialect());
    let mut sconn = src.new_conn().await?;
    let mut tconn = dst.new_conn().await?;
    let mut total_rows = 0u64;
    for (ti, name) in o.tables.iter().enumerate() {
        p.update(ti as u64, Some(o.tables.len() as u64), Some(format!("{name}…")));
        let info = metadata::describe(&mut sconn, sd, &o.source_schema, name, ObjectKind::Table).await?;
        let target = qualified(&o.target_schema, name, td);
        if o.create_tables {
            for stmt in sqlgen::create_table(&info, sd, &sqlgen::CreateOpts { to: td, schema: &o.target_schema, name, foreign_keys: false, indexes: true }) {
                if let Err(e) = tconn.run(&stmt, 0).await {
                    let m = format!("{e:#}").to_ascii_lowercase();
                    if !(m.contains("exist") || m.contains("already")) {
                        return Err(e.context(format!("create {target}")));
                    }
                    break; // table exists -> keep it
                }
            }
        }
        let cols: Vec<String> = info.columns.iter().map(|c| c.name.clone()).collect();
        let tinfo = metadata::describe(&mut tconn, td, &o.target_schema, name, ObjectKind::Table).await.with_context(|| format!("target table {target} not found"))?;
        let types: Vec<Option<String>> = cols.iter().map(|c| tinfo.columns.iter().find(|x| x.name.eq_ignore_ascii_case(c)).map(|x| x.data_type.clone())).collect();
        let select = format!("SELECT {} FROM {}", cols.iter().map(|c| quote_ident(c, sd)).collect::<Vec<_>>().join(", "), qualified(&o.source_schema, name, sd));
        tconn.begin().await?;
        if o.truncate {
            if let Err(e) = tconn.run(&format!("DELETE FROM {target}"), 0).await {
                let _ = tconn.rollback().await;
                return Err(e);
            }
        }
        let (tx, mut rx) = mpsc::channel::<Batch>(4);
        let batch = o.batch_size.max(1);
        let producer = async {
            let r = sconn.stream(&select, batch, &tx).await;
            drop(tx);
            r
        };
        let identity_insert = td == Dialect::Mssql && tinfo.columns.iter().any(|c| c.auto_increment);
        let consumer = async {
            if identity_insert {
                tconn.run(&format!("SET IDENTITY_INSERT {target} ON"), 0).await?;
            }
            let mut n = 0u64;
            while let Some(b) = rx.recv().await {
                if let Batch::Rows(rows) = b {
                    let rows: Vec<Row> = rows.into_iter().map(|r| coerce_row(r, &types)).collect();
                    for stmt in sqlgen::inserts(&target, &cols, &types, &rows, td, batch) {
                        tconn.run(&stmt, 0).await?;
                    }
                    n += rows.len() as u64;
                    p.update(ti as u64, Some(o.tables.len() as u64), Some(format!("{name}: {n} rows")));
                }
            }
            if identity_insert {
                tconn.run(&format!("SET IDENTITY_INSERT {target} OFF"), 0).await?;
            }
            Ok::<u64, anyhow::Error>(n)
        };
        let (a, b) = tokio::join!(producer, consumer);
        match (a, b) {
            (Ok(()), Ok(n)) => {
                tconn.commit().await?;
                total_rows += n;
            }
            (Err(e), _) | (_, Err(e)) => {
                let _ = tconn.rollback().await;
                return Err(e.context(format!("transfer of {name} failed and was rolled back")));
            }
        }
        // Keep sequences / identities in sync for PostgreSQL targets.
        if td == Dialect::Postgres {
            for c in tinfo.columns.iter().filter(|c| c.auto_increment) {
                let sql = format!(
                    "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE((SELECT MAX({}) FROM {}), 0) + 1, false)",
                    crate::sql::quote_literal(&Value::String(target.clone()), td, None),
                    crate::sql::quote_literal(&Value::String(c.name.clone()), td, None),
                    quote_ident(&c.name, td),
                    target
                );
                let _ = tconn.run(&sql, 1).await;
            }
        }
    }
    Ok(format!("{} table(s), {total_rows} rows transferred", o.tables.len()))
}

// ------------------------------------------------------------------------------------------
// Backup / restore

#[tauri::command]
pub async fn backup(state: S<'_>, options: BackupOptions) -> AppResult<String> {
    state.check_path(&options.file_path)?;
    let st = state.inner().clone();
    let label = format!("Backup {}", std::path::Path::new(&options.file_path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(spawn_task(st.clone(), label, move |p| Box::pin(async move { backup::run_backup(st, options, p).await })))
}

#[tauri::command]
pub async fn restore(state: S<'_>, options: RestoreOptions) -> AppResult<String> {
    state.check_path(&options.file_path)?;
    let st = state.inner().clone();
    let label = format!("Restore {}", std::path::Path::new(&options.file_path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(spawn_task(st.clone(), label, move |p| Box::pin(async move { backup::run_restore(st, options, p).await })))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTools {
    pub pg_dump: Option<String>,
    pub pg_restore: Option<String>,
    pub mysqldump: Option<String>,
    pub mariadb_dump: Option<String>,
}

#[tauri::command]
pub fn native_tools() -> NativeTools {
    let f = |n: &str| which::which(n).ok().map(|p| p.to_string_lossy().into_owned());
    NativeTools { pg_dump: f("pg_dump"), pg_restore: f("pg_restore"), mysqldump: f("mysqldump"), mariadb_dump: f("mariadb-dump") }
}
