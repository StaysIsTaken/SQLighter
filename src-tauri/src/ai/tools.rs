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
        name: "list_tables",
        description: "List tables and views of a schema (default: the schema/database selected in SQLighter). Use 'filter' to search table names (case-insensitive substring).",
        schema: conn_prop(json!({"type": "object", "properties": {"schema": {"type": "string"}, "filter": {"type": "string", "description": "Only tables whose name contains this text"}}, "required": []})),
    });
    out.push(ToolDef {
        name: "describe_table",
        description: "Columns (type, nullability, default), primary key, indexes and foreign keys of a table.",
        schema: conn_prop(json!({"type": "object", "properties": {"table": {"type": "string"}, "schema": {"type": "string"}}, "required": ["table"]})),
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
            let filter = arg(args, "filter").map(|f| f.to_lowercase().replace([' ', '_'], ""));
            let objs = metadata::objects(c, d, &schema).await?;
            let lines: Vec<String> = objs
                .iter()
                .filter(|o| matches!(o.kind, ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView))
                .filter(|o| filter.as_deref().is_none_or(|f| o.name.to_lowercase().replace([' ', '_'], "").contains(f)))
                .map(|o| format!("{} ({})", o.name, format!("{:?}", o.kind).to_lowercase().replace("materializedview", "materialized view")))
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
        "describe_table" => {
            let id = resolve_connection(state, scope, args)?;
            let s = state.session(&id).await?;
            let d = s.dialect();
            let mut g = s.meta().await?;
            let c = g.as_mut().unwrap();
            let table = arg(args, "table").context("parameter 'table' is required")?;
            let (schema, table) = match (arg(args, "schema"), table.split_once('.')) {
                (Some(sc), _) => (sc.to_string(), table.to_string()),
                (None, Some((a, b))) => (a.to_string(), b.to_string()),
                (None, None) => (resolve_schema(c, d, scope, None).await?, table.to_string()),
            };
            let t = metadata::describe(c, d, &schema, &table, ObjectKind::Table).await?;
            Ok(serde_json::to_string_pretty(&t)?)
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
    Ok(format!(
        "Database: {} — {}\nDialect: {:?}\nSchema: {} ({} tables; unqualified names refer to it)\n{heading}:\n{}",
        s.cfg.db_type.label(),
        s.server_version.lines().next().unwrap_or(""),
        d,
        schema,
        tables.len(),
        if out.is_empty() { "(no tables)\n".to_string() } else { out }
    ))
}
