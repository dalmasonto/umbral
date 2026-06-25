//! PG-gated proof of **cross-boundary M2M** — the "tenant parent ↔ SHARED
//! child" slice of gaps2 #69, the harder case the sibling
//! `m2m_isolation_postgres.rs` deliberately left out of scope.
//!
//! `#[ignore]` + self-skips cleanly unless a test Postgres is configured via
//! `UMBRAL_TENANTS_TEST_PG` (or `UMBRAL_TEST_POSTGRES_URL` / `DATABASE_URL`).
//! Mirrors the isolation test's gating, clean-slate, and provisioning; the
//! new surface is the **shared child**.
//!
//! ## The shape (deliberate)
//!
//! - `XTag` is owned by a SHARED app (`sharedtags`) → its table lands in
//!   `public`. Every tenant sees the SAME tag rows.
//! - `XArticle` (with `M2M<XTag>`) is a TENANT app (`app`) → `xarticle` and
//!   the auto-generated junction `xarticle_tags` land in each tenant schema.
//! - The junction's FK `child_id REFERENCES "xtag"(...)` is rendered BARE.
//!   Under the schema-migrate `search_path = "<schema>", public`, that bare
//!   `xtag` resolves to `public.xtag` (the tenant schema has no `xtag`), so
//!   the cross-boundary FK *creates without erroring*. Before the
//!   public-fallback search_path, this migration failed `relation "xtag"
//!   does not exist` — that failure is exactly what this test guards.
//!
//! ## What this proves
//!
//! 1. `run_for_schema_in` creates the junction in BOTH tenant schemas WITHOUT
//!    erroring — the cross-boundary FK to `public.xtag` resolves.
//! 2. `xtag` exists in `public`, NOT in the tenant schemas.
//! 3. The CHILD is shared: a single `public.xtag` row is linked by BOTH
//!    tenants' articles (same id/label visible to each), while the LINKS are
//!    isolated (one junction row per tenant schema, neither sees the other's).
//!
//! Run it:
//! ```text
//! UMBRAL_TENANTS_TEST_PG=postgres://app:apppass@localhost:5433/appdb \
//!   cargo test -p umbral-tenants --test m2m_cross_boundary_postgres -- --ignored --nocapture
//! ```

#![allow(dead_code, private_interfaces)]

use std::collections::HashSet;

use umbral::db::{RouteContext, Schema, TenantKey};
use umbral::migrate::ModelMeta;
use umbral::orm::M2M;
use umbral::prelude::Plugin;
use umbral_tenants::{Tenant, TenantsPlugin};

/// Tenant-owned PARENT (app plugin). Its table + the M2M junction land in
/// each tenant schema. The junction name the macro derives is
/// `<table>_<field>` → `xarticle_tags`.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "xarticle")]
pub struct XArticle {
    pub id: i64,
    pub title: String,
    #[sqlx(skip)]
    #[serde(skip)]
    pub tags: M2M<XTag>,
}

/// SHARED CHILD — owned by the `sharedtags` plugin, which is in
/// `shared_apps`, so `xtag` lands in `public`. Every tenant sees the same
/// rows.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbral::orm::Model)]
#[umbral(table = "xtag")]
pub struct XTag {
    pub id: i64,
    pub label: String,
}

/// A minimal in-test plugin whose ONLY job is to own `XTag` under the app
/// label `sharedtags`, so the shared/tenant split (which is by plugin name)
/// keeps `xtag` in `public`.
struct SharedTagsPlugin;

impl Plugin for SharedTagsPlugin {
    fn name(&self) -> &'static str {
        "sharedtags"
    }
    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<XTag>()]
    }
}

/// The macro-derived junction name (`<parent_table>_<field>`).
const JUNCTION_TABLE: &str = "xarticle_tags";

