//! Starknet explorer — schema-per-tenant multitenancy where **networks are tenants**.
//!
//! ## The mental model
//!
//! A blockchain explorer serves the SAME app for several networks. The data
//! splits cleanly in two:
//!
//! - **Per-network data** (transactions, addresses, tokens) must be **isolated
//!   per network**: Sepolia's transactions are invisible when you're serving
//!   Mainnet, and vice-versa. So the `explorer` plugin is a **TENANT app** — its
//!   `transaction` / `address` / `token` tables live in *each network's own
//!   Postgres schema*, never in `public`.
//! - **Cross-network data** (API keys, blog posts) is **shared** across every
//!   network. The `access` and `content` plugins are **SHARED apps** — their
//!   `api_key` / `blog_post` tables live once, in `public`, and read identically
//!   no matter which network you resolve.
//!
//! The split is made entirely by **plugin name**: you list your TENANT apps with
//! `.tenant_apps([...])`, and EVERYTHING ELSE — built-ins, external plugins, your
//! shared apps — stays in `public` by default (the safe direction). Here:
//!
//! ```text
//!   tenant_apps = ["explorer"]                          →  one schema per network
//!   everything else (tenants / access / content / …)    →  public (shared)
//! ```
//!
//! A "tenant" is a network: `sepolia` (Starknet Sepolia testnet) and `mainnet`
//! (Starknet Mainnet), each resolved by the `X-Network` header (or an
//! `*.localhost` subdomain: `sepolia.localhost`, `mainnet.localhost`).
//!
//! ## Run it (Postgres-only)
//!
//! ```text
//! cd examples/starknet-explorer
//! export UMBRA_DATABASE_URL=postgres://user:pass@localhost/starknet_explorer
//!
//! cargo run -- makemigrations      # generate migration files for all 4 apps
//! cargo run -- migrate             # run_shared: SHARED apps (tenants/access/content) -> public ONLY
//! cargo run                        # boot: create_tenant sepolia + mainnet (explorer tables per schema), serve
//!
//! # seed + read, isolated per network:
//! curl -H 'X-Network: sepolia.localhost' localhost:3000/txs/seed
//! curl -H 'X-Network: mainnet.localhost' localhost:3000/txs/seed
//! curl -H 'X-Network: sepolia.localhost' localhost:3000/txs   # only Sepolia's tx
//! curl -H 'X-Network: mainnet.localhost' localhost:3000/txs   # only Mainnet's tx
//! curl localhost:3000/blog                                    # shared, no network needed
//! ```
//!
//! ## Why `run_shared` and not `migrate`/`run`
//!
//! The plain `migrate` (`umbra::migrate::run()`) would migrate **every** app
//! into `public`, including the `explorer` tenant tables — exactly what we do
//! NOT want, because `transaction`/`address`/`token` must exist only inside each
//! network's schema. `umbra::migrate::run_shared(&shared_set)` migrates ONLY the
//! shared apps into `public`. The `explorer` tenant tables are then created
//! per-network by `create_tenant` (CREATE SCHEMA + migrate the tenant apps into
//! that schema) on boot.
//!
//! Adding a third network later is just one more `create_tenant(...)` (or a
//! `cargo run -- migrate_schemas`, which re-creates + migrates every active
//! network's schema, idempotently). No code change, no new `public` tables.

use serde::{Deserialize, Serialize};

use umbra::prelude::*;
use umbra_tenants::{MissingTenant, TenantsPlugin};

// ---------------------------------------------------------------------------
// `explorer` — the TENANT app. The one app named in `tenant_apps`, so these
// tables are created in EACH network's schema and isolated per network.
// ---------------------------------------------------------------------------

/// A Starknet transaction. Per-network: Sepolia's txs and Mainnet's txs live in
/// separate schemas and never mix.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "transaction")]
pub struct Transaction {
    pub id: i64,
    #[umbra(unique)]
    pub hash: String,
    pub block_number: i64,
    pub sender_address: String,
    /// e.g. "INVOKE" / "DEPLOY" / "DECLARE".
    pub kind: String,
    /// e.g. "ACCEPTED" / "REVERTED".
    pub status: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A Starknet account / contract address. Per-network.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "address")]
