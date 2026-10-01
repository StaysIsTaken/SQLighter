//! Tools that AI agents (built-in chat and MCP clients) may call. Access is limited by the
//! configured AiAccessLevel: "schema" allows metadata only, "read" additionally allows single
//! read-only statements that run inside a READ ONLY transaction which is always rolled back.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::db::{cell_str, metadata};
use crate::model::{AiAccessLevel, ObjectKind};
use crate::state::AppState;

#[derive(Clone, Debug)]
pub struct Scope {
    /// Fixed connection (built-in chat) or None = any connection (external MCP client).
    pub connection_id: Option<String>,
    pub access: AiAccessLevel,
    /// May request data-modifying SQL (each statement needs approval in the UI).
    pub allow_write: bool,
    /// May open SQL in a new editor tab.
    pub allow_open_editor: bool,
    pub label: String,
    /// Schema / database selected in SQLighter; default for unqualified names.
    pub schema: Option<String>,
}

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

pub fn definitions(scope: &Scope) -> Vec<ToolDef> {
    let fixed = scope.connection_id.is_some();
    let conn_prop = |mut v: Value| {
        if !fixed {
            v["properties"]["connection"] = json!({"type": "string", "description": "Connection name or id (see list_connections)"});
            let req = v["required"].as_array().cloned().unwrap_or_default();
            let mut r = vec![json!("connection")];
            r.extend(req);
            v["required"] = Value::Array(r);
        }
        v
    };
    let mut out = vec![];
    if scope.access == AiAccessLevel::None {
        return out;
    }
    if !fixed {
        out.push(ToolDef {
            name: "list_connections",
            description: "List the database connections configured in SQLighter (name, id, database type).",
            schema: json!({"type": "object", "properties": {}, "required": []}),
        });
    }
    out.push(ToolDef {
        name: "list_schemas",
        description: "List the schemas / databases of the connection and which one is selected (the default for unqualified names).",
        schema: conn_prop(json!({"type": "object", "properties": {}, "required": []})),
    });
    out.push(ToolDef {
        name: "list_tables",
        description: "List tables and views of a schema (default: the schema/database selected in SQLighter). Use 'filter' to search table names (case-insensitive substring).",
        schema: conn_prop(json!({"type": "object", "properties": {"schema": {"type": "string"}, "filter": {"type": "string", "description": "Only tables whose name contains this text"}}, "required": []})),
    });
    out.push(ToolDef {
        name: "list_objects",
        description: "List database objects of a schema by kind: tables, views, materialized views, functions, procedures, sequences (default: all kinds). Use 'filter' to search names.",
        schema: conn_prop(json!({"type": "object", "properties": {
            "schema": {"type": "string"},
            "kind": {"type": "string", "enum": ["table", "view", "materialized_view", "function", "procedure", "sequence"]},
            "filter": {"type": "string", "description": "Only objects whose name contains this text"}
        }, "required": []})),
    });
    out.push(ToolDef {
        name: "describe_table",
        description: "Columns (type, nullability, default, comment), primary key, indexes and foreign keys of a table or view. Pass 'tables' to describe several at once.",
        schema: conn_prop(json!({"type": "object", "properties": {
            "table": {"type": "string", "description": "Table name, optionally schema-qualified (schema.table)"},
            "tables": {"type": "array", "items": {"type": "string"}, "description": "Several tables at once (max 20)"},
            "schema": {"type": "string"}
        }, "required": []})),
    });
    out.push(ToolDef {
        name: "search_columns",
        description: "Find columns by name across all tables and views of a schema (case-insensitive substring, '_' and spaces ignored). Useful to find where a value like 'workflowid' or 'customer_id' is stored and which tables can be joined.",
        schema: conn_prop(json!({"type": "object", "properties": {
            "name": {"type": "string", "description": "Part of the column name"},
            "schema": {"type": "string"}
        }, "required": ["name"]})),
    });
    out.push(ToolDef {
        name: "list_relationships",
        description: "Foreign key relationships: for one table its outgoing references and the tables referencing it, otherwise all relationships of the schema. Shows the columns to JOIN on.",
        schema: conn_prop(json!({"type": "object", "properties": {"table": {"type": "string"}, "schema": {"type": "string"}}, "required": []})),
    });
    out.push(ToolDef {
        name: "get_ddl",
        description: "CREATE statement (DDL) of a table, view, materialized view, function, procedure or sequence, e.g. to read a view's query or a procedure's code.",
        schema: conn_prop(json!({"type": "object", "properties": {
            "name": {"type": "string", "description": "Object name, optionally schema-qualified"},
            "kind": {"type": "string", "enum": ["table", "view", "materialized_view", "function", "procedure", "sequence"], "description": "Detected automatically if omitted"},
            "schema": {"type": "string"}
        }, "required": ["name"]})),
    });
    if scope.access == AiAccessLevel::Read {
        out.push(ToolDef {
            name: "run_query",
            description: "Run ONE read-only SQL statement (SELECT/WITH/SHOW/EXPLAIN) and get up to 50 rows. Runs in a read-only transaction; data-modifying statements are rejected.",
            schema: conn_prop(json!({"type": "object", "properties": {"sql": {"type": "string"}}, "required": ["sql"]})),
        });
    }
    if scope.allow_write {
        out.push(ToolDef {
            name: "execute_sql",
            description: "Execute SQL that modifies data or schema. The user must approve every call in SQLighter; rejected calls return an error.",
            schema: conn_prop(json!({"type": "object", "properties": {"sql": {"type": "string"}}, "required": ["sql"]})),
        });
    }
    if scope.allow_open_editor {
        out.push(ToolDef {
            name: "open_in_editor",
            description: "Open SQL in a new SQLighter editor tab so the user can review and run it.",
            schema: conn_prop(json!({"type": "object", "properties": {"sql": {"type": "string"}, "title": {"type": "string"}}, "required": ["sql"]})),
        });
    }
    out
}

