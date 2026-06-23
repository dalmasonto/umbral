//! PG-gated proof of **database-per-tenant** cross-database isolation
//! ([`TenantStrategy::Database`]).
//!
//! `#[ignore]` + skips cleanly unless the parent agent provides the env. The
//! real proof:
//!
//! 1. Build an app with `TenantsPlugin::new().strategy(Database).shared_apps(["tenants"])` plus a tenant-owned model (`TPost`) and the shared `Tenant` registry; wire the **default/registry** database.
//! 2. Migrate the default DB (the `tenant` registry lands there).
//! 3. `register_tenant_database` for tenant A (pool → DB A) and tenant B (pool → DB B). Each onboard inserts the registry row in the default DB, registers the runtime pool under the tenant's alias, and migrates the tenant app (`app`/`tpost`) into that tenant's own database.
//! 4. Under each tenant's `scope`, write a `TPost` row.
//! 5. Assert **cross-database isolation**: A's row lives in DB A only, B's in DB B only, neither is visible from the other, and the shared `Tenant` registry (in the default DB) lists both.
//!
//! ## What the parent agent must provide
//!
//! **THREE Postgres databases** (or two tenant DBs + a registry DB), via env:
//!
//! - `UMBRA_TENANTS_TEST_PG` — the **default / registry** database URL
//!   (required; if unset the test self-skips).
//! - `UMBRA_TENANTS_TEST_PG_A` — tenant **A**'s database URL.
//! - `UMBRA_TENANTS_TEST_PG_B` — tenant **B**'s database URL.
//!
//! If `_A` / `_B` are unset, the test derives them from the base URL by swapping
//! the database-name path segment to `<dbname>_tnt_a` / `<dbname>_tnt_b`. Those
//! derived databases must already exist (the framework does not `CREATE
//! DATABASE` — that's an ops concern, and it can't run in a transaction). When
//! derivation can't produce distinct, connectable URLs the test self-skips with
//! a message naming the vars to set.
//!
//! Run it:
//! ```text
//! UMBRA_TENANTS_TEST_PG=postgres://app:apppass@localhost:5433/appdb \
//! UMBRA_TENANTS_TEST_PG_A=postgres://app:apppass@localhost:5433/tnt_a \
//! UMBRA_TENANTS_TEST_PG_B=postgres://app:apppass@localhost:5433/tnt_b \
//!   cargo test -p umbra-tenants --test db_per_tenant_postgres -- --ignored --nocapture
//! ```

use umbra::db::{DbPool, RouteContext, TenantKey};
use umbra_tenants::{Tenant, TenantStrategy, TenantsPlugin};

/// A tenant-owned model. NOT a shared app → in Database mode its `tpost` table +
/// rows live in each tenant's own database, isolated per tenant.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "tpost")]
pub struct TPost {
    pub id: i64,
    pub title: String,
}

