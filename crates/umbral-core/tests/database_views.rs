//! Database views (features #73).
//!
//! A `#[umbral(view = "SELECT ...")]` model is backed by a database VIEW instead of
//! a table. The migration engine emits `CREATE VIEW` for it, the ORM reads through it
//! like any other model, and every write path refuses it.
//!
//! These tests drive the REAL pipeline — `diff()` → `render_operation_for("sqlite")`
//! → execute — against a live database, insert real rows through the typed ORM, and
//! read the aggregation back out of the view. A test that only asserted on the
//! emitted SQL string would pass just as happily if the view returned nothing.

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use umbral::migrate::{ModelMeta, Operation, Snapshot, diff, render_operation_for};
use umbral::orm::write::WriteError;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dv_order")]
pub struct DvOrder {
    pub id: i64,
    pub customer: String,
    pub amount: i64,
}

/// A table whose name CONTAINS the view's dependency as a prefix. It exists purely
/// to catch a substring match in the dependency scanner: touching `dv_order_line`
/// must NOT recreate a view that only reads `dv_order`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "dv_order_line")]
pub struct DvOrderLine {
    pub id: i64,
    pub sku: String,
}

/// `CAST(... AS BIGINT)` rather than a bare `SUM(amount)`: on Postgres, `SUM` over a
/// bigint column returns NUMERIC, which will not decode into this model's `i64` field.
/// The CAST is standard SQL, so the SAME view body works on both backends — which is
/// the spelling the docs recommend, verified here and in `materialized_view_postgres`.
const TOTALS_SQL: &str = "SELECT MIN(id) AS id, customer, CAST(SUM(amount) AS BIGINT) AS total FROM dv_order GROUP BY customer";

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(
    table = "dv_customer_total",
    view = "SELECT MIN(id) AS id, customer, CAST(SUM(amount) AS BIGINT) AS total FROM dv_order GROUP BY customer"
)]
pub struct DvCustomerTotal {
    pub id: i64,
    pub customer: String,
    pub total: i64,
}

fn snapshot(models: Vec<ModelMeta>) -> Snapshot {
    Snapshot { models }
}

fn all_models() -> Vec<ModelMeta> {
    vec![
        ModelMeta::for_::<DvOrder>(),
        ModelMeta::for_::<DvOrderLine>(),
        ModelMeta::for_::<DvCustomerTotal>(),
    ]
}

fn kinds(ops: &[Operation]) -> Vec<String> {
    ops.iter()
        .map(|op| match op {
            Operation::CreateTable { table, .. } => format!("CREATE TABLE {table}"),
            Operation::CreateView { name, .. } => format!("CREATE VIEW {name}"),
            Operation::DropView { name, .. } => format!("DROP VIEW {name}"),
            Operation::AddColumn { table, column } => format!("ADD COL {table}.{}", column.name),
            other => format!("{:?}", std::mem::discriminant(other)),
        })
        .collect()
}

async fn boot() -> sqlx::SqlitePool {
    static ONCE: tokio::sync::OnceCell<sqlx::SqlitePool> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async {
        let settings = umbral::Settings::from_env().expect("figment defaults");
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("dv.sqlite");
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
            .database("default", pool.clone())
            .model::<DvOrder>()
            .model::<DvOrderLine>()
            .model::<DvCustomerTotal>()
            .build()
            .expect("App::build");

        // The schema comes from the migration engine, not from hand-written DDL.
        // That is the point: if `diff` orders the CREATE VIEW before the table it
        // selects from, this panics with "no such table" and the test has done its
        // job.
        let ops = diff(&Snapshot::default(), &snapshot(all_models())).expect("diff");
        for op in &ops {
            for stmt in render_operation_for(op, "sqlite") {
                sqlx::query(&stmt)
                    .execute(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("applying `{stmt}` failed: {e}"));
            }
        }
        pool
    })
    .await
    .clone()
}

/// The end-to-end claim: declare a view model, migrate, and read aggregated rows
/// back through the ORM as if it were any other model.
#[tokio::test]
async fn view_model_reads_aggregated_rows_through_the_orm() {
    boot().await;

    for (customer, amount) in [("ada", 30), ("ada", 12), ("grace", 7)] {
        DvOrder::objects()
            .create(DvOrder {
                id: 0,
                customer: customer.to_string(),
                amount,
            })
            .await
            .expect("seed order");
    }

    // Read the VIEW through the ordinary typed QuerySet — filter and all.
    let ada = DvCustomerTotal::objects()
        .filter(dv_customer_total::CUSTOMER.eq("ada"))
        .first()
        .await
        .expect("query view")
        .expect("ada has orders");
    assert_eq!(
        ada.total, 42,
        "the view must return the SUM the database computed, not a row we wrote"
    );

    let all = DvCustomerTotal::objects()
        .fetch()
        .await
        .expect("fetch view");
    assert_eq!(
        all.len(),
        2,
        "one row per customer, grouped by the view's SQL"
    );

    // ...and it stays live. A view is a stored query, so a new order changes what it
    // returns with no write to the view itself.
    DvOrder::objects()
        .create(DvOrder {
            id: 0,
            customer: "grace".to_string(),
            amount: 100,
        })
        .await
        .expect("seed another");
    let grace = DvCustomerTotal::objects()
        .filter(dv_customer_total::CUSTOMER.eq("grace"))
        .first()
        .await
        .expect("query view")
        .expect("grace has orders");
    assert_eq!(grace.total, 107, "the view recomputed on read");
}

