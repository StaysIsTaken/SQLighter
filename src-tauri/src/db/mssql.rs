//! Microsoft SQL Server driver (tiberius over rustls).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::TryStreamExt;
use serde_json::Value;
use tiberius::{AuthMethod, Client, ColumnData, ColumnType, Config, EncryptionLevel, QueryItem};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::{hex_value, Batch};
use crate::model::{ColumnMeta, ConnectionConfig, ConnectionSecrets, QueryResult, Row, TlsMode};
use crate::ssh::SshTunnel;

pub trait Rw: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Rw for T {}
type Stream = Compat<Box<dyn Rw>>;

pub struct MsConn {
    client: Client<Stream>,
}

impl MsConn {
    pub async fn connect(cfg: &ConnectionConfig, secrets: &ConnectionSecrets, tunnel: Option<Arc<SshTunnel>>) -> Result<MsConn> {
        let mut c = Config::new();
        c.host(&cfg.host);
        c.port(if cfg.port == 0 { 1433 } else { cfg.port });
        if !cfg.database.is_empty() {
            c.database(&cfg.database);
        }
        if let Some(inst) = cfg.instance_name.as_deref().filter(|s| !s.is_empty()) {
            c.instance_name(inst);
        }
        c.application_name("SQLighter");
        c.authentication(AuthMethod::sql_server(&cfg.user, secrets.password.clone().unwrap_or_default()));
        c.readonly(cfg.read_only);
        match cfg.tls.mode {
            TlsMode::Disabled => c.encryption(EncryptionLevel::NotSupported),
            TlsMode::VerifyFull => {
                c.encryption(EncryptionLevel::Required);
                if let Some(ca) = cfg.tls.ca_file.as_deref().filter(|s| !s.is_empty()) {
                    c.trust_cert_ca(ca);
                }
            }
            TlsMode::VerifyCa | TlsMode::Pinned => {
                bail!("SQL Server supports 'Verify full' only. Provide the server's CA certificate file if it is self-signed.")
            }
        }
        if let (Some(cert), Some(key)) = (cfg.tls.cert_file.as_deref().filter(|s| !s.is_empty()), cfg.tls.key_file.as_deref().filter(|s| !s.is_empty())) {
            c.client_certificate(cert, key);
        }
        let stream: Box<dyn Rw> = match &tunnel {
            Some(t) => Box::new(t.stream().await?),
            None => {
                let addr = c.get_addr();
                let tcp = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect(&addr))
                    .await
                    .with_context(|| format!("connection to {addr} timed out"))?
                    .with_context(|| format!("could not connect to {addr}"))?;
                tcp.set_nodelay(true)?;
                Box::new(tcp)
            }
        };
        let client = Client::connect(c, stream.compat_write()).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(MsConn { client })
    }

    pub async fn run(&mut self, sql: &str, max_rows: usize) -> Result<Vec<QueryResult>> {
        let kw = crate::sql::leading_keyword(sql);
        let is_dml = matches!(kw.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE") && !crate::sql::returns_rows_hint(sql);
        if is_dml {
            // sp_executesql reports the affected row count.
            let r = self.client.execute(sql, &[]).await.map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(vec![QueryResult::command(sql, Some(r.total()))]);
        }
        let mut stream = self.client.simple_query(sql).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut out = Vec::new();
        let mut current: Option<(Vec<ColumnMeta>, Vec<Row>, bool)> = None;
        while let Some(item) = stream.try_next().await.map_err(|e| anyhow::anyhow!("{e}"))? {
            match item {
                QueryItem::Metadata(m) => {
                    if let Some((c, r, t)) = current.take() {
                        out.push(QueryResult::rows(sql, c, r, t));
                    }
                    let cols = m.columns().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(type_name(c.column_type())) }).collect();
                    current = Some((cols, Vec::new(), false));
                }
                QueryItem::Row(row) => {
                    if let Some((_, rows, t)) = current.as_mut() {
                        if rows.len() < max_rows {
                            rows.push(convert_row(&row));
                        } else {
                            *t = true;
                        }
                    }
                }
            }
        }
        if let Some((c, r, t)) = current.take() {
            out.push(QueryResult::rows(sql, c, r, t));
        }
        if out.is_empty() {
            out.push(QueryResult::command(sql, None));
        }
        Ok(out)
    }

    pub async fn stream(&mut self, sql: &str, batch: usize, tx: &mpsc::Sender<Batch>) -> Result<()> {
        let mut stream = self.client.simple_query(sql).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut buf = Vec::with_capacity(batch);
        let mut first = true;
        while let Some(item) = stream.try_next().await.map_err(|e| anyhow::anyhow!("{e}"))? {
            match item {
                QueryItem::Metadata(m) => {
                    if !first {
                        break; // only the first result set is exported
                    }
                    first = false;
                    let cols = m.columns().iter().map(|c| ColumnMeta { name: c.name().to_string(), type_name: Some(type_name(c.column_type())) }).collect();
                    tx.send(Batch::Columns(cols)).await.ok();
                }
                QueryItem::Row(row) => {
                    buf.push(convert_row(&row));
                    if buf.len() >= batch && tx.send(Batch::Rows(std::mem::take(&mut buf))).await.is_err() {
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
        match self.run("SELECT @@VERSION", 1).await {
            Ok(r) => r.first().and_then(|r| r.rows.first()).and_then(|r| r.first()).and_then(|v| v.as_str().map(|s| s.lines().next().unwrap_or("").to_string())).unwrap_or_default(),
            Err(_) => String::new(),
        }
    }
}

fn type_name(t: ColumnType) -> String {
    use ColumnType::*;
    match t {
        Null => "null",
        Bit | Bitn => "bit",
        Int1 => "tinyint",
        Int2 => "smallint",
        Int4 | Intn => "int",
        Int8 => "bigint",
        Datetime4 => "smalldatetime",
        Float4 => "real",
        Float8 | Floatn => "float",
        Money | Money4 => "money",
        Datetime | Datetimen => "datetime",
        Guid => "uniqueidentifier",
        Decimaln | Numericn => "decimal",
        Daten => "date",
        Timen => "time",
        Datetime2 => "datetime2",
        DatetimeOffsetn => "datetimeoffset",
        BigVarBin => "varbinary",
        BigVarChar => "varchar",
        BigBinary => "binary",
        BigChar => "char",
        NVarchar => "nvarchar",
        NChar => "nchar",
        Xml => "xml",
        Udt => "udt",
        Text => "text",
        Image => "image",
        NText => "ntext",
        SSVariant => "sql_variant",
    }
    .to_string()
}

fn convert_row(row: &tiberius::Row) -> Row {
    let types: Vec<ColumnType> = row.columns().iter().map(|c| c.column_type()).collect();
    row.cells()
        .enumerate()
        .map(|(i, (_, data))| convert(row, i, data, types[i]))
        .collect()
}

fn convert(row: &tiberius::Row, i: usize, d: &ColumnData<'static>, _t: ColumnType) -> Value {
    match d {
        ColumnData::U8(v) => v.map(Value::from).unwrap_or(Value::Null),
        ColumnData::I16(v) => v.map(Value::from).unwrap_or(Value::Null),
        ColumnData::I32(v) => v.map(Value::from).unwrap_or(Value::Null),
        ColumnData::I64(v) => v.map(|n| super::num_i64(&n.to_string())).unwrap_or(Value::Null),
        ColumnData::F32(v) => v.and_then(|f| serde_json::Number::from_f64(f as f64)).map(Value::Number).unwrap_or(Value::Null),
        ColumnData::F64(v) => v.and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null),
        ColumnData::Bit(v) => v.map(Value::Bool).unwrap_or(Value::Null),
        ColumnData::String(v) => v.as_ref().map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        ColumnData::Guid(v) => v.map(|g| Value::String(g.to_string().to_uppercase())).unwrap_or(Value::Null),
        ColumnData::Binary(v) => v.as_ref().map(|b| hex_value(b)).unwrap_or(Value::Null),
        ColumnData::Numeric(v) => v.map(|n| Value::String(n.to_string())).unwrap_or(Value::Null),
        ColumnData::Xml(v) => v.as_ref().map(|x| Value::String(x.to_string())).unwrap_or(Value::Null),
        ColumnData::DateTime(None) | ColumnData::SmallDateTime(None) | ColumnData::DateTime2(None) => Value::Null,
        ColumnData::DateTime(_) | ColumnData::SmallDateTime(_) | ColumnData::DateTime2(_) => row
            .try_get::<chrono::NaiveDateTime, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::String(d.format("%Y-%m-%d %H:%M:%S%.f").to_string()))
            .unwrap_or(Value::Null),
        ColumnData::Date(_) => row.try_get::<chrono::NaiveDate, _>(i).ok().flatten().map(|d| Value::String(d.to_string())).unwrap_or(Value::Null),
        ColumnData::Time(_) => row.try_get::<chrono::NaiveTime, _>(i).ok().flatten().map(|d| Value::String(d.to_string())).unwrap_or(Value::Null),
        ColumnData::DateTimeOffset(_) => row
            .try_get::<chrono::DateTime<chrono::FixedOffset>, _>(i)
            .ok()
            .flatten()
            .map(|d| Value::String(d.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string()))
            .unwrap_or(Value::Null),
    }
}