pub struct Address {
    pub id: i64,
    #[umbra(unique)]
    pub address: String,
    pub class_hash: String,
    pub nonce: i64,
}

/// A token deployed on this network. Per-network.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "token")]
pub struct Token {
    pub id: i64,
    #[umbra(unique)]
    pub contract_address: String,
    pub name: String,
    pub symbol: String,
    pub decimals: i32,
}

/// The TENANT plugin. Because `explorer` is named in `tenant_apps`, its three
/// tables are migrated into each network's schema, not into `public`.
#[derive(Debug, Default, Clone)]
pub struct ExplorerPlugin;

impl Plugin for ExplorerPlugin {
    fn name(&self) -> &'static str {
        "explorer"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        use umbra::migrate::ModelMeta;
        vec![
            ModelMeta::for_::<Transaction>(),
            ModelMeta::for_::<Address>(),
            ModelMeta::for_::<Token>(),
        ]
    }
}

// ---------------------------------------------------------------------------
// `access` — a SHARED app (not a tenant app → public). One copy, all networks.
// ---------------------------------------------------------------------------

/// An API key. Cross-network: the same keys authorize requests on every
/// network, so they live once in `public`.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "api_key")]
pub struct ApiKey {
    pub id: i64,
    #[umbra(unique)]
    pub key: String,
    pub label: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default, Clone)]
pub struct AccessPlugin;

impl Plugin for AccessPlugin {
    fn name(&self) -> &'static str {
        "access"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        use umbra::migrate::ModelMeta;
        vec![ModelMeta::for_::<ApiKey>()]
    }
}

// ---------------------------------------------------------------------------
// `content` — a SHARED app (not a tenant app → public). Same blog everywhere.
// ---------------------------------------------------------------------------

/// A blog post. Cross-network marketing content: identical under every network.
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "blog_post")]
pub struct BlogPost {
    pub id: i64,
    #[umbra(unique)]
    pub slug: String,
    pub title: String,
    pub body: String,
    pub published_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Default, Clone)]
pub struct ContentPlugin;

