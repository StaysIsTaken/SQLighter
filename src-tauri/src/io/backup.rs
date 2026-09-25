//! Backups: a portable SQL dump written by SQLighter (all databases), or the native tools
//! (pg_dump, mysqldump / mariadb-dump, SQLite online backup API). Restore executes SQL dumps
//! statement by statement or uses pg_restore for PostgreSQL custom-format archives.

use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tokio::sync::mpsc;

use super::Progress;
use crate::db::{metadata, Batch};
use crate::model::*;
use crate::sql::{qualified, quote_ident, split_statements};
use crate::state::AppState;

fn open_writer(path: &str) -> Result<Box<dyn Write + Send>> {
    let f = std::fs::File::create(path).with_context(|| format!("create {path}"))?;
    if path.ends_with(".gz") {
        Ok(Box::new(BufWriter::new(flate2::write::GzEncoder::new(f, flate2::Compression::default()))))
    } else {
        Ok(Box::new(BufWriter::with_capacity(1 << 16, f)))
    }
}

pub async fn run_backup(state: Arc<AppState>, o: BackupOptions, p: Progress) -> Result<String> {
    let s = state.session(&o.connection_id).await?;
    match o.method {
        BackupMethod::Native => native_backup(&s, &o, &p).await,
        BackupMethod::Sql => sql_backup(&s, &o, &p).await,
    }
}

async fn sql_backup(s: &crate::db::session::Session, o: &BackupOptions, p: &Progress) -> Result<String> {
    let d = s.dialect();
    let mut conn = s.new_conn().await?;
    let schema = match &o.schema {
        Some(x) if !x.is_empty() => x.clone(),
        _ => metadata::default_schema(&mut conn, d).await?,
    };
    let objects = metadata::objects(&mut conn, d, &schema).await?;
    let tables: Vec<DbObject> = objects
        .iter()
        .filter(|x| x.kind == ObjectKind::Table && o.tables.as_ref().is_none_or(|t| t.is_empty() || t.contains(&x.name)))
        .cloned()
        .collect();
    let views: Vec<DbObject> = objects
        .iter()
        .filter(|x| x.kind == ObjectKind::View && o.tables.as_ref().is_none_or(|t| t.is_empty() || t.contains(&x.name)))
        .cloned()
        .collect();
    let mut w = open_writer(&o.file_path)?;
    writeln!(w, "-- SQLighter backup")?;
    writeln!(w, "-- Database: {} ({})", s.cfg.name, s.cfg.db_type.label())?;
    writeln!(w, "-- Server:   {}", s.server_version.lines().next().unwrap_or(""))?;
    writeln!(w, "-- Schema:   {schema}")?;
    writeln!(w, "-- Created:  {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z"))?;
    writeln!(w)?;
    match d {
        Dialect::Mysql => writeln!(w, "SET FOREIGN_KEY_CHECKS = 0;\nSET NAMES utf8mb4;\n")?,
        Dialect::Sqlite => writeln!(w, "PRAGMA foreign_keys = OFF;\nBEGIN;\n")?,
        Dialect::Postgres => writeln!(w, "SET client_encoding = 'UTF8';\nBEGIN;\n")?,
        _ => {}
    }
    let total = tables.len() as u64;
    let mut rows_total = 0u64;
    let mut deferred_fks: Vec<String> = vec![];
    // Tables are created without foreign keys first; FKs are added at the end so that the
    // data can be restored in any order.
    for (i, t) in tables.iter().enumerate() {
        p.update(i as u64, Some(total), Some(t.name.clone()));
        let info = metadata::describe(&mut conn, d, &schema, &t.name, ObjectKind::Table).await?;
        let target = qualified(&schema, &t.name, d);
        writeln!(w, "\n-- ----------------------------------------------------------\n-- Table {target}\n-- ----------------------------------------------------------")?;
        if o.drop_statements {
            writeln!(w, "{};", drop_table(&target, d))?;
        }
        if o.include_ddl {
            let fk_inline = d == Dialect::Sqlite; // SQLite cannot ALTER TABLE ADD CONSTRAINT
            let stmts = crate::sqlgen::create_table(&info, d, &crate::sqlgen::CreateOpts { to: d, schema: &schema, name: &t.name, foreign_keys: fk_inline, indexes: true });
            for st in stmts {
                writeln!(w, "{st};")?;
            }
            if !fk_inline {
                for fk in &info.foreign_keys {
                    deferred_fks.push(format!(
                        "ALTER TABLE {target} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}){};",
                        quote_ident(&fk.name, d),
                        fk.columns.iter().map(|c| quote_ident(c, d)).collect::<Vec<_>>().join(", "),
                        qualified(&fk.ref_schema, &fk.ref_table, d),
                        fk.ref_columns.iter().map(|c| quote_ident(c, d)).collect::<Vec<_>>().join(", "),
                        fk.on_delete.as_deref().filter(|a| *a != "NO ACTION").map(|a| format!(" ON DELETE {a}")).unwrap_or_default()
                    ));
                }
            }
        }
        if o.include_data {
            let cols: Vec<String> = info.columns.iter().map(|c| c.name.clone()).collect();
            let types: Vec<Option<String>> = info.columns.iter().map(|c| Some(c.data_type.clone())).collect();
            let select = format!("SELECT {} FROM {target}", cols.iter().map(|c| quote_ident(c, d)).collect::<Vec<_>>().join(", "));
            let identity = d == Dialect::Mssql && info.columns.iter().any(|c| c.auto_increment);
            if identity {
                writeln!(w, "SET IDENTITY_INSERT {target} ON;")?;
            }
            let (tx, mut rx) = mpsc::channel::<Batch>(4);
            let producer = async {
                let r = conn.stream(&select, 1000, &tx).await;
                drop(tx);
                r
            };
            let consumer = async {
                let mut n = 0u64;
                while let Some(b) = rx.recv().await {
                    if let Batch::Rows(rows) = b {
                        for st in crate::sqlgen::inserts(&target, &cols, &types, &rows, d, 100) {
                            w.write_all(st.as_bytes())?;
                            w.write_all(b";\n")?;
                        }
                        n += rows.len() as u64;
                        p.update(i as u64, Some(total), Some(format!("{}: {n} rows", t.name)));
                    }
                }
                Ok::<u64, anyhow::Error>(n)
            };
            let (a, b) = tokio::join!(producer, consumer);
            a?;
            rows_total += b?;
            if identity {
                writeln!(w, "SET IDENTITY_INSERT {target} OFF;")?;
            }
        }
    }
    if o.include_ddl {
        if !deferred_fks.is_empty() {
            writeln!(w, "\n-- Foreign keys")?;
            for fk in &deferred_fks {
                writeln!(w, "{fk}")?;
            }
        }
        for v in &views {
            if let Ok(ddl) = metadata::ddl(&mut conn, d, &schema, &v.name, ObjectKind::View).await {
                writeln!(w, "\n-- View {}", v.name)?;
                if o.drop_statements {
                    writeln!(w, "DROP VIEW IF EXISTS {};", qualified(&schema, &v.name, d))?;
                }
                let ddl = ddl.trim().trim_end_matches(';').to_string();
                writeln!(w, "{ddl};")?;
            }
        }
    }
    match d {
        Dialect::Mysql => writeln!(w, "\nSET FOREIGN_KEY_CHECKS = 1;")?,
        Dialect::Sqlite => writeln!(w, "\nCOMMIT;\nPRAGMA foreign_keys = ON;")?,
        Dialect::Postgres => writeln!(w, "\nCOMMIT;")?,
        _ => {}
    }
    w.flush()?;
    drop(w);
    Ok(format!("Backup finished: {} table(s), {rows_total} rows", tables.len()))
}

