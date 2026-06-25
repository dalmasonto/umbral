//! PG-gated proof of schema-per-tenant **isolation**.
//!
//! `#[ignore]` + skips cleanly unless a test Postgres is configured via
//! `UMBRAL_TENANTS_TEST_PG` (or `DATABASE_URL`). The real proof:
//!
//! 1. Build an app with the `TenantsPlugin` + a tenant-owned model (`TPost`)
//!    and the shared `Tenant` registry.
//! 2. Generate migrations from the registered models into a temp dir, migrate
//!    the SHARED apps into `public`, then provision two tenants — each gets a
//!    `CREATE SCHEMA` + the tenant app migrated into its own schema (via the
//!    core `run_for_schema_in` helper the management layer wraps).
//! 3. Write a `TPost` row under tenant A's `scope`, another under B's.
//! 4. Assert isolation: A's query under A's ctx sees only A's row; B sees only
//!    B's; both schemas exist; and a SHARED `Tenant` row is visible under both.
//!
//! Run it:
//! ```text
//! UMBRAL_TENANTS_TEST_PG=postgres://app:apppass@localhost:5433/appdb \
//!   cargo test -p umbral-tenants --test isolation_postgres -- --ignored --nocapture
//! ```

use std::collections::HashSet;

use umbral::db::{RouteContext, Schema, TenantKey};
use umbral_tenants::{Tenant, TenantsPlugin};

/// A tenant-owned model. NOT a shared app → its `tpost` table lands in each
/// tenant's schema; its rows are isolated per tenant.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "tpost")]
pub struct TPost {
    pub id: i64,
    pub title: String,
}

