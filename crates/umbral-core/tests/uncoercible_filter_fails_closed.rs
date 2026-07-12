//! An uncoercible filter value must match NO rows, never ALL of them (gaps3 #56).
//!
//! `filter_eq_string` takes a value that arrived as a string — a URL segment, a query
//! param, a form field — and coerces it to the column's declared type. When the
//! coercion fails (`"abc"` against a BigInt PK) it used to **drop the predicate**.
//!
//! Dropping a predicate does not narrow the query. It widens it to everything. And the
//! commonest caller of this function is a by-primary-key lookup, which means:
//!
//!     DynQuerySet::for_meta(&model).filter_eq_string("id", "abc").delete()
//!         -->  DELETE FROM widget            <-- no WHERE. The whole table.
//!
//! Fail-open is the wrong default for a filter. These tests pin fail-CLOSED.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "uf_widget")]
pub struct UfWidget {
    pub id: i64,
    pub name: String,
}

async fn boot() {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("uf.sqlite");
        std::mem::forget(tmp);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("pool");
        umbral::App::builder()
            .settings(settings)
            .database("default", pool)
            .model::<UfWidget>()
            .build()
            .expect("App::build");
        sqlx::query(
            "CREATE TABLE uf_widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        )
        .execute(&umbral::db::pool())
        .await
        .expect("ddl");
    })
    .await;
}

async fn seed(n: i64) {
    UfWidget::objects()
        .bulk_create(
            (0..n)
                .map(|i| UfWidget {
                    id: 0,
                    name: format!("w{i}"),
                })
                .collect(),
        )
        .await
        .expect("seed");
}

/// **The one that matters.** A by-id DELETE with an id that cannot be an id must delete
/// NOTHING. Before the fix this dropped the WHERE clause and emptied the table.
#[tokio::test]
async fn a_by_id_delete_with_an_uncoercible_id_deletes_nothing() {
    boot().await;
    let meta = ModelMeta::for_::<UfWidget>();

    seed(3).await;
    let before = UfWidget::objects().count().await.expect("count");
    assert!(before >= 3, "seed failed");

    let deleted = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "abc") // not a BigInt, and never can be
        .delete()
        .await
        .expect("delete must not error");

    assert_eq!(
        deleted, 0,
        "an id that cannot name a row must delete NO rows — dropping the predicate here \
         turns `DELETE ... WHERE id = 'abc'` into `DELETE FROM uf_widget`"
    );
    // `>=`, not `==`: the tests in this file share one database and run in parallel, so a
    // sibling may legitimately have inserted since `before` was taken. What must never
    // happen is the count going DOWN.
    let after = UfWidget::objects().count().await.expect("count");
    assert!(
        after >= before,
        "rows disappeared ({before} -> {after}) — a bad id emptied the table"
    );
}

/// The same shape on the read path: a bad id must not silently return somebody else's
/// row. `filter_eq_string(pk, junk).limit(1)` is how the admin loads a detail page.
#[tokio::test]
async fn a_by_id_read_with_an_uncoercible_id_finds_nothing() {
    boot().await;
    seed(2).await;
    let meta = ModelMeta::for_::<UfWidget>();

    let rows = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "not-a-number")
        .fetch_as_strings()
        .await
        .expect("fetch");
    assert!(
        rows.is_empty(),
        "a bad id returned {} row(s) — the first row of the table is not the row that \
         was asked for",
        rows.len()
    );

    let n = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "not-a-number")
        .count()
        .await
        .expect("count");
    assert_eq!(n, 0);

    assert!(
        !DynQuerySet::for_meta(&meta)
            .filter_eq_string("id", "not-a-number")
            .exists()
            .await
            .expect("exists"),
        "exists() said yes to an id that cannot exist"
    );
}

/// An UPDATE is the same hazard as a DELETE: a dropped predicate rewrites every row.
#[tokio::test]
async fn a_by_id_update_with_an_uncoercible_id_updates_nothing() {
    boot().await;
    seed(2).await;
    let meta = ModelMeta::for_::<UfWidget>();

    let mut values = serde_json::Map::new();
    values.insert("name".into(), serde_json::json!("CLOBBERED"));

    let affected = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", "abc")
        .update_json(&values)
        .await
        .expect("update must not error");
    assert_eq!(affected, 0, "a bad id must update no rows");

    let clobbered = UfWidget::objects()
        .filter(uf_widget::NAME.eq("CLOBBERED"))
        .count()
        .await
        .expect("count");
    assert_eq!(clobbered, 0, "every row in the table was rewritten");
}

/// Fail-closed must not become fail-always: a value that DOES coerce still matches, and
/// a genuinely-string column still accepts any string. Otherwise the fix would quietly
/// break every ordinary lookup.
#[tokio::test]
async fn coercible_values_and_string_columns_are_unaffected() {
    boot().await;
    let meta = ModelMeta::for_::<UfWidget>();

    let w = UfWidget::objects()
        .create(UfWidget {
            id: 0,
            name: "findable".to_string(),
        })
        .await
        .expect("create");

    let by_id = DynQuerySet::for_meta(&meta)
        .filter_eq_string("id", &w.id.to_string())
        .count()
        .await
        .expect("count");
    assert_eq!(by_id, 1, "a valid id must still find its row");

    // `name` is TEXT: every string is a legal value, so nothing is ever uncoercible and
    // nothing may be filtered out.
    let by_name = DynQuerySet::for_meta(&meta)
        .filter_eq_string("name", "findable")
        .count()
        .await
        .expect("count");
    assert_eq!(by_name, 1, "a TEXT column must accept any string");

    let missing = DynQuerySet::for_meta(&meta)
        .filter_eq_string("name", "nobody-has-this-name")
        .count()
        .await
        .expect("count");
    assert_eq!(missing, 0);
}

/// An unknown COLUMN is the same fail-open shape wearing a different hat: filtering on a
/// column that does not exist must not quietly match every row.
#[tokio::test]
async fn an_unknown_column_matches_nothing() {
    boot().await;
    seed(2).await;
    let meta = ModelMeta::for_::<UfWidget>();

    let n = DynQuerySet::for_meta(&meta)
        .filter_eq_string("no_such_column", "whatever")
        .count()
        .await
        .expect("count");
    assert_eq!(
        n, 0,
        "filtering on a column that does not exist returned rows"
    );
}
