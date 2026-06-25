//! Feature #61 — CSV import (`import_table_rows`). String cells are
//! coerced to each column's type and inserted through the validated
//! dynamic write path, then read back as typed model rows. One
//! `App::build` (settings init is one-shot).

#![allow(dead_code)]

use sqlx::SqlitePool;

#[derive(
    Debug, Clone, PartialEq, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model,
)]
#[umbral(table = "csv_widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
    pub qty: i64,
    pub active: bool,
    pub note: Option<String>,
}

async fn boot() -> SqlitePool {
    let settings = umbral::Settings::from_env().expect("settings");
    let pool = umbral::db::connect_sqlite("sqlite::memory:")
        .await
        .expect("sqlite");
    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .model::<Widget>()
        .build()
        .expect("App::build");
    sqlx::query(
        "CREATE TABLE csv_widget (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            qty BIGINT NOT NULL,
            active BOOLEAN NOT NULL,
            note TEXT
        )",
    )
    .execute(&pool)
    .await
    .expect("create table");
    pool
}

fn s(v: &str) -> String {
    v.to_string()
}

#[tokio::test]
async fn imports_csv_rows_with_per_column_coercion() {
    let pool = boot().await;

    let meta = umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == "csv_widget")
        .expect("model registered");

    let headers = vec![s("name"), s("qty"), s("active"), s("note")];
    let rows = vec![
        // strings → i64, bool; "hi" stays text
        vec![s("alpha"), s("5"), s("true"), s("hi")],
        // qty 0, active false, empty note on a nullable column → NULL
        vec![s("beta"), s("0"), s("0"), s("")],
        // a bad qty → this row fails and is reported, the rest still import
        vec![s("gamma"), s("not-a-number"), s("true"), s("x")],
    ];

    let report = umbral::orm::import_table_rows(&meta, &headers, &rows).await;
    assert_eq!(report.inserted, 2, "two good rows imported: {report:?}");
    assert_eq!(report.errors.len(), 1, "the bad row is reported");
    // The bad row is line 4 (header=1, data rows 2/3/4).
    assert_eq!(report.errors[0].0, 4);

    // Read the rows back as typed Widgets and verify the coercions landed.
    let mut widgets = Widget::objects()
        .on(&pool)
        .fetch()
        .await
        .expect("read back");
    widgets.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(widgets.len(), 2);

    let alpha = &widgets[0];
    assert_eq!(alpha.name, "alpha");
    assert_eq!(alpha.qty, 5); // string "5" → i64
    assert!(alpha.active); // "true" → true
    assert_eq!(alpha.note.as_deref(), Some("hi"));

    let beta = &widgets[1];
    assert_eq!(beta.name, "beta");
    assert_eq!(beta.qty, 0);
    assert!(!beta.active); // "0" → false
    assert_eq!(beta.note, None); // empty cell on a nullable column → NULL
}
