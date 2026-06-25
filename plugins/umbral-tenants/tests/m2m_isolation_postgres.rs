//! PG-gated proof of **M2M-relation isolation under schema-per-tenant** —
//! the deferred "#88b" slice of gaps2 #69.
//!
//! `#[ignore]` + skips cleanly unless a test Postgres is configured via
//! `UMBRAL_TENANTS_TEST_PG` (or `UMBRAL_TEST_POSTGRES_URL` / `DATABASE_URL`).
//! It mirrors `isolation_postgres.rs`'s gating, clean-slate, and provisioning
//! exactly — the only new surface is an **M2M relation on a tenant-owned model**.
//!
//! ## Tenancy choice (deliberate)
//!
//! BOTH the parent (`TArticle`) and the child (`TTag`) are TENANT-owned: only
//! the `tenants` registry is shared. The whole graph — `tarticle`, `ttag`, and
//! the auto-generated junction `tarticle_tags` — therefore lands inside each
//! tenant schema. That keeps every leg of the M2M (parent row, child row, the
//! junction's two FK references) resolving against the SAME schema under the
//! pinned `search_path`, so isolation is total.
//!
//! The harder case — a SHARED `TTag` with a junction that spans
//! `public.ttag` ↔ `tenant.tarticle` — is deliberately OUT of scope here: the
//! `CreateM2MTable` DDL renders bare table names (`REFERENCES "ttag"(...)`),
//! which under a pinned tenant `search_path` would resolve `ttag` in the tenant
//! schema and fail against a public-only child table. That cross-boundary
//! junction is a separate deferred edge (noted in gaps2 #69), not this test.
//!
//! ## What this proves
//!
//! 1. `run_for_schema_in` applies the tenant app's WHOLE migration into each
//!    schema — including the `CreateM2MTable` op — so the junction table exists
//!    in BOTH `tenant_a` and `tenant_b` (asserted via `information_schema`).
//! 2. The M2M typed API (`.add()` / `.fetch()`) routes the junction to the
//!    active tenant's schema (the ~15 `schema_qualified_table(junction)` calls
//!    in `orm/m2m.rs`), so tenant A's links are invisible under tenant B's scope
//!    and vice-versa.
//!
//! Run it:
//! ```text
//! UMBRAL_TENANTS_TEST_PG=postgres://app:apppass@localhost:5433/appdb \
//!   cargo test -p umbral-tenants --test m2m_isolation_postgres -- --ignored --nocapture
//! ```

#![allow(dead_code, private_interfaces)]

use std::collections::HashSet;

use umbral::db::{RouteContext, Schema, TenantKey};
use umbral::orm::M2M;
use umbral_tenants::{Tenant, TenantsPlugin};

/// Tenant-owned PARENT. NOT a shared app → `tarticle` (and its M2M junction)
/// land in each tenant's schema. The junction name the macro derives is
/// `<table>_<field>` → `tarticle_tags`.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "tarticle")]
pub struct TArticle {
    pub id: i64,
    pub title: String,
    /// The M2M field — skipped from FIELDS by the macro; the diff engine emits
    /// a `CreateM2MTable` for `tarticle_tags`, and the hydrate hook seeds
    /// parent_id + junction_table on every loaded row.
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<TTag>,
}

/// Tenant-owned CHILD. Also lands in each tenant schema → the whole M2M graph
/// is isolated per tenant.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "ttag")]
pub struct TTag {
    pub id: i64,
    pub label: String,
}

/// The junction the macro derives from `<parent_table>_<field>`.
const JUNCTION_TABLE: &str = "tarticle_tags";

