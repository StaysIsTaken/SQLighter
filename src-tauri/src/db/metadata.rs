//! Catalog queries per dialect: schemas, objects, table structure and DDL.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{cell_bool, cell_i64, cell_opt, cell_str, Conn};
use crate::model::{ColumnInfo, DbObject, Dialect, ForeignKeyInfo, IndexInfo, ObjectKind, TableInfo};
use crate::sql::{qualified, quote_ident, quote_literal};

fn lit(s: &str, d: Dialect) -> String {
    quote_literal(&Value::String(s.to_string()), d, None)
}

pub async fn schemas(c: &mut Conn, d: Dialect) -> Result<Vec<String>> {
    let sql = match d {
        Dialect::Postgres => "SELECT nspname FROM pg_namespace WHERE nspname NOT LIKE 'pg\\_toast%' AND nspname NOT LIKE 'pg\\_temp%' ORDER BY nspname",
        Dialect::Mysql => "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
        Dialect::Sqlite => "SELECT name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq",
        Dialect::Mssql => "SELECT name FROM sys.schemas WHERE name NOT IN ('guest','INFORMATION_SCHEMA','sys') AND name NOT LIKE 'db[_]%' ORDER BY name",
        Dialect::Oracle => "SELECT username FROM all_users ORDER BY username",
    };
    Ok(c.rows(sql).await?.iter().map(|r| cell_str(&r[0])).collect())
}

pub async fn default_schema(c: &mut Conn, d: Dialect) -> Result<String> {
    let sql = match d {
        Dialect::Postgres => "SELECT current_schema()",
        Dialect::Mysql => "SELECT DATABASE()",
        Dialect::Sqlite => return Ok("main".into()),
        Dialect::Mssql => "SELECT SCHEMA_NAME()",
        Dialect::Oracle => "SELECT SYS_CONTEXT('USERENV','CURRENT_SCHEMA') FROM dual",
    };
    Ok(c.rows(sql).await?.first().and_then(|r| cell_opt(&r[0])).unwrap_or_default())
}

