//! `umbra-tenants` — schema-per-tenant multitenancy management for umbra.
//!
//! The django-tenants shape: **one** Postgres database, **one schema per
//! tenant**, and a shared `public` schema for cross-tenant apps (the tenant
//! registry itself, auth, sessions, …). A request is mapped to a tenant by its
//! `Host` subdomain or an explicit header; the rest of the request then runs
//! under that tenant's [`RouteContext`], and the [`TenantRouter`] schema-
//! qualifies every tenant-owned table to the tenant's schema with **zero extra
//! round-trips** (it routes through the SQL builder's `schema_qualified_table`
//! seam — no `SET search_path` per request).
//!
//! This crate builds the *management* layer on top of the
//! [`umbra::db::DatabaseRouter`] + [`umbra::db::RouteContext`] foundation:
//!
//! - [`Tenant`] — the registry model (lives in `public`).
//! - [`TenantRouter`] — the per-table schema router.
//! - [`TenantsPlugin`] — wires the router, the resolution middleware, the
//!   migration command, and the SHARED_APPS configuration.
//! - [`TenantsPlugin::create_tenant`] — provision a tenant: insert the row +
//!   `CREATE SCHEMA` + migrate the tenant apps into it (via the core
//!   [`umbra::migrate::run_for_schema`] helper).
//! - the `migrate_schemas` CLI command — (re)migrate every active tenant's
//!   schema, idempotently.
//!
//! ## SHARED_APPS vs tenant apps
//!
//! A **shared app** keeps its tables in `public` — every tenant sees the same
//! rows. The tenants app itself, plus the usual auth/sessions/permissions
//! built-ins, are shared by default. **Every other app is a tenant app**: its
//! tables are created in each tenant's schema and its rows are isolated per
//! tenant.
//!
//! ## Postgres-only
//!
//! SQLite has no schemas, so schema-per-tenant is Postgres-only. The router
//! must never return a schema on a SQLite-bound request; the core schema-
//! migrate helper errors clearly on a SQLite pool.
//!
//! See `arch.md` and `docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use umbra::db::{DatabaseRouter, RouteContext, Schema, TenantKey};
use umbra::migrate::ModelMeta;
use umbra::prelude::*;

/// The tenant registry. Lives in `public` (a SHARED app) — every tenant
/// resolution reads it without a tenant context, so it stays in the shared
/// schema. One row per provisioned tenant.
///
/// `#[derive(Model)]` snake-cases the struct to the table name `tenant`; the
/// generated column-constant module is therefore [`tenant`].
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbra::orm::Model)]
#[umbra(table = "tenant")]
pub struct Tenant {
    pub id: i64,
    /// The Postgres schema this tenant's tables live in. Unique, and validated
    /// as a safe PG identifier ([`Schema::new`]) at create time.
    #[umbra(unique)]
    pub schema_name: String,
    /// Human-friendly display name.
    pub name: String,
    /// The resolution key: the `Host` subdomain or explicit header value that
    /// maps an inbound request to this tenant (e.g. `acme.example.com` or just
    /// `acme`). Unique.
    #[umbra(unique)]
    pub domain: String,
    /// Inactive tenants are skipped by resolution and by `migrate_schemas`.
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// Errors the management layer surfaces.
#[derive(Debug)]
pub enum TenantError {
    /// `schema_name` failed [`Schema::new`] validation (not a safe PG
    /// identifier). Carries the offending value.
    InvalidSchemaName(String),
    /// A write error inserting the [`Tenant`] row (e.g. a duplicate `domain`
    /// or `schema_name`).
    Write(umbra::orm::WriteError),
    /// A read error querying the registry.
    Query(sqlx::Error),
    /// The core schema-migrate helper failed (e.g. SQLite pool, or a DDL error
    /// applying a tenant migration).
    Migrate(umbra::migrate::MigrateError),
}

impl std::fmt::Display for TenantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TenantError::InvalidSchemaName(s) => write!(
                f,
                "umbra-tenants: `{s}` is not a valid Postgres schema identifier \
                 (must match ^[A-Za-z_][A-Za-z0-9_]*$, 1..=63 chars)"
            ),
            TenantError::Write(e) => write!(f, "umbra-tenants: write: {e}"),
            TenantError::Query(e) => write!(f, "umbra-tenants: query: {e}"),
            TenantError::Migrate(e) => write!(f, "umbra-tenants: migrate: {e}"),
        }
    }
}