fn arg<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn resolve_connection(state: &AppState, scope: &Scope, args: &Value) -> Result<String> {
    if let Some(id) = &scope.connection_id {
        return Ok(id.clone());
    }
    let want = arg(args, "connection").context("parameter 'connection' is required")?;
    let tree = state.store.tree();
    tree.connections
        .iter()
        .find(|c| c.config.id == want || c.config.name.eq_ignore_ascii_case(want))
        .map(|c| c.config.id.clone())
        .with_context(|| format!("unknown connection '{want}'"))
}

fn kind_label(k: ObjectKind) -> &'static str {
    match k {
        ObjectKind::Table => "table",
        ObjectKind::View => "view",
        ObjectKind::MaterializedView => "materialized view",
        ObjectKind::Function => "function",
        ObjectKind::Procedure => "procedure",
        ObjectKind::Sequence => "sequence",
    }
}

fn parse_kind(s: &str) -> Result<ObjectKind> {
    Ok(match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").trim_end_matches('s') {
        "table" => ObjectKind::Table,
        "view" => ObjectKind::View,
        "materialized_view" | "matview" => ObjectKind::MaterializedView,
        "function" => ObjectKind::Function,
        "procedure" | "proc" => ObjectKind::Procedure,
        "sequence" => ObjectKind::Sequence,
        other => bail!("unknown object kind '{other}' (table, view, materialized_view, function, procedure, sequence)"),
    })
}

/// Search key: case-insensitive, ignoring '_' and spaces ("Workflow ID" finds "workflowid").
fn norm(s: &str) -> String {
    s.to_lowercase().replace([' ', '_'], "")
}

/// Splits "schema.name" unless a schema is given explicitly; falls back to the selected schema.
async fn split_name(c: &mut crate::db::Conn, d: crate::model::Dialect, scope: &Scope, schema: Option<&str>, name: &str) -> Result<(String, String)> {
    Ok(match (schema, name.split_once('.')) {
        (Some(sc), _) => (sc.to_string(), name.to_string()),
        (None, Some((a, b))) if !matches!(d, crate::model::Dialect::Postgres) || !name.contains('(') => (a.to_string(), b.to_string()),
        _ => (resolve_schema(c, d, scope, None).await?, name.to_string()),
    })
}

