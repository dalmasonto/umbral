//! PG-gated proof of a **boundary-spanning data migration** — the
//! shared/tenant `RunSql` slice of gaps2 #69.
//!
//! `#[ignore]` + self-skips cleanly unless a test Postgres is configured via
//! `UMBRA_TENANTS_TEST_PG` (or `UMBRA_TEST_POSTGRES_URL` / `DATABASE_URL`).
//!
//! ## The shape
//!
//! - `Plan` is owned by a SHARED app (`catalog`) → `plan` lives in `public`,
//!   a cross-tenant lookup seeded once.
//! - `Subscription` is a TENANT app (`app`) → `subscription` lives in each
//!   tenant schema.
//! - A hand-authored `RunSql` data migration on the TENANT `app` reads the
//!   shared `public.plan` lookup and writes one `subscription` row per plan
//!   into the *current* tenant schema. Because the schema-migrate op loop
//!   runs every op under `search_path = "<schema>", public`, the INSERT lands
//!   in the tenant schema while the SELECT reads `public.plan` — the
//!   boundary-spanning data migration.
//!
//! ## What this proves
//!
//! 1. A tenant-app `RunSql` is applied PER tenant schema (each schema's
//!    `subscription` gets its own rows).
//! 2. Those rows are DERIVED from the shared `public.plan` data
//!    (boundary-spanning: the tenant write reads the public lookup).
//! 3. The rows are ISOLATED per tenant (tenant_a's subscriptions are
//!    invisible to tenant_b and vice-versa).
//!
//! Run it:
//! ```text
//! UMBRA_TENANTS_TEST_PG=postgres://app:apppass@localhost:5433/appdb \
//!   cargo test -p umbra-tenants --test data_migration_boundary_postgres \
//!   -- --ignored --nocapture
//! ```

#![allow(dead_code, private_interfaces)]

use std::collections::HashSet;
use std::path::Path;

use umbra::db::Schema;
use umbra::migrate::{MigrationFile, ModelMeta, Operation};
use umbra::prelude::Plugin;
use umbra_tenants::{Tenant, TenantsPlugin};

/// SHARED lookup (the `catalog` app) → lives in `public`.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "plan")]
pub struct Plan {
    pub id: i64,
    pub code: String,
    pub price_cents: i64,
}

/// TENANT model (the `app` plugin) → lives in each tenant schema. The
/// `RunSql` data migration writes one row here per shared plan.
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, umbra::orm::Model)]
#[umbra(table = "subscription")]
pub struct Subscription {
    pub id: i64,
    pub plan_code: String,
    pub amount_cents: i64,
}

/// Minimal in-test plugin owning `Plan` under the shared app label
/// `catalog`.
struct CatalogPlugin;

impl Plugin for CatalogPlugin {
    fn name(&self) -> &'static str {
        "catalog"
    }
    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<Plan>()]
    }
}

fn pg_url() -> Option<String> {
    std::env::var("UMBRA_TENANTS_TEST_PG")
        .or_else(|_| std::env::var("UMBRA_TEST_POSTGRES_URL"))
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| u.starts_with("postgres"))
}

