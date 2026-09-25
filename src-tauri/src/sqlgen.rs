//! SQL generation used by backup, export, import and DB-to-DB transfer
//! (the UI-side generators live in src/shared/sqlgen.ts).

use serde_json::Value;

use crate::model::{ColumnInfo, Dialect, TableInfo};
use crate::sql::{qualified, quote_ident, quote_literal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cat {
    SmallInt,
    Int,
    BigInt,
    Decimal,
    Float,
    Bool,
    Char,
    Varchar,
    Text,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Binary,
    Json,
    Uuid,
    Other,
}

pub fn category(dt: &str) -> Cat {
    let t = dt.trim().to_ascii_lowercase();
    let sw = |p: &str| t.starts_with(p);
    if t.is_empty() {
        return Cat::Text;
    }
    if t == "bool" || t == "boolean" || t == "bit" || t == "tinyint(1)" {
        return Cat::Bool;
    }
    if sw("smallint") || sw("int2") || sw("tinyint") || sw("smallserial") {
        return Cat::SmallInt;
    }
    if sw("bigint") || sw("int8") || sw("bigserial") {
        return Cat::BigInt;
    }
    if sw("int") || sw("integer") || sw("int4") || sw("mediumint") || sw("serial") {
        return Cat::Int;
    }
    if sw("numeric") || sw("decimal") || sw("number") || sw("money") || sw("smallmoney") || sw("dec") {
        return Cat::Decimal;
    }
    if sw("real") || sw("float") || sw("double") || sw("binary_float") || sw("binary_double") {
        return Cat::Float;
    }
    if sw("uuid") || sw("uniqueidentifier") {
        return Cat::Uuid;
    }
    if sw("json") {
        return Cat::Json;
    }
    if ["bytea", "blob", "longblob", "mediumblob", "tinyblob", "binary", "varbinary", "image", "raw", "long raw"].iter().any(|p| sw(p)) {
        return Cat::Binary;
    }
    if sw("timestamptz") || sw("datetimeoffset") || t.contains("with time zone") || t.contains("with local time zone") {
        return Cat::TimestampTz;
    }
    if sw("timestamp") || sw("datetime") || sw("smalldatetime") {
        return Cat::Timestamp;
    }
    if t == "date" {
        return Cat::Date;
    }
    if sw("time") {
        return Cat::Time;
    }
    if sw("varchar") || sw("nvarchar") || sw("character varying") || sw("varchar2") || sw("nvarchar2") || sw("string") {
        return if t.contains("(max)") { Cat::Text } else { Cat::Varchar };
    }
    if sw("char") || sw("nchar") || sw("character") || sw("bpchar") {
        return Cat::Char;
    }
    if ["text", "clob", "citext", "xml", "ntext", "long"].iter().any(|p| t.contains(p)) {
        return Cat::Text;
    }
    Cat::Other
}

fn paren_nums(dt: &str) -> (Option<i64>, Option<i64>) {
    let Some(a) = dt.find('(') else { return (None, None) };
    let Some(b) = dt[a..].find(')') else { return (None, None) };
    let inner = &dt[a + 1..a + b];
    let mut it = inner.split(',').map(|x| x.trim().parse::<i64>().ok());
    (it.next().flatten(), it.next().flatten())
}

pub fn map_type(c: &ColumnInfo, from: Dialect, to: Dialect) -> String {
    if from == to && !c.data_type.is_empty() {
        return c.data_type.clone();
    }
    let cat = category(&c.data_type);
    let (p1, p2) = paren_nums(&c.data_type);
    let len = c.max_length.filter(|l| *l > 0).or(p1);
    let prec = c.precision.or(p1);
    let scale = c.scale.or(p2).unwrap_or(0);
    let dec = prec.map(|p| format!("({p}, {scale})")).unwrap_or_default();
    use Cat::*;
    match to {
        Dialect::Postgres => match cat {
            SmallInt => "SMALLINT".into(),
            Int => "INTEGER".into(),
            BigInt => "BIGINT".into(),
            Decimal => format!("NUMERIC{dec}"),
            Float => "DOUBLE PRECISION".into(),
            Bool => "BOOLEAN".into(),
            Char => format!("CHAR({})", len.unwrap_or(1)),
            Varchar => len.map(|l| format!("VARCHAR({l})")).unwrap_or("TEXT".into()),
            Text | Other => "TEXT".into(),
            Date => "DATE".into(),
            Time => "TIME".into(),
            Timestamp => "TIMESTAMP".into(),
            TimestampTz => "TIMESTAMPTZ".into(),
            Binary => "BYTEA".into(),
            Json => "JSONB".into(),
            Uuid => "UUID".into(),
        },
        Dialect::Mysql => match cat {
            SmallInt => "SMALLINT".into(),
            Int => "INT".into(),
            BigInt => "BIGINT".into(),
            Decimal => if dec.is_empty() { "DECIMAL(38, 10)".into() } else { format!("DECIMAL{dec}") },
            Float => "DOUBLE".into(),
            Bool => "TINYINT(1)".into(),
            Char => format!("CHAR({})", len.unwrap_or(1).min(255)),
            Varchar => format!("VARCHAR({})", len.unwrap_or(255).min(16383)),
            Text | Other => "LONGTEXT".into(),
            Date => "DATE".into(),
            Time => "TIME".into(),
            Timestamp | TimestampTz => "DATETIME(6)".into(),
            Binary => "LONGBLOB".into(),
            Json => "JSON".into(),
            Uuid => "CHAR(36)".into(),
        },
        Dialect::Sqlite => match cat {
            SmallInt | Int | BigInt | Bool => "INTEGER".into(),
            Decimal => "NUMERIC".into(),
            Float => "REAL".into(),
            Binary => "BLOB".into(),
            _ => "TEXT".into(),
        },
        Dialect::Mssql => match cat {
            SmallInt => "SMALLINT".into(),
            Int => "INT".into(),
            BigInt => "BIGINT".into(),
            Decimal => if dec.is_empty() { "DECIMAL(38, 10)".into() } else { format!("DECIMAL{dec}") },
            Float => "FLOAT".into(),
            Bool => "BIT".into(),
            Char => format!("NCHAR({})", len.unwrap_or(1).min(4000)),
            Varchar => match len {
                Some(l) if l <= 4000 => format!("NVARCHAR({l})"),
                _ => "NVARCHAR(MAX)".into(),
            },
            Text | Json | Other => "NVARCHAR(MAX)".into(),
            Date => "DATE".into(),
            Time => "TIME".into(),
            Timestamp => "DATETIME2".into(),
            TimestampTz => "DATETIMEOFFSET".into(),
            Binary => "VARBINARY(MAX)".into(),
            Uuid => "UNIQUEIDENTIFIER".into(),
        },
        Dialect::Oracle => match cat {
            SmallInt => "NUMBER(5)".into(),
            Int => "NUMBER(10)".into(),
            BigInt => "NUMBER(19)".into(),
            Decimal => format!("NUMBER{dec}"),
            Float => "BINARY_DOUBLE".into(),
            Bool => "NUMBER(1)".into(),
            Char => format!("CHAR({})", len.unwrap_or(1)),
            Varchar => match len {
                Some(l) if l <= 4000 => format!("VARCHAR2({l} CHAR)"),
                _ => "CLOB".into(),
            },
            Text | Json | Other => "CLOB".into(),
            Date => "DATE".into(),
            Time => "VARCHAR2(20)".into(),
            Timestamp => "TIMESTAMP".into(),
            TimestampTz => "TIMESTAMP WITH TIME ZONE".into(),
            Binary => "BLOB".into(),
            Uuid => "VARCHAR2(36)".into(),
        },
    }
}

fn portable_default(v: &str) -> bool {
    let t = v.trim();
    let up = t.to_ascii_uppercase();
    t.parse::<f64>().is_ok() || (t.starts_with('\'') && t.ends_with('\'') && t.len() >= 2) || ["NULL", "TRUE", "FALSE", "CURRENT_TIMESTAMP"].contains(&up.as_str())
}

pub struct CreateOpts<'a> {
    pub to: Dialect,
    pub schema: &'a str,
    pub name: &'a str,
    pub foreign_keys: bool,
    pub indexes: bool,
}

