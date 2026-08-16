//! Coverage for Phase 3 of the Postgres rollout: `inspectdb` against a
//! real Postgres database.
//!
//! Two layers of coverage:
//!
//! - **Type-level pin.** Compile-only verification that the public
//!   `introspect_pool_pg` surface is reachable through the facade and
//!   accepts a `&PgPool`.
//! - **Full round trip.** A `#[tokio::test]` marked `#[ignore]` that
//!   runs only when `UMBRAL_TEST_POSTGRES_URL` is set. Creates a table
//!   with one of every catalogue type, drops it through
//!   `introspect_pool_pg`, asserts the schema came back the way it went
//!   in (column count, types, nullability, primary key).

use sqlx::PgPool;
use umbral::inspect::{IntrospectedSchema, introspect_pool_pg};
use umbral::orm::SqlType;

/// Compile-only pin: the Phase 3 surface exists and accepts `&PgPool`.
/// If `introspect_pool_pg` is dropped from the facade or its signature
/// changes, this fails at the build.
#[test]
fn pg_pool_typechecks_against_introspect_pool_pg() {
    #[allow(dead_code)]
    async fn _unreachable(
        pg_pool: &PgPool,
    ) -> Result<IntrospectedSchema, umbral::inspect::InspectError> {
        introspect_pool_pg(pg_pool).await
    }
}

/// End-to-end against a real Postgres. Same gate as the Phase 2.5
/// QuerySet test — set `UMBRAL_TEST_POSTGRES_URL` and run via
/// `cargo test --test postgres_inspect -- --ignored`.
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn full_round_trip_against_real_postgres() {
    let url = std::env::var("UMBRAL_TEST_POSTGRES_URL")
        .expect("UMBRAL_TEST_POSTGRES_URL must be set to run the ignored Postgres test");
    let pool = PgPool::connect(&url)
        .await
        .expect("connect to Postgres at UMBRAL_TEST_POSTGRES_URL");

    // Clean state from any prior run.
    sqlx::query("DROP TABLE IF EXISTS umbral_phase3_kitchen_sink")
        .execute(&pool)
        .await
        .expect("drop prior table");

    // One of every catalogue type. Mix of nullable / non-nullable so the
    // round-trip exercises both paths through `is_nullable`.
    sqlx::query(
        "CREATE TABLE umbral_phase3_kitchen_sink ( \
            id BIGSERIAL PRIMARY KEY, \
            small SMALLINT NOT NULL, \
            medium INTEGER NOT NULL, \
            big BIGINT NOT NULL, \
            real_v REAL NOT NULL, \
            double_v DOUBLE PRECISION NOT NULL, \
            flag BOOLEAN NOT NULL, \
            note TEXT NOT NULL, \
            varchar_note VARCHAR(64), \
            day DATE NOT NULL, \
            clock TIME NOT NULL, \
            at TIMESTAMP WITH TIME ZONE, \
            uid UUID NOT NULL \
         )",
    )
    .execute(&pool)
    .await
    .expect("create kitchen sink table");

    let schema = introspect_pool_pg(&pool)
        .await
        .expect("introspect_pool_pg should succeed");

    let table = schema
        .tables
        .iter()
        .find(|t| t.table == "umbral_phase3_kitchen_sink")
        .expect("kitchen sink table should appear in the introspected schema");

    // Lookup helper — column-name → IntrospectedColumn.
    let by_name: std::collections::HashMap<&str, &umbral::inspect::IntrospectedColumn> =
        table.columns.iter().map(|c| (c.name.as_str(), c)).collect();

    // PK is non-nullable BigInt.
    let id = by_name.get("id").expect("id column");
    assert!(id.primary_key);
    assert!(!id.nullable);
    assert_eq!(id.ty, SqlType::BigInt);

    // Type catalogue round-trips.
    let cases: &[(&str, SqlType)] = &[
        ("small", SqlType::SmallInt),
        ("medium", SqlType::Integer),
        ("big", SqlType::BigInt),
        ("real_v", SqlType::Real),
        ("double_v", SqlType::Double),
        ("flag", SqlType::Boolean),
        ("note", SqlType::Text),
        ("varchar_note", SqlType::Text),
        ("day", SqlType::Date),
        ("clock", SqlType::Time),
        ("at", SqlType::Timestamptz),
        ("uid", SqlType::Uuid),
    ];
    for (name, ty) in cases {
        let col = by_name
            .get(name)
            .unwrap_or_else(|| panic!("introspection missed `{name}`"));
        assert_eq!(col.ty, *ty, "type mismatch on `{name}`: got {:?}", col.ty);
    }

    // Nullability: `varchar_note` and `at` were declared without NOT NULL.
    assert!(by_name["varchar_note"].nullable, "VARCHAR is nullable");
    assert!(by_name["at"].nullable, "timestamptz is nullable");
    // Everything else is non-nullable.
    assert!(!by_name["small"].nullable);
    assert!(!by_name["note"].nullable);
}