pub async fn objects(c: &mut Conn, d: Dialect, schema: &str) -> Result<Vec<DbObject>> {
    let s = lit(schema, d);
    let mut out = Vec::new();
    let obj = |name: String, kind: ObjectKind, comment: Option<String>| DbObject { name, kind, schema: schema.to_string(), comment };
    match d {
        Dialect::Postgres => {
            let rows = c
                .rows(&format!(
                    "SELECT c.relname, c.relkind::text, obj_description(c.oid, 'pg_class') FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     WHERE n.nspname = {s} AND c.relkind IN ('r','p','v','m','f','S') ORDER BY c.relname"
                ))
                .await?;
            for r in rows {
                let kind = match cell_str(&r[1]).as_str() {
                    "v" => ObjectKind::View,
                    "m" => ObjectKind::MaterializedView,
                    "S" => ObjectKind::Sequence,
                    _ => ObjectKind::Table,
                };
                out.push(obj(cell_str(&r[0]), kind, cell_opt(&r[2])));
            }
            if let Ok(rows) = c
                .rows(&format!(
                    "SELECT p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')', p.prokind::text FROM pg_proc p \
                     JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = {s} AND p.prokind IN ('f','p') ORDER BY 1"
                ))
                .await
            {
                for r in rows {
                    let kind = if cell_str(&r[1]) == "p" { ObjectKind::Procedure } else { ObjectKind::Function };
                    out.push(obj(cell_str(&r[0]), kind, None));
                }
            }
        }
        Dialect::Mysql => {
            for r in c.rows(&format!("SELECT TABLE_NAME, TABLE_TYPE, TABLE_COMMENT FROM information_schema.TABLES WHERE TABLE_SCHEMA = {s} ORDER BY TABLE_NAME")).await? {
                let kind = if cell_str(&r[1]).contains("VIEW") { ObjectKind::View } else { ObjectKind::Table };
                out.push(obj(cell_str(&r[0]), kind, cell_opt(&r[2]).filter(|x| !x.is_empty())));
            }
            for r in c.rows(&format!("SELECT ROUTINE_NAME, ROUTINE_TYPE FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = {s} ORDER BY ROUTINE_NAME")).await? {
                let kind = if cell_str(&r[1]) == "PROCEDURE" { ObjectKind::Procedure } else { ObjectKind::Function };
                out.push(obj(cell_str(&r[0]), kind, None));
            }
        }
        Dialect::Sqlite => {
            let q = quote_ident(schema, d);
            for r in c.rows(&format!("SELECT name, type FROM {q}.sqlite_master WHERE type IN ('table','view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name")).await? {
                let kind = if cell_str(&r[1]) == "view" { ObjectKind::View } else { ObjectKind::Table };
                out.push(obj(cell_str(&r[0]), kind, None));
            }
        }
        Dialect::Mssql => {
            for r in c
                .rows(&format!(
                    "SELECT o.name, RTRIM(o.type) FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
                     WHERE s.name = {s} AND o.type IN ('U','V','P','FN','IF','TF','SO') AND o.is_ms_shipped = 0 ORDER BY o.name"
                ))
                .await?
            {
                let kind = match cell_str(&r[1]).as_str() {
                    "U" => ObjectKind::Table,
                    "V" => ObjectKind::View,
                    "P" => ObjectKind::Procedure,
                    "SO" => ObjectKind::Sequence,
                    _ => ObjectKind::Function,
                };
                out.push(obj(cell_str(&r[0]), kind, None));
            }
        }
        Dialect::Oracle => {
            let mut seen = std::collections::HashSet::new();
            for r in c
                .rows(&format!(
                    "SELECT object_name, object_type FROM all_objects WHERE owner = {s} AND object_type IN \
                     ('MATERIALIZED VIEW','TABLE','VIEW','PROCEDURE','FUNCTION','SEQUENCE','PACKAGE') ORDER BY object_name, object_type DESC"
                ))
                .await?
            {
                let name = cell_str(&r[0]);
                if !seen.insert(name.clone()) {
                    continue;
                }
                let kind = match cell_str(&r[1]).as_str() {
                    "VIEW" => ObjectKind::View,
                    "MATERIALIZED VIEW" => ObjectKind::MaterializedView,
                    "PROCEDURE" | "PACKAGE" => ObjectKind::Procedure,
                    "FUNCTION" => ObjectKind::Function,
                    "SEQUENCE" => ObjectKind::Sequence,
                    _ => ObjectKind::Table,
                };
                out.push(obj(name, kind, None));
            }
        }
    }
    Ok(out)
}

fn group_indexes(rows: Vec<(String, bool, bool, String)>) -> Vec<IndexInfo> {
    let mut map: BTreeMap<String, IndexInfo> = BTreeMap::new();
    for (name, unique, primary, col) in rows {
        let e = map.entry(name.clone()).or_insert(IndexInfo { name, columns: vec![], unique, primary });
        e.columns.push(col);
    }
    map.into_values().collect()
}

fn group_fks(rows: Vec<(String, String, String, String, String, Option<String>, Option<String>)>) -> Vec<ForeignKeyInfo> {
    let mut map: BTreeMap<String, ForeignKeyInfo> = BTreeMap::new();
    for (name, col, rs, rt, rc, del, upd) in rows {
        let e = map.entry(name.clone()).or_insert(ForeignKeyInfo {
            name,
            columns: vec![],
            ref_schema: rs,
            ref_table: rt,
            ref_columns: vec![],
            on_delete: del,
            on_update: upd,
        });
        e.columns.push(col);
        e.ref_columns.push(rc);
    }
    map.into_values().collect()
}