fn pg_url() -> Option<String> {
    std::env::var("UMBRAL_TENANTS_TEST_PG")
        .or_else(|_| std::env::var("UMBRAL_TEST_POSTGRES_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| u.starts_with("postgres"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a test Postgres (UMBRAL_TENANTS_TEST_PG / DATABASE_URL)"]
async fn m2m_relation_isolated_per_tenant_schema() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping m2m_relation_isolated_per_tenant_schema: set UMBRAL_TENANTS_TEST_PG \
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
    for t in ["tarticle_tags", "tarticle", "ttag", "tenant", "umbral_migrations"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
            .execute(&pool)
            .await
            .ok();
    }

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();

    // SHARED = the tenants registry only. The implicit "app" plugin (which owns
    // `TArticle` + `TTag`) is therefore a TENANT app: the whole M2M graph lands
    // in each tenant schema and the TenantRouter schema-qualifies it.
    let plugin = TenantsPlugin::new().shared_apps(["tenants"]);

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .plugin(plugin)
        .model::<TArticle>()
        .model::<TTag>()
        .build()
        .expect("App::build");

    // Generate migrations from the registered models into a temp dir.
    let tmp = tempdir_migrations();
    umbral::migrate::make_in(std::path::Path::new(&tmp))
        .await
        .expect("make migrations");

    // Shared set for the SCHEMA migration: keep `tenants` (the registry) in
    // public; treat `app` (which owns the M2M graph) as a TENANT app.
    let shared_for_schema: HashSet<String> = ["tenants"].iter().map(|s| s.to_string()).collect();

    // 1) Migrate the PUBLIC apps so the `tenant` registry table exists.
    umbral::migrate::run_in(std::path::Path::new(&tmp))
        .await
        .expect("public migrate");

    // 2) Provision two tenant schemas; migrate the tenant app (article + tag +
    //    junction) into each.
    for name in ["tenant_a", "tenant_b"] {
        let schema = Schema::new(name).unwrap();
        umbral::migrate::run_for_schema_in(
            std::path::Path::new(&tmp),
            &schema,
            &shared_for_schema,
        )
        .await
        .expect("schema migrate");
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

    // The junction table must exist in BOTH tenant schemas — proof that
    // `run_for_schema_in` applied the `CreateM2MTable` op into each schema, not
    // just the base tables.
    for s in ["tenant_a", "tenant_b"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = $2)",
        )
        .bind(s)
        .bind(JUNCTION_TABLE)
        .fetch_one(&pool)
        .await
        .expect("junction-exists query");
        assert!(
            exists,
            "junction `{JUNCTION_TABLE}` must be created inside schema {s}"
        );
    }

    // 3) Under each tenant's scope, create a tag + an article and attach the tag
    //    (M2M). The typed `.add()` schema-qualifies the junction to the active
    //    tenant's schema.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    umbral::db::route_context_scope(ctx_a, async {
        let tag = TTag::objects()
            .create(TTag {
                id: 0,
                label: "rust-a".into(),
            })
            .await
            .expect("create A tag");
        let article = TArticle::objects()
            .create(TArticle {
                id: 0,
                title: "A's article".into(),
                tags: M2M::empty(),
            })
            .await
            .expect("create A article");
        article.tags.add(&tag).await.expect("attach A tag");
    })
    .await;

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    umbral::db::route_context_scope(ctx_b, async {
        let tag = TTag::objects()
            .create(TTag {
                id: 0,
                label: "rust-b".into(),
            })
            .await
            .expect("create B tag");
        let article = TArticle::objects()
            .create(TArticle {
                id: 0,
                title: "B's article".into(),
                tags: M2M::empty(),
            })
            .await
            .expect("create B article");
        article.tags.add(&tag).await.expect("attach B tag");
    })
    .await;

    // 4a) ISOLATION via the typed object graph: under A's scope the article's
    //     tags == [A's tag]; the junction holds exactly A's link; B's rows are
    //     invisible.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    umbral::db::route_context_scope(ctx_a, async {
        let articles = TArticle::objects().fetch().await.expect("fetch A articles");
        assert_eq!(articles.len(), 1, "tenant A sees exactly its own article");
        assert_eq!(articles[0].title, "A's article");

        let tags = articles[0].tags.fetch().await.expect("fetch A tags");
        assert_eq!(tags.len(), 1, "A's article has exactly its own tag");
        assert_eq!(tags[0].label, "rust-a", "A's tag is A's, not B's");

        let tag_count = TTag::objects().count().await.expect("count A tags");
        assert_eq!(tag_count, 1, "tenant A sees exactly one tag (its own)");
    })
    .await;

    // 4b) Likewise under B's scope.
    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    umbral::db::route_context_scope(ctx_b, async {
        let articles = TArticle::objects().fetch().await.expect("fetch B articles");
        assert_eq!(articles.len(), 1, "tenant B sees exactly its own article");
        assert_eq!(articles[0].title, "B's article");

        let tags = articles[0].tags.fetch().await.expect("fetch B tags");
        assert_eq!(tags.len(), 1, "B's article has exactly its own tag");
        assert_eq!(tags[0].label, "rust-b", "B's tag is B's, not A's");

        let tag_count = TTag::objects().count().await.expect("count B tags");
        assert_eq!(tag_count, 1, "tenant B sees exactly one tag (its own)");
    })
    .await;

    // 4c) Direct SQL cross-check: each schema's junction has exactly one row,
    //     and the junction in tenant_a is invisible to tenant_b (separate
    //     tables). (Raw SQL allowed: a test owns its fixtures / assertions.)
    for s in ["tenant_a", "tenant_b"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {s}.{JUNCTION_TABLE}"))
            .fetch_one(&pool)
            .await
            .expect("count junction rows");
        assert_eq!(n, 1, "schema {s}'s junction holds exactly its own link");
    }

    eprintln!(
        "m2m_relation_isolated_per_tenant_schema: PASS \
         (junction created in both schemas, M2M links isolated per tenant)"
    );
}

/// A throwaway migrations dir under the OS temp dir, unique per run.
fn tempdir_migrations() -> String {
    let base = std::env::temp_dir().join(format!(
        "umbral-tenants-m2m-mig-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create temp migrations dir");
    base.to_string_lossy().into_owned()
}