fn base_url() -> Option<String> {
    std::env::var("UMBRA_TENANTS_TEST_PG")
        .or_else(|_| std::env::var("UMBRA_TEST_POSTGRES_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| u.starts_with("postgres"))
}

/// Derive a tenant DB URL by swapping the database-name path segment. e.g.
/// `postgres://u:p@h:5433/appdb` + `tnt_a` → `postgres://u:p@h:5433/appdb_tnt_a`.
fn derive_url(base: &str, suffix: &str) -> Option<String> {
    // Split off any `?query`.
    let (head, query) = match base.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (base, None),
    };
    let slash = head.rfind('/')?;
    let dbname = &head[slash + 1..];
    if dbname.is_empty() {
        return None;
    }
    let mut out = format!("{}/{}_{}", &head[..slash], dbname, suffix);
    if let Some(q) = query {
        out.push('?');
        out.push_str(q);
    }
    Some(out)
}

fn tenant_url(var: &str, base: &str, suffix: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|u| u.starts_with("postgres"))
        .or_else(|| derive_url(base, suffix))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs three test Postgres DBs (UMBRA_TENANTS_TEST_PG[_A|_B])"]
async fn database_per_tenant_isolation() {
    let Some(base) = base_url() else {
        eprintln!(
            "skipping database_per_tenant_isolation: set UMBRA_TENANTS_TEST_PG (the default/registry \
             DB) plus UMBRA_TENANTS_TEST_PG_A and UMBRA_TENANTS_TEST_PG_B (the two tenant DBs) to run \
             it. The parent agent must provide THREE Postgres databases (or two tenant DBs that the \
             base URL's db-name suffixing can reach)."
        );
        return;
    };
    let (Some(url_a), Some(url_b)) = (
        tenant_url("UMBRA_TENANTS_TEST_PG_A", &base, "tnt_a"),
        tenant_url("UMBRA_TENANTS_TEST_PG_B", &base, "tnt_b"),
    ) else {
        eprintln!(
            "skipping database_per_tenant_isolation: could not resolve the two tenant DB URLs. Set \
             UMBRA_TENANTS_TEST_PG_A and UMBRA_TENANTS_TEST_PG_B to two distinct, existing Postgres \
             databases."
        );
        return;
    };
    if url_a == url_b || url_a == base || url_b == base {
        eprintln!(
            "skipping database_per_tenant_isolation: the registry DB and the two tenant DBs must be \
             three DISTINCT databases (got overlap). Set UMBRA_TENANTS_TEST_PG[_A|_B] explicitly."
        );
        return;
    }

    // Connect all three. If a tenant DB can't be reached (doesn't exist), skip
    // cleanly — the framework does not CREATE DATABASE.
    let registry = sqlx::PgPool::connect(&base).await.expect("connect registry pg");
    let Ok(pool_a) = sqlx::PgPool::connect(&url_a).await else {
        eprintln!(
            "skipping database_per_tenant_isolation: tenant DB A ({url_a}) is unreachable; create it \
             first (the framework does not CREATE DATABASE)."
        );
        return;
    };
    let Ok(pool_b) = sqlx::PgPool::connect(&url_b).await else {
        eprintln!(
            "skipping database_per_tenant_isolation: tenant DB B ({url_b}) is unreachable; create it \
             first (the framework does not CREATE DATABASE)."
        );
        return;
    };

    // Clean slate for a deterministic run (allowed: the test owns its fixtures
    // and bypasses make/run; we are NOT wiping a user DB).
    for t in ["tenant", "umbra_migrations", "tpost"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
            .execute(&registry)
            .await
            .ok();
    }
    for p in [&pool_a, &pool_b] {
        for t in ["tpost", "umbra_migrations"] {
            sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
                .execute(p)
                .await
                .ok();
        }
    }

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = base.clone();

    // SHARED = the tenants registry only. The implicit "app" plugin (owns
    // `TPost`) is therefore a TENANT app → its table + rows live in each
    // tenant's own database.
    let plugin = TenantsPlugin::new()
        .strategy(TenantStrategy::Database)
        .shared_apps(["tenants"]);

    umbra::App::builder()
        .settings(settings)
        .database("default", registry.clone())
        .plugin(plugin)
        .model::<TPost>()
        .build()
        .expect("App::build");

    // Generate migrations from the registered models into a temp dir.
    let tmp = tempdir_migrations();
    umbra::migrate::make_in(std::path::Path::new(&tmp))
        .await
        .expect("make migrations");

    // 1) Migrate the default/registry DB so the `tenant` table exists there.
    umbra::migrate::run_in(std::path::Path::new(&tmp))
        .await
        .expect("registry migrate");

    // 2) Onboard the two tenants. register_tenant_database inserts the registry
    //    row (in the default DB), registers the runtime pool, and migrates the
    //    tenant app into that tenant's own database via migrate_apps_into_pool_in.
    //    We call the lower-level steps with the temp migrations dir so the test
    //    doesn't touch cwd.
    let shared = std::collections::HashSet::from(["tenants".to_string()]);
    for (alias, domain, pool) in [
        ("acme_db", "acme.localhost", DbPool::Postgres(pool_a.clone())),
        ("globex_db", "globex.localhost", DbPool::Postgres(pool_b.clone())),
    ] {
        // Registry row in the default DB (no tenant ctx → routes to default).
        Tenant::objects()
            .create(Tenant {
                id: 0,
                schema_name: alias.to_string(),
                name: alias.to_string(),
                domain: domain.to_string(),
                is_active: true,
                created_at: chrono::Utc::now(),
            })
            .await
            .expect("create tenant row");
        umbra::db::register_tenant_pool(alias, pool);
        let n = umbra::migrate::migrate_apps_into_pool_in(
            std::path::Path::new(&tmp),
            alias,
            &shared,
        )
        .await
        .expect("migrate tenant DB");
        assert!(n >= 1, "tenant {alias} should have applied >=1 migration");
    }

    // 3) Write one TPost under each tenant's scope. The TenantRouter (Database
    //    mode, installed by on_ready) routes `tpost` to the tenant's pool.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("acme_db"));
    umbra::db::route_context_scope(ctx_a, async {
        TPost::objects()
            .create(TPost { id: 0, title: "A's post".into() })
            .await
            .expect("create A post");
    })
    .await;

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("globex_db"));
    umbra::db::route_context_scope(ctx_b, async {
        TPost::objects()
            .create(TPost { id: 0, title: "B's post".into() })
            .await
            .expect("create B post");
    })
    .await;

    // 4a) ORM round-trip: A's ctx sees only A's row; B's only B's.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("acme_db"));
    let a_posts = umbra::db::route_context_scope(ctx_a, async {
        TPost::objects().fetch().await.expect("fetch A")
    })
    .await;
    assert_eq!(a_posts.len(), 1, "tenant A sees exactly its own row");
    assert_eq!(a_posts[0].title, "A's post");

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("globex_db"));
    let b_posts = umbra::db::route_context_scope(ctx_b, async {
        TPost::objects().fetch().await.expect("fetch B")
    })
    .await;
    assert_eq!(b_posts.len(), 1, "tenant B sees exactly its own row");
    assert_eq!(b_posts[0].title, "B's post");

    // 4b) Cross-DATABASE check via direct SQL: each tenant's row is in its OWN
    //     database only, and invisible from the other tenant's DB.
    let titles_a: Vec<String> =
        sqlx::query_scalar("SELECT title FROM public.tpost ORDER BY id")
            .fetch_all(&pool_a)
            .await
            .expect("direct select A");
    let titles_b: Vec<String> =
        sqlx::query_scalar("SELECT title FROM public.tpost ORDER BY id")
            .fetch_all(&pool_b)
            .await
            .expect("direct select B");
    assert_eq!(titles_a, vec!["A's post".to_string()], "DB A holds only A's row");
    assert_eq!(titles_b, vec!["B's post".to_string()], "DB B holds only B's row");

    // The registry DB has NO tpost rows of its own (the tenant app was never
    // migrated/written there — it's a tenant app). The table may not even exist
    // in the registry DB; either "no table" or "zero rows" proves isolation.
    let registry_tpost: Option<i64> =
        sqlx::query_scalar("SELECT count(*) FROM public.tpost")
            .fetch_one(&registry)
            .await
            .ok();
    if let Some(c) = registry_tpost {
        assert_eq!(c, 0, "registry DB must hold no tenant rows");
    }

    // 5) SHARED registry: the default DB lists BOTH tenants. Shared data lives
    //    with the app, not in any tenant DB.
    let n_tenants: i64 = sqlx::query_scalar("SELECT count(*) FROM public.tenant")
        .fetch_one(&registry)
        .await
        .expect("count tenant registry");
    assert_eq!(n_tenants, 2, "registry lists both onboarded tenants");

    // And the tenant DBs hold NO copy of the registry table.
    for (label, p) in [("A", &pool_a), ("B", &pool_b)] {
        let has_tenant_table: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema='public' AND table_name='tenant')",
        )
        .fetch_one(p)
        .await
        .expect("tenant-table existence check");
        assert!(
            !has_tenant_table,
            "tenant DB {label} must NOT carry the shared `tenant` registry table"
        );
    }

    eprintln!(
        "database_per_tenant_isolation: PASS (2 separate databases, isolated tenant rows, shared \
         registry in the default DB)"
    );
}

/// A throwaway migrations dir under the OS temp dir, unique per run.
fn tempdir_migrations() -> String {
    let base = std::env::temp_dir().join(format!(
        "umbra-tenants-dbmig-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create temp migrations dir");
    base.to_string_lossy().into_owned()
}