pub async fn describe(c: &mut Conn, d: Dialect, schema: &str, name: &str, kind: ObjectKind) -> Result<TableInfo> {
    let mut t = TableInfo {
        schema: schema.to_string(),
        name: name.to_string(),
        kind,
        columns: vec![],
        primary_key: vec![],
        indexes: vec![],
        foreign_keys: vec![],
        comment: None,
        row_estimate: None,
    };
    let s = lit(schema, d);
    let n = lit(name, d);
    match d {
        Dialect::Postgres => {
            let reg = lit(&qualified(schema, name, d), d);
            let info = c.rows(&format!("SELECT c.relkind::text, obj_description(c.oid, 'pg_class'), c.reltuples::bigint FROM pg_class c WHERE c.oid = {reg}::regclass")).await?;
            if let Some(r) = info.first() {
                t.kind = match cell_str(&r[0]).as_str() {
                    "v" => ObjectKind::View,
                    "m" => ObjectKind::MaterializedView,
                    _ => ObjectKind::Table,
                };
                t.comment = cell_opt(&r[1]);
                t.row_estimate = cell_i64(&r[2]).filter(|x| *x >= 0);
            }
            for r in c
                .rows(&format!(
                    "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, pg_get_expr(d.adbin, d.adrelid), \
                     col_description(a.attrelid, a.attnum), a.attidentity::text \
                     FROM pg_attribute a LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                     WHERE a.attrelid = {reg}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum"
                ))
                .await?
            {
                let default = cell_opt(&r[3]);
                let identity = cell_str(&r[5]);
                let auto = identity == "a" || identity == "d" || default.as_deref().is_some_and(|x| x.starts_with("nextval("));
                t.columns.push(ColumnInfo {
                    name: cell_str(&r[0]),
                    data_type: cell_str(&r[1]),
                    nullable: cell_bool(&r[2]),
                    default_value: default,
                    auto_increment: auto,
                    comment: cell_opt(&r[4]),
                    ..Default::default()
                });
            }
            let idx = c
                .rows(&format!(
                    "SELECT i.relname, ix.indisunique, ix.indisprimary, \
                     array_to_string(ARRAY(SELECT pg_get_indexdef(ix.indexrelid, k + 1, true) FROM generate_subscripts(ix.indkey, 1) k ORDER BY k), chr(31)) \
                     FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid WHERE ix.indrelid = {reg}::regclass ORDER BY i.relname"
                ))
                .await?;
            for r in idx {
                let cols: Vec<String> = cell_str(&r[3]).split('\u{1f}').map(|x| x.trim_matches('"').to_string()).collect();
                let primary = cell_bool(&r[2]);
                if primary {
                    t.primary_key = cols.clone();
                }
                t.indexes.push(IndexInfo { name: cell_str(&r[0]), columns: cols, unique: cell_bool(&r[1]), primary });
            }
            let act = |x: &str| match x {
                "r" => "RESTRICT",
                "c" => "CASCADE",
                "n" => "SET NULL",
                "d" => "SET DEFAULT",
                _ => "NO ACTION",
            };
            let fks = c
                .rows(&format!(
                    "SELECT con.conname, \
                     array_to_string(ARRAY(SELECT a.attname FROM unnest(con.conkey) WITH ORDINALITY k(n, o) JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.n ORDER BY k.o), chr(31)), \
                     fn.nspname, fc.relname, \
                     array_to_string(ARRAY(SELECT a.attname FROM unnest(con.confkey) WITH ORDINALITY k(n, o) JOIN pg_attribute a ON a.attrelid = con.confrelid AND a.attnum = k.n ORDER BY k.o), chr(31)), \
                     con.confdeltype::text, con.confupdtype::text \
                     FROM pg_constraint con JOIN pg_class fc ON fc.oid = con.confrelid JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
                     WHERE con.contype = 'f' AND con.conrelid = {reg}::regclass ORDER BY con.conname"
                ))
                .await
                .unwrap_or_default();
            for r in fks {
                t.foreign_keys.push(ForeignKeyInfo {
                    name: cell_str(&r[0]),
                    columns: cell_str(&r[1]).split('\u{1f}').map(String::from).collect(),
                    ref_schema: cell_str(&r[2]),
                    ref_table: cell_str(&r[3]),
                    ref_columns: cell_str(&r[4]).split('\u{1f}').map(String::from).collect(),
                    on_delete: Some(act(&cell_str(&r[5])).to_string()),
                    on_update: Some(act(&cell_str(&r[6])).to_string()),
                });
            }
        }
        Dialect::Mysql => {
            if let Some(r) = c.rows(&format!("SELECT TABLE_TYPE, TABLE_COMMENT, TABLE_ROWS FROM information_schema.TABLES WHERE TABLE_SCHEMA = {s} AND TABLE_NAME = {n}")).await?.first() {
                t.kind = if cell_str(&r[0]).contains("VIEW") { ObjectKind::View } else { ObjectKind::Table };
                t.comment = cell_opt(&r[1]).filter(|x| !x.is_empty());
                t.row_estimate = cell_i64(&r[2]);
            }
            for r in c
                .rows(&format!(
                    "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT, EXTRA, COLUMN_COMMENT, CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE \
                     FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = {s} AND TABLE_NAME = {n} ORDER BY ORDINAL_POSITION"
                ))
                .await?
            {
                let extra = cell_str(&r[4]).to_ascii_lowercase();
                t.columns.push(ColumnInfo {
                    name: cell_str(&r[0]),
                    data_type: cell_str(&r[1]),
                    nullable: cell_bool(&r[2]),
                    default_value: cell_opt(&r[3]).filter(|x| x != "NULL"),
                    auto_increment: extra.contains("auto_increment"),
                    comment: cell_opt(&r[5]).filter(|x| !x.is_empty()),
                    max_length: cell_i64(&r[6]),
                    precision: cell_i64(&r[7]),
                    scale: cell_i64(&r[8]),
                    ..Default::default()
                });
            }
            let rows = c
                .rows(&format!(
                    "SELECT INDEX_NAME, NON_UNIQUE, COLUMN_NAME FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = {s} AND TABLE_NAME = {n} ORDER BY INDEX_NAME, SEQ_IN_INDEX"
                ))
                .await?;
            t.indexes = group_indexes(
                rows.iter()
                    .map(|r| {
                        let name = cell_str(&r[0]);
                        let primary = name == "PRIMARY";
                        (name, !cell_bool(&r[1]), primary, cell_str(&r[2]))
                    })
                    .collect(),
            );
            if let Some(pk) = t.indexes.iter().find(|i| i.primary) {
                t.primary_key = pk.columns.clone();
            }
            let fks = c
                .rows(&format!(
                    "SELECT k.CONSTRAINT_NAME, k.COLUMN_NAME, k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, r.DELETE_RULE, r.UPDATE_RULE \
                     FROM information_schema.KEY_COLUMN_USAGE k JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
                     ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME AND r.TABLE_NAME = k.TABLE_NAME \
                     WHERE k.TABLE_SCHEMA = {s} AND k.TABLE_NAME = {n} AND k.REFERENCED_TABLE_NAME IS NOT NULL ORDER BY k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
                ))
                .await?;
            t.foreign_keys = group_fks(
                fks.iter()
                    .map(|r| (cell_str(&r[0]), cell_str(&r[1]), cell_str(&r[2]), cell_str(&r[3]), cell_str(&r[4]), cell_opt(&r[5]), cell_opt(&r[6])))
                    .collect(),
            );
        }
        Dialect::Sqlite => {
            let q = quote_ident(schema, d);
            if let Some(r) = c.rows(&format!("SELECT type FROM {q}.sqlite_master WHERE name = {n}")).await?.first() {
                t.kind = if cell_str(&r[0]) == "view" { ObjectKind::View } else { ObjectKind::Table };
            }
            let cols = c.rows(&format!("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info({n}, {s}) ORDER BY cid")).await?;
            let mut pk: Vec<(i64, String)> = vec![];
            for r in &cols {
                let p = cell_i64(&r[4]).unwrap_or(0);
                if p > 0 {
                    pk.push((p, cell_str(&r[0])));
                }
            }
            pk.sort();
            t.primary_key = pk.iter().map(|x| x.1.clone()).collect();
            for r in &cols {
                let name = cell_str(&r[0]);
                let ty = cell_str(&r[1]);
                let is_rowid = t.primary_key.len() == 1 && t.primary_key[0] == name && ty.eq_ignore_ascii_case("INTEGER");
                t.columns.push(ColumnInfo {
                    is_primary_key: t.primary_key.contains(&name),
                    name,
                    data_type: ty,
                    nullable: !cell_bool(&r[2]),
                    default_value: cell_opt(&r[3]),
                    // INTEGER PRIMARY KEY is an alias of the rowid and auto-assigned.
                    auto_increment: is_rowid,
                    ..Default::default()
                });
            }
            let idx = c.rows(&format!("SELECT name, \"unique\", origin FROM pragma_index_list({n}, {s})")).await?;
            for r in idx {
                let iname = cell_str(&r[0]);
                let cols = c.rows(&format!("SELECT name FROM pragma_index_info({}, {s}) ORDER BY seqno", lit(&iname, d))).await?;
                t.indexes.push(IndexInfo {
                    name: iname,
                    columns: cols.iter().map(|x| cell_str(&x[0])).collect(),
                    unique: cell_bool(&r[1]),
                    primary: cell_str(&r[2]) == "pk",
                });
            }
            let fks = c.rows(&format!("SELECT id, \"from\", \"table\", \"to\", on_delete, on_update FROM pragma_foreign_key_list({n}, {s}) ORDER BY id, seq")).await?;
            t.foreign_keys = group_fks(
                fks.iter()
                    .map(|r| {
                        (format!("fk_{}_{}", name, cell_str(&r[0])), cell_str(&r[1]), schema.to_string(), cell_str(&r[2]), cell_str(&r[3]), cell_opt(&r[4]), cell_opt(&r[5]))
                    })
                    .collect(),
            );
            if t.kind == ObjectKind::Table {
                if let Ok(r) = c.rows(&format!("SELECT COUNT(*) FROM {}", qualified(schema, name, d))).await {
                    t.row_estimate = r.first().and_then(|x| cell_i64(&x[0]));
                }
            }
        }
        Dialect::Mssql => {
            let obj = lit(&qualified(schema, name, d), d);
            if let Some(r) = c.rows(&format!("SELECT RTRIM(type) FROM sys.objects WHERE object_id = OBJECT_ID({obj})")).await?.first() {
                t.kind = if cell_str(&r[0]) == "V" { ObjectKind::View } else { ObjectKind::Table };
            }
            for r in c
                .rows(&format!(
                    "SELECT c.name, TYPE_NAME(c.user_type_id) + CASE \
                       WHEN TYPE_NAME(c.user_type_id) IN ('varchar','char','varbinary','binary') THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length AS varchar(10)) END + ')' \
                       WHEN TYPE_NAME(c.user_type_id) IN ('nvarchar','nchar') THEN '(' + CASE WHEN c.max_length = -1 THEN 'max' ELSE CAST(c.max_length / 2 AS varchar(10)) END + ')' \
                       WHEN TYPE_NAME(c.user_type_id) IN ('decimal','numeric') THEN '(' + CAST(c.precision AS varchar(10)) + ',' + CAST(c.scale AS varchar(10)) + ')' \
                       WHEN TYPE_NAME(c.user_type_id) IN ('datetime2','time','datetimeoffset') THEN '(' + CAST(c.scale AS varchar(10)) + ')' ELSE '' END, \
                     c.is_nullable, OBJECT_DEFINITION(c.default_object_id), c.is_identity, CAST(ep.value AS nvarchar(max)) \
                     FROM sys.columns c LEFT JOIN sys.extended_properties ep ON ep.major_id = c.object_id AND ep.minor_id = c.column_id AND ep.name = 'MS_Description' \
                     WHERE c.object_id = OBJECT_ID({obj}) ORDER BY c.column_id"
                ))
                .await?
            {
                t.columns.push(ColumnInfo {
                    name: cell_str(&r[0]),
                    data_type: cell_str(&r[1]),
                    nullable: cell_bool(&r[2]),
                    default_value: cell_opt(&r[3]).map(|x| strip_parens(&x)),
                    auto_increment: cell_bool(&r[4]),
                    comment: cell_opt(&r[5]),
                    ..Default::default()
                });
            }
            let rows = c
                .rows(&format!(
                    "SELECT i.name, i.is_unique, i.is_primary_key, c.name FROM sys.indexes i \
                     JOIN sys.index_columns ic ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
                     JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
                     WHERE i.object_id = OBJECT_ID({obj}) AND ic.is_included_column = 0 AND i.name IS NOT NULL ORDER BY i.name, ic.key_ordinal"
                ))
                .await?;
            t.indexes = group_indexes(rows.iter().map(|r| (cell_str(&r[0]), cell_bool(&r[1]), cell_bool(&r[2]), cell_str(&r[3]))).collect());
            if let Some(pk) = t.indexes.iter().find(|i| i.primary) {
                t.primary_key = pk.columns.clone();
            }
            let fks = c
                .rows(&format!(
                    "SELECT fk.name, pc.name, SCHEMA_NAME(rt.schema_id), rt.name, rc.name, fk.delete_referential_action_desc, fk.update_referential_action_desc \
                     FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
                     JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
                     JOIN sys.tables rt ON rt.object_id = fkc.referenced_object_id \
                     JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
                     WHERE fk.parent_object_id = OBJECT_ID({obj}) ORDER BY fk.name, fkc.constraint_column_id"
                ))
                .await?;
            t.foreign_keys = group_fks(
                fks.iter()
                    .map(|r| {
                        (
                            cell_str(&r[0]),
                            cell_str(&r[1]),
                            cell_str(&r[2]),
                            cell_str(&r[3]),
                            cell_str(&r[4]),
                            cell_opt(&r[5]).map(|x| x.replace('_', " ")),
                            cell_opt(&r[6]).map(|x| x.replace('_', " ")),
                        )
                    })
                    .collect(),
            );
            if let Ok(r) = c.rows(&format!("SELECT SUM(p.rows) FROM sys.partitions p WHERE p.object_id = OBJECT_ID({obj}) AND p.index_id IN (0, 1)")).await {
                t.row_estimate = r.first().and_then(|x| cell_i64(&x[0]));
            }
        }
        Dialect::Oracle => {
            for r in c
                .rows(&format!(
                    "SELECT c.column_name, c.data_type || CASE WHEN c.data_type IN ('VARCHAR2','NVARCHAR2','CHAR','NCHAR','RAW') THEN '(' || c.char_length || ')' \
                     WHEN c.data_type = 'NUMBER' AND c.data_precision IS NOT NULL THEN '(' || c.data_precision || ',' || c.data_scale || ')' ELSE '' END, \
                     c.nullable, c.data_default, c.identity_column, cc.comments \
                     FROM all_tab_columns c LEFT JOIN all_col_comments cc ON cc.owner = c.owner AND cc.table_name = c.table_name AND cc.column_name = c.column_name \
                     WHERE c.owner = {s} AND c.table_name = {n} ORDER BY c.column_id"
                ))
                .await?
            {
                t.columns.push(ColumnInfo {
                    name: cell_str(&r[0]),
                    data_type: cell_str(&r[1]),
                    nullable: cell_bool(&r[2]),
                    default_value: cell_opt(&r[3]).map(|x| x.trim().to_string()),
                    auto_increment: cell_bool(&r[4]),
                    comment: cell_opt(&r[5]),
                    ..Default::default()
                });
            }
            let pk = c
                .rows(&format!(
                    "SELECT cc.column_name FROM all_constraints c JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
                     WHERE c.owner = {s} AND c.table_name = {n} AND c.constraint_type = 'P' ORDER BY cc.position"
                ))
                .await?;
            t.primary_key = pk.iter().map(|r| cell_str(&r[0])).collect();
            let rows = c
                .rows(&format!(
                    "SELECT i.index_name, i.uniqueness, ic.column_name FROM all_indexes i JOIN all_ind_columns ic ON ic.index_owner = i.owner AND ic.index_name = i.index_name \
                     WHERE i.table_owner = {s} AND i.table_name = {n} ORDER BY i.index_name, ic.column_position"
                ))
                .await?;
            let pkc = t.primary_key.clone();
            let mut idx = group_indexes(rows.iter().map(|r| (cell_str(&r[0]), cell_str(&r[1]) == "UNIQUE", false, cell_str(&r[2]))).collect());
            for i in idx.iter_mut() {
                i.primary = !pkc.is_empty() && i.columns == pkc && i.unique;
            }
            t.indexes = idx;
            let fks = c
                .rows(&format!(
                    "SELECT c.constraint_name, cc.column_name, r.owner, r.table_name, rc.column_name, c.delete_rule \
                     FROM all_constraints c JOIN all_cons_columns cc ON cc.owner = c.owner AND cc.constraint_name = c.constraint_name \
                     JOIN all_constraints r ON r.owner = c.r_owner AND r.constraint_name = c.r_constraint_name \
                     JOIN all_cons_columns rc ON rc.owner = r.owner AND rc.constraint_name = r.constraint_name AND rc.position = cc.position \
                     WHERE c.owner = {s} AND c.table_name = {n} AND c.constraint_type = 'R' ORDER BY c.constraint_name, cc.position"
                ))
                .await?;
            t.foreign_keys = group_fks(
                fks.iter()
                    .map(|r| (cell_str(&r[0]), cell_str(&r[1]), cell_str(&r[2]), cell_str(&r[3]), cell_str(&r[4]), cell_opt(&r[5]), None))
                    .collect(),
            );
            if let Ok(r) = c.rows(&format!("SELECT num_rows FROM all_tables WHERE owner = {s} AND table_name = {n}")).await {
                t.row_estimate = r.first().and_then(|x| cell_i64(&x[0]));
            }
        }
    }
    for col in t.columns.iter_mut() {
        col.is_primary_key = t.primary_key.contains(&col.name);
    }
    if t.columns.is_empty() && matches!(t.kind, ObjectKind::Table | ObjectKind::View) {
        anyhow::bail!("Table {schema}.{name} not found");
    }
    Ok(t)
}