fn drop_table(target: &str, d: Dialect) -> String {
    match d {
        Dialect::Oracle => format!("BEGIN EXECUTE IMMEDIATE 'DROP TABLE {} CASCADE CONSTRAINTS'; EXCEPTION WHEN OTHERS THEN NULL; END;\n/", target.replace('\'', "''")),
        Dialect::Postgres => format!("DROP TABLE IF EXISTS {target} CASCADE"),
        _ => format!("DROP TABLE IF EXISTS {target}"),
    }
}

async fn native_backup(s: &crate::db::session::Session, o: &BackupOptions, p: &Progress) -> Result<String> {
    let cfg = &s.cfg;
    p.update(0, None, Some("running native backup tool…".into()));
    match cfg.db_type {
        DbType::Sqlite => {
            let conn = s.new_conn().await?;
            if let crate::db::Conn::Lite(l) = &conn {
                l.backup_to(&o.file_path).await?;
            }
            Ok("SQLite backup finished".into())
        }
        DbType::Postgres | DbType::Cockroach => {
            let bin = which::which("pg_dump").context("pg_dump not found in PATH. Install the PostgreSQL client tools or use the SQL backup method.")?;
            let (host, port) = endpoint(s).await?;
            let mut cmd = tokio::process::Command::new(bin);
            let custom = o.file_path.ends_with(".dump") || o.file_path.ends_with(".backup");
            cmd.arg("--no-password").arg("-h").arg(&host).arg("-p").arg(port.to_string()).arg("-U").arg(&cfg.user).arg("-f").arg(&o.file_path);
            cmd.arg(if custom { "-Fc" } else { "-Fp" });
            if let Some(schema) = o.schema.as_deref().filter(|x| !x.is_empty()) {
                cmd.arg("-n").arg(schema);
            }
            for t in o.tables.iter().flatten() {
                let sch = o.schema.as_deref().filter(|x| !x.is_empty()).unwrap_or("public");
                cmd.arg("-t").arg(format!("{}.{}", quote_ident(sch, Dialect::Postgres), quote_ident(t, Dialect::Postgres)));
            }
            if !o.include_data {
                cmd.arg("--schema-only");
            }
            if !o.include_ddl {
                cmd.arg("--data-only");
            }
            if o.drop_statements && !custom {
                cmd.arg("--clean").arg("--if-exists");
            }
            cmd.arg(if cfg.database.is_empty() { "postgres" } else { &cfg.database });
            // Secrets go through the environment, never the command line (visible in `ps`).
            if let Some(pw) = s.secrets().password.as_deref() {
                cmd.env("PGPASSWORD", pw);
            }
            pg_ssl_env(&mut cmd, cfg)?;
            run_tool(cmd).await?;
            Ok("pg_dump finished".into())
        }
        DbType::Mysql | DbType::Mariadb => {
            let bin = which::which("mariadb-dump").or_else(|_| which::which("mysqldump")).context("mysqldump not found in PATH. Install the MySQL/MariaDB client or use the SQL backup method.")?;
            let is_mariadb_tool = bin.file_name().map(|f| f.to_string_lossy().contains("mariadb")).unwrap_or(false) || tool_is_mariadb(&bin).await;
            let (host, port) = endpoint(s).await?;
            let mut cmd = tokio::process::Command::new(bin);
            // Password via a private temporary defaults file (not argv, not the environment).
            let tmp = defaults_file(s.secrets().password.as_deref().unwrap_or(""))?;
            cmd.arg(format!("--defaults-extra-file={}", tmp.display()));
            cmd.arg("-h").arg(&host).arg("-P").arg(port.to_string()).arg("-u").arg(&cfg.user);
            cmd.arg("--single-transaction").arg("--routines").arg("--triggers").arg("--hex-blob").arg(format!("--result-file={}", o.file_path));
            if !o.include_data {
                cmd.arg("--no-data");
            }
            if !o.include_ddl {
                cmd.arg("--no-create-info");
            }
            if !o.drop_statements {
                cmd.arg("--skip-add-drop-table");
            }
            match cfg.tls.mode {
                TlsMode::Disabled => {
                    cmd.arg(if is_mariadb_tool { "--skip-ssl" } else { "--ssl-mode=DISABLED" });
                }
                mode => {
                    if is_mariadb_tool {
                        cmd.arg("--ssl");
                        if mode == TlsMode::VerifyFull {
                            cmd.arg("--ssl-verify-server-cert");
                        }
                    } else {
                        cmd.arg(if mode == TlsMode::VerifyFull { "--ssl-mode=VERIFY_IDENTITY" } else { "--ssl-mode=VERIFY_CA" });
                    }
                    if let Some(ca) = cfg.tls.ca_file.as_deref().filter(|x| !x.is_empty()) {
                        cmd.arg(format!("--ssl-ca={ca}"));
                    }
                }
            }
            let db = o.schema.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| cfg.database.clone());
            if db.is_empty() {
                bail!("Choose a database (schema) to back up");
            }
            cmd.arg(&db);
            for t in o.tables.iter().flatten() {
                cmd.arg(t);
            }
            let r = run_tool(cmd).await;
            let _ = std::fs::remove_file(&tmp);
            r?;
            Ok("mysqldump finished".into())
        }
        _ => bail!("Native backup is not available for {}. Use the SQL backup method.", cfg.db_type.label()),
    }
}