impl std::error::Error for TenantError {}

impl From<umbra::orm::WriteError> for TenantError {
    fn from(e: umbra::orm::WriteError) -> Self {
        TenantError::Write(e)
    }
}
impl From<sqlx::Error> for TenantError {
    fn from(e: sqlx::Error) -> Self {
        TenantError::Query(e)
    }
}
impl From<umbra::migrate::MigrateError> for TenantError {
    fn from(e: umbra::migrate::MigrateError) -> Self {
        TenantError::Migrate(e)
    }
}

/// Policy for a request whose `Host`/header maps to no active tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingTenant {
    /// Fall through to `public` (no tenant context). The public/marketing site
    /// served on the bare domain works. **Default.**
    #[default]
    FallThroughToPublic,
    /// Reject with `404 Not Found` — for an app where every request MUST be a
    /// tenant.
    NotFound,
}

/// The default SHARED app labels: tables that stay in `public`. The tenants
/// app itself is always shared (the registry must be readable without a tenant
/// context), plus the usual cross-tenant built-ins.
const DEFAULT_SHARED_APPS: &[&str] = &["app", "tenants", "auth", "sessions", "permissions"];

/// Schema-per-tenant management plugin.
///
/// Register it with the consumer's `App::builder().plugin(TenantsPlugin::new())`.
/// It installs the [`TenantRouter`] (in `on_ready`), the resolution middleware
/// (via `wrap_router`), the [`Tenant`] model + migration, and the
/// `migrate_schemas` CLI command.
pub struct TenantsPlugin {
    /// App labels whose tables stay in `public`. The router's SHARED **table**
    /// set is computed from these at `on_ready` (via [`Self::shared_table_set`]),
    /// once the model registry is published.
    shared_apps: Vec<String>,
    /// If set, the resolver extracts the left-most `Host` label as the tenant
    /// key when the host ends in `.<base>` (e.g. base `example.com` →
    /// `acme.example.com` resolves tenant `acme`).
    subdomain_base: Option<String>,
    /// If set, this request header (case-insensitive) carries an explicit
    /// tenant key. An explicit header **wins** over the subdomain.
    tenant_header: Option<String>,
    /// What to do when no active tenant matches. Default:
    /// [`MissingTenant::FallThroughToPublic`].
    on_missing: MissingTenant,
}

impl Default for TenantsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantsPlugin {
    /// A new plugin with sensible defaults: the [`DEFAULT_SHARED_APPS`] set, the
    /// `X-Tenant` header as the explicit resolution key, no subdomain base, and
    /// [`MissingTenant::FallThroughToPublic`].
    pub fn new() -> Self {
        Self {
            shared_apps: DEFAULT_SHARED_APPS.iter().map(|s| s.to_string()).collect(),
            subdomain_base: None,
            tenant_header: Some("X-Tenant".to_string()),
            on_missing: MissingTenant::default(),
        }
    }

    /// Override the SHARED app labels (tables that stay in `public`). The
    /// tenants app's own table (`tenant`) is always added so the registry stays
    /// shared regardless of what the caller passes.
    pub fn shared_apps<I, S>(mut self, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.shared_apps = apps.into_iter().map(Into::into).collect();
        if !self.shared_apps.iter().any(|a| a == "tenants") {
            self.shared_apps.push("tenants".to_string());
        }
        self
    }