/// Finds an object by exact name, else case-insensitively (models often guess the case wrong).
async fn find_object(c: &mut crate::db::Conn, d: crate::model::Dialect, schema: &str, name: &str, kind: Option<ObjectKind>) -> Result<crate::model::DbObject> {
    let objs = metadata::objects(c, d, schema).await?;
    let fits = |o: &&crate::model::DbObject| kind.is_none_or(|k| o.kind == k);
    // PostgreSQL routines are listed with their argument types: "fn(integer)".
    let base = |n: &str| n.split('(').next().unwrap_or(n).to_string();
    let found = objs
        .iter()
        .filter(fits)
        .find(|o| o.name == name)
        .or_else(|| objs.iter().filter(fits).find(|o| o.name.eq_ignore_ascii_case(name)))
        .or_else(|| objs.iter().filter(fits).find(|o| base(&o.name).eq_ignore_ascii_case(name)));
    match found {
        Some(o) => Ok(o.clone()),
        None => {
            let similar: Vec<&str> = objs.iter().filter(fits).filter(|o| norm(&o.name).contains(&norm(name))).map(|o| o.name.as_str()).take(10).collect();
            if similar.is_empty() {
                bail!("'{name}' was not found in schema {schema}. Use list_tables / list_objects to see the available names.")
            }
            bail!("'{name}' was not found in schema {schema}. Similar names: {}", similar.join(", "))
        }
    }
}

/// Explicit argument, else the schema selected in SQLighter, else the connection's default.
async fn resolve_schema(c: &mut crate::db::Conn, d: crate::model::Dialect, scope: &Scope, explicit: Option<&str>) -> Result<String> {
    if let Some(x) = explicit.or(scope.schema.as_deref()).map(str::trim).filter(|x| !x.is_empty()) {
        return Ok(x.to_string());
    }
    let def = metadata::default_schema(c, d).await?;
    if !def.is_empty() {
        return Ok(def);
    }
    let all = metadata::schemas(c, d).await.unwrap_or_default();
    bail!("No schema/database is selected for this connection. Pass the 'schema' parameter. Available: {}", all.join(", "))
}