async fn tool_is_mariadb(bin: &Path) -> bool {
    tokio::process::Command::new(bin).arg("--version").output().await.map(|o| String::from_utf8_lossy(&o.stdout).contains("MariaDB")).unwrap_or(false)
}

fn defaults_file(password: &str) -> Result<std::path::PathBuf> {
    let p = std::env::temp_dir().join(format!("sqlighter-{}.cnf", uuid::Uuid::new_v4()));
    let escaped = password.replace('\\', "\\\\").replace('"', "\\\"");
    crate::store::write_private(&p, format!("[client]\npassword=\"{escaped}\"\n").as_bytes())?;
    Ok(p)
}

fn pg_ssl_env(cmd: &mut tokio::process::Command, cfg: &ConnectionConfig) -> Result<()> {
    match cfg.tls.mode {
        TlsMode::Disabled => {
            cmd.env("PGSSLMODE", "disable");
        }
        TlsMode::VerifyFull | TlsMode::VerifyCa => {
            cmd.env("PGSSLMODE", if cfg.tls.mode == TlsMode::VerifyFull { "verify-full" } else { "verify-ca" });
            match cfg.tls.ca_file.as_deref().filter(|x| !x.is_empty()) {
                Some(ca) => cmd.env("PGSSLROOTCERT", ca),
                None => cmd.env("PGSSLROOTCERT", "system"),
            };
            if let Some(c) = cfg.tls.cert_file.as_deref().filter(|x| !x.is_empty()) {
                cmd.env("PGSSLCERT", c);
            }
            if let Some(k) = cfg.tls.key_file.as_deref().filter(|x| !x.is_empty()) {
                cmd.env("PGSSLKEY", k);
            }
        }
        TlsMode::Pinned => bail!("pg_dump cannot verify pinned certificates. Use the SQL backup method for this connection."),
    }
    Ok(())
}