fn pg_url() -> Option<String> {
    // Accept the tenants-specific var, the framework-wide convention
    // (`UMBRAL_TEST_POSTGRES_URL`, used by the other PG-gated suites), or a plain
    // `DATABASE_URL`.
    std::env::var("UMBRAL_TENANTS_TEST_PG")
        .or_else(|_| std::env::var("UMBRAL_TEST_POSTGRES_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| u.starts_with("postgres"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a test Postgres (UMBRAL_TENANTS_TEST_PG / DATABASE_URL)"]
async fn schema_per_tenant_isolation() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping schema_per_tenant_isolation: set UMBRAL_TENANTS_TEST_PG \
             (or DATABASE_URL) to a test Postgres to run it"
        );
        return;
    };

    let pool = sqlx::PgPool::connect(&url).await.expect("connect pg");

    // Clean slate for a deterministic run: drop the two tenant schemas + the
    // public registry/migration tables this test owns. (Allowed: a test bypasses
    // make/run and owns its fixtures; we are NOT wiping a user DB.)
    for s in ["tenant_a", "tenant_b"] {
        sqlx::query(&format!("DROP SCHEMA IF EXISTS {s} CASCADE"))
            .execute(&pool)
            .await
            .expect("drop tenant schema");
    }
    for t in ["tpost", "tenant", "umbral_migrations"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
            .execute(&pool)
            .await
            .ok();
    }

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();

    // SHARED = the tenants registry only. The implicit "app" plugin (which owns
    // `TPost`) is therefore a TENANT app: `tpost` lands in each tenant schema
    // and the TenantRouter (installed by on_ready) schema-qualifies it.
    let plugin = TenantsPlugin::new().shared_apps(["tenants"]);

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .plugin(plugin)
        .model::<TPost>()
        .build()
        .expect("App::build");

    // Generate migrations from the registered models into a temp dir.
    let tmp = tempdir_migrations();
    umbral::migrate::make_in(std::path::Path::new(&tmp))
        .await
        .expect("make migrations");

    // Shared set for the SCHEMA migration: keep `tenants` (the registry) in
    // public, but treat `app` (which owns `tpost`) as a TENANT app so `tpost`
    // is created inside each tenant schema.
    let shared_for_schema: HashSet<String> = ["tenants"].iter().map(|s| s.to_string()).collect();

    // 1) Migrate the PUBLIC apps (the whole set) so the `tenant` registry table
    //    exists in public. run_in walks every plugin into the default pool.
    umbral::migrate::run_in(std::path::Path::new(&tmp))
        .await
        .expect("public migrate");

    // 2) Provision two tenant schemas and migrate the tenant app (`app`/`tpost`)
    //    into each via the core helper the management layer wraps.
    for name in ["tenant_a", "tenant_b"] {
        let schema = Schema::new(name).unwrap();
        // CREATE SCHEMA + migrate tenant apps into it.
        umbral::migrate::run_for_schema_in(
            std::path::Path::new(&tmp),
            &schema,
            &shared_for_schema,
        )
        .await
        .expect("schema migrate");
        // Register the tenant row in public (the resolution key).
        Tenant::objects()
            .create(Tenant {
                id: 0,
                schema_name: name.to_string(),
                name: name.to_string(),
                domain: format!("{name}.example.com"),
                is_active: true,
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("create tenant row");
    }

    // Both tenant schemas exist.
    for s in ["tenant_a", "tenant_b"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.schemata WHERE schema_name = $1)",
        )
        .bind(s)
        .fetch_one(&pool)
        .await
        .expect("schema exists query");
        assert!(exists, "schema {s} should exist");
    }

    // 3) Write one TPost under each tenant's scope. The TenantRouter (installed
    //    by the plugin's on_ready) schema-qualifies `tpost` to the active
    //    tenant's schema.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    umbral::db::route_context_scope(ctx_a, async {
        TPost::objects()
            .create(TPost {
                id: 0,
                title: "A's post".into(),
            })
            .await
            .expect("create A post");
    })
    .await;

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    umbral::db::route_context_scope(ctx_b, async {
        TPost::objects()
            .create(TPost {
                id: 0,
                title: "B's post".into(),
            })
            .await
            .expect("create B post");
    })
    .await;

    // 4) ISOLATION: A's ctx sees only A's row; B's ctx sees only B's row.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    let a_posts = umbral::db::route_context_scope(ctx_a, async {
        TPost::objects().fetch().await.expect("fetch A")
    })
    .await;
    assert_eq!(a_posts.len(), 1, "tenant A sees exactly its own row");
    assert_eq!(a_posts[0].title, "A's post");

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    let b_posts = umbral::db::route_context_scope(ctx_b, async {
        TPost::objects().fetch().await.expect("fetch B")
    })
    .await;
    assert_eq!(b_posts.len(), 1, "tenant B sees exactly its own row");
    assert_eq!(b_posts[0].title, "B's post");

    // Direct SQL cross-check: each schema's tpost has exactly one row.
    for (s, want) in [("tenant_a", "A's post"), ("tenant_b", "B's post")] {
        let titles: Vec<String> =
            sqlx::query_scalar(&format!("SELECT title FROM {s}.tpost ORDER BY id"))
                .fetch_all(&pool)
                .await
                .expect("direct select");
        assert_eq!(titles, vec![want.to_string()], "schema {s} isolated");
    }

    // 5) SHARED model: the `tenant` registry (public) is visible under BOTH
    //    tenants' contexts — shared data is NOT isolated.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    let seen_a = umbral::db::route_context_scope(ctx_a, async {
        Tenant::objects().count().await.expect("count tenants under A")
    })
    .await;
    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    let seen_b = umbral::db::route_context_scope(ctx_b, async {
        Tenant::objects().count().await.expect("count tenants under B")
    })
    .await;
    assert_eq!(seen_a, 2, "both tenant rows visible under A (shared registry)");
    assert_eq!(seen_b, 2, "both tenant rows visible under B (shared registry)");

    eprintln!("schema_per_tenant_isolation: PASS (2 schemas, isolated tenant rows, shared registry)");
}

/// A throwaway migrations dir under the OS temp dir, unique per run.
fn tempdir_migrations() -> String {
    let base = std::env::temp_dir().join(format!(
        "umbral-tenants-mig-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create temp migrations dir");
    base.to_string_lossy().into_owned()
}