pub async fn call(state: &Arc<AppState>, scope: &Scope, name: &str, args: &Value) -> Result<String> {
    if !definitions(scope).iter().any(|d| d.name == name) {
        bail!("tool '{name}' is not available (access level: {:?})", scope.access);
    }
    match name {
        "list_connections" => {
            let tree = state.store.tree();
            let list: Vec<Value> = tree
                .connections
                .iter()
                .map(|c| json!({"name": c.config.name, "id": c.config.id, "type": c.config.db_type.label(), "database": c.config.database, "readOnly": c.config.read_only}))
                .collect();
            Ok(serde_json::to_string_pretty(&list)?)
        }
        "list_tables" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let schema = resolve_schema(c, d, scope, arg(args, "schema")).await?;
            let filter = arg(args, "filter").map(norm);
            let objs = metadata::objects(c, d, &schema).await?;
            let lines: Vec<String> = objs
                .iter()
                .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView))
                .filter(|o| filter.as_deref().is_none_or(|f| norm(&o.name).contains(f)))
                .map(|o| format!("{} ({})", o.name, kind_label(o.kind)))
                .collect();
            if lines.is_empty() {
                return Ok(match filter {
                    Some(_) => format!("Schema {schema}: no table matches the filter. Try a shorter or English/German variant, or list without filter."),
                    None => format!("Schema {schema} contains no tables."),
                });
            }
            if lines.len() > 200 {
                // Keep large schemas short: names only, comma separated.
                let names: Vec<&str> = lines.iter().map(|l| l.split(" (").next().unwrap_or(l)).collect();
                return Ok(format!("Schema {schema} ({} objects, use 'filter' to narrow down):\n{}", names.len(), names.join(", ")));
            }
            Ok(format!("Schema {schema} ({} objects):\n{}", lines.len(), lines.join("\n")))
        }
        "list_schemas" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let all = metadata::schemas(c, d).await?;
            let current = resolve_schema(c, d, scope, None).await.unwrap_or_default();
            let lines: Vec<String> = all.iter().map(|x| if *x == current { format!("{x} (selected)") } else { x.clone() }).collect();
            Ok(format!("{} schema(s):\n{}", lines.len(), lines.join("\n")))
        }
        "list_objects" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let schema = resolve_schema(c, d, scope, arg(args, "schema")).await?;
            let kind = arg(args, "kind").map(parse_kind).transpose()?;
            let filter = arg(args, "filter").map(norm);
            let objs = metadata::objects(c, d, &schema).await?;
            let lines: Vec<String> = objs
                .iter()
                .filter(|o| kind.is_none_or(|k| o.kind == k))
                .filter(|o| filter.as_deref().is_none_or(|f| norm(&o.name).contains(f)))
                .map(|o| match &o.comment {
                    Some(cm) => format!("{} ({}) - {}", o.name, kind_label(o.kind), cm),
                    None => format!("{} ({})", o.name, kind_label(o.kind)),
                })
                .collect();
            if lines.is_empty() {
                return Ok(format!("Schema {schema}: no matching objects."));
            }
            let shown = lines.len().min(500);
            let more = if lines.len() > shown { format!("\n… {} more, use 'filter' or 'kind' to narrow down", lines.len() - shown) } else { String::new() };
            Ok(format!("Schema {schema} ({} objects):\n{}{more}", lines.len(), lines[..shown].join("\n")))
        }
        "describe_table" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let mut names: Vec<String> = args.get("tables").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            if let Some(t) = arg(args, "table") {
                names.insert(0, t.to_string());
            }
            if names.is_empty() {
                bail!("parameter 'table' (or 'tables') is required");
            }
            if names.len() > 20 {
                bail!("at most 20 tables per call");
            }
            let mut out = vec![];
            for name in &names {
                let (schema, table) = split_name(c, d, scope, arg(args, "schema"), name).await?;
                let r = async {
                    let mut t = metadata::describe(c, d, &schema, &table, ObjectKind::Table).await?;
                    if t.columns.is_empty() {
                        // Wrong case or a typo: look the name up and retry.
                        let o = find_object(c, d, &schema, &table, None).await?;
                        t = metadata::describe(c, d, &schema, &o.name, o.kind).await?;
                    }
                    anyhow::Ok(t)
                }
                .await;
                out.push(match r {
                    Ok(t) => serde_json::to_string_pretty(&t)?,
                    Err(e) => format!("{name}: ERROR: {}", crate::error::chain_message(&e)),
                });
            }
            Ok(out.join("\n\n"))
        }
        "search_columns" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let schema = resolve_schema(c, d, scope, arg(args, "schema")).await?;
            let want = norm(arg(args, "name").context("parameter 'name' is required")?);
            let hits: Vec<String> = metadata::schema_columns(c, d, &schema)
                .await?
                .into_iter()
                .filter(|(_, col, _)| norm(col).contains(&want))
                .map(|(t, col, ty)| format!("{t}.{col} {ty}"))
                .collect();
            if hits.is_empty() {
                return Ok(format!("Schema {schema}: no column name contains '{want}'. Try a shorter part of the name or another schema (list_schemas)."));
            }
            let shown = hits.len().min(300);
            let more = if hits.len() > shown { format!("\n… {} more, use a more specific name", hits.len() - shown) } else { String::new() };
            Ok(format!("Schema {schema}, {} column(s) (table.column type):\n{}{more}", hits.len(), hits[..shown].join("\n")))
        }
        "list_relationships" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let (schema, table) = match arg(args, "table") {
                Some(t) => {
                    let (sc, t) = split_name(c, d, scope, arg(args, "schema"), t).await?;
                    (sc, Some(t))
                }
                None => (resolve_schema(c, d, scope, arg(args, "schema")).await?, None),
            };
            let edges = metadata::schema_foreign_keys(c, d, &schema).await?;
            let fmt = |e: &metadata::ForeignKeyEdge| {
                let q = |sc: &str, t: &str| if sc == schema { t.to_string() } else { format!("{sc}.{t}") };
                let refs = if e.fk.ref_columns.iter().all(|x| x.is_empty()) { "(primary key)".to_string() } else { e.fk.ref_columns.join(", ") };
                format!("{}({}) -> {}({})", q(&e.schema, &e.table), e.fk.columns.join(", "), q(&e.fk.ref_schema, &e.fk.ref_table), refs)
            };
            match table {
                Some(t) => {
                    let is = |sc: &str, x: &str| sc == schema && x.eq_ignore_ascii_case(&t);
                    let outgoing: Vec<String> = edges.iter().filter(|e| is(&e.schema, &e.table)).map(fmt).collect();
                    let incoming: Vec<String> = edges.iter().filter(|e| is(&e.fk.ref_schema, &e.fk.ref_table)).map(fmt).collect();
                    if outgoing.is_empty() && incoming.is_empty() {
                        return Ok(format!(
                            "{t} has no declared foreign keys and is not referenced by any. Use search_columns to find columns with matching names that may still be joinable."
                        ));
                    }
                    let list = |v: &[String]| if v.is_empty() { "(none)".to_string() } else { v.join("\n") };
                    Ok(format!("{t} references:\n{}\n\nReferenced by:\n{}", list(&outgoing), list(&incoming)))
                }
                None => {
                    if edges.is_empty() {
                        return Ok(format!("Schema {schema} has no declared foreign keys. Use search_columns to find columns with matching names."));
                    }
                    let lines: Vec<String> = edges.iter().map(fmt).collect();
                    let shown = lines.len().min(500);
                    let more = if lines.len() > shown { format!("\n… {} more, pass 'table' to narrow down", lines.len() - shown) } else { String::new() };
                    Ok(format!("Schema {schema}, {} foreign key(s) (table(columns) -> referenced table(columns)):\n{}{more}", lines.len(), lines[..shown].join("\n")))
                }
            }
        }
        "get_ddl" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let name = arg(args, "name").context("parameter 'name' is required")?;
            let kind = arg(args, "kind").map(parse_kind).transpose()?;
            let (schema, name) = split_name(c, d, scope, arg(args, "schema"), name).await?;
            let o = find_object(c, d, &schema, &name, kind).await?;
            let ddl = metadata::ddl(c, d, &schema, &o.name, o.kind).await?;
            if ddl.trim().is_empty() {
                bail!("the database returned no definition for {} {} (missing privileges?)", kind_label(o.kind), o.name);
            }
            // Keep huge procedures from flooding the context.
            const MAX: usize = 30_000;
            Ok(if ddl.len() > MAX {
                let cut = (0..=MAX).rev().find(|i| ddl.is_char_boundary(*i)).unwrap_or(0);
                format!("{}\n… (truncated, {} characters in total)", &ddl[..cut], ddl.len())
            } else {
                ddl
            })
        }
        "run_query" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let sql = arg(args, "sql").context("parameter 'sql' is required")?;
            let r = s.read_only_query(sql, 50, scope.schema.as_deref()).await?;
            Ok(format_rows(&r))
        }
        "execute_sql" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let sql = arg(args, "sql").context("parameter 'sql' is required")?;
            if s.cfg.read_only {
                bail!("the connection is read-only");
            }
            if !state.request_approval(&scope.label, &s.cfg.name, sql).await {
                bail!("the user rejected this statement");
            }
            let es = crate::db::session::ExecSettings { confirm_destructive: false, max_rows: 50, timeout: Some(std::time::Duration::from_secs(300)) };
            let r = s.execute(sql, &crate::model::ExecuteOptions { max_rows: Some(50), confirmed: true, schema: None }, &es).await?;
            let mut out = vec![];
            for x in r.results {
                if let Some(e) = x.error {
                    out.push(format!("ERROR: {e}"));
                } else if x.is_result_set {
                    out.push(format_rows(&x));
                } else {
                    out.push(format!("OK, {} row(s) affected", x.affected_rows.map(|n| n.to_string()).unwrap_or("?".into())));
                }
            }
            Ok(out.join("\n\n"))
        }
        "open_in_editor" => {
            use tauri::Emitter;
            let sql = arg(args, "sql").context("parameter 'sql' is required")?;
            let conn = resolve_connection(state, scope, args).ok();
            state.app.emit("open-sql", json!({"sql": sql, "connectionId": conn, "title": arg(args, "title")}))?;
            Ok("Opened in a new SQLighter editor tab. The user will review and run it.".into())
        }
        _ => bail!("unknown tool {name}"),
    }
}