/// Host/port for external tools; goes through the SSH tunnel's loopback forward if configured.
async fn endpoint(s: &crate::db::session::Session) -> Result<(String, u16)> {
    let port = if s.cfg.port == 0 { crate::db::session::default_port(s.cfg.db_type) } else { s.cfg.port };
    match &s.tunnel {
        Some(t) => {
            if s.cfg.tls.mode != TlsMode::Disabled && s.cfg.tls.mode != TlsMode::VerifyCa {
                bail!("Native tools cannot verify the server name through an SSH tunnel with TLS 'Verify full'. Use the SQL backup method.");
            }
            Ok(("127.0.0.1".into(), t.local_port().await?))
        }
        None => Ok((s.cfg.host.clone(), port)),
    }
}

async fn run_tool(mut cmd: tokio::process::Command) -> Result<()> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = cmd.output().await.context("failed to start backup tool")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------

pub async fn run_restore(state: Arc<AppState>, o: RestoreOptions, p: Progress) -> Result<String> {
    let s = state.session(&o.connection_id).await?;
    if s.cfg.read_only {
        bail!("This connection is read-only");
    }
    let mut head = [0u8; 5];
    let n = std::fs::File::open(&o.file_path)?.read(&mut head)?;
    if n == 5 && &head == b"PGDMP" {
        return pg_restore(&s, &o).await;
    }
    let path = o.file_path.clone();
    let text = tokio::task::spawn_blocking(move || -> Result<String> {
        let f = std::fs::File::open(&path)?;
        let mut s = String::new();
        if path.ends_with(".gz") {
            BufReader::new(flate2::read::GzDecoder::new(f)).read_to_string(&mut s)?;
        } else {
            let mut r = BufReader::new(f);
            // Validate UTF-8 lazily for a clearer error.
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).context("the file is not valid UTF-8 text")? == 0 {
                    break;
                }
                s.push_str(&line);
            }
        }
        Ok(s)
    })
    .await??;
    let d = s.dialect();
    let stmts = split_statements(&text, d);
    let total = stmts.len() as u64;
    let mut conn = s.new_conn().await?;
    let mut errors = Vec::new();
    for (i, st) in stmts.iter().enumerate() {
        if let Err(e) = conn.run(st, 0).await {
            let msg = format!("statement {}: {e:#}\n{}", i + 1, crate::db::session::truncate(st, 300));
            if o.stop_on_error {
                bail!(msg);
            }
            errors.push(msg);
        }
        if i % 50 == 0 || i as u64 + 1 == total {
            p.update(i as u64 + 1, Some(total), None);
        }
    }
    if errors.is_empty() {
        Ok(format!("{total} statements executed"))
    } else {
        Ok(format!("{total} statements executed, {} failed. First error: {}", errors.len(), errors[0]))
    }
}

async fn pg_restore(s: &crate::db::session::Session, o: &RestoreOptions) -> Result<String> {
    let bin = which::which("pg_restore").context("pg_restore not found in PATH")?;
    let (host, port) = endpoint(s).await?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--no-password").arg("-h").arg(&host).arg("-p").arg(port.to_string()).arg("-U").arg(&s.cfg.user);
    cmd.arg("-d").arg(if s.cfg.database.is_empty() { "postgres" } else { &s.cfg.database });
    if o.stop_on_error {
        cmd.arg("--exit-on-error");
    }
    cmd.arg(&o.file_path);
    if let Some(pw) = s.secrets().password.as_deref() {
        cmd.env("PGPASSWORD", pw);
    }
    pg_ssl_env(&mut cmd, &s.cfg)?;
    run_tool(cmd).await?;
    Ok("pg_restore finished".into())
}