    /// Extract the tenant key from the `Host` subdomain when the host ends in
    /// `.<base>`. E.g. `subdomain_base("example.com")` resolves
    /// `acme.example.com` → tenant key `acme`.
    pub fn subdomain_base(mut self, base: impl Into<String>) -> Self {
        self.subdomain_base = Some(base.into());
        self
    }

    /// Use `header` as the explicit tenant-key request header (wins over the
    /// subdomain). Defaults to `X-Tenant`.
    pub fn tenant_header(mut self, header: impl Into<String>) -> Self {
        self.tenant_header = Some(header.into());
        self
    }

    /// Set the missing-tenant policy. Default:
    /// [`MissingTenant::FallThroughToPublic`].
    pub fn on_missing_tenant(mut self, policy: MissingTenant) -> Self {
        self.on_missing = policy;
        self
    }

    /// The set of SHARED **app labels** (used to filter migrations: the tenant
    /// apps are every plugin NOT in here).
    pub fn shared_app_set(&self) -> HashSet<String> {
        self.shared_apps.iter().cloned().collect()
    }

    /// Collect the SHARED apps' model **table names** into a set, so the router
    /// can decide per-table whether a table is shared (→ `public`) or tenant-
    /// owned (→ the active tenant's schema). Walks every registered plugin's
    /// `models()`, keeping the tables whose owning plugin is a shared app. The
    /// `tenant` table is always included (belt-and-braces).
    ///
    /// Reads the ambient model registry, so it's meaningful only after
    /// `App::build()` has published it (i.e. in `on_ready`).
    fn shared_table_set(&self) -> HashSet<String> {
        let shared = self.shared_app_set();
        let mut tables = HashSet::new();
        tables.insert("tenant".to_string());
        for plugin in umbra::migrate::plugin_order() {
            if !shared.contains(&plugin) {
                continue;
            }
            for meta in umbra::migrate::models_for_plugin(&plugin) {
                tables.insert(meta.table.clone());
            }
        }
        tables
    }

    /// Provision a tenant: validate the schema name, insert the [`Tenant`] row
    /// in `public` (via the ORM), then `CREATE SCHEMA` + migrate the tenant apps
    /// into it (via the core [`umbra::migrate::run_for_schema`] helper).
    ///
    /// Idempotent on the schema (the core helper uses `CREATE SCHEMA IF NOT
    /// EXISTS` and a per-schema migration ledger). The row insert is NOT
    /// idempotent — `domain` / `schema_name` are unique, so a second call with
    /// the same values surfaces a uniqueness error from the ORM.
    pub async fn create_tenant(
        &self,
        name: impl Into<String>,
        schema_name: impl Into<String>,
        domain: impl Into<String>,
    ) -> Result<Tenant, TenantError> {
        let schema_name = schema_name.into();
        // Validate up front so an invalid name never reaches the DB.
        let schema = Schema::new(schema_name.clone())
            .ok_or_else(|| TenantError::InvalidSchemaName(schema_name.clone()))?;

        // Insert the registry row in `public`. There is no tenant context here
        // (we're resolving/provisioning), so the router routes `tenant` to
        // `public` — exactly what we want.
        let row = Tenant {
            id: 0,
            schema_name: schema_name.clone(),
            name: name.into(),
            domain: domain.into(),
            is_active: true,
            created_at: Utc::now(),
        };
        let saved = Tenant::objects().create(row).await?;

        // Create the schema + migrate the tenant apps into it.
        let migrated = umbra::migrate::run_for_schema(&schema, &self.shared_app_set()).await?;
        tracing::info!(
            schema = %schema.as_str(),
            domain = %saved.domain,
            migrated,
            "umbra-tenants: provisioned tenant"
        );
        Ok(saved)
    }
}

/// The schema-per-tenant [`DatabaseRouter`].
///
/// `schema_for_table` is the whole policy:
/// - a SHARED table → `None` (stays in `public`),
/// - a tenant-owned table under a tenant context → the tenant's schema,
/// - a tenant-owned table with **no** tenant context → `None` (public — the
///   bare-domain / background path).
///
/// `db_for_read` / `db_for_write` keep the defaults: schema-per-tenant is ONE
/// database, so there's no per-tenant pool routing.
#[derive(Debug, Clone)]
pub struct TenantRouter {
    shared_tables: Arc<HashSet<String>>,
}

