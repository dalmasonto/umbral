//! Phase 4.1 — Postgres Array field type.
//!
//! Coverage layers:
//!
//! - **Derive classification.** `Vec<i64>` on a model produces a
//!   `SqlType::Array(ArrayElement::BigInt)` field spec.
//! - **Backend gating.** Booting an App with an Array-having model
//!   against SQLite fails with `field.backend` in
//!   `BuildError::SystemCheckFailed`.
//! - **DDL rendering.** `migrate::render_operation_for` against
//!   `"postgres"` emits `bigint[]` for an Array(BigInt) column.
//! - **Type-level pin.** The model derives cleanly and its column
//!   constants expose the Phase 4.1 surface (ArrayCol /
//!   NullableArrayCol).
//! - **Live PG round-trip** behind `#[ignore]`, gated on
//!   UMBRA_TEST_POSTGRES_URL.

use umbra::orm::{ArrayElement, Model, SqlType};

#[derive(Debug, Clone, sqlx::FromRow, umbra::orm::Model)]
#[umbra(table = "umbra_phase41_event")]
pub struct Event {
    pub id: i64,
    pub kind: String,
    pub tags: Vec<String>,
    pub scores: Option<Vec<i64>>,
}

/// The derive classifies `Vec<String>` and `Option<Vec<i64>>` as the
/// right Array variants. The element kind comes through unchanged.
#[test]
fn derive_classifies_vec_as_array_sqltype() {
    let by_name: std::collections::HashMap<&str, &umbra::orm::FieldSpec> = <Event as Model>::FIELDS
        .iter()
        .map(|f| (f.name, f))
        .collect();

    let tags = by_name.get("tags").expect("tags field");
    assert_eq!(tags.ty, SqlType::Array(ArrayElement::Text));
    assert!(!tags.nullable, "Vec<T> by itself is non-nullable");

    let scores = by_name.get("scores").expect("scores field");
    assert_eq!(scores.ty, SqlType::Array(ArrayElement::BigInt));
    assert!(scores.nullable, "Option<Vec<T>> is the nullable variant");
}

/// `ArrayElement::to_sql_type()` lifts each element kind to its
/// matching SqlType.
#[test]
fn array_element_round_trips_through_to_sql_type() {
    assert_eq!(ArrayElement::SmallInt.to_sql_type(), SqlType::SmallInt);
    assert_eq!(ArrayElement::Integer.to_sql_type(), SqlType::Integer);
    assert_eq!(ArrayElement::BigInt.to_sql_type(), SqlType::BigInt);
    assert_eq!(ArrayElement::Real.to_sql_type(), SqlType::Real);
    assert_eq!(ArrayElement::Double.to_sql_type(), SqlType::Double);
    assert_eq!(ArrayElement::Boolean.to_sql_type(), SqlType::Boolean);
    assert_eq!(ArrayElement::Text.to_sql_type(), SqlType::Text);
    assert_eq!(ArrayElement::Uuid.to_sql_type(), SqlType::Uuid);
}

/// `render_operation_for` against the Postgres dialect emits a column
/// type that includes the array suffix `[]` for an Array field.
#[test]
fn postgres_ddl_renders_array_suffix() {
    use umbra::migrate::{Column, Operation, render_operation_for};

    let op = Operation::CreateTable {
        table: "umbra_phase41_event".to_string(),
        columns: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
            is_string_repr: false,
            max_length: 0,
            },
            Column {
                name: "tags".to_string(),
                ty: SqlType::Array(ArrayElement::Text),
                primary_key: false,
                nullable: false,
                fk_target: None,
                noform: false,
                noedit: false,
            is_string_repr: false,
            max_length: 0,
            },
        ],
    };

    let stmts = render_operation_for(&op, "postgres");
    assert_eq!(stmts.len(), 1);
    let sql = &stmts[0];
    assert!(
        sql.contains("[]"),
        "Postgres Array should render with `[]` suffix; got {sql}"
    );
    // The element type is the standard text mapping. sea-query renders
    // postgres TEXT as lowercase `text`.
    let lower = sql.to_ascii_lowercase();
    assert!(
        lower.contains("text[]") || lower.contains("text []") || lower.contains("text  []"),
        "expected `text[]` for Vec<String> column; got {sql}"
    );
}

