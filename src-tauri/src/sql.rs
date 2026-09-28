//! Dialect-aware SQL helpers (Rust port of src/shared/sql.ts): statement splitting, identifier
//! quoting, literals and statement classification.

use serde_json::Value;

use crate::model::Dialect;

/// Splits a script into statements. Handles quotes, comments, PostgreSQL dollar quoting, MySQL
/// DELIMITER, SQL Server GO, Oracle "/" terminators and BEGIN...END blocks.
pub fn split_statements(sql: &str, dialect: Dialect) -> Vec<String> {
    let s: Vec<char> = sql.chars().collect();
    let n = s.len();
    let mut out = Vec::new();
    let mut delimiter: Vec<char> = vec![';'];
    let mut i = 0;
    let mut start = 0;
    let mut depth: i32 = 0;
    let mut last_word = String::new();

    let push = |out: &mut Vec<String>, from: usize, to: usize| {
        let text: String = s[from..to].iter().collect();
        let t = text.trim();
        if !t.is_empty() && !strip_comments(t).trim().is_empty() {
            out.push(t.to_string());
        }
    };
    let at_line_start = |pos: usize| -> bool {
        let mut p = pos as isize - 1;
        while p >= 0 && (s[p as usize] == ' ' || s[p as usize] == '\t') {
            p -= 1;
        }
        p < 0 || s[p as usize] == '\n' || s[p as usize] == '\r'
    };
    let starts_with_ci = |pos: usize, word: &str| -> bool {
        let w: Vec<char> = word.chars().collect();
        pos + w.len() <= n && s[pos..pos + w.len()].iter().zip(w.iter()).all(|(a, b)| a.eq_ignore_ascii_case(b))
    };

    while i < n {
        let c = s[i];
        let c2 = if i + 1 < n { s[i + 1] } else { '\0' };

        if dialect == Dialect::Mysql && (c == 'd' || c == 'D') && at_line_start(i) && starts_with_ci(i, "delimiter") && i + 9 < n && s[i + 9].is_whitespace() {
            push(&mut out, start, i);
            let mut e = i;
            while e < n && s[e] != '\n' {
                e += 1;
            }
            let d: String = s[i + 9..e].iter().collect();
            let d = d.trim();
            if !d.is_empty() {
                delimiter = d.chars().collect();
            }
            i = (e + 1).min(n);
            start = i;
            depth = 0;
            continue;
        }
        if dialect == Dialect::Mssql && (c == 'g' || c == 'G') && (c2 == 'o' || c2 == 'O') && at_line_start(i) {
            let mut e = i + 2;
            while e < n && (s[e] == ' ' || s[e] == '\t') {
                e += 1;
            }
            if e >= n || s[e] == '\n' || s[e] == '\r' {
                push(&mut out, start, i);
                i = (e + 1).min(n);
                start = i;
                continue;
            }
        }
        if c == '-' && c2 == '-' || (c == '#' && dialect == Dialect::Mysql) {
            while i < n && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && c2 == '*' {
            i += 2;
            while i + 1 < n && !(s[i] == '*' && s[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if c == '\'' || c == '"' || (c == '`' && dialect == Dialect::Mysql) {
            i = skip_quoted(&s, i, c, dialect == Dialect::Mysql && c != '`');
            continue;
        }
        if c == '[' && dialect == Dialect::Mssql {
            while i < n && s[i] != ']' {
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        if c == '$' && dialect == Dialect::Postgres {
            let mut j = i + 1;
            while j < n && (s[j].is_ascii_alphanumeric() || s[j] == '_') {
                j += 1;
            }
            if j < n && s[j] == '$' && (j == i + 1 || !s[i + 1].is_ascii_digit()) {
                let tag: Vec<char> = s[i..=j].to_vec();
                let mut k = j + 1;
                let mut found = n;
                while k + tag.len() <= n {
                    if s[k..k + tag.len()] == tag[..] {
                        found = k + tag.len();
                        break;
                    }
                    k += 1;
                }
                i = found;
                continue;
            }
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i + 1;
            while j < n && (s[j].is_ascii_alphanumeric() || s[j] == '_' || s[j] == '$' || s[j] == '#') {
                j += 1;
            }
            let word: String = s[i..j].iter().collect::<String>().to_ascii_uppercase();
            if delimiter == [';'] && dialect != Dialect::Mssql && dialect != Dialect::Postgres {
                let next_word = {
                    let mut k = j;
                    while k < n && s[k].is_whitespace() {
                        k += 1;
                    }
                    let mut e = k;
                    while e < n && (s[e].is_ascii_alphanumeric() || s[e] == '_' || s[e] == ';') {
                        e += 1;
                    }
                    s[k..e].iter().collect::<String>().to_ascii_uppercase()
                };
                if word == "BEGIN" || word == "CASE" {
                    let is_tx = word == "BEGIN"
                        && (next_word.is_empty()
                            || next_word.starts_with(';')
                            || ["TRANSACTION", "WORK", "DEFERRED", "IMMEDIATE", "EXCLUSIVE", "TRAN"].contains(&next_word.as_str()));
                    if !is_tx {
                        depth += 1;
                    }
                } else if word == "END" && depth > 0 {
                    let nw = next_word.trim_end_matches(';');
                    if !["IF", "LOOP", "CASE", "WHILE", "REPEAT"].contains(&nw) {
                        depth -= 1;
                    }
                }
                if dialect == Dialect::Oracle && word == "DECLARE" && last_word.is_empty() {
                    depth += 1;
                }
            }
            last_word = word;
            i = j;
            continue;
        }
        if dialect == Dialect::Oracle && c == '/' && at_line_start(i) {
            let mut e = i + 1;
            while e < n && (s[e] == ' ' || s[e] == '\t') {
                e += 1;
            }
            if e >= n || s[e] == '\n' || s[e] == '\r' {
                push(&mut out, start, i);
                i = e;
                start = i;
                depth = 0;
                last_word.clear();
                continue;
            }
        }
        if depth <= 0 && i + delimiter.len() <= n && s[i..i + delimiter.len()] == delimiter[..] {
            push(&mut out, start, i);
            i += delimiter.len();
            start = i;
            depth = 0;
            last_word.clear();
            continue;
        }
        i += 1;
    }
    push(&mut out, start, n);
    out
}

fn skip_quoted(s: &[char], i: usize, q: char, backslash: bool) -> usize {
    let mut j = i + 1;
    while j < s.len() {
        if backslash && s[j] == '\\' {
            j += 2;
            continue;
        }
        if s[j] == q {
            if j + 1 < s.len() && s[j + 1] == q {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    s.len()
}

pub fn strip_comments(sql: &str) -> String {
    let s: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let c2 = s.get(i + 1).copied().unwrap_or('\0');
        if c == '-' && c2 == '-' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && c2 == '*' {
            i += 2;
            while i + 1 < s.len() && !(s[i] == '*' && s[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(s.len());
            out.push(' ');
            continue;
        }
        if c == '\'' || c == '"' || c == '`' {
            let e = skip_quoted(&s, i, c, false);
            out.extend(&s[i..e]);
            i = e;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn strip_strings(sql: &str) -> String {
    let s: Vec<char> = sql.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == '\'' || c == '"' || c == '`' {
            let e = skip_quoted(&s, i, c, false);
            out.push(c);
            out.push(c);
            i = e;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

pub fn leading_keyword(stmt: &str) -> String {
    let s = strip_comments(stmt);
    let t = s.trim_start().trim_start_matches('(').trim_start();
    t.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase()
}

fn has_word(s: &str, words: &[&str]) -> bool {
    let lower = s.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for w in words {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(w) {
            let p = from + pos;
            let before = p == 0 || !(bytes[p - 1].is_ascii_alphanumeric() || bytes[p - 1] == b'_');
            let after_i = p + w.len();
            let after = after_i >= bytes.len() || !(bytes[after_i].is_ascii_alphanumeric() || bytes[after_i] == b'_');
            if before && after {
                return true;
            }
            from = p + w.len();
        }
    }
    false
}

/// Conservative read-only check (first line of defence; the database-side READ ONLY transaction
/// is the real enforcement for AI agents / MCP).
pub fn is_read_only(stmt: &str) -> bool {
    let s = strip_comments(stmt);
    let s = s.trim().trim_end_matches(';').trim();
    let body = strip_strings(s);
    if body.contains(';') {
        return false;
    }
    let kw = leading_keyword(s);
    match kw.as_str() {
        "SELECT" | "WITH" | "VALUES" | "TABLE" => {
            !has_word(&body, &["insert", "update", "delete", "merge", "create", "alter", "drop", "truncate", "grant", "revoke", "into", "call", "exec", "execute", "lock"])
        }
        "SHOW" | "DESCRIBE" | "DESC" => true,
        "EXPLAIN" => !has_word(&body, &["analyze", "analyse"]),
        "PRAGMA" => !body.contains('='),
        _ => false,
    }
}

/// Functions that reach outside the database or affect other sessions (server files, network,
/// other connections, locks, configuration). Blocked for AI agents / MCP clients even in
/// read-only queries.
fn agent_blocked_functions(d: Dialect) -> &'static [&'static str] {
    match d {
        Dialect::Postgres => &[
            "pg_read_file", "pg_read_binary_file", "pg_ls_dir", "pg_stat_file", "pg_ls_logdir", "pg_ls_waldir", "pg_ls_tmpdir",
            "pg_ls_archive_statusdir", "pg_file_write", "pg_file_rename", "pg_file_unlink", "lo_import", "lo_export", "lo_put",
            "lo_create", "lo_creat", "lo_unlink", "lo_from_bytea", "dblink", "dblink_exec", "dblink_connect", "dblink_connect_u",
            "dblink_send_query", "pg_terminate_backend", "pg_cancel_backend", "pg_reload_conf", "pg_rotate_logfile", "pg_promote",
            "pg_switch_wal", "pg_create_restore_point", "pg_logical_emit_message", "set_config", "pg_advisory_lock",
            "pg_advisory_lock_shared", "pg_advisory_xact_lock", "pg_advisory_xact_lock_shared", "pg_try_advisory_lock",
            "pg_try_advisory_xact_lock", "pg_notify",
        ],
        Dialect::Mysql => &[
            "load_file", "get_lock", "release_lock", "release_all_locks", "benchmark", "sys_exec", "sys_eval", "master_pos_wait",
            "source_pos_wait", "outfile", "dumpfile",
        ],
        Dialect::Sqlite => &["load_extension", "readfile", "writefile", "edit", "fts3_tokenizer"],
        Dialect::Oracle => &["httpuritype"],
        Dialect::Mssql => &["openrowset", "opendatasource", "openquery"],
    }
}

/// SQL Server runs everything sent in one batch, with or without semicolons, and has no
/// read-only transactions; statements that are not undone by a rollback are blocked.
const MSSQL_BLOCKED_WORDS: &[&str] = &[
    "kill", "shutdown", "dbcc", "backup", "restore", "checkpoint", "reconfigure", "waitfor", "deny", "begin", "save", "use",
    "set", "declare", "bulk", "open", "fetch", "close", "deallocate", "raiserror", "throw", "disable", "enable", "revert",
    "setuser", "go",
];

/// Read-only PRAGMAs SQLite agents may use.
const SQLITE_AGENT_PRAGMAS: &[&str] = &[
    "table_info", "table_xinfo", "table_list", "index_list", "index_info", "index_xinfo", "foreign_key_list", "database_list",
    "collation_list", "function_list", "module_list", "pragma_list",
];

/// Reduces a statement to the code the database will execute, the way `d` tokenizes it: string
/// contents are removed, identifier contents are kept (so `"pg_read_file"(...)` is still seen),
/// comments are removed. Constructs that different server settings could tokenize differently
/// are rejected, so the result is exactly what the server runs.
fn agent_code(stmt: &str, d: Dialect) -> std::result::Result<String, String> {
    let s: Vec<char> = stmt.chars().collect();
    let n = s.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    while i < n {
        let c = s[i];
        let c2 = s.get(i + 1).copied().unwrap_or('\0');
        let c3 = s.get(i + 2).copied().unwrap_or('\0');
        // Comments
        if c == '/' && c2 == '*' {
            if c3 == '!' || (c3.eq_ignore_ascii_case(&'m') && s.get(i + 3) == Some(&'!')) {
                return Err("MySQL executable comments (/*! ... */) are not allowed".into());
            }
            let mut j = i + 2;
            while j + 1 < n && !(s[j] == '*' && s[j + 1] == '/') {
                j += 1;
            }
            i = (j + 2).min(n);
            out.push(' ');
            continue;
        }
        if c == '-' && c2 == '-' {
            // MySQL: "--" starts a comment only when followed by whitespace; "1--1" is code.
            if d == Dialect::Mysql && !(c3 == '\0' || c3.is_whitespace()) {
                return Err("'--' must be followed by a space".into());
            }
            while i < n && s[i] != '\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        if c == '#' && d == Dialect::Mysql {
            while i < n && s[i] != '\n' {
                i += 1;
            }
            out.push(' ');
            continue;
        }
        // PostgreSQL dollar quoting: $tag$ ... $tag$
        if c == '$' && d == Dialect::Postgres && !(i > 0 && is_ident_char(s[i - 1])) {
            let mut j = i + 1;
            while j < n && (s[j].is_alphanumeric() || s[j] == '_') && !(j == i + 1 && s[j].is_ascii_digit()) {
                j += 1;
            }
            if j < n && s[j] == '$' {
                let tag: String = s[i..=j].iter().collect();
                let body_start = j + 1;
                let rest: String = s[body_start..].iter().collect();
                let end = rest.find(&tag).map(|p| body_start + rest[..p].chars().count()).unwrap_or(n);
                let body: String = s[body_start..end.min(n)].iter().collect();
                out.push(' ');
                out.push_str(&body); // treated as code: dollar strings often hold function bodies
                out.push(' ');
                i = (end + tag.chars().count()).min(n);
                continue;
            }
        }
        // Oracle alternative quoting: q'[...]', nq'{...}'
        if c == '\'' && d == Dialect::Oracle && i > 0 && s[i - 1].eq_ignore_ascii_case(&'q') && !(i > 1 && is_ident_char(s[i - 2]) && !s[i - 2].eq_ignore_ascii_case(&'n')) {
            let open = c2;
            let close = match open {
                '[' => ']',
                '(' => ')',
                '{' => '}',
                '<' => '>',
                x => x,
            };
            let mut j = i + 2;
            while j + 1 < n && !(s[j] == close && s[j + 1] == '\'') {
                j += 1;
            }
            out.push_str("''");
            i = (j + 2).min(n);
            continue;
        }
        let ident_quote = match (c, d) {
            ('"', Dialect::Mysql) => false,
            ('"', _) | ('`', Dialect::Mysql) | ('`', Dialect::Sqlite) => true,
            ('[', Dialect::Mssql) | ('[', Dialect::Sqlite) => true,
            _ => false,
        };
        if c == '\'' || c == '"' || c == '`' && matches!(d, Dialect::Mysql | Dialect::Sqlite) || c == '[' && ident_quote {
            let close = if c == '[' { ']' } else { c };
            let mut j = i + 1;
            while j < n {
                if s[j] == close {
                    if j + 1 < n && s[j + 1] == close {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            let inner: String = s[(i + 1).min(n)..j.min(n)].iter().collect();
            // MySQL (depending on NO_BACKSLASH_ESCAPES / ANSI_QUOTES) and PostgreSQL (E'' strings,
            // standard_conforming_strings) may treat a backslash as an escape: where the text ends
            // would then depend on server settings.
            if matches!(d, Dialect::Mysql | Dialect::Postgres) && inner.contains('\\') {
                return Err("backslashes inside quoted text are not allowed".into());
            }
            // MySQL "..." is an identifier under ANSI_QUOTES: its content is checked as code too.
            if ident_quote || (c == '"' && d == Dialect::Mysql) {
                out.push(' ');
                out.push_str(&inner);
                out.push(' ');
            } else {
                out.push_str("''");
            }
            i = (j + 1).min(n);
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// Strict check for SQL from AI agents and MCP clients. Their SQL is untrusted (prompt injection
/// through data the model reads), so unlike [`is_read_only`] this tokenizes like the target
/// database and also blocks functions that reach outside the database.
pub fn check_agent_query(stmt: &str, d: Dialect) -> std::result::Result<(), String> {
    let code = agent_code(stmt, d)?;
    let code = code.trim().trim_end_matches(|c: char| c == ';' || c.is_whitespace()).to_string();
    if code.contains(';') {
        return Err("only a single statement is allowed".into());
    }
    let kw: String = code.trim_start_matches(|c: char| c.is_whitespace() || c == '(').chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    let words: Vec<String> = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let has = |list: &[&str]| words.iter().find(|w| list.contains(&w.as_str())).cloned();
    match kw.as_str() {
        "SELECT" | "WITH" | "VALUES" | "TABLE" => {}
        "SHOW" | "DESCRIBE" | "DESC" if d != Dialect::Sqlite => {}
        "EXPLAIN" => {}
        "PRAGMA" if d == Dialect::Sqlite => {
            // PRAGMA [schema.]name[(arg)] - only a few informational pragmas, never "= value".
            let start = code.to_ascii_uppercase().find("PRAGMA").map(|p| p + 6).unwrap_or(0);
            let head = code[start..].split(['(', '=']).next().unwrap_or("").trim();
            let name = head.rsplit('.').next().unwrap_or(head).trim().to_ascii_lowercase();
            if !SQLITE_AGENT_PRAGMAS.contains(&name.as_str()) || code.contains('=') {
                return Err(format!("PRAGMA {name} is not allowed"));
            }
            return Ok(());
        }
        _ => return Err("only SELECT, WITH, SHOW, DESCRIBE and EXPLAIN are allowed".into()),
    }
    const MODIFYING: &[&str] = &[
        "insert", "update", "delete", "merge", "create", "alter", "drop", "truncate", "grant", "revoke", "into", "call", "exec",
        "execute", "lock", "commit", "rollback", "savepoint", "analyze", "analyse", "copy", "attach", "detach", "vacuum",
    ];
    if let Some(w) = has(MODIFYING) {
        return Err(format!("'{w}' is not allowed in a read-only query"));
    }
    if let Some(w) = has(agent_blocked_functions(d)) {
        return Err(format!("'{w}' is not allowed for AI agents"));
    }
    if d == Dialect::Mssql {
        if let Some(w) = has(MSSQL_BLOCKED_WORDS) {
            return Err(format!("'{w}' is not allowed in a read-only query"));
        }
        if let Some(w) = words.iter().find(|w| w.starts_with("xp_") || w.starts_with("sp_")) {
            return Err(format!("'{w}' is not allowed for AI agents"));
        }
    }
    if d == Dialect::Oracle {
        if let Some(w) = words.iter().find(|w| w.starts_with("dbms_") || w.starts_with("utl_")) {
            return Err(format!("'{w}' is not allowed for AI agents"));
        }
    }
    Ok(())
}

pub struct Danger {
    pub destructive: bool,
    pub modifies: bool,
    pub reason: Option<&'static str>,
}

pub fn analyze(stmt: &str) -> Danger {
    let body = strip_strings(&strip_comments(stmt));
    let kw = leading_keyword(&body);
    let d = |r: &'static str| Danger { destructive: true, modifies: true, reason: Some(r) };
    match kw.as_str() {
        "" => Danger { destructive: false, modifies: false, reason: None },
        "DROP" => d("DROP"),
        "TRUNCATE" => d("TRUNCATE"),
        "DELETE" if !has_word(&body, &["where"]) => d("DELETE without WHERE"),
        "UPDATE" if !has_word(&body, &["where"]) => d("UPDATE without WHERE"),
        "ALTER" if has_word(&body, &["drop"]) => d("ALTER ... DROP"),
        _ if is_read_only(stmt) => Danger { destructive: false, modifies: false, reason: None },
        _ => Danger { destructive: false, modifies: true, reason: None },
    }
}

/// True if the statement produces a result set (used by drivers that must pick an API).
pub fn returns_rows_hint(stmt: &str) -> bool {
    matches!(
        leading_keyword(stmt).as_str(),
        "SELECT" | "WITH" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "VALUES" | "TABLE" | "PRAGMA" | "EXEC" | "EXECUTE" | "CALL" | "SP_HELP"
    ) || has_word(&strip_strings(&strip_comments(stmt)), &["returning", "output"])
}

// ------------------------------------------------------------------------------------------

pub fn quote_ident(name: &str, d: Dialect) -> String {
    match d {
        Dialect::Mysql => format!("`{}`", name.replace('`', "``")),
        Dialect::Mssql => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn qualified(schema: &str, name: &str, d: Dialect) -> String {
    if schema.is_empty() || (d == Dialect::Sqlite && schema == "main") {
        quote_ident(name, d)
    } else {
        format!("{}.{}", quote_ident(schema, d), quote_ident(name, d))
    }
}

pub fn is_hex_binary(s: &str) -> bool {
    s.len() >= 2 && s.starts_with("\\x") && s[2..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn quote_literal(v: &Value, d: Dialect, data_type: Option<&str>) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => {
            if d == Dialect::Postgres {
                if *b { "TRUE" } else { "FALSE" }.into()
            } else {
                if *b { "1" } else { "0" }.into()
            }
        }
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            let t = data_type.unwrap_or("").to_ascii_lowercase();
            if is_hex_binary(s) && (t.is_empty() || ["bytea", "blob", "binary", "image", "raw"].iter().any(|x| t.contains(x))) {
                let hex = &s[2..];
                return match d {
                    Dialect::Postgres => format!("'\\x{hex}'::bytea"),
                    Dialect::Mssql => format!("0x{hex}"),
                    Dialect::Oracle => format!("HEXTORAW('{hex}')"),
                    _ => format!("X'{hex}'"),
                };
            }
            let mut e = s.replace('\'', "''");
            if d == Dialect::Mysql {
                e = e.replace('\\', "\\\\");
            }
            if d == Dialect::Mssql && !s.is_ascii() {
                format!("N'{e}'")
            } else {
                format!("'{e}'")
            }
        }
        other => quote_literal(&Value::String(other.to_string()), d, data_type),
    }
}

pub fn limit_query(base: &str, limit: usize, offset: usize, d: Dialect, has_order: bool) -> String {
    match d {
        Dialect::Mssql => format!(
            "{base}{} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY",
            if has_order { "" } else { " ORDER BY (SELECT NULL)" }
        ),
        Dialect::Oracle => format!("{base} OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY"),
        _ => format!("{base} LIMIT {limit} OFFSET {offset}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic() {
        let v = split_statements("select 1; select 'a;b'; -- c;\nselect \"x;\"", Dialect::Postgres);
        assert_eq!(v, vec!["select 1", "select 'a;b'", "-- c;\nselect \"x;\""]);
    }

    #[test]
    fn split_dollar_quotes() {
        let sql = "create function f() returns int as $$ begin return 1; end; $$ language plpgsql; select 1;";
        let v = split_statements(sql, Dialect::Postgres);
        assert_eq!(v.len(), 2);
        assert!(v[0].ends_with("plpgsql"));
    }

    #[test]
    fn split_mysql_delimiter() {
        let sql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END//\nDELIMITER ;\nSELECT 3;";
        let v = split_statements(sql, Dialect::Mysql);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v[0].starts_with("CREATE PROCEDURE"));
        assert_eq!(v[1], "SELECT 3");
    }

    #[test]
    fn split_mssql_go() {
        let v = split_statements("select 1\nGO\nselect 2\ngo\n", Dialect::Mssql);
        assert_eq!(v, vec!["select 1", "select 2"]);
    }

    #[test]
    fn split_sqlite_trigger() {
        let sql = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = 1; INSERT INTO c VALUES (1); END; SELECT 1;";
        let v = split_statements(sql, Dialect::Sqlite);
        assert_eq!(v.len(), 2, "{v:?}");
    }

    #[test]
    fn split_begin_transaction() {
        let v = split_statements("BEGIN; INSERT INTO a VALUES (1); COMMIT;", Dialect::Sqlite);
        assert_eq!(v.len(), 3, "{v:?}");
    }

    #[test]
    fn read_only_detection() {
        assert!(is_read_only("SELECT * FROM t"));
        assert!(is_read_only("with x as (select 1) select * from x;"));
        assert!(is_read_only("select 'delete' from t"));
        assert!(!is_read_only("select * into t2 from t"));
        assert!(!is_read_only("with d as (delete from t returning *) select * from d"));
        assert!(!is_read_only("select 1; drop table t"));
        assert!(!is_read_only("update t set a = 1"));
        assert!(!is_read_only("explain analyze delete from t"));
        assert!(!is_read_only("select * from t for update"));
        assert!(is_read_only("select updated_at from t"));
    }

    #[test]
    fn agent_queries_allowed() {
        use Dialect::*;
        for (sql, d) in [
            ("SELECT * FROM jrusers WHERE username = 'admin'", Mysql),
            ("select u.username, count(*) from jrusers u join jruserjob j on j.username = u.username group by u.username;", Mysql),
            ("SELECT 1 -- comment\n", Mysql),
            ("SELECT 1 # comment", Mysql),
            ("SELECT \"a\" FROM t", Postgres),
            ("WITH x AS (SELECT 1) SELECT * FROM x", Postgres),
            ("SELECT $$text$$", Postgres),
            ("EXPLAIN SELECT * FROM t", Postgres),
            ("SHOW TABLES", Mysql),
            ("SELECT [name] FROM [dbo].[users]", Mssql),
            ("SELECT q'[it's]' FROM dual", Oracle),
            ("PRAGMA table_info(users)", Sqlite),
            ("PRAGMA main.index_list('users')", Sqlite),
            ("SELECT updated_at, created_by FROM t", Postgres),
        ] {
            assert_eq!(check_agent_query(sql, d), Ok(()), "{sql}");
        }
    }

    #[test]
    fn agent_queries_blocked() {
        use Dialect::*;
        for (sql, d) in [
            // MySQL executes /*! ... */ and "--x"; multi statements would follow.
            ("SELECT 1 /*!; COMMIT; DELETE FROM jrusers */", Mysql),
            ("SELECT 1 /*M!100000 ; DELETE FROM t */", Mysql),
            ("SELECT 1 --1; COMMIT; DELETE FROM jrusers", Mysql),
            ("SELECT 1 /*! INTO OUTFILE '/tmp/x' */", Mysql),
            // Backslash escapes make the end of a string depend on server settings.
            ("SELECT 'a\\'' ; DELETE FROM t; -- '", Mysql),
            ("SELECT E'\\'', pg_read_file('/etc/passwd') -- '", Postgres),
            // Dollar quotes hide a quote from naive parsers.
            ("SELECT $$'$$, pg_read_file('/etc/passwd') -- '", Postgres),
            ("SELECT \"pg_read_file\"('/etc/passwd')", Postgres),
            ("SELECT pg_catalog.pg_read_file('/etc/passwd')", Postgres),
            ("SELECT dblink_exec('db', 'DELETE FROM t')", Postgres),
            ("SELECT pg_terminate_backend(123)", Postgres),
            ("SELECT LOAD_FILE('/etc/passwd')", Mysql),
            ("SELECT `load_file`('/etc/passwd')", Mysql),
            ("SELECT \"load_file\"('/etc/passwd')", Mysql),
            // SQL Server batches need no semicolon.
            ("SELECT 1 KILL 53", Mssql),
            ("SELECT 1 SHUTDOWN WITH NOWAIT", Mssql),
            ("SELECT * FROM OPENROWSET('SQLNCLI', 'x', 'SELECT 1')", Mssql),
            ("SELECT 1 EXEC xp_cmdshell 'dir'", Mssql),
            ("SELECT master.dbo.xp_dirtree('c:')", Mssql),
            ("SELECT [xp_cmdshell]", Mssql),
            ("SELECT utl_http.request('http://x') FROM dual", Oracle),
            ("SELECT dbms_pipe.receive_message('x', 1) FROM dual", Oracle),
            ("SELECT load_extension('x')", Sqlite),
            ("PRAGMA writable_schema = 1", Sqlite),
            ("PRAGMA journal_mode(WAL)", Sqlite),
            ("SELECT 1; DROP TABLE t", Postgres),
            ("DELETE FROM t", Postgres),
            ("EXPLAIN ANALYZE DELETE FROM t", Postgres),
            ("SELECT * INTO t2 FROM t", Mssql),
            ("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d", Postgres),
            ("SELECT * FROM t FOR UPDATE", Mysql),
        ] {
            assert!(check_agent_query(sql, d).is_err(), "not blocked: {sql}");
        }
    }

    #[test]
    fn danger() {
        assert!(analyze("DELETE FROM t").destructive);
        assert!(!analyze("DELETE FROM t WHERE id = 1").destructive);
        assert!(analyze("drop table x").destructive);
        assert!(analyze("insert into x values (1)").modifies);
        assert!(!analyze("select 1").modifies);
    }

    #[test]
    fn literals() {
        assert_eq!(quote_literal(&Value::String("O'Brien".into()), Dialect::Postgres, None), "'O''Brien'");
        assert_eq!(quote_literal(&Value::String("a\\b".into()), Dialect::Mysql, None), "'a\\\\b'");
        assert_eq!(quote_literal(&Value::String("\\xff00".into()), Dialect::Mssql, Some("varbinary")), "0xff00");
        assert_eq!(quote_literal(&Value::Bool(true), Dialect::Mysql, None), "1");
        assert_eq!(quote_ident("a]b", Dialect::Mssql), "[a]]b]");
        assert_eq!(quote_ident("a\"b", Dialect::Postgres), "\"a\"\"b\"");
    }
}