/// A view is read-only, and the framework says so before the driver does.
#[tokio::test]
async fn every_write_path_refuses_a_view() {
    boot().await;

    // --- typed path ---
    let err = DvCustomerTotal::objects()
        .create(DvCustomerTotal {
            id: 0,
            customer: "mallory".to_string(),
            total: 999,
        })
        .await
        .expect_err("create against a view must fail");
    assert!(
        matches!(&err, WriteError::ReadOnlyView { table } if table == "dv_customer_total"),
        "expected ReadOnlyView, got {err:?}"
    );
    assert_eq!(err.code(), "read_only_view");

    let err = DvCustomerTotal::objects()
        .filter(dv_customer_total::CUSTOMER.eq("ada"))
        .update_values(serde_json::json!({"total": 0}).as_object().unwrap().clone())
        .await
        .expect_err("update against a view must fail");
    assert!(matches!(err, WriteError::ReadOnlyView { .. }), "{err:?}");

    let err = DvCustomerTotal::objects()
        .filter(dv_customer_total::CUSTOMER.eq("ada"))
        .delete()
        .await
        .expect_err("delete against a view must fail");
    assert!(
        err.to_string().contains("read-only"),
        "the delete error must explain itself, got: {err}"
    );

    // --- dynamic path: this is what the admin and REST actually run on, so a guard
    // that only covered the typed path would leave the framework's own surfaces able
    // to POST to a view.
    let meta = ModelMeta::for_::<DvCustomerTotal>();
    let body = serde_json::json!({"customer": "mallory", "total": 999})
        .as_object()
        .unwrap()
        .clone();
    let err = umbral::orm::DynQuerySet::for_meta(&meta)
        .insert_json(&body)
        .await
        .expect_err("REST/admin insert against a view must fail");
    assert!(matches!(err, WriteError::ReadOnlyView { .. }), "{err:?}");

    // And the rejection is visible to a human: it is nobody's *field* that is wrong,
    // so it has to land in the non-field bucket rather than being silently dropped.
    let non_field = err.non_field_errors();
    assert!(
        non_field.iter().any(|m| m.contains("cannot be written")),
        "a read-only rejection must surface to the user, got {non_field:?}"
    );

    // Nothing was written, obviously — but assert it, because "the error was returned"
    // and "the row is absent" are different claims.
    let mallory = DvCustomerTotal::objects()
        .filter(dv_customer_total::CUSTOMER.eq("mallory"))
        .first()
        .await
        .expect("query view");
    assert!(mallory.is_none(), "no write may have landed");
}

/// The view is created AFTER the table it selects from. Get this backwards and the
/// migration dies at apply time with "no such table".
#[test]
fn create_view_is_ordered_after_the_table_it_reads() {
    let ops = diff(&Snapshot::default(), &snapshot(all_models())).expect("diff");
    let k = kinds(&ops);
    let table_at = k
        .iter()
        .position(|s| s == "CREATE TABLE dv_order")
        .expect("table created");
    let view_at = k
        .iter()
        .position(|s| s == "CREATE VIEW dv_customer_total")
        .expect("view created");
    assert!(
        table_at < view_at,
        "the view must be created after its table, got {k:?}"
    );
}

/// Editing a view's SQL is a drop and a recreate — a view stores nothing, so there is
/// no ALTER to write and nothing to preserve.
#[test]
fn changing_the_sql_drops_and_recreates() {
    let before = snapshot(all_models());
    let mut after_models = all_models();
    let totals = after_models
        .iter_mut()
        .find(|m| m.table == "dv_customer_total")
        .unwrap();
    totals.view = Some(
        "SELECT MIN(id) AS id, customer, COUNT(*) AS total FROM dv_order GROUP BY customer"
            .to_string(),
    );

    let ops = diff(&before, &snapshot(after_models)).expect("diff");
    assert_eq!(
        kinds(&ops),
        vec![
            "DROP VIEW dv_customer_total",
            "CREATE VIEW dv_customer_total"
        ],
        "an SQL edit is exactly a drop + a create"
    );
    // And the recreate carries the NEW body, not the old one.
    match &ops[1] {
        Operation::CreateView { sql, .. } => assert!(sql.contains("COUNT(*)")),
        other => panic!("expected CreateView, got {other:?}"),
    }
}

