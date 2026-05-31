//! Coverage for Phase 3 of the Postgres rollout: `inspectdb` against a
//! real Postgres database.
//!
//! Two layers of coverage:
//!
//! - **Type-level pin.** Compile-only verification that the public
//!   `introspect_pool_pg` surface is reachable through the facade and
//!   accepts a `&PgPool`.
//! - **Full round trip.** A `#[tokio::test]` marked `#[ignore]` that
//!   runs only when `UMBRA_TEST_POSTGRES_URL` is set. Creates a table
//!   with one of every catalogue type, drops it through
//!   `introspect_pool_pg`, asserts the schema came back the way it went
//!   in (column count, types, nullability, primary key).

use sqlx::PgPool;
use umbra::inspect::{IntrospectedSchema, introspect_pool_pg};
use umbra::orm::SqlType;

/// Compile-only pin: the Phase 3 surface exists and accepts `&PgPool`.
/// If `introspect_pool_pg` is dropped from the facade or its signature
/// changes, this fails at the build.
#[test]
fn pg_pool_typechecks_against_introspect_pool_pg() {
    #[allow(dead_code)]
    async fn _unreachable(
        pg_pool: &PgPool,
    ) -> Result<IntrospectedSchema, umbra::inspect::InspectError> {
        introspect_pool_pg(pg_pool).await
    }
}

/// End-to-end against a real Postgres. Same gate as the Phase 2.5
/// QuerySet test — set `UMBRA_TEST_POSTGRES_URL` and run via
/// `cargo test --test postgres_inspect -- --ignored`.
#[tokio::test]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn full_round_trip_against_real_postgres() {
    let url = std::env::var("UMBRA_TEST_POSTGRES_URL")
        .expect("UMBRA_TEST_POSTGRES_URL must be set to run the ignored Postgres test");
    let pool = PgPool::connect(&url)
        .await
        .expect("connect to Postgres at UMBRA_TEST_POSTGRES_URL");

    // Clean state from any prior run.
    sqlx::query("DROP TABLE IF EXISTS umbra_phase3_kitchen_sink")
        .execute(&pool)
        .await
        .expect("drop prior table");

    // One of every catalogue type. Mix of nullable / non-nullable so the
    // round-trip exercises both paths through `is_nullable`.
    sqlx::query(
        "CREATE TABLE umbra_phase3_kitchen_sink ( \
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
        .find(|t| t.table == "umbra_phase3_kitchen_sink")
        .expect("kitchen sink table should appear in the introspected schema");

    // Lookup helper — column-name → IntrospectedColumn.
    let by_name: std::collections::HashMap<&str, &umbra::inspect::IntrospectedColumn> =
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
