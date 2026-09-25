//! Integration tests against real databases.
//!
//! Run with:  SQLIGHTER_IT=1 cargo test --test integration -- --test-threads=1
//! Expects PostgreSQL on 127.0.0.1:5432 (postgres/postgres, database "sqltest") and
//! MariaDB/MySQL on 127.0.0.1:3306 (tester/secret, database "sqltest"). Override with
//! SQLIGHTER_PG_* / SQLIGHTER_MY_* environment variables. SQLite always runs.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqlighter_lib::db::metadata;
use sqlighter_lib::db::session::{ExecSettings, Session};
use sqlighter_lib::model::*;
use sqlighter_lib::sqlgen;

fn enabled() -> bool {
    std::env::var("SQLIGHTER_IT").is_ok()
}

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

fn cfg(t: DbType) -> ConnectionConfig {
    let (host, port, db, user) = match t {
        DbType::Postgres => (env("SQLIGHTER_PG_HOST", "127.0.0.1"), env("SQLIGHTER_PG_PORT", "5432"), env("SQLIGHTER_PG_DB", "sqltest"), env("SQLIGHTER_PG_USER", "postgres")),
        _ => (env("SQLIGHTER_MY_HOST", "127.0.0.1"), env("SQLIGHTER_MY_PORT", "3306"), env("SQLIGHTER_MY_DB", "sqltest"), env("SQLIGHTER_MY_USER", "tester")),
    };
    ConnectionConfig {
        id: format!("it-{t:?}"),
        name: "it".into(),
        db_type: t,
        folder_id: None,
        color: None,
        host,
        port: port.parse().unwrap(),
        database: db,
        user,
        file_path: None,
        service_name: None,
        instance_name: None,
        save_password: false,
        read_only: false,
        production: false,
        tls: TlsConfig { mode: TlsMode::Disabled, ..Default::default() },
        ssh: SshConfig::default(),
        order: None,
        created_at: 0,
    }
}

fn secrets(t: DbType) -> ConnectionSecrets {
    let pw = match t {
        DbType::Postgres => env("SQLIGHTER_PG_PASSWORD", "postgres"),
        _ => env("SQLIGHTER_MY_PASSWORD", "secret"),
    };
    ConnectionSecrets { password: Some(pw), ..Default::default() }
}

fn no_prompt() -> sqlighter_lib::ssh::PromptFn {
    Arc::new(|_| Box::pin(async { false }))
}

fn es() -> ExecSettings {
    ExecSettings { confirm_destructive: true, max_rows: 1000, timeout: None }
}

async fn exec(s: &Session, sql: &str) -> ExecuteResponse {
    let r = s.execute(sql, &ExecuteOptions { max_rows: None, confirmed: true }, &es()).await.unwrap();
    for x in &r.results {
        assert!(x.error.is_none(), "{sql}: {:?}", x.error);
    }
    r
}

