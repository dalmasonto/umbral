//! Option-C schema-per-tenant — the real-Postgres isolation proof. Two schemas
//! (`t_a`, `t_b`) each hold their OWN `sq_pg_widget` table. A router maps the
//! ambient `RouteContext`'s tenant key to a `Schema`, so the SAME ORM call,
//! run under two different `route_context_scope`s, reads and writes DISJOINT
//! rows. This is the cross-tenant isolation guarantee: a query issued as
//! tenant A can never see tenant B's rows, because the generated SQL is
//! schema-qualified per the ambient context.
//!
//! Self-skips unless `UMBRA_TEST_POSTGRES_URL` points at a server:
//!   UMBRA_TEST_POSTGRES_URL=postgres://… cargo test -p umbra-core \
//!     --test router_schema_postgres -- --ignored
//!
//! NOTE: unverified without a live Postgres (this harness has none). It is
//! `#[ignore]`d and only confirmed to COMPILE here.

#![allow(dead_code)]

use sqlx::PgPool;
use umbra::db::{DatabaseRouter, RouteContext, Schema, TenantKey};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sq_pg_widget")]
pub struct SqPgWidget {
    pub id: i64,
    pub name: String,
}

/// Maps the request's tenant key onto a Postgres schema. `acme -> t_a`,
/// `globex -> t_b`. No tenant (background/boot) -> no schema (bare table,
/// which here would hit the connection's default search_path).
struct TenantSchemaRouter;
impl DatabaseRouter for TenantSchemaRouter {
    fn schema_for(&self, ctx: &RouteContext) -> Option<Schema> {
        match ctx.tenant().map(|t| t.as_str()) {
            Some("acme") => Schema::new("t_a"),
            Some("globex") => Schema::new("t_b"),
            _ => None,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs UMBRA_TEST_POSTGRES_URL pointing at a Postgres server"]
async fn schema_router_isolates_tenants_on_postgres() {
    let Ok(url) = std::env::var("UMBRA_TEST_POSTGRES_URL") else {
        eprintln!("skipping: UMBRA_TEST_POSTGRES_URL not set");
        return;
    };
    let pool = PgPool::connect(&url).await.expect("connect Postgres");

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = url.clone();
    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .router(TenantSchemaRouter)
        .model::<SqPgWidget>()
        .build()
        .expect("App::build");

    // Fresh per-tenant schemas, each with its own copy of the table.
    for ddl in [
        "DROP SCHEMA IF EXISTS t_a CASCADE",
        "DROP SCHEMA IF EXISTS t_b CASCADE",
        "CREATE SCHEMA t_a",
        "CREATE SCHEMA t_b",
        "CREATE TABLE t_a.sq_pg_widget (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)",
        "CREATE TABLE t_b.sq_pg_widget (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL)",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("ddl");
    }

    let tenant_a = RouteContext::new().with_tenant(TenantKey::new("acme"));
    let tenant_b = RouteContext::new().with_tenant(TenantKey::new("globex"));

    // Write one row AS tenant A. The router qualifies the INSERT to t_a, so
    // the row lands in t_a.sq_pg_widget only.
    umbra::db::route_context_scope(tenant_a.clone(), async {
        SqPgWidget::objects()
            .create(SqPgWidget {
                id: 0,
                name: "a-only".into(),
            })
            .await
            .expect("insert as tenant A");
    })
    .await;

    // Write a different row AS tenant B -> t_b only.
    umbra::db::route_context_scope(tenant_b.clone(), async {
        SqPgWidget::objects()
            .create(SqPgWidget {
                id: 0,
                name: "b-only".into(),
            })
            .await
            .expect("insert as tenant B");
    })
    .await;

    // Reading AS tenant A sees ONLY t_a's row.
    let a_rows = umbra::db::route_context_scope(tenant_a.clone(), async {
        SqPgWidget::objects()
            .fetch()
            .await
            .expect("read as tenant A")
    })
    .await;
    assert_eq!(a_rows.len(), 1, "tenant A sees exactly its own row");
    assert_eq!(a_rows[0].name, "a-only");

    // Reading AS tenant B sees ONLY t_b's row — never tenant A's.
    let b_rows = umbra::db::route_context_scope(tenant_b.clone(), async {
        SqPgWidget::objects()
            .fetch()
            .await
            .expect("read as tenant B")
    })
    .await;
    assert_eq!(b_rows.len(), 1, "tenant B sees exactly its own row");
    assert_eq!(b_rows[0].name, "b-only");

    // The hard isolation assertion: neither tenant can observe the other's data.
    assert!(
        a_rows.iter().all(|w| w.name != "b-only"),
        "tenant A must NOT see tenant B's rows"
    );
    assert!(
        b_rows.iter().all(|w| w.name != "a-only"),
        "tenant B must NOT see tenant A's rows"
    );

    // Direct cross-check against the physical tables confirms the rows
    // physically landed in distinct schemas.
    let a_count: i64 = sqlx::query_scalar("SELECT count(*) FROM t_a.sq_pg_widget")
        .fetch_one(&pool)
        .await
        .expect("count t_a");
    let b_count: i64 = sqlx::query_scalar("SELECT count(*) FROM t_b.sq_pg_widget")
        .fetch_one(&pool)
        .await
        .expect("count t_b");
    assert_eq!(a_count, 1, "t_a holds exactly the tenant-A row");
    assert_eq!(b_count, 1, "t_b holds exactly the tenant-B row");

    // Clean up.
    for ddl in [
        "DROP SCHEMA IF EXISTS t_a CASCADE",
        "DROP SCHEMA IF EXISTS t_b CASCADE",
    ] {
        sqlx::query(ddl).execute(&pool).await.expect("cleanup");
    }
}
