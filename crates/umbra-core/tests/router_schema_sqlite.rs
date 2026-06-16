//! Behavioral schema-per-tenant isolation, proven on SQLite via ATTACH.
//!
//! SQLite has no native schemas, but `ATTACH DATABASE ':memory:' AS tenant_a`
//! plus `"tenant_a"."table"` references behave exactly like a Postgres schema.
//! That lets us prove the option-C schema-qualification path ISOLATES tenant
//! data end-to-end — through real `Model::objects()` reads and writes — in CI,
//! where the live-Postgres isolation test (`router_schema_postgres`, #[ignore])
//! cannot run. This is the behavioral counterpart to the SQL-string assertion
//! in `router_schema_qualified`.

#![allow(dead_code)]

use sqlx::sqlite::SqlitePoolOptions;

use umbra::db::{DatabaseRouter, RouteContext, Schema, TenantKey};

#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "sch_widget")]
pub struct Widget {
    pub id: i64,
    pub name: String,
}

/// Maps the request's tenant straight to a schema of the same name. A request
/// with no tenant (the default context) qualifies nothing.
struct TenantSchemaRouter;
impl DatabaseRouter for TenantSchemaRouter {
    fn schema_for(&self, ctx: &RouteContext) -> Option<Schema> {
        ctx.tenant().and_then(|t| Schema::new(t.as_str()))
    }
}

async fn in_tenant<F, T>(tenant: &str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    umbra::db::route_context_scope(RouteContext::new().with_tenant(TenantKey::new(tenant)), fut)
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn schema_router_isolates_tenant_data_via_attach() {
    // ONE connection so the ATTACHed in-memory schemas persist across every
    // query the ORM runs (a second connection would get fresh, empty :memory:
    // databases).
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("pool");

    for schema in ["tenant_a", "tenant_b"] {
        sqlx::query(&format!("ATTACH DATABASE ':memory:' AS {schema}"))
            .execute(&pool)
            .await
            .expect("attach schema");
        sqlx::query(&format!(
            "CREATE TABLE {schema}.sch_widget \
             (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)"
        ))
        .execute(&pool)
        .await
        .expect("create tenant table");
    }
    // The default (main) schema table, for the no-tenant / spawned-task path.
    sqlx::query(
        "CREATE TABLE sch_widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .execute(&pool)
    .await
    .expect("create main table");

    umbra::App::builder()
        .settings(umbra::Settings::from_env().expect("settings"))
        .database("default", pool.clone())
        .router(TenantSchemaRouter)
        .model::<Widget>()
        .build()
        .expect("App::build");

    // Write one row per tenant, each inside its own routing scope.
    in_tenant("tenant_a", async {
        Widget::objects()
            .create(Widget {
                id: 0,
                name: "a-row".into(),
            })
            .await
            .expect("create in tenant_a");
    })
    .await;
    in_tenant("tenant_b", async {
        Widget::objects()
            .create(Widget {
                id: 0,
                name: "b-row".into(),
            })
            .await
            .expect("create in tenant_b");
    })
    .await;

    // Each tenant sees ONLY its own row through the ORM read path.
    let a_rows = in_tenant("tenant_a", async {
        Widget::objects().fetch().await.expect("fetch tenant_a")
    })
    .await;
    assert_eq!(a_rows.len(), 1, "tenant_a sees exactly its own row");
    assert_eq!(a_rows[0].name, "a-row");

    let b_rows = in_tenant("tenant_b", async {
        Widget::objects().fetch().await.expect("fetch tenant_b")
    })
    .await;
    assert_eq!(b_rows.len(), 1, "tenant_b sees exactly its own row");
    assert_eq!(b_rows[0].name, "b-row");

    // Cross-check against the raw attached schema tables: exactly one row in
    // each, proving the writes were routed by schema, not co-mingled.
    let a_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenant_a.sch_widget")
        .fetch_one(&pool)
        .await
        .unwrap();
    let b_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenant_b.sch_widget")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        (a_count, b_count),
        (1, 1),
        "one row landed in each tenant schema"
    );

    // Spawn-safety end-to-end: a background task spawned from INSIDE a tenant
    // scope does NOT inherit the tenant (task-locals don't cross `spawn`). Its
    // write must land in the default (main) schema, never the parent's tenant
    // schema — the hard rule that stops a worker silently running as the wrong
    // tenant.
    in_tenant("tenant_a", async {
        let handle = tokio::spawn(async {
            Widget::objects()
                .create(Widget {
                    id: 0,
                    name: "bg".into(),
                })
                .await
                .expect("spawned create");
        });
        handle.await.expect("join spawned task");
    })
    .await;

    let main_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main.sch_widget")
        .fetch_one(&pool)
        .await
        .unwrap();
    let a_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenant_a.sch_widget")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        main_count, 1,
        "the spawned task wrote to the default schema, not a tenant"
    );
    assert_eq!(
        a_after, 1,
        "the spawned task did NOT inherit tenant_a (still only its original row)"
    );
}
