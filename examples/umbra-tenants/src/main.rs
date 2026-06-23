//! Schema-per-tenant multitenancy with `umbra-tenants`.
//!
//! One Postgres database, one schema per tenant, a shared `public` for the
//! tenant registry. A `Note` model is tenant-owned: each tenant's notes live in
//! its own schema and are invisible to other tenants. The `TenantRouter`
//! (installed by `TenantsPlugin`) schema-qualifies `note` to the active
//! tenant's schema with zero extra round-trips.
//!
//! ## Run it
//!
//! Schema-per-tenant is Postgres-only. Point `UMBRA_DATABASE_URL` at a Postgres:
//! ```text
//! cd examples/umbra-tenants
//! export UMBRA_DATABASE_URL=postgres://user:pass@localhost/umbra_tenants_demo
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

use serde::{Deserialize, Serialize};

use umbra::prelude::*;
use umbra_tenants::{MissingTenant, TenantsPlugin};

/// A tenant-owned model. NOT a shared app, so its `note` table is created in
/// each tenant's schema and its rows are isolated per tenant.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "note")]
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
            "umbra-tenants example: schema-per-tenant is Postgres-only. \
             Set UMBRA_DATABASE_URL=postgres://… and run again."
        );
        return Ok(());
    }

    let pool = umbra::db::connect(&settings.database_url).await?;

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
        match umbra::cli::dispatch(app.plugins(), args.clone()).await? {
            umbra::cli::DispatchOutcome::Matched(name) => {
                tracing::info!(%name, "command done");
                return Ok(());
            }
            // `migrate` / `makemigrations` aren't plugin commands; run them here.
            _ => {
                if args.get(1).map(String::as_str) == Some("migrate") {
                    let n = umbra::migrate::run().await?;
                    tracing::info!(applied = n, "migrate (public) done");
                    return Ok(());
                }
                if args.get(1).map(String::as_str) == Some("makemigrations") {
                    let written = umbra::migrate::make().await?;
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
    "umbra schema-per-tenant demo\n\n\
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
