use turso::{Builder, Value};

async fn connect() -> turso::Connection {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    db.connect().unwrap()
}

async fn rows(conn: &turso::Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut rows = conn.query(sql, ()).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(
            (0..row.column_count())
                .map(|i| row.get_value(i).unwrap())
                .collect(),
        );
    }
    out
}

#[tokio::test]
async fn application_collation_orders_and_compares_in_queries() {
    let conn = connect().await;
    // Case-insensitive ASCII, as a stand-in for any application-defined order.
    conn.register_collation("ascii_ci", |a, b| {
        a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
    })
    .unwrap();
    conn.execute("CREATE TABLE t (v TEXT)", ()).await.unwrap();
    conn.execute("INSERT INTO t VALUES ('b'), ('A'), ('c')", ())
        .await
        .unwrap();
    assert_eq!(
        rows(&conn, "SELECT v FROM t ORDER BY v COLLATE ascii_ci").await,
        vec![
            vec![Value::Text("A".into())],
            vec![Value::Text("b".into())],
            vec![Value::Text("c".into())]
        ]
    );
    assert_eq!(
        rows(&conn, "SELECT v FROM t WHERE v = 'C' COLLATE ascii_ci")
            .await
            .len(),
        1
    );
    // Connection-local collations cannot be part of the shared schema.
    assert!(conn
        .execute("CREATE TABLE u (v TEXT COLLATE ascii_ci)", ())
        .await
        .is_err());
}

#[tokio::test]
async fn application_scalar_function_gets_arguments_and_returns_values_and_errors() {
    let conn = connect().await;
    conn.register_scalar_function("quarter", 1, true, |args| match &args[0] {
        Value::Text(d) => {
            let month: i64 = d
                .get(5..7)
                .and_then(|m| m.parse().ok())
                .ok_or("not a date")?;
            Ok(Value::Integer((month + 2) / 3))
        }
        Value::Null => Ok(Value::Null),
        _ => Err("quarter() expects a date".to_string()),
    })
    .unwrap();
    conn.register_scalar_function("echo", -1, true, |args| {
        Ok(Value::Text(format!("{args:?}")))
    })
    .unwrap();
    conn.register_scalar_function("boom", 0, false, |_| panic!("boom"))
        .unwrap();
    assert_eq!(
        rows(&conn, "SELECT quarter('2026-10-27'), quarter(NULL)").await,
        vec![vec![Value::Integer(4), Value::Null]]
    );
    assert_eq!(
        rows(&conn, "SELECT echo(1, 2.5, 'x', x'00ff', NULL)").await,
        vec![vec![Value::Text(
            "[Integer(1), Real(2.5), Text(\"x\"), Blob([0, 255]), Null]".into()
        )]]
    );
    let err = conn.query("SELECT quarter(42)", ()).await;
    let err = match err {
        Err(e) => e.to_string(),
        Ok(mut r) => r
            .next()
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default(),
    };
    assert!(err.contains("quarter() expects a date"), "{err}");
    let panicked = match conn.query("SELECT boom()", ()).await {
        Err(e) => e.to_string(),
        Ok(mut r) => r
            .next()
            .await
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default(),
    };
    assert!(panicked.contains("panicked"), "{panicked}");
}

/// SQL LIKE matching (`%`, `_`) on already case-folded text, for the test's own like().
fn like_match(pattern: &[char], text: &[char]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some(('%', rest)) => (0..=text.len()).any(|i| like_match(rest, &text[i..])),
        Some(('_', rest)) => !text.is_empty() && like_match(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && like_match(rest, &text[1..]),
    }
}

#[tokio::test]
async fn application_functions_override_builtins_and_the_like_operator() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    let plain = db.connect().unwrap();
    // Unicode-aware replacements, as SQLite's ICU extension provides.
    conn.register_scalar_function("like", 2, true, |args| match (&args[0], &args[1]) {
        (Value::Text(p), Value::Text(t)) => {
            let fold = |s: &str| s.to_lowercase().chars().collect::<Vec<_>>();
            Ok(Value::Integer(like_match(&fold(p), &fold(t)) as i64))
        }
        _ => Ok(Value::Null),
    })
    .unwrap();
    conn.register_scalar_function("upper", 1, true, |args| match &args[0] {
        Value::Text(t) => Ok(Value::Text(t.to_uppercase())),
        other => Ok(other.clone()),
    })
    .unwrap();
    conn.execute("CREATE TABLE p (name TEXT)", ())
        .await
        .unwrap();
    conn.execute("INSERT INTO p VALUES ('Åsa'), ('Anna'), ('åke')", ())
        .await
        .unwrap();
    assert_eq!(
        rows(
            &conn,
            "SELECT 'Åsa' LIKE 'å%', 'Åsa' NOT LIKE 'å%', like('å%', 'Åsa'), upper('åsa')"
        )
        .await,
        vec![vec![
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(1),
            Value::Text("ÅSA".into())
        ]]
    );
    assert_eq!(
        rows(&conn, "SELECT count(*) FROM p WHERE name LIKE 'Å%'").await,
        vec![vec![Value::Integer(2)]]
    );
    // Another connection without the registrations keeps the built-ins.
    assert_eq!(
        rows(&plain, "SELECT 'Åsa' LIKE 'å%', upper('åsa')").await,
        vec![vec![Value::Integer(0), Value::Text("åSA".into())]]
    );
}