/// A native Postgres enum column (`CREATE TYPE ... AS ENUM`) is recovered as a
/// TEXT-backed `choices` column carrying the enum type name + labels (in
/// `enumsortorder`), so the renderer folds it into a generated `Choices` enum.
/// This exercises the `pg_enum` introspection the SQLite path can't reach (no
/// native enum type).
#[tokio::test]
#[ignore = "needs UMBRAL_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn native_pg_enum_recovers_as_choices_column() {
    let url = std::env::var("UMBRAL_TEST_POSTGRES_URL")
        .expect("UMBRAL_TEST_POSTGRES_URL must be set to run the ignored Postgres test");
    let pool = PgPool::connect(&url)
        .await
        .expect("connect to Postgres at UMBRAL_TEST_POSTGRES_URL");

    // Clean state — drop the table before the type it depends on.
    sqlx::query("DROP TABLE IF EXISTS umbral_enum_probe")
        .execute(&pool)
        .await
        .expect("drop prior table");
    sqlx::query("DROP TYPE IF EXISTS umbral_payment_method")
        .execute(&pool)
        .await
        .expect("drop prior type");

    sqlx::query("CREATE TYPE umbral_payment_method AS ENUM ('STRIPE', 'CRYPTO', 'AQUAFIER')")
        .execute(&pool)
        .await
        .expect("create enum type");
    sqlx::query(
        "CREATE TABLE umbral_enum_probe ( \
            id BIGSERIAL PRIMARY KEY, \
            method umbral_payment_method NOT NULL, \
            fallback umbral_payment_method \
         )",
    )
    .execute(&pool)
    .await
    .expect("create table with enum columns");

    let schema = introspect_pool_pg(&pool)
        .await
        .expect("introspect_pool_pg should succeed");
    let table = schema
        .tables
        .iter()
        .find(|t| t.table == "umbral_enum_probe")
        .expect("enum probe table should appear");
    let by_name: std::collections::HashMap<&str, &umbral::inspect::IntrospectedColumn> =
        table.columns.iter().map(|c| (c.name.as_str(), c)).collect();

    // A native enum column: TEXT-backed, carrying the type name + labels in
    // declaration order (NOT alphabetical — CRYPTO/STRIPE would sort wrong).
    let method = by_name.get("method").expect("method column");
    assert_eq!(method.ty, SqlType::Text, "enum stores as TEXT + CHECK");
    assert_eq!(method.enum_type.as_deref(), Some("umbral_payment_method"));
    assert_eq!(method.choices, vec!["STRIPE", "CRYPTO", "AQUAFIER"]);
    assert!(!method.nullable);

    // The nullable column of the same enum type recovers the same labels.
    let fallback = by_name.get("fallback").expect("fallback column");
    assert_eq!(fallback.enum_type.as_deref(), Some("umbral_payment_method"));
    assert_eq!(fallback.choices, vec!["STRIPE", "CRYPTO", "AQUAFIER"]);
    assert!(fallback.nullable);

    // A plain column is untouched — no spurious choices.
    assert!(by_name["id"].choices.is_empty());
    assert!(by_name["id"].enum_type.is_none());

    sqlx::query("DROP TABLE umbral_enum_probe")
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DROP TYPE umbral_payment_method")
        .execute(&pool)
        .await
        .ok();
}