/// The property that makes this feature safe rather than merely present.
///
/// Postgres refuses to drop or retype a column a live view selects from. So a
/// migration that touches `dv_order` while `dv_customer_total` reads it must move the
/// view out of the way FIRST and put it back LAST — even though the view's own SQL
/// never changed and the user never touched it.
#[test]
fn touching_a_table_recreates_the_views_that_read_it() {
    let before = snapshot(all_models());
    let mut after_models = all_models();
    let order = after_models
        .iter_mut()
        .find(|m| m.table == "dv_order")
        .unwrap();
    let mut new_col = order.fields[1].clone();
    new_col.name = "note".to_string();
    new_col.nullable = true;
    order.fields.push(new_col);

    let ops = diff(&before, &snapshot(after_models)).expect("diff");
    assert_eq!(
        kinds(&ops),
        vec![
            "DROP VIEW dv_customer_total",
            "ADD COL dv_order.note",
            "CREATE VIEW dv_customer_total",
        ],
        "the view must be dropped before the table op and recreated after it"
    );
}

/// The dependency scan is whole-word. `dv_order_line` merely *starts with* the name
/// the view reads, and a substring match would recreate the view on every unrelated
/// change to it — noise at best, and evidence the scanner cannot be trusted to know
/// what a view actually depends on.
#[test]
fn a_table_whose_name_merely_contains_the_dependency_does_not_recreate_the_view() {
    let before = snapshot(all_models());
    let mut after_models = all_models();
    let line = after_models
        .iter_mut()
        .find(|m| m.table == "dv_order_line")
        .unwrap();
    let mut new_col = line.fields[1].clone();
    new_col.name = "qty".to_string();
    new_col.nullable = true;
    line.fields.push(new_col);

    let ops = diff(&before, &snapshot(after_models)).expect("diff");
    assert_eq!(
        kinds(&ops),
        vec!["ADD COL dv_order_line.qty"],
        "the view reads `dv_order`, not `dv_order_line` — leave it alone"
    );
}

/// A model that stops being a view becomes a table, and vice versa. Both directions
/// have to produce the two ops in the order that works.
#[test]
fn a_view_can_become_a_table() {
    let before = snapshot(all_models());
    let mut after_models = all_models();
    let totals = after_models
        .iter_mut()
        .find(|m| m.table == "dv_customer_total")
        .unwrap();
    totals.view = None; // now a real table

    let ops = diff(&before, &snapshot(after_models)).expect("diff");
    let k = kinds(&ops);
    assert_eq!(k[0], "DROP VIEW dv_customer_total");
    assert!(
        k.contains(&"CREATE TABLE dv_customer_total".to_string()),
        "got {k:?}"
    );
}

/// Postgres renders a materialized view with the MATERIALIZED keyword; the plain one
/// without. The SQLite renderer never sees a materialized view (the boot check kills
/// it first) — see `materialized_view_sqlite_boot.rs`.
#[test]
fn materialized_renders_only_on_postgres() {
    let op = Operation::CreateView {
        name: "standings".to_string(),
        sql: "SELECT 1".to_string(),
        materialized: true,
    };
    let pg = render_operation_for(&op, "postgres").join(";");
    assert!(
        pg.contains("CREATE MATERIALIZED VIEW"),
        "postgres must emit MATERIALIZED, got: {pg}"
    );

    let plain = Operation::CreateView {
        name: "standings".to_string(),
        sql: "SELECT 1".to_string(),
        materialized: false,
    };
    let pg = render_operation_for(&plain, "postgres").join(";");
    assert!(pg.contains("CREATE VIEW"), "got: {pg}");
    assert!(!pg.contains("MATERIALIZED"), "got: {pg}");

    let lite = render_operation_for(&plain, "sqlite").join(";");
    assert!(lite.contains("CREATE VIEW"), "got: {lite}");
}

/// The attribute reaches the trait. Cheap, but it is the seam the whole feature hangs
/// off: if the derive drops the SQL, everything above silently becomes a no-op model.
#[test]
fn the_derive_carries_the_sql_onto_the_trait() {
    use umbral::orm::Model;
    assert_eq!(DvCustomerTotal::VIEW, Some(TOTALS_SQL));
    assert!(!DvCustomerTotal::MATERIALIZED);
    assert_eq!(DvOrder::VIEW, None);
}
