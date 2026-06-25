//! Schema-per-tenant multitenancy with `umbral-tenants`.
//!
//! One Postgres database, one schema per tenant, a shared `public` for the
//! tenant registry. A `Note` model is tenant-owned: each tenant's notes live in
//! its own schema and are invisible to other tenants. The `TenantRouter`
//! (installed by `TenantsPlugin`) schema-qualifies `note` to the active
//! tenant's schema with zero extra round-trips.
//!
//! ## Run it
//!
//! Schema-per-tenant is Postgres-only. Point `UMBRAL_DATABASE_URL` at a Postgres:
//! ```text
//! cd examples/umbral-tenants
//! export UMBRAL_DATABASE_URL=postgres://user:pass@localhost/umbral_tenants_demo
//!
//! # 1. Migrate the SHARED apps (the `tenant` registry) into public:
//! cargo run -- migrate
//!
//! # 2. Boot the server. On startup it provisions the "Acme" tenant
//! #    (create_tenant: insert the row + CREATE SCHEMA + migrate the tenant
//! #    apps into schema "acme"), then serves.
//! cargo run
//!
//! # 3. Talk to a tenant. The resolution middleware reads the `X-Tenant`
//! #    header (or an `*.localhost` subdomain) → looks the tenant up by domain
//! #    → scopes the request under that tenant's schema:
//! curl -H 'X-Tenant: acme.localhost' localhost:3000/notes/add
//! curl -H 'X-Tenant: acme.localhost' localhost:3000/notes
//! #    The bare domain (no header) falls through to public — no tenant.
//! ```
//!
//! When you add more tenants later, `cargo run -- migrate_schemas` (re)creates
//! and migrates every active tenant's schema, idempotently.
//!
//! ## Database-per-tenant (the stronger-isolation alternative)
//!
//! This example uses schema-per-tenant. For a **database per tenant** — one
//! whole Postgres database per tenant, the strongest isolation — flip the
//! strategy and onboard each tenant with its own pool. The operator provisions
//! the database (and opens the pool); the framework owns routing + the registry
//! row + migrating the tenant apps into that database:
//!
//! ```ignore
//! use umbral_tenants::{TenantStrategy, TenantsPlugin};
//!
//! let plugin = TenantsPlugin::new()
//!     .strategy(TenantStrategy::Database)   // many DBs, not many schemas
//!     .shared_apps(["tenants"]);            // the registry stays in the app DB
//!
//! // …App::builder().plugin(plugin)…build()…
//!
//! // Operator created the `acme_db` Postgres database and opened a pool for it:
//! let acme_pool = umbral::db::connect("postgres://user:pass@host/acme_db").await?;
//! plugin
//!     .register_tenant_database("Acme", "acme_db", "acme.localhost", acme_pool)
//!     .await?;
//! // The registry row lands in the default DB; `acme_db` gets its own
//! // `umbral_migrations` ledger + the tenant tables. A request resolving to
//! // `acme.localhost` then routes its tenant-owned queries to the `acme_db` pool.
//! ```

use serde::{Deserialize, Serialize};

use umbral::prelude::*;
use umbral_tenants::{MissingTenant, TenantsPlugin};

/// A tenant-owned model. NOT a shared app, so its `note` table is created in
/// each tenant's schema and its rows are isolated per tenant.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "note")]
pub struct Note {
    pub id: i64,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let settings = Settings::from_env()?;
    if !settings.database_url.starts_with("postgres") {
        eprintln!(
            "umbral-tenants example: schema-per-tenant is Postgres-only. \
             Set UMBRAL_DATABASE_URL=postgres://… and run again."
        );
        return Ok(());
    }

    let pool = umbral::db::connect(&settings.database_url).await?;

    // Dispatch CLI subcommands (`migrate`, `migrate_schemas`, …) before serving.
    // The framework binary normally owns this; the example wires the minimum.
    let args: Vec<String> = std::env::args().collect();
    let is_serve = args.len() <= 1;