fn strip_parens(s: &str) -> String {
    let mut t = s.trim();
    while t.starts_with('(') && t.ends_with(')') && t.len() >= 2 {
        t = &t[1..t.len() - 1];
    }
    t.to_string()
}

/// Native DDL where the database offers it, generated DDL otherwise.
pub async fn ddl(c: &mut Conn, d: Dialect, schema: &str, name: &str, kind: ObjectKind) -> Result<String> {
    let s = lit(schema, d);
    let n = lit(name, d);
    let first = |rows: Vec<Vec<Value>>, col: usize| rows.first().and_then(|r| r.get(col)).map(cell_str).unwrap_or_default();
    match (d, kind) {
        (Dialect::Mysql, ObjectKind::Table) => {
            let r = c.rows(&format!("SHOW CREATE TABLE {}", qualified(schema, name, d))).await?;
            return Ok(first(r, 1) + ";");
        }
        (Dialect::Mysql, ObjectKind::View) => {
            let r = c.rows(&format!("SHOW CREATE VIEW {}", qualified(schema, name, d))).await?;
            return Ok(first(r, 1) + ";");
        }
        (Dialect::Mysql, ObjectKind::Procedure) | (Dialect::Mysql, ObjectKind::Function) => {
            let what = if kind == ObjectKind::Procedure { "PROCEDURE" } else { "FUNCTION" };
            let r = c.rows(&format!("SHOW CREATE {what} {}", qualified(schema, name, d))).await?;
            return Ok(format!("DELIMITER //\n{}//\nDELIMITER ;", first(r, 2)));
        }
        (Dialect::Sqlite, _) => {
            let q = quote_ident(schema, d);
            let rows = c.rows(&format!("SELECT sql FROM {q}.sqlite_master WHERE (name = {n} OR tbl_name = {n}) AND sql IS NOT NULL ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 ELSE 1 END")).await?;
            return Ok(rows.iter().map(|r| cell_str(&r[0]) + ";").collect::<Vec<_>>().join("\n"));
        }
        (Dialect::Postgres, ObjectKind::View) | (Dialect::Postgres, ObjectKind::MaterializedView) => {
            let reg = lit(&qualified(schema, name, d), d);
            let r = c.rows(&format!("SELECT pg_get_viewdef({reg}::regclass, true)")).await?;
            let what = if kind == ObjectKind::View { "OR REPLACE VIEW" } else { "MATERIALIZED VIEW" };
            return Ok(format!("CREATE {what} {} AS\n{}", qualified(schema, name, d), first(r, 0)));
        }
        (Dialect::Postgres, ObjectKind::Function) | (Dialect::Postgres, ObjectKind::Procedure) => {
            let r = c
                .rows(&format!(
                    "SELECT pg_get_functiondef(p.oid) FROM pg_proc p JOIN pg_namespace ns ON ns.oid = p.pronamespace \
                     WHERE ns.nspname = {s} AND p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')' = {n}"
                ))
                .await?;
            return Ok(first(r, 0));
        }
        (Dialect::Postgres, ObjectKind::Sequence) => {
            return Ok(format!("CREATE SEQUENCE {};", qualified(schema, name, d)));
        }
        (Dialect::Mssql, k) if k != ObjectKind::Table => {
            let r = c.rows(&format!("SELECT OBJECT_DEFINITION(OBJECT_ID({}))", lit(&qualified(schema, name, d), d))).await?;
            return Ok(first(r, 0));
        }
        (Dialect::Oracle, k) => {
            let t = match k {
                ObjectKind::Table => "TABLE",
                ObjectKind::View => "VIEW",
                ObjectKind::MaterializedView => "MATERIALIZED_VIEW",
                ObjectKind::Function => "FUNCTION",
                ObjectKind::Procedure => "PROCEDURE",
                ObjectKind::Sequence => "SEQUENCE",
            };
            let r = c.rows(&format!("SELECT DBMS_METADATA.GET_DDL('{t}', {n}, {s}) FROM dual")).await.context("DBMS_METADATA.GET_DDL")?;
            return Ok(first(r, 0));
        }
        _ => {}
    }
    let t = describe(c, d, schema, name, kind).await?;
    let stmts = crate::sqlgen::create_table(&t, d, &crate::sqlgen::CreateOpts { to: d, schema, name, foreign_keys: true, indexes: true });
    let mut out = stmts.join(";\n") + ";";
    if d == Dialect::Postgres {
        if let Some(cm) = &t.comment {
            out.push_str(&format!("\nCOMMENT ON TABLE {} IS {};", qualified(schema, name, d), lit(cm, d)));
        }
        for col in &t.columns {
            if let Some(cm) = &col.comment {
                out.push_str(&format!("\nCOMMENT ON COLUMN {}.{} IS {};", qualified(schema, name, d), quote_ident(&col.name, d), lit(cm, d)));
            }
        }
    }
    Ok(out)
}