impl Plugin for ContentPlugin {
    fn name(&self) -> &'static str {
        "content"
    }

    fn models(&self) -> Vec<umbra::migrate::ModelMeta> {
        use umbra::migrate::ModelMeta;
        vec![ModelMeta::for_::<BlogPost>()]
    }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let settings = Settings::from_env()?;
    if !settings.database_url.starts_with("postgres") {
        eprintln!(
            "starknet-explorer example: schema-per-tenant (networks as tenants) is \
             Postgres-only. Set UMBRA_DATABASE_URL=postgres://… and run again."
        );
        return Ok(());
    }

    let pool = umbra::db::connect(&settings.database_url).await?;

    let args: Vec<String> = std::env::args().collect();
    let is_serve = args.len() <= 1;

    // The plugin: SHARED = the tenants registry + access + content. Everything
    // else (the `explorer` app) is tenant-owned, so its tables go in each
    // network's schema. Resolve the active network by the `X-Network` header OR
    // an `*.localhost` subdomain; no network → fall through to public (the
    // shared blog / API keys still work without a network).
    let app = App::builder()
        .settings(settings)
        .database("default", pool)
        .plugin(
            TenantsPlugin::new()
                // Declare only the TENANT app, by the PLUGIN itself — its name
                // comes from `ExplorerPlugin::name()`, so a typo can't desync the
                // tenant set from the real app name. Everything else (built-ins,
                // `access`, `content`, anything you add later) is shared by
                // default — the safe direction: forget an app and it stays in
                // `public`, never accidentally fragmented per network.
                .tenant_app(&ExplorerPlugin)
                .tenant_header("X-Network") // resolve the network by header
                .subdomain_base("localhost") // …or sepolia.localhost / mainnet.localhost
                .on_missing_tenant(MissingTenant::FallThroughToPublic),
        )
        .plugin(ExplorerPlugin)
        .plugin(AccessPlugin)
        .plugin(ContentPlugin)
        .routes(
            Routes::new()
                .get("/", root)
                .get("/txs", list_txs)
                .get("/txs/seed", seed_tx)
                .get("/tokens", list_tokens)
                .get("/tokens/seed", seed_token)
                .get("/blog", list_blog)
                .get("/blog/seed", seed_blog)
                .get("/apikeys", list_apikeys),
        )
        .build()?;

    // CLI dispatch. The plugin contributes `migrate_schemas`; `makemigrations`
    // and `migrate` are handled here.
    if !is_serve {
        // The KEY difference from a non-tenant app: `migrate` runs `run_shared`,
        // not `run`. `run_shared` migrates ONLY the shared apps
        // (tenants/access/content) into `public`, so the `explorer` tenant
        // tables (transaction/address/token) are NEVER created in `public` —
        // they exist only inside each network's schema (created by
        // `create_tenant` on boot, or by `migrate_schemas`).
        match umbra::cli::dispatch(app.plugins(), args.clone()).await? {
            umbra::cli::DispatchOutcome::Matched(name) => {
                tracing::info!(%name, "command done");
                return Ok(());
            }
            _ => {
                if args.get(1).map(String::as_str) == Some("migrate") {
                    // Shared apps → public. `shared_app_set()` resolves the
                    // `tenant_apps(["explorer"])` split for us: shared = every
                    // registered app EXCEPT `explorer`. (`migrate_schemas` does
                    // this AND the tenant schemas in one go; on a first run
                    // there are no tenants yet, so the public step is enough.)
                    let shared = TenantsPlugin::new().tenant_app(&ExplorerPlugin).shared_app_set();
                    let n = umbra::migrate::run_shared(&shared).await?;
                    tracing::info!(applied = n, "migrate (shared apps → public) done");
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

    // Provision both networks on boot (idempotent on the schema). `create_tenant`
    // inserts the registry row + CREATE SCHEMA + migrates the `explorer` tenant
    // tables (transaction/address/token) into that network's schema.
    let plugin = TenantsPlugin::new().tenant_app(&ExplorerPlugin);
    for (name, schema, domain) in [
        ("Starknet Sepolia", "sepolia", "sepolia.localhost"),
        ("Starknet Mainnet", "mainnet", "mainnet.localhost"),
    ] {
        match plugin.create_tenant(name, schema, domain).await {
            Ok(t) => {
                tracing::info!(schema = %t.schema_name, domain = %t.domain, "provisioned network")
            }
            Err(e) => {
                tracing::warn!("{name} may already exist (idempotent on schema): {e}")
            }
        }
    }

    // Bind address is configurable via `UMBRA_BIND` (default `127.0.0.1:3000`)
    // so two examples can run side by side without colliding on a port.
    let bind = std::env::var("UMBRA_BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    tracing::info!(
        "serving on http://{bind}  (try: curl -H 'X-Network: sepolia.localhost' {bind}/txs/seed)"
    );
    app.serve(bind.parse::<std::net::SocketAddr>()?).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers. `/txs` + `/tokens` differ per `X-Network` (tenant-isolated);
// `/blog` + `/apikeys` are the same regardless of network (shared in public).
// ---------------------------------------------------------------------------

/// Which network is active + the curl hints.
async fn root() -> Json<umbra::_serde_json::Value> {
    let ctx = umbra::db::route_context();
    let active = ctx
        .tenant()
        .map(|t| t.as_str().to_string())
        .unwrap_or_else(|| "(none — falling through to public)".into());
    Json(umbra::_serde_json::json!({
        "app": "umbra starknet explorer — networks are tenants",
        "active_network": active,
        "hint": "Resolve a network with the X-Network header (or an *.localhost subdomain).",
        "per_network_isolated": ["/txs", "/txs/seed", "/tokens", "/tokens/seed"],
        "shared_across_networks": ["/blog", "/blog/seed", "/apikeys"],
        "try": [
            "curl -H 'X-Network: sepolia.localhost' localhost:3000/txs/seed",
            "curl -H 'X-Network: mainnet.localhost' localhost:3000/txs/seed",
            "curl -H 'X-Network: sepolia.localhost' localhost:3000/txs",
            "curl localhost:3000/blog"
        ],
    }))
}

/// TENANT-ISOLATED read: only this network's transactions.
async fn list_txs() -> Json<umbra::_serde_json::Value> {
    match Transaction::objects().fetch().await {
        Ok(txs) => Json(umbra::_serde_json::json!({ "count": txs.len(), "transactions": txs })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": format!("did you send X-Network? {e}") })),
    }
}

/// TENANT-ISOLATED write: seed one demo tx into the ACTIVE network's schema.
/// Run it under Sepolia and under Mainnet and the two networks accumulate
/// DIFFERENT transactions.
async fn seed_tx() -> Json<umbra::_serde_json::Value> {
    let now = chrono::Utc::now();
    let tx = Transaction {
        id: 0,
        hash: format!("0x{:x}", now.timestamp_nanos_opt().unwrap_or_default()),
        block_number: 1_000_000 + (now.timestamp() % 1000),
        sender_address: "0xabc...sender".into(),
        kind: "INVOKE".into(),
        status: "ACCEPTED".into(),
        timestamp: now,
    };
    match Transaction::objects().create(tx).await {
        Ok(saved) => Json(umbra::_serde_json::json!({ "created": saved })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": format!("did you send X-Network? {e}") })),
    }
}

/// TENANT-ISOLATED read: only this network's tokens.
async fn list_tokens() -> Json<umbra::_serde_json::Value> {
    match Token::objects().fetch().await {
        Ok(tokens) => Json(umbra::_serde_json::json!({ "count": tokens.len(), "tokens": tokens })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": format!("did you send X-Network? {e}") })),
    }
}

/// TENANT-ISOLATED write: seed one demo token into the ACTIVE network's schema.
async fn seed_token() -> Json<umbra::_serde_json::Value> {
    let now = chrono::Utc::now();
    let token = Token {
        id: 0,
        contract_address: format!("0x{:x}", now.timestamp_nanos_opt().unwrap_or_default()),
        name: "Demo Token".into(),
        symbol: "DEMO".into(),
        decimals: 18,
    };
    match Token::objects().create(token).await {
        Ok(saved) => Json(umbra::_serde_json::json!({ "created": saved })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": format!("did you send X-Network? {e}") })),
    }
}

/// SHARED read: blog posts are identical under every network (live in public).
async fn list_blog() -> Json<umbra::_serde_json::Value> {
    match BlogPost::objects().fetch().await {
        Ok(posts) => Json(umbra::_serde_json::json!({ "count": posts.len(), "posts": posts })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": e.to_string() })),
    }
}

/// SHARED write: a blog post lands in public — visible under every network.
async fn seed_blog() -> Json<umbra::_serde_json::Value> {
    let now = chrono::Utc::now();
    let post = BlogPost {
        id: 0,
        slug: format!("post-{}", now.timestamp()),
        title: "Indexing Starknet, one block at a time".into(),
        body: "Shared content, served identically on Sepolia and Mainnet.".into(),
        published_at: now,
    };
    match BlogPost::objects().create(post).await {
        Ok(saved) => Json(umbra::_serde_json::json!({ "created": saved })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": e.to_string() })),
    }
}

/// SHARED read: API keys are identical under every network (live in public).
async fn list_apikeys() -> Json<umbra::_serde_json::Value> {
    match ApiKey::objects().fetch().await {
        Ok(keys) => Json(umbra::_serde_json::json!({ "count": keys.len(), "api_keys": keys })),
        Err(e) => Json(umbra::_serde_json::json!({ "error": e.to_string() })),
    }
}