/// Hand-author the tenant `app`'s `RunSql` data migration. Forward SQL
/// reads the SHARED `public.plan` lookup and inserts a derived
/// `subscription` row per plan. Under the schema-migrate `search_path =
/// "<schema>", public`, the bare `subscription` resolves to the tenant
/// schema (it's first) and `public.plan` is read explicitly.
fn write_boundary_data_migration(dir: &Path, snapshot: umbra::migrate::Snapshot) {
    let file = MigrationFile {
        id: "0002_seed_subscriptions".to_string(),
        plugin: "app".to_string(),
        depends_on: Vec::new(),
        operations: vec![Operation::RunSql {
            sql: "INSERT INTO subscription (plan_code, amount_cents) \
                  SELECT code, price_cents FROM public.plan"
                .to_string(),
            reverse_sql: Some("DELETE FROM subscription".to_string()),
        }],
        snapshot_after: snapshot,
    };
    let plugin_dir = dir.join("app");
    std::fs::create_dir_all(&plugin_dir).expect("create app dir");
    let json = serde_json::to_string_pretty(&file).expect("serialize data migration");
    std::fs::write(plugin_dir.join("0002_seed_subscriptions.json"), json)
        .expect("write data migration");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a test Postgres (UMBRA_TENANTS_TEST_PG / DATABASE_URL)"]
async fn data_migration_reads_shared_public_writes_tenant_rows() {
    let Some(url) = pg_url() else {
        eprintln!(
            "skipping data_migration_reads_shared_public_writes_tenant_rows: set \
             UMBRA_TENANTS_TEST_PG (or DATABASE_URL) to a test Postgres to run it"
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
    for t in ["subscription", "plan", "tenant", "umbra_migrations"] {
        sqlx::query(&format!("DROP TABLE IF EXISTS public.{t} CASCADE"))
            .execute(&pool)
            .await
            .ok();
    }

    let mut settings = umbra::Settings::from_env().expect("settings");
    settings.database_url = url.clone();

    // SHARED = tenants registry + `catalog` (owns `plan`). The `app` plugin
    // (owns `Subscription` + the data migration) is a TENANT app.
    let plugin = TenantsPlugin::new().shared_apps(["tenants", "catalog"]);

    umbra::App::builder()
        .settings(settings)
        .database("default", pool.clone())
        .plugin(plugin)
        .plugin(CatalogPlugin)
        .model::<Subscription>()
        .build()
        .expect("App::build");

    let tmp = tempdir_migrations();
    umbra::migrate::make_in(Path::new(&tmp))
        .await
        .expect("make migrations");

    let shared_for_schema: HashSet<String> =
        ["tenants", "catalog"].iter().map(|s| s.to_string()).collect();

    // 1) Migrate the PUBLIC apps: `tenant` registry + the shared `plan`.
    umbra::migrate::run_in(Path::new(&tmp))
        .await
        .expect("public migrate");

    // Seed the SHARED lookup in public (ORM; no tenant ctx → routes to
    // public).
    for (code, price) in [("free", 0i64), ("pro", 1900), ("team", 4900)] {
        Plan::objects()
            .create(Plan {
                id: 0,
                code: code.to_string(),
                price_cents: price,
            })
            .await
            .expect("seed shared plan");
    }

    // Hand-author the tenant data migration (after `make_in` so it doesn't
    // collide with the auto-detected 0001 schema migration). Carry the app
    // plugin's current snapshot forward — a data migration has no schema
    // effect.
    let app_snapshot = umbra::migrate::Snapshot::current_for("app");
    write_boundary_data_migration(Path::new(&tmp), app_snapshot);

    // 2) Provision two tenant schemas; migrate the tenant app into each. The
    //    schema migration creates `subscription` AND runs the boundary-
    //    spanning `RunSql` (reads public.plan, writes tenant rows).
    for name in ["tenant_a", "tenant_b"] {
        let schema = Schema::new(name).unwrap();
        umbra::migrate::run_for_schema_in(Path::new(&tmp), &schema, &shared_for_schema)
            .await
            .expect("schema migrate incl. boundary RunSql");
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

    // Assert 1 + 2: each tenant schema's `subscription` got rows DERIVED from
    // the shared public.plan lookup (3 plans → 3 subscriptions per schema).
    for s in ["tenant_a", "tenant_b"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {s}.subscription"))
            .fetch_one(&pool)
            .await
            .expect("count tenant subscriptions");
        assert_eq!(
            n, 3,
            "schema {s} got one subscription per shared plan (boundary-spanning)"
        );

        // The amounts came from public.plan.price_cents — prove the read
        // crossed the boundary, not a hard-coded constant.
        let pro_amount: i64 = sqlx::query_scalar(&format!(
            "SELECT amount_cents FROM {s}.subscription WHERE plan_code = 'pro'"
        ))
        .fetch_one(&pool)
        .await
        .expect("pro subscription exists");
        assert_eq!(
            pro_amount, 1900,
            "the tenant row's amount derives from public.plan (pro = 1900)"
        );
    }

    // Assert 3: isolation — subscriptions are per-schema, not shared. Insert
    // an extra tenant-a-only subscription directly and confirm tenant_b's
    // count is unaffected.
    sqlx::query("INSERT INTO tenant_a.subscription (plan_code, amount_cents) VALUES ('extra', 1)")
        .execute(&pool)
        .await
        .expect("insert tenant_a-only row");
    let a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenant_a.subscription")
        .fetch_one(&pool)
        .await
        .expect("count a");
    let b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tenant_b.subscription")
        .fetch_one(&pool)
        .await
        .expect("count b");
    assert_eq!(a, 4, "tenant_a sees its own 3 derived + 1 extra");
    assert_eq!(b, 3, "tenant_b is isolated from tenant_a's extra row");

    // And `subscription` was NOT created in public (it's a tenant table).
    let sub_in_public: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'subscription')",
    )
    .fetch_one(&pool)
    .await
    .expect("subscription-in-public query");
    assert!(
        !sub_in_public,
        "the tenant `subscription` table must not be created in public"
    );

    eprintln!(
        "data_migration_reads_shared_public_writes_tenant_rows: PASS \
         (per-tenant RunSql derived from shared public.plan, isolated per schema)"
    );
}

/// A throwaway migrations dir under the OS temp dir, unique per run.
fn tempdir_migrations() -> String {
    let base = std::env::temp_dir().join(format!(
        "umbra-tenants-data-mig-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::create_dir_all(&base).expect("create temp migrations dir");
    base.to_string_lossy().into_owned()
}
