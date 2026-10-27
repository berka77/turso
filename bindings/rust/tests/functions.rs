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
