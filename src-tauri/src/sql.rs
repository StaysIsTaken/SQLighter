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