async fn common_suite(s: &Session, schema: &str) {
    let d = s.dialect();
    let ddl = match d {
        Dialect::Postgres => vec![
            "DROP TABLE IF EXISTS it_orders",
            "DROP TABLE IF EXISTS it_users",
            "CREATE TABLE it_users (id SERIAL PRIMARY KEY, name VARCHAR(100) NOT NULL, active BOOLEAN DEFAULT true, score NUMERIC(10,2), data BYTEA, created TIMESTAMP DEFAULT now())",
            "CREATE TABLE it_orders (id BIGINT PRIMARY KEY, user_id INT REFERENCES it_users(id) ON DELETE CASCADE, total DOUBLE PRECISION)",
            "CREATE INDEX it_orders_user ON it_orders(user_id)",
        ],
        Dialect::Mysql => vec![
            "DROP TABLE IF EXISTS it_orders",
            "DROP TABLE IF EXISTS it_users",
            "CREATE TABLE it_users (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(100) NOT NULL, active TINYINT(1) DEFAULT 1, score DECIMAL(10,2), data BLOB, created DATETIME DEFAULT CURRENT_TIMESTAMP)",
            "CREATE TABLE it_orders (id BIGINT PRIMARY KEY, user_id INT, total DOUBLE, CONSTRAINT fk_u FOREIGN KEY (user_id) REFERENCES it_users(id) ON DELETE CASCADE)",
            "CREATE INDEX it_orders_user ON it_orders(user_id)",
        ],
        _ => vec![
            "DROP TABLE IF EXISTS it_orders",
            "DROP TABLE IF EXISTS it_users",
            "CREATE TABLE it_users (id INTEGER PRIMARY KEY, name VARCHAR(100) NOT NULL, active BOOLEAN DEFAULT 1, score NUMERIC, data BLOB, created TEXT)",
            "CREATE TABLE it_orders (id BIGINT PRIMARY KEY, user_id INT REFERENCES it_users(id) ON DELETE CASCADE, total REAL)",
            "CREATE INDEX it_orders_user ON it_orders(user_id)",
        ],
    };
    for q in ddl {
        exec(s, q).await;
    }
    let bin = match d {
        Dialect::Postgres => "'\\xdeadbeef'::bytea",
        _ => "X'DEADBEEF'",
    };
    let r = exec(s, &format!("INSERT INTO it_users (name, score, data) VALUES ('Ann', 12.5, {bin}), ('O''Brien', NULL, NULL); INSERT INTO it_orders VALUES (1, 1, 9.99), (2, 1, 20)")).await;
    assert_eq!(r.results[0].affected_rows, Some(2));
    let r = exec(s, "SELECT u.id, name, score, data, total FROM it_users u JOIN it_orders o ON o.user_id = u.id ORDER BY o.id").await;
    let rs = &r.results[0];
    assert!(rs.is_result_set);
    assert_eq!(rs.rows.len(), 2);
    assert_eq!(rs.rows[0][0], Value::from(1));
    assert_eq!(rs.rows[0][1], Value::from("Ann"));
    assert_eq!(rs.rows[0][3], Value::from("\\xdeadbeef"));
    assert_eq!(rs.rows[0][4], serde_json::json!(9.99));

    // Truncation
    let r = s.execute("SELECT * FROM it_orders", &ExecuteOptions { max_rows: Some(1), confirmed: false }, &es()).await.unwrap();
    assert!(r.results[0].truncated);

    // Destructive statements need confirmation
    let r = s.execute("DELETE FROM it_orders", &ExecuteOptions::default(), &es()).await.unwrap();
    assert!(r.needs_confirmation.is_some());
    assert!(r.results.is_empty());

    // Manual transaction mode + rollback
    s.set_auto_commit(false).await.unwrap();
    exec(s, "INSERT INTO it_orders VALUES (3, 2, 1)").await;
    assert!(s.in_tx.load(std::sync::atomic::Ordering::SeqCst));
    s.rollback().await.unwrap();
    s.set_auto_commit(true).await.unwrap();
    let r = exec(s, "SELECT COUNT(*) FROM it_orders").await;
    assert_eq!(r.results[0].rows[0][0], Value::from(2));

    // Read-only agent query
    let r = s.read_only_query("SELECT name FROM it_users ORDER BY id", 10).await.unwrap();
    assert_eq!(r.rows.len(), 2);
    assert!(s.read_only_query("DELETE FROM it_users", 10).await.is_err());
    assert!(s.read_only_query("SELECT 1; DROP TABLE it_users", 10).await.is_err());

    // Metadata
    let mut g = s.meta().await.unwrap();
    let c = g.as_mut().unwrap();
    let schemas = metadata::schemas(c, d).await.unwrap();
    assert!(schemas.iter().any(|x| x == schema), "{schemas:?}");
    let objs = metadata::objects(c, d, schema).await.unwrap();
    assert!(objs.iter().any(|o| o.name == "it_users" && o.kind == ObjectKind::Table));
    let t = metadata::describe(c, d, schema, "it_users", ObjectKind::Table).await.unwrap();
    assert_eq!(t.primary_key, vec!["id"]);
    assert!(t.columns.iter().find(|c| c.name == "id").unwrap().auto_increment, "{:?}", t.columns);
    assert!(!t.columns.iter().find(|c| c.name == "name").unwrap().nullable);
    let o = metadata::describe(c, d, schema, "it_orders", ObjectKind::Table).await.unwrap();
    assert_eq!(o.foreign_keys.len(), 1, "{:?}", o.foreign_keys);
    assert_eq!(o.foreign_keys[0].ref_table, "it_users");
    assert_eq!(o.foreign_keys[0].on_delete.as_deref(), Some("CASCADE"));
    assert!(o.indexes.iter().any(|i| i.columns == vec!["user_id"]), "{:?}", o.indexes);
    let ddl = metadata::ddl(c, d, schema, "it_users", ObjectKind::Table).await.unwrap();
    assert!(ddl.to_uppercase().contains("CREATE TABLE"), "{ddl}");

    // Generated DDL can recreate the table in the same database
    let stmts = sqlgen::create_table(&t, d, &sqlgen::CreateOpts { to: d, schema, name: "it_users_copy", foreign_keys: false, indexes: true });
    let _ = c.run("DROP TABLE IF EXISTS it_users_copy", 0).await;
    for st in stmts {
        c.run(&st, 0).await.unwrap_or_else(|e| panic!("{st}: {e:#}"));
    }
    c.run("DROP TABLE it_users_copy", 0).await.unwrap();

    // Grid edits in a transaction
    drop(g);
    let n = s.apply_in_transaction(&["UPDATE it_users SET name = 'Anna' WHERE id = 1".to_string()]).await.unwrap();
    assert_eq!(n, 1);
    assert!(s.apply_in_transaction(&["UPDATE it_users SET name = 'X' WHERE id = 1".to_string(), "INSERT INTO nope VALUES (1)".to_string()]).await.is_err());
    let r = exec(s, "SELECT name FROM it_users WHERE id = 1").await;
    assert_eq!(r.results[0].rows[0][0], Value::from("Anna"), "failed change set must be rolled back");

    // Streaming
    let mut conn = s.new_conn().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let prod = async {
        conn.stream("SELECT * FROM it_users", 1, &tx).await.unwrap();
        drop(tx);
    };
    let cons = async {
        let mut rows = 0;
        let mut cols = 0;
        while let Some(b) = rx.recv().await {
            match b {
                sqlighter_lib::db::Batch::Columns(c) => cols = c.len(),
                sqlighter_lib::db::Batch::Rows(r) => rows += r.len(),
            }
        }
        (cols, rows)
    };
    let (_, (cols, rows)) = tokio::join!(prod, cons);
    assert_eq!((cols, rows), (6, 2));
}