pub fn create_table(t: &TableInfo, from: Dialect, o: &CreateOpts) -> Vec<String> {
    let to = o.to;
    let target = qualified(o.schema, o.name, to);
    let mut lines = Vec::new();
    let single_auto_pk = t.primary_key.len() == 1 && t.columns.iter().any(|c| c.name == t.primary_key[0] && c.auto_increment);
    let mut inline_pk = false;
    for c in &t.columns {
        let mut ty = map_type(c, from, to);
        let mut extra = String::new();
        let convert_auto = c.auto_increment && from != to;
        if convert_auto {
            match to {
                Dialect::Postgres => ty = if category(&c.data_type) == Cat::BigInt { "BIGINT GENERATED BY DEFAULT AS IDENTITY".into() } else { "INTEGER GENERATED BY DEFAULT AS IDENTITY".into() },
                Dialect::Mysql => extra = " AUTO_INCREMENT".into(),
                Dialect::Mssql => extra = " IDENTITY(1,1)".into(),
                Dialect::Oracle => extra = " GENERATED BY DEFAULT AS IDENTITY".into(),
                Dialect::Sqlite => {
                    if single_auto_pk {
                        ty = "INTEGER".into();
                        extra = " PRIMARY KEY AUTOINCREMENT".into();
                        inline_pk = true;
                    }
                }
            }
        }
        let mut line = format!("  {} {}{}", quote_ident(&c.name, to), ty, extra);
        if !c.nullable && !inline_pk {
            line.push_str(" NOT NULL");
        }
        if let Some(def) = c.default_value.as_deref().filter(|d| !d.is_empty()) {
            if !convert_auto && (from == to || portable_default(def)) {
                line.push_str(&format!(" DEFAULT {def}"));
            }
        }
        lines.push(line);
    }
    if !t.primary_key.is_empty() && !inline_pk {
        lines.push(format!("  PRIMARY KEY ({})", t.primary_key.iter().map(|c| quote_ident(c, to)).collect::<Vec<_>>().join(", ")));
    }
    if o.foreign_keys {
        for fk in &t.foreign_keys {
            let ref_schema = if from == to { fk.ref_schema.as_str() } else { o.schema };
            let mut l = format!(
                "  CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
                quote_ident(&fk.name, to),
                fk.columns.iter().map(|c| quote_ident(c, to)).collect::<Vec<_>>().join(", "),
                qualified(ref_schema, &fk.ref_table, to),
                fk.ref_columns.iter().map(|c| quote_ident(c, to)).collect::<Vec<_>>().join(", ")
            );
            if let Some(a) = fk.on_delete.as_deref().filter(|a| *a != "NO ACTION" && !a.is_empty()) {
                l.push_str(&format!(" ON DELETE {a}"));
            }
            if let Some(a) = fk.on_update.as_deref().filter(|a| *a != "NO ACTION" && !a.is_empty()) {
                if to != Dialect::Oracle {
                    l.push_str(&format!(" ON UPDATE {a}"));
                }
            }
            lines.push(l);
        }
    }
    let mut out = vec![format!("CREATE TABLE {target} (\n{}\n)", lines.join(",\n"))];
    if o.indexes {
        for idx in &t.indexes {
            if idx.primary || (!t.primary_key.is_empty() && idx.columns == t.primary_key) {
                continue;
            }
            // Index names are schema-qualified only in Oracle; elsewhere they follow the table.
            let idx_name = if to == Dialect::Oracle { qualified(o.schema, &idx.name, to) } else { quote_ident(&idx.name, to) };
            out.push(format!(
                "CREATE {}INDEX {} ON {} ({})",
                if idx.unique { "UNIQUE " } else { "" },
                idx_name,
                target,
                idx.columns.iter().map(|c| quote_ident(c, to)).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    out
}

/// Multi-row INSERT statements (without trailing semicolon).
pub fn inserts(target: &str, columns: &[String], types: &[Option<String>], rows: &[Vec<Value>], d: Dialect, batch: usize) -> Vec<String> {
    let cols = columns.iter().map(|c| quote_ident(c, d)).collect::<Vec<_>>().join(", ");
    let lit = |r: &Vec<Value>| {
        format!(
            "({})",
            r.iter().enumerate().map(|(i, v)| quote_literal(v, d, types.get(i).and_then(|t| t.as_deref()))).collect::<Vec<_>>().join(", ")
        )
    };
    let batch = batch.max(1).min(if d == Dialect::Mssql { 1000 } else { 10_000 });
    let mut out = Vec::new();
    for chunk in rows.chunks(batch) {
        if d == Dialect::Oracle && chunk.len() > 1 {
            out.push(format!(
                "INSERT ALL\n{}\nSELECT 1 FROM DUAL",
                chunk.iter().map(|r| format!("  INTO {target} ({cols}) VALUES {}", lit(r))).collect::<Vec<_>>().join("\n")
            ));
        } else if chunk.len() == 1 {
            out.push(format!("INSERT INTO {target} ({cols}) VALUES {}", lit(&chunk[0])));
        } else {
            out.push(format!("INSERT INTO {target} ({cols}) VALUES\n  {}", chunk.iter().map(lit).collect::<Vec<_>>().join(",\n  ")));
        }
    }
    out
}

/// Infers column types for importing data into a new table.
pub fn infer_columns(names: &[String], rows: &[Vec<Value>]) -> Vec<ColumnInfo> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let (mut all_int, mut all_num, mut all_bool, mut all_date, mut all_ts, mut any) = (true, true, true, true, true, false);
            let mut max_len = 0usize;
            for r in rows {
                let v = r.get(i).unwrap_or(&Value::Null);
                let s = match v {
                    Value::Null => continue,
                    Value::Bool(_) => {
                        any = true;
                        all_int = false;
                        all_num = false;
                        all_date = false;
                        all_ts = false;
                        continue;
                    }
                    Value::Number(n) => n.to_string(),
                    Value::String(s) if s.is_empty() => continue,
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                any = true;
                if !matches!(v, Value::Bool(_)) {
                    let low = s.to_ascii_lowercase();
                    if low != "true" && low != "false" {
                        all_bool = false;
                    }
                }
                max_len = max_len.max(s.chars().count());
                if s.len() > 18 || s.parse::<i64>().is_err() {
                    all_int = false;
                }
                if s.parse::<f64>().is_err() {
                    all_num = false;
                }
                if !is_date(&s) {
                    all_date = false;
                }
                if !is_timestamp(&s) {
                    all_ts = false;
                }
            }
            let data_type = if !any {
                "text".to_string()
            } else if all_bool {
                "boolean".into()
            } else if all_int {
                "bigint".into()
            } else if all_num {
                "double precision".into()
            } else if all_date {
                "date".into()
            } else if all_ts {
                "timestamp".into()
            } else if max_len > 255 {
                "text".into()
            } else {
                format!("varchar({})", (max_len.max(1)).next_power_of_two().max(16))
            };
            ColumnInfo { name: name.clone(), data_type, nullable: true, ..Default::default() }
        })
        .collect()
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn is_timestamp(s: &str) -> bool {
    s.len() >= 16 && is_date(&s[..10]) && (s.as_bytes()[10] == b' ' || s.as_bytes()[10] == b'T') && s.as_bytes()[13] == b':'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ObjectKind;

    fn col(name: &str, t: &str, auto: bool) -> ColumnInfo {
        ColumnInfo { name: name.into(), data_type: t.into(), nullable: !auto, is_primary_key: auto, auto_increment: auto, ..Default::default() }
    }

    #[test]
    fn create_table_cross_dialect() {
        let t = TableInfo {
            schema: "public".into(),
            name: "users".into(),
            kind: ObjectKind::Table,
            columns: vec![col("id", "integer", true), col("name", "character varying(100)", false), col("data", "jsonb", false)],
            primary_key: vec!["id".into()],
            indexes: vec![],
            foreign_keys: vec![],
            comment: None,
            row_estimate: None,
        };
        let my = create_table(&t, Dialect::Postgres, &CreateOpts { to: Dialect::Mysql, schema: "", name: "users", foreign_keys: true, indexes: true });
        assert!(my[0].contains("`id` INT AUTO_INCREMENT NOT NULL"), "{}", my[0]);
        assert!(my[0].contains("`name` VARCHAR(100)"));
        assert!(my[0].contains("`data` JSON"));
        let lite = create_table(&t, Dialect::Postgres, &CreateOpts { to: Dialect::Sqlite, schema: "main", name: "users", foreign_keys: true, indexes: true });
        assert!(lite[0].contains("\"id\" INTEGER PRIMARY KEY AUTOINCREMENT"), "{}", lite[0]);
        assert!(!lite[0].contains("PRIMARY KEY (\"id\")"));
    }

    #[test]
    fn insert_batches() {
        let rows = vec![vec![Value::from(1), Value::from("a")], vec![Value::from(2), Value::Null]];
        let v = inserts("t", &["id".into(), "n".into()], &[], &rows, Dialect::Postgres, 10);
        assert_eq!(v, vec!["INSERT INTO t (\"id\", \"n\") VALUES\n  (1, 'a'),\n  (2, NULL)"]);
        let o = inserts("t", &["id".into()], &[], &rows.iter().map(|r| vec![r[0].clone()]).collect::<Vec<_>>(), Dialect::Oracle, 10);
        assert!(o[0].starts_with("INSERT ALL"));
    }

    #[test]
    fn infer() {
        let rows = vec![vec![Value::from("1"), Value::from("2024-01-01"), Value::from("x")], vec![Value::from("22"), Value::from("2024-02-01"), Value::from("hello")]];
        let c = infer_columns(&["a".into(), "b".into(), "c".into()], &rows);
        assert_eq!(c[0].data_type, "bigint");
        assert_eq!(c[1].data_type, "date");
        assert_eq!(c[2].data_type, "varchar(16)");
    }
}