fn format_rows(r: &crate::model::QueryResult) -> String {
    let cols: Vec<&str> = r.columns.iter().map(|c| c.name.as_str()).collect();
    let mut out = String::new();
    out.push_str(&cols.join(" | "));
    out.push('\n');
    for row in &r.rows {
        let cells: Vec<String> = row
            .iter()
            .map(|v| {
                let s = if v.is_null() { "NULL".to_string() } else { cell_str(v) };
                if s.chars().count() > 200 {
                    s.chars().take(200).collect::<String>() + "…"
                } else {
                    s
                }
            })
            .collect();
        out.push_str(&cells.join(" | "));
        out.push('\n');
    }
    out.push_str(&format!("({} row(s){})", r.rows.len(), if r.truncated { ", truncated" } else { "" }));
    out
}

/// Compact schema description for the system prompt, at most about `budget` characters:
/// all tables with columns if that fits, otherwise only the table names.
pub async fn schema_context(state: &Arc<AppState>, connection_id: &str, schema: Option<&str>, budget: usize) -> Result<String> {
    let s = state.session(connection_id).await?;
    let d = s.dialect();
    let mut g = s.meta().await?;
    let c = g.as_mut().unwrap();
    let schema = match schema.filter(|x| !x.is_empty()) {
        Some(x) => x.to_string(),
        None => metadata::default_schema(c, d).await.unwrap_or_default(),
    };
    let cols = metadata::schema_columns(c, d, &schema).await.unwrap_or_default();
    let mut tables: Vec<(String, Vec<String>)> = vec![];
    for (t, col, ty) in cols {
        match tables.last_mut() {
            Some((name, parts)) if *name == t => parts.push(format!("{col} {ty}")),
            _ => tables.push((t, vec![format!("{col} {ty}")])),
        }
    }
    let full: String = tables.iter().map(|(t, p)| format!("{t}({})\n", p.join(", "))).collect();
    // Declared foreign keys tell the model how to JOIN without looking every table up.
    let rels: String = metadata::schema_foreign_keys(c, d, &schema)
        .await
        .unwrap_or_default()
        .iter()
        .filter(|e| e.schema == schema && e.fk.ref_schema == schema)
        .map(|e| format!("{}({}) -> {}({})\n", e.table, e.fk.columns.join(", "), e.fk.ref_table, e.fk.ref_columns.join(", ")))
        .collect();
    let (heading, out) = if full.len() <= budget {
        ("Tables (name(column type, ...))", full)
    } else {
        let mut names = String::new();
        for (i, (t, _)) in tables.iter().enumerate() {
            if names.len() + t.len() + 2 > budget {
                names.push_str(&format!("… and {} more (use list_tables with a filter)", tables.len() - i));
                break;
            }
            if i > 0 {
                names.push_str(", ");
            }
            names.push_str(t);
        }
        names.push('\n');
        ("Tables (names only - the schema is large; use describe_table for columns)", names)
    };
    let rels = if !rels.is_empty() && out.len() + rels.len() <= budget {
        format!("Foreign keys (table(columns) -> referenced table(columns)):\n{rels}")
    } else if !rels.is_empty() {
        "Foreign keys: use list_relationships.\n".to_string()
    } else {
        String::new()
    };
    Ok(format!(
        "Database: {} — {}\nDialect: {:?}\nSchema: {} ({} tables; unqualified names refer to it)\n{heading}:\n{}{rels}",
        s.cfg.db_type.label(),
        s.server_version.lines().next().unwrap_or(""),
        d,
        schema,
        tables.len(),
        if out.is_empty() { "(no tables)\n".to_string() } else { out }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(access: AiAccessLevel) -> Scope {
        Scope { connection_id: Some("c".into()), access, allow_write: false, allow_open_editor: false, label: String::new(), schema: None }
    }

    #[test]
    fn schema_tools_need_schema_access_only() {
        let names: Vec<&str> = definitions(&scope(AiAccessLevel::Schema)).iter().map(|d| d.name).collect();
        for t in ["list_schemas", "list_tables", "list_objects", "describe_table", "search_columns", "list_relationships", "get_ddl"] {
            assert!(names.contains(&t), "{t} missing in {names:?}");
        }
        assert!(!names.contains(&"run_query"));
        assert!(definitions(&scope(AiAccessLevel::None)).is_empty());
    }

    #[test]
    fn kinds_and_names_are_forgiving() {
        assert_eq!(parse_kind("Views").unwrap(), ObjectKind::View);
        assert_eq!(parse_kind("materialized view").unwrap(), ObjectKind::MaterializedView);
        assert_eq!(parse_kind("procedure").unwrap(), ObjectKind::Procedure);
        assert!(parse_kind("index").is_err());
        assert_eq!(norm("Workflow_ID"), norm("workflowid"));
    }
}