#[tokio::test]
async fn postgres() {
    if !enabled() {
        return;
    }
    sqlighter_lib::tls::install_default_provider();
    let (s, _) = Session::open(cfg(DbType::Postgres), secrets(DbType::Postgres), no_prompt(), true).await.unwrap();
    assert!(s.server_version.contains("PostgreSQL"));
    common_suite(&s, "public").await;

    // Types
    let r = exec(&s, "SELECT 1::int2, 9007199254740993::int8, 1.5::float8, 'NaN'::float8, true, NULL::text, '{\"a\":1}'::jsonb, 12.30::numeric, DATE '2024-01-02'").await;
    let row = &r.results[0].rows[0];
    assert_eq!(row[0], Value::from(1));
    assert_eq!(row[1], Value::from("9007199254740993"));
    assert_eq!(row[2], serde_json::json!(1.5));
    assert_eq!(row[3], Value::from("NaN"));
    assert_eq!(row[4], Value::Bool(true));
    assert_eq!(row[5], Value::Null);
    assert_eq!(row[6], Value::from("{\"a\": 1}"));
    assert_eq!(row[7], Value::from("12.30"));
    assert_eq!(row[8], Value::from("2024-01-02"));

    // Errors carry details
    let r = s.execute("SELECT * FROM does_not_exist", &ExecuteOptions::default(), &es()).await.unwrap();
    assert!(r.results[0].error.as_ref().unwrap().contains("does_not_exist"));

    // Cancellation
    let s = Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move { s2.execute("SELECT pg_sleep(20)", &ExecuteOptions::default(), &es()).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.cancel().await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel did not stop the query").unwrap();
    assert!(r.results[0].error.as_ref().unwrap().contains("cancel"), "{:?}", r.results[0].error);
    // Session is still usable afterwards
    exec(&s, "SELECT 1").await;

    // Read-only connection is enforced by the server as well
    let mut c = cfg(DbType::Postgres);
    c.read_only = true;
    let (ro, _) = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.unwrap();
    let e = ro.execute("CREATE TABLE x_ro (a int)", &ExecuteOptions { max_rows: None, confirmed: true }, &es()).await;
    assert!(e.is_err());
    s.close().await;
}

#[tokio::test]
async fn postgres_tls_required_for_remote_hosts() {
    let mut c = cfg(DbType::Postgres);
    c.host = "db.example.com".into();
    let r = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await;
    let msg = format!("{:#}", r.err().unwrap());
    assert!(msg.contains("TLS is disabled"), "{msg}");
}

#[tokio::test]
async fn postgres_verify_full_rejects_untrusted_server() {
    if !enabled() {
        return;
    }
    sqlighter_lib::tls::install_default_provider();
    // The local test server has no TLS (or a self-signed cert). Either way no connection may be
    // established silently.
    let mut c = cfg(DbType::Postgres);
    c.tls.mode = TlsMode::VerifyFull;
    assert!(Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.is_err());
}

#[tokio::test]
async fn mariadb() {
    if !enabled() {
        return;
    }
    sqlighter_lib::tls::install_default_provider();
    let (s, _) = Session::open(cfg(DbType::Mariadb), secrets(DbType::Mariadb), no_prompt(), true).await.unwrap();
    common_suite(&s, "sqltest").await;
    let r = exec(&s, "SELECT 1, 18446744073709551615, CAST(1.5 AS DOUBLE), DATE '2024-01-02', TIMESTAMP '2024-01-02 03:04:05', 12.30, NULL, 'x'").await;
    let row = &r.results[0].rows[0];
    assert_eq!(row[0], Value::from(1));
    assert_eq!(row[1], Value::from("18446744073709551615"));
    assert_eq!(row[2], serde_json::json!(1.5));
    assert_eq!(row[3], Value::from("2024-01-02"));
    assert_eq!(row[4], Value::from("2024-01-02 03:04:05"));
    assert_eq!(row[5], Value::from("12.30"));
    assert_eq!(row[6], Value::Null);
    // Multiple result sets from one call
    exec(&s, "DROP PROCEDURE IF EXISTS it_two").await;
    exec(&s, "CREATE PROCEDURE it_two() BEGIN SELECT 1 AS a; SELECT 2 AS b; END").await;
    let r = exec(&s, "CALL it_two()").await;
    assert_eq!(r.results.iter().filter(|x| x.is_result_set).count(), 2);
    // Cancellation via KILL QUERY
    let s = Arc::new(s);
    let s2 = s.clone();
    let h = tokio::spawn(async move { s2.execute("SELECT SLEEP(20)", &ExecuteOptions::default(), &es()).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(500)).await;
    s.cancel().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h).await.expect("cancel did not stop the query").unwrap();
    exec(&s, "SELECT 1").await;
    s.close().await;
}

#[tokio::test]
async fn sqlite() {
    let path = std::env::temp_dir().join(format!("sqlighter-it-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut c = cfg(DbType::Sqlite);
    c.file_path = Some(path.to_string_lossy().into_owned());
    let (s, _) = Session::open(c, ConnectionSecrets::default(), no_prompt(), true).await.unwrap();
    common_suite(&s, "main").await;
    // Trigger with BEGIN ... END is executed as one statement
    exec(&s, "CREATE TRIGGER it_trg AFTER INSERT ON it_orders BEGIN UPDATE it_users SET score = 1 WHERE id = NEW.user_id; END; INSERT INTO it_orders VALUES (10, 2, 1);").await;
    let r = exec(&s, "SELECT score FROM it_users WHERE id = 2").await;
    assert_eq!(r.results[0].rows[0][0], Value::from(1));
    // UNIQUE constraints produce internal auto-indexes that must not leak into generated DDL
    exec(&s, "CREATE TABLE it_u (id INTEGER PRIMARY KEY, email TEXT UNIQUE)").await;
    {
        let mut g = s.meta().await.unwrap();
        let info = metadata::describe(g.as_mut().unwrap(), Dialect::Sqlite, "main", "it_u", ObjectKind::Table).await.unwrap();
        assert!(info.indexes.iter().all(|i| !i.name.starts_with("sqlite_")), "{:?}", info.indexes);
        let c = g.as_mut().unwrap();
        for st in sqlgen::create_table(&info, Dialect::Sqlite, &sqlgen::CreateOpts { to: Dialect::Sqlite, schema: "main", name: "it_u2", foreign_keys: true, indexes: true }) {
            c.run(&st, 0).await.unwrap_or_else(|e| panic!("{st}: {e:#}"));
        }
    }
    // Backup API
    let bak = path.with_extension("bak.db");
    if let sqlighter_lib::db::Conn::Lite(l) = s.new_conn().await.unwrap() {
        l.backup_to(&bak.to_string_lossy()).await.unwrap();
    }
    assert!(std::fs::metadata(&bak).unwrap().len() > 0);
    s.close().await;
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&bak);
}

/// Cross-dialect: table structure read from PostgreSQL is recreated in SQLite and MariaDB.
#[tokio::test]
async fn cross_dialect_create() {
    if !enabled() {
        return;
    }
    sqlighter_lib::tls::install_default_provider();
    let (pg, _) = Session::open(cfg(DbType::Postgres), secrets(DbType::Postgres), no_prompt(), true).await.unwrap();
    exec(&pg, "DROP TABLE IF EXISTS it_x; CREATE TABLE it_x (id SERIAL PRIMARY KEY, n VARCHAR(20), j JSONB, u UUID, t TIMESTAMPTZ, b BOOLEAN, d NUMERIC(8,3), bin BYTEA)").await;
    let info = {
        let mut g = pg.meta().await.unwrap();
        metadata::describe(g.as_mut().unwrap(), Dialect::Postgres, "public", "it_x", ObjectKind::Table).await.unwrap()
    };
    let (my, _) = Session::open(cfg(DbType::Mariadb), secrets(DbType::Mariadb), no_prompt(), true).await.unwrap();
    exec(&my, "DROP TABLE IF EXISTS it_x").await;
    for st in sqlgen::create_table(&info, Dialect::Postgres, &sqlgen::CreateOpts { to: Dialect::Mysql, schema: "sqltest", name: "it_x", foreign_keys: false, indexes: true }) {
        exec(&my, &st).await;
    }
    exec(&my, "INSERT INTO it_x (n, b, d) VALUES ('a', 1, 1.5)").await;
    let path = std::env::temp_dir().join(format!("sqlighter-x-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut c = cfg(DbType::Sqlite);
    c.file_path = Some(path.to_string_lossy().into_owned());
    let (lite, _) = Session::open(c, ConnectionSecrets::default(), no_prompt(), true).await.unwrap();
    for st in sqlgen::create_table(&info, Dialect::Postgres, &sqlgen::CreateOpts { to: Dialect::Sqlite, schema: "main", name: "it_x", foreign_keys: false, indexes: true }) {
        exec(&lite, &st).await;
    }
    exec(&lite, "INSERT INTO it_x (n) VALUES ('a'); INSERT INTO it_x (n) VALUES ('b')").await;
    let r = exec(&lite, "SELECT MAX(id) FROM it_x").await;
    assert_eq!(r.results[0].rows[0][0], Value::from(2));
    let _ = std::fs::remove_file(&path);
}

/// TLS against a PostgreSQL server whose certificate is issued by a private test CA.
/// Run with SQLIGHTER_TLS_DIR=<dir containing ca.crt and evil.crt>.
#[tokio::test]
async fn postgres_tls_modes() {
    let Ok(dir) = std::env::var("SQLIGHTER_TLS_DIR") else { return };
    sqlighter_lib::tls::install_default_provider();
    let base = || {
        let mut c = cfg(DbType::Postgres);
        c.host = "localhost".into();
        c
    };
    let ssl_used = |s: &Session| {
        let s = s as *const Session;
        async move {
            let s = unsafe { &*s };
            let r = exec(s, "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()").await;
            r.results[0].rows[0][0] == Value::Bool(true)
        }
    };

    // verify-full with the right CA: connection is encrypted
    let mut c = base();
    c.tls = TlsConfig { mode: TlsMode::VerifyFull, ca_file: Some(format!("{dir}/ca.crt")), ..Default::default() };
    let (s, _) = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.expect("verify-full with correct CA");
    assert!(ssl_used(&s).await);

    // verify-full with public roots only: the private CA is not trusted
    let mut c = base();
    c.tls = TlsConfig { mode: TlsMode::VerifyFull, ..Default::default() };
    assert!(Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.is_err());

    // A certificate from another CA (simulated MITM) is rejected
    let mut c = base();
    c.tls = TlsConfig { mode: TlsMode::VerifyFull, ca_file: Some(format!("{dir}/evil.crt")), ..Default::default() };
    let e = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.err().unwrap();
    assert!(format!("{e:#}").to_lowercase().contains("certificate"), "{e:#}");

    // Host name must match in verify-full, may differ in verify-ca
    let mut c = base();
    c.host = "127.0.0.2".into();
    c.tls = TlsConfig { mode: TlsMode::VerifyFull, ca_file: Some(format!("{dir}/ca.crt")), ..Default::default() };
    let e = Session::open(c.clone(), secrets(DbType::Postgres), no_prompt(), true).await.err().unwrap();
    assert!(format!("{e:#}").to_lowercase().contains("certificate"), "{e:#}");
    c.tls.mode = TlsMode::VerifyCa;
    Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.expect("verify-ca ignores the host name");

    // Pinning: fetch the certificate (TOFU), then connect with the pinned fingerprint
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", 5432)).await.unwrap();
    let info = sqlighter_lib::tls::probe_postgres_cert(tcp, "localhost", 5432).await.unwrap();
    assert!(info.subject.contains("localhost"));
    let mut c = base();
    c.tls = TlsConfig { mode: TlsMode::Pinned, pinned_fingerprint: Some(info.fingerprint.clone()), ..Default::default() };
    let (s, _) = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.expect("pinned");
    assert!(ssl_used(&s).await);
    let mut c = base();
    c.tls = TlsConfig { mode: TlsMode::Pinned, pinned_fingerprint: Some("00".repeat(32)), ..Default::default() };
    let e = Session::open(c, secrets(DbType::Postgres), no_prompt(), true).await.err().unwrap();
    assert!(format!("{e:#}").contains("CHANGED"), "{e:#}");
}

/// SSH tunnel with host key verification against a real sshd.
/// Run with SQLIGHTER_SSH_KEY=<private key> (and optionally SQLIGHTER_SSH_PORT, SQLIGHTER_SSH_USER).
#[tokio::test]
async fn ssh_tunnel_host_key_verification() {
    let Ok(key) = std::env::var("SQLIGHTER_SSH_KEY") else { return };
    sqlighter_lib::tls::install_default_provider();
    let ssh = |fp: Option<String>| SshConfig {
        enabled: true,
        host: "127.0.0.1".into(),
        port: env("SQLIGHTER_SSH_PORT", "2222").parse().unwrap(),
        user: env("SQLIGHTER_SSH_USER", "root"),
        auth: SshAuth::Key,
        key_file: Some(key.clone()),
        host_key_fingerprint: fp,
    };
    let seen: Arc<std::sync::Mutex<Vec<HostKeyPrompt>>> = Default::default();
    let prompt = |accept: bool| -> sqlighter_lib::ssh::PromptFn {
        let seen = seen.clone();
        Arc::new(move |p: HostKeyPrompt| {
            seen.lock().unwrap().push(p);
            Box::pin(async move { accept })
        })
    };

    // 1. Unknown host key: the user is asked (TOFU); accepting returns the fingerprint to store.
    let mut c = cfg(DbType::Postgres);
    c.host = "127.0.0.1".into();
    c.ssh = ssh(None);
    let (s, fp) = Session::open(c.clone(), secrets(DbType::Postgres), prompt(true), true).await.expect("tunnel with TOFU");
    let fp = fp.expect("new fingerprint to persist");
    assert!(fp.starts_with("SHA256:"), "{fp}");
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(seen.lock().unwrap()[0].previous.is_none());
    exec(&s, "SELECT 1").await;
    s.close().await;

    // 2. Known key: no prompt.
    c.ssh = ssh(Some(fp.clone()));
    let (s, newfp) = Session::open(c.clone(), secrets(DbType::Postgres), prompt(false), true).await.expect("tunnel with known key");
    assert!(newfp.is_none());
    assert_eq!(seen.lock().unwrap().len(), 1);
    s.close().await;

    // 3. Changed key (simulated MITM): the user is warned with the previous key; rejecting aborts.
    c.ssh = ssh(Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into()));
    let e = Session::open(c.clone(), secrets(DbType::Postgres), prompt(false), true).await.err().expect("must be rejected");
    assert!(format!("{e:#}").contains("man-in-the-middle"), "{e:#}");
    let last = seen.lock().unwrap().last().cloned().unwrap();
    assert!(last.previous.is_some());

    // 4. MySQL/MariaDB through the tunnel (loopback port forwarding).
    let mut m = cfg(DbType::Mariadb);
    m.host = "127.0.0.1".into();
    m.ssh = ssh(Some(fp));
    let (s, _) = Session::open(m, secrets(DbType::Mariadb), prompt(false), true).await.expect("mariadb through tunnel");
    let r = exec(&s, "SELECT 40 + 2").await;
    assert_eq!(r.results[0].rows[0][0], Value::from(42));
    s.close().await;
}