    // The plugin: SHARED = the tenants registry only (so `note`, owned by the
    // implicit "app", is a TENANT app). Resolve tenants by the `X-Tenant`
    // header OR an `*.localhost` subdomain; a request with no tenant falls
    // through to public (the bare-domain marketing site).
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(
            TenantsPlugin::new()
                .shared_apps(["tenants"])
                .tenant_header("X-Tenant")
                .subdomain_base("localhost")
                .on_missing_tenant(MissingTenant::FallThroughToPublic),
        )
        .model::<Note>()
        .routes(
            Routes::new()
                .get("/", root)
                .get("/notes", list_notes)
                .get("/notes/add", add_note),
        )
        .build()?;

    // Plugin CLI dispatch (`migrate`, `makemigrations`, `migrate_schemas`).
    if !is_serve {
        match umbral::cli::dispatch(app.plugins(), args.clone()).await? {
            umbral::cli::DispatchOutcome::Matched(name) => {
                tracing::info!(%name, "command done");
                return Ok(());
            }
            // `migrate` / `makemigrations` aren't plugin commands; run them here.
            _ => {
                if args.get(1).map(String::as_str) == Some("migrate") {
                    let n = umbral::migrate::run().await?;
                    tracing::info!(applied = n, "migrate (public) done");
                    return Ok(());
                }
                if args.get(1).map(String::as_str) == Some("makemigrations") {
                    let written = umbral::migrate::make().await?;
                    tracing::info!(?written, "makemigrations done");
                    return Ok(());
                }
                eprintln!("unknown command: {:?}", &args[1..]);
                return Ok(());
            }
        }
    }

    // Provision the demo "Acme" tenant on boot (idempotent on the schema).
    // create_tenant: insert the registry row + CREATE SCHEMA "acme" + migrate
    // the tenant apps (the `note` table) into it.
    let plugin = TenantsPlugin::new().shared_apps(["tenants"]);
    match plugin.create_tenant("Acme", "acme", "acme.localhost").await {
        Ok(t) => tracing::info!(schema = %t.schema_name, domain = %t.domain, "provisioned Acme"),
        Err(e) => tracing::warn!("Acme may already exist (idempotent on schema): {e}"),
    }

    tracing::info!("serving on http://127.0.0.1:3000  (try: curl -H 'X-Tenant: acme.localhost' localhost:3000/notes/add)");
    app.serve("127.0.0.1:3000".parse::<std::net::SocketAddr>()?)
        .await?;
    Ok(())
}

async fn root() -> &'static str {
    "umbral schema-per-tenant demo\n\n\
     Resolve a tenant with the X-Tenant header (or an *.localhost subdomain):\n\
       curl -H 'X-Tenant: acme.localhost' localhost:3000/notes/add\n\
       curl -H 'X-Tenant: acme.localhost' localhost:3000/notes\n\n\
     Each tenant's notes live in its own Postgres schema; the bare domain\n\
     (no X-Tenant) falls through to public — no tenant.\n\n\
     Add tenants later, then: cargo run -- migrate_schemas\n"
}

/// WRITE: under a resolved tenant, `create` writes into that tenant's schema.
async fn add_note() -> String {
    let note = Note {
        id: 0,
        body: format!("a note created at {}", chrono::Utc::now()),
        created_at: chrono::Utc::now(),
    };
    match Note::objects().create(note).await {
        Ok(saved) => format!("created note id={} for the active tenant\n", saved.id),
        Err(e) => format!("create failed (did you send X-Tenant?): {e}\n"),
    }
}

/// READ: under a resolved tenant, `fetch` reads only that tenant's schema.
async fn list_notes() -> String {
    match Note::objects().fetch().await {
        Ok(notes) => {
            let mut out = format!("{} note(s) for the active tenant:\n", notes.len());
            for n in &notes {
                out.push_str(&format!("  [{}] {} @ {}\n", n.id, n.body, n.created_at));
            }
            out
        }
        Err(e) => format!("list failed (did you send X-Tenant?): {e}\n"),
    }
}