/// Booting an App with an Array-having model against SQLite produces
/// a `field.backend` finding. The boot fails with `SystemCheckFailed`
/// carrying that finding.
///
/// This test seeds the registry through `App::builder().model::<Event>()`,
/// so it can only run once per test binary (the OnceLocks are
/// process-wide). Marked `#[ignore]` because it pollutes the registry
/// for sibling tests; run with `cargo test array_field -- --ignored`.
#[tokio::test]
#[ignore = "pollutes the process-wide model registry; run isolated"]
async fn field_backend_rejects_array_on_sqlite() {
    use umbra::{App, Settings};
    use umbra_core::app::BuildError;

    let mut settings = Settings::from_env().expect("figment defaults load");
    settings.database_url = "sqlite::memory:".to_string();
    let sqlite_pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

    let result = App::builder()
        .settings(settings)
        .database("default", sqlite_pool)
        .model::<Event>()
        .build();

    match result {
        Err(BuildError::SystemCheckFailed { findings }) => {
            let has = findings.iter().any(|f| f.check_id == "field.backend");
            assert!(
                has,
                "expected a field.backend finding; got {:?}",
                findings.iter().map(|f| f.check_id).collect::<Vec<_>>(),
            );
        }
        Err(other) => panic!("expected SystemCheckFailed, got {other:?}"),
        Ok(_) => panic!("expected build to fail; SQLite + Vec<i64> should be rejected"),
    }
}

/// Type-level pin: the column constants the derive emits expose the
/// Phase 4.1 surface. If `ArrayCol` or `NullableArrayCol` regress,
/// this fails at the build.
#[test]
fn column_const_module_has_array_types() {
    // The derive emits a sibling `event` module. The constants must
    // have the right type. The compiler enforces this — if the cast
    // fails, the test fails to build.
    use umbra::orm::column::{ArrayCol, NullableArrayCol};
    let _: ArrayCol<Event> = event::TAGS;
    let _: NullableArrayCol<Event> = event::SCORES;
}

/// End-to-end against a real Postgres. Set
/// `UMBRA_TEST_POSTGRES_URL` and run via
/// `cargo test --test array_field -- --ignored`.
#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn array_field_round_trips_through_postgres() {
    let url =
        std::env::var("UMBRA_TEST_POSTGRES_URL").expect("UMBRA_TEST_POSTGRES_URL must be set");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    sqlx::query("DROP TABLE IF EXISTS umbra_phase41_event")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE umbra_phase41_event ( \
            id BIGSERIAL PRIMARY KEY, \
            kind TEXT NOT NULL, \
            tags TEXT[] NOT NULL, \
            scores BIGINT[] \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO umbra_phase41_event (kind, tags, scores) VALUES ($1, $2, $3), ($4, $5, NULL)",
    )
    .bind("startup")
    .bind(vec!["info".to_string(), "boot".to_string()])
    .bind(vec![10i64, 20, 30])
    .bind("draft")
    .bind(vec!["wip".to_string()])
    .execute(&pool)
    .await
    .unwrap();

    // PG-only models (Vec<T> here) can't satisfy `.fetch()`'s dual
    // FromRow bound, so they use the `.fetch_pg(&pool)` terminal that
    // bounds on FromRow<PgRow> alone. The pool is passed at the
    // terminal instead of through `.on_pg(...)`.
    let mut rows = Event::objects().fetch_pg(&pool).await.unwrap();
    rows.sort_by_key(|r| r.id);
    assert_eq!(rows.len(), 2);

    assert_eq!(rows[0].kind, "startup");
    assert_eq!(rows[0].tags, vec!["info".to_string(), "boot".to_string()]);
    assert_eq!(rows[0].scores, Some(vec![10, 20, 30]));

    assert_eq!(rows[1].kind, "draft");
    assert_eq!(rows[1].tags, vec!["wip".to_string()]);
    assert!(rows[1].scores.is_none());
}