impl TenantRouter {
    /// Build a router from the SHARED table-name set (the tables that stay in
    /// `public`).
    pub fn new(shared_tables: HashSet<String>) -> Self {
        Self {
            shared_tables: Arc::new(shared_tables),
        }
    }
}

impl DatabaseRouter for TenantRouter {
    fn schema_for_table(&self, ctx: &RouteContext, table: &str) -> Option<Schema> {
        // SHARED table → public, regardless of tenant context.
        if self.shared_tables.contains(table) {
            return None;
        }
        // Tenant-owned table: route to the active tenant's schema if one is set,
        // else public (bare-domain / background work — no silent tenant guess).
        match ctx.tenant() {
            Some(key) => Schema::new(key.as_str()),
            None => None,
        }
    }
}

/// Resolve a tenant key from the request's `Host` subdomain and/or the explicit
/// header, per the plugin's config. The header wins. Returns the raw key (the
/// `domain` to look up), or `None` when nothing matched.
///
/// Pure (no DB / no ambient state): unit-testable in isolation.
pub fn resolve_tenant_key(
    headers: &http::HeaderMap,
    tenant_header: Option<&str>,
    subdomain_base: Option<&str>,
) -> Option<String> {
    // 1. Explicit header wins.
    if let Some(name) = tenant_header {
        if let Some(val) = headers.get(name) {
            if let Ok(s) = val.to_str() {
                let s = s.trim();
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    // 2. Subdomain of Host, when a base is configured.
    if let Some(base) = subdomain_base {
        if let Some(host) = host_header(headers) {
            // Strip any :port.
            let host = host.split(':').next().unwrap_or(host);
            let suffix = format!(".{base}");
            if let Some(sub) = host.strip_suffix(&suffix) {
                // Only the left-most label (`a.b.example.com` → `a`); a bare
                // `example.com` (no subdomain) strips to nothing → no tenant.
                let label = sub.split('.').next().unwrap_or("");
                if !label.is_empty() && label != "www" {
                    return Some(label.to_string());
                }
            }
        }
    }
    None
}

/// Read the `Host` header as a string (forwarded `X-Forwarded-Host` is left to
/// a reverse proxy / the host-guard layer; here we read the literal `Host`).
fn host_header(headers: &http::HeaderMap) -> Option<&str> {
    headers.get(http::header::HOST).and_then(|v| v.to_str().ok())
}

impl Plugin for TenantsPlugin {
    fn name(&self) -> &'static str {
        "tenants"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<Tenant>()]
    }

    fn commands(&self) -> Vec<Box<dyn umbra::cli::PluginCommand>> {
        vec![Box::new(MigrateSchemasCommand {
            shared_apps: self.shared_app_set(),
        })]
    }

    fn wrap_router(&self, router: axum::Router) -> axum::Router {
        // The resolution middleware. We capture the resolution config by value
        // (the closures must be 'static) into a small Arc'd config.
        let cfg = Arc::new(ResolverConfig {
            tenant_header: self.tenant_header.clone(),
            subdomain_base: self.subdomain_base.clone(),
            on_missing: self.on_missing,
        });
        router.layer(axum::middleware::from_fn_with_state(
            cfg,
            tenant_resolution_middleware,
        ))
    }

    fn on_ready(&self, _ctx: &umbra::plugin::AppContext) -> Result<(), umbra::plugin::PluginError> {
        // Install the TenantRouter now that the model registry is published, so
        // `shared_table_set()` sees every plugin's tables. First-write-wins: if
        // the app also called `App::builder().router(...)` (installed during
        // build, before on_ready), that one already won — document "don't also
        // set .router(...)".
        let router = TenantRouter::new(self.shared_table_set());
        umbra::db::install_router_from_plugin(Arc::new(router));
        Ok(())
    }
}

/// Resolution-middleware config captured into the `from_fn` state.
#[derive(Clone)]
struct ResolverConfig {
    tenant_header: Option<String>,
    subdomain_base: Option<String>,
    on_missing: MissingTenant,
}

/// The resolution middleware: extract the tenant key, look the [`Tenant`] up in
/// `public` (the lookup runs with no tenant context, so the router routes
/// `tenant` to `public`), and — if found & active — `scope` the rest of the
/// request under that tenant. Otherwise apply the missing-tenant policy.
async fn tenant_resolution_middleware(
    axum::extract::State(cfg): axum::extract::State<Arc<ResolverConfig>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let key = resolve_tenant_key(
        req.headers(),
        cfg.tenant_header.as_deref(),
        cfg.subdomain_base.as_deref(),
    );

    let Some(key) = key else {
        // No key in the request at all → public path (or 404 by policy).
        return apply_missing(cfg.on_missing, next, req).await;
    };

    // Look the tenant up by domain, in public. ORM-only.
    let found = Tenant::objects()
        .filter(tenant::DOMAIN.eq(&key) & tenant::IS_ACTIVE.eq(true))
        .first()
        .await;

    match found {
        Ok(Some(t)) => {
            let ctx = RouteContext::new().with_tenant(TenantKey::new(t.schema_name));
            umbra::db::route_context_scope(ctx, next.run(req)).await
        }
        // Not found / inactive → missing-tenant policy.
        Ok(None) => apply_missing(cfg.on_missing, next, req).await,
        Err(e) => {
            tracing::error!(key = %key, "umbra-tenants: tenant lookup failed: {e}");
            apply_missing(cfg.on_missing, next, req).await
        }
    }
}

async fn apply_missing(
    policy: MissingTenant,
    next: axum::middleware::Next,
    req: axum::extract::Request,
) -> axum::response::Response {
    match policy {
        MissingTenant::FallThroughToPublic => next.run(req).await,
        MissingTenant::NotFound => {
            use axum::response::IntoResponse;
            (axum::http::StatusCode::NOT_FOUND, "tenant not found").into_response()
        }
    }
}

/// The `migrate_schemas` management command: for every active [`Tenant`], ensure
/// its schema exists and migrate the tenant apps into it (idempotent). Shared
/// apps are migrated into `public` by the normal `migrate`.
struct MigrateSchemasCommand {
    shared_apps: HashSet<String>,
}

#[async_trait]
impl umbra::cli::PluginCommand for MigrateSchemasCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("migrate_schemas").about(
            "Create + migrate the schema of every active tenant (schema-per-tenant). \
             Idempotent. Run after `migrate` (which handles the shared `public` apps).",
        )
    }

    async fn run(&self, _matches: &clap::ArgMatches) -> Result<(), umbra::cli::CliError> {
        let tenants = Tenant::objects()
            .filter(tenant::IS_ACTIVE.eq(true))
            .fetch()
            .await?;
        if tenants.is_empty() {
            tracing::info!("umbra-tenants migrate_schemas: no active tenants");
            return Ok(());
        }
        let mut total: u64 = 0;
        for t in &tenants {
            let schema = match Schema::new(t.schema_name.clone()) {
                Some(s) => s,
                None => {
                    tracing::warn!(
                        schema = %t.schema_name,
                        domain = %t.domain,
                        "umbra-tenants migrate_schemas: skipping tenant with invalid schema name"
                    );
                    continue;
                }
            };
            let n = umbra::migrate::run_for_schema(&schema, &self.shared_apps).await?;
            total += n;
            tracing::info!(
                schema = %schema.as_str(),
                domain = %t.domain,
                applied = n,
                "umbra-tenants migrate_schemas: migrated tenant schema"
            );
        }
        tracing::info!(
            tenants = tenants.len(),
            applied = total,
            "umbra-tenants migrate_schemas: done"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderMap;

    fn shared_set(tables: &[&str]) -> HashSet<String> {
        tables.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn router_shared_table_is_public_regardless_of_tenant() {
        let router = TenantRouter::new(shared_set(&["tenant", "auth_user"]));
        // Under a tenant ctx, a shared table still routes to public (None).
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        assert!(router.schema_for_table(&ctx, "tenant").is_none());
        assert!(router.schema_for_table(&ctx, "auth_user").is_none());
        // No tenant ctx: shared still public.
        let ctx = RouteContext::new();
        assert!(router.schema_for_table(&ctx, "tenant").is_none());
    }

    #[test]
    fn router_tenant_table_routes_to_schema_under_tenant_ctx() {
        let router = TenantRouter::new(shared_set(&["tenant"]));
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        let schema = router.schema_for_table(&ctx, "post");
        assert_eq!(schema.unwrap().as_str(), "acme");
    }

    #[test]
    fn router_tenant_table_with_no_tenant_is_public() {
        let router = TenantRouter::new(shared_set(&["tenant"]));
        let ctx = RouteContext::new();
        assert!(router.schema_for_table(&ctx, "post").is_none());
    }

    #[test]
    fn router_rejects_bad_tenant_key_as_schema() {
        // A tenant key that isn't a valid PG identifier yields no schema (the
        // SQL builder then emits a bare table) — Schema::new is the gate.
        let router = TenantRouter::new(shared_set(&["tenant"]));
        let ctx = RouteContext::new().with_tenant(TenantKey::new("bad key;--"));
        assert!(router.schema_for_table(&ctx, "post").is_none());
    }

    #[test]
    fn resolve_header_wins_over_subdomain() {
        let mut h = HeaderMap::new();
        h.insert("x-tenant", "fromheader".parse().unwrap());
        h.insert(http::header::HOST, "acme.example.com".parse().unwrap());
        let key = resolve_tenant_key(&h, Some("X-Tenant"), Some("example.com"));
        assert_eq!(key.as_deref(), Some("fromheader"));
    }

    #[test]
    fn resolve_subdomain_when_no_header() {
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, "acme.example.com".parse().unwrap());
        let key = resolve_tenant_key(&h, Some("X-Tenant"), Some("example.com"));
        assert_eq!(key.as_deref(), Some("acme"));
    }

    #[test]
    fn resolve_subdomain_strips_port() {
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, "acme.example.com:8080".parse().unwrap());
        let key = resolve_tenant_key(&h, None, Some("example.com"));
        assert_eq!(key.as_deref(), Some("acme"));
    }

    #[test]
    fn resolve_bare_domain_has_no_tenant() {
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, "example.com".parse().unwrap());
        let key = resolve_tenant_key(&h, None, Some("example.com"));
        assert!(key.is_none());
        // www is treated as the bare site, not a tenant.
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, "www.example.com".parse().unwrap());
        assert!(resolve_tenant_key(&h, None, Some("example.com")).is_none());
    }

    #[test]
    fn resolve_nothing_configured_is_none() {
        let mut h = HeaderMap::new();
        h.insert(http::header::HOST, "acme.example.com".parse().unwrap());
        // No header configured, no subdomain base → no tenant.
        assert!(resolve_tenant_key(&h, None, None).is_none());
    }

    #[test]
    fn create_tenant_rejects_bad_schema_name() {
        // The schema-name validation is the FIRST thing create_tenant does; a
        // bad name never reaches the DB. We can assert the validation gate
        // (Schema::new) directly — it's the exact check create_tenant runs.
        assert!(Schema::new("acme").is_some());
        assert!(Schema::new("1acme").is_none());
        assert!(Schema::new("ac me").is_none());
        assert!(Schema::new("drop\";--").is_none());
    }

    #[test]
    fn shared_apps_always_includes_tenants() {
        let plugin = TenantsPlugin::new().shared_apps(["auth", "sessions"]);
        assert!(plugin.shared_app_set().contains("tenants"));
    }
}