fn pg_url() -> Option<String> {
    std::env::var("UMBRAL_TENANTS_TEST_PG")
        .or_else(|_| std::env::var("UMBRAL_TEST_POSTGRES_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| u.starts_with("postgres"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a test Postgres (UMBRAL_TENANTS_TEST_PG / DATABASE_URL)"]
async fn m2m_cross_boundary_tenant_parent_shared_child() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping m2m_cross_boundary_tenant_parent_shared_child: set \
             UMBRAL_TENANTS_TEST_PG (or DATABASE_URL) to a test Postgres to run it"
        );
        return;
    };

    let pool = sqlx::PgPool::connect(&url).await.expect("connect pg");

    // Clean slate. (Allowed: a test owns its fixtures; not a user DB.)
    for s in ["tenant_a", "tenant_b"] {
        sqlx::query(&format!("DROP SCHEMA IF EXISTS {s} CASCADE"))
            .execute(&pool)
            .await
            .expect("drop tenant schema");
    }
    for t in [
        "xarticle_tags",
        "xarticle",
        "xtag",
        "tenant",
        "umbral_migrations",
    ] {
        sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
            .execute(&pool)
            .await
            .ok();
    }

    let mut settings = umbral::Settings::from_env().expect("settings");
    settings.database_url = url.clone();

    // SHARED = the tenants registry + the `sharedtags` app (owns `xtag`).
    // The `app` plugin (owns `XArticle` + the junction) is a TENANT app.
    let plugin = TenantsPlugin::new().shared_apps(["tenants", "sharedtags"]);

    umbral::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .plugin(plugin)
        .plugin(SharedTagsPlugin)
        .model::<XArticle>()
        .build()
        .expect("App::build");

    let tmp = tempdir_migrations();
    umbral::migrate::make_in(std::path::Path::new(&tmp))
        .await
        .expect("make migrations");

    // Shared apps for the SCHEMA migration: `tenants` (registry) +
    // `sharedtags` (the `xtag` lookup) stay in `public`.
    let shared_for_schema: HashSet<String> = ["tenants", "sharedtags"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // 1) Migrate ONLY the SHARED apps into public: the `tenant` registry +
    //    `sharedtags` (the `xtag` lookup). Using `run_shared_in` (not the
    //    unfiltered `run_in`) keeps the TENANT app's `xarticle` + its M2M
    //    junction OUT of public — they belong only in each tenant schema, where
    //    the junction's FK to `public.xtag` resolves via the `<schema>, public`
    //    search_path. (Unfiltered `run_in` would create the junction in public
    //    with an FK to `xtag` before `xtag` exists there → it would error.)
    umbral::migrate::run_shared_in(std::path::Path::new(&tmp), &shared_for_schema)
        .await
        .expect("public (shared-only) migrate");

    // 2) Provision two tenant schemas; migrate the tenant app (article +
    //    junction) into each. This MUST NOT error: the junction's FK to the
    //    public `xtag` resolves via the `<schema>, public` search_path.
    for name in ["tenant_a", "tenant_b"] {
        let schema = Schema::new(name).unwrap();
        umbral::migrate::run_for_schema_in(std::path::Path::new(&tmp), &schema, &shared_for_schema)
            .await
            .expect("schema migrate (cross-boundary FK must resolve to public.xtag)");
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

    // Assert 1: the junction exists in BOTH tenant schemas (proof the
    // cross-boundary CreateM2MTable applied without erroring).
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

    // Assert 2: `xtag` lives in `public`, NOT in the tenant schemas.
    let xtag_in_public: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'xtag')",
    )
    .fetch_one(&pool)
    .await
    .expect("xtag-in-public query");
    assert!(xtag_in_public, "the shared child `xtag` must live in public");
    for s in ["tenant_a", "tenant_b"] {
        let xtag_in_tenant: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
             WHERE table_schema = $1 AND table_name = 'xtag')",
        )
        .bind(s)
        .fetch_one(&pool)
        .await
        .expect("xtag-in-tenant query");
        assert!(
            !xtag_in_tenant,
            "the shared child `xtag` must NOT be duplicated into schema {s}"
        );
    }

    // 3) Create ONE shared tag in public (no tenant ctx → router routes the
    //    shared `xtag` to public). Both tenants will link to this SAME row.
    let shared_tag = XTag::objects()
        .create(XTag {
            id: 0,
            label: "rust".into(),
        })
        .await
        .expect("create shared tag in public");
    assert!(shared_tag.id > 0, "shared tag got a real id");

    // Under tenant A's scope: create A's article, attach the SHARED tag.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    umbral::db::route_context_scope(ctx_a, async {
        let article = XArticle::objects()
            .create(XArticle {
                id: 0,
                title: "A's article".into(),
                tags: M2M::empty(),
            })
            .await
            .expect("create A article");
        article
            .tags
            .add(&shared_tag)
            .await
            .expect("attach shared tag under A");
    })
    .await;

    // Under tenant B's scope: create B's article, attach the SAME shared tag.
    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    umbral::db::route_context_scope(ctx_b, async {
        let article = XArticle::objects()
            .create(XArticle {
                id: 0,
                title: "B's article".into(),
                tags: M2M::empty(),
            })
            .await
            .expect("create B article");
        article
            .tags
            .add(&shared_tag)
            .await
            .expect("attach shared tag under B");
    })
    .await;

    // Assert 3a: each tenant's article resolves its tag to the SAME public
    // row (shared child) — same id + label visible to both.
    let ctx_a = RouteContext::new().with_tenant(TenantKey::new("tenant_a"));
    umbral::db::route_context_scope(ctx_a, async {
        let articles = XArticle::objects().fetch().await.expect("fetch A articles");
        assert_eq!(articles.len(), 1, "tenant A sees exactly its own article");
        let tags = articles[0].tags.fetch().await.expect("fetch A tags");
        assert_eq!(tags.len(), 1, "A's article has one tag (the shared one)");
        assert_eq!(tags[0].id, shared_tag.id, "A links the SHARED public tag");
        assert_eq!(tags[0].label, "rust");
    })
    .await;

    let ctx_b = RouteContext::new().with_tenant(TenantKey::new("tenant_b"));
    umbral::db::route_context_scope(ctx_b, async {
        let articles = XArticle::objects().fetch().await.expect("fetch B articles");
        assert_eq!(articles.len(), 1, "tenant B sees exactly its own article");
        let tags = articles[0].tags.fetch().await.expect("fetch B tags");
        assert_eq!(tags.len(), 1, "B's article has one tag (the shared one)");
        assert_eq!(
            tags[0].id, shared_tag.id,
            "B links the SAME shared public tag as A"
        );
        assert_eq!(tags[0].label, "rust");
    })
    .await;

    // Assert 3b: the LINKS are isolated — exactly one junction row per tenant
    // schema, neither sees the other's. (Raw SQL: a test owns its
    // assertions.)
    for s in ["tenant_a", "tenant_b"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {s}.{JUNCTION_TABLE}"))
            .fetch_one(&pool)
            .await
            .expect("count junction rows");
        assert_eq!(
            n, 1,
            "schema {s}'s junction holds exactly its own link to the shared tag"
        );
    }

    // And the public `xtag` holds exactly the one shared row (not duplicated
    // per tenant).
    let public_tags: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM public.xtag")
        .fetch_one(&pool)
        .await
        .expect("count public tags");
    assert_eq!(public_tags, 1, "exactly one shared tag row in public");

    eprintln!(
        "m2m_cross_boundary_tenant_parent_shared_child: PASS \
         (shared public.xtag, per-tenant junctions, cross-boundary FK resolved)"
    );
}

/// A throwaway migrations dir under the OS temp dir, unique per run.
fn tempdir_migrations() -> String {
    let base = std::env::temp_dir().join(format!(
        "umbral-tenants-m2m-xb-mig-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create temp migrations dir");
    base.to_string_lossy().into_owned()
}
