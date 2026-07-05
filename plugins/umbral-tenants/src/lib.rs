//! `umbral-tenants` — schema-per-tenant multitenancy management for umbral.
//!
//! The shape: **one** Postgres database, **one schema per
//! tenant**, and a shared `public` schema for cross-tenant apps (the tenant
//! registry itself, auth, sessions, …). A request is mapped to a tenant by its
//! `Host` subdomain or an explicit header; the rest of the request then runs
//! under that tenant's [`RouteContext`], and the [`TenantRouter`] schema-
//! qualifies every tenant-owned table to the tenant's schema with **zero extra
//! round-trips** (it routes through the SQL builder's `schema_qualified_table`
//! seam — no `SET search_path` per request).
//!
//! This crate builds the *management* layer on top of the
//! [`umbral::db::DatabaseRouter`] + [`umbral::db::RouteContext`] foundation:
//!
//! - [`Tenant`] — the registry model (lives in `public`).
//! - [`TenantRouter`] — the per-table schema router.
//! - [`TenantsPlugin`] — wires the router, the resolution middleware, the
//!   migration command, and the SHARED_APPS configuration.
//! - [`TenantsPlugin::create_tenant`] — provision a tenant: insert the row +
//!   `CREATE SCHEMA` + migrate the tenant apps into it (via the core
//!   [`umbral::migrate::run_for_schema`] helper).
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

use umbral::db::{Alias, DatabaseRouter, DbPool, RouteContext, Schema, TenantKey};
use umbral::migrate::ModelMeta;
use umbral::prelude::*;

/// Which isolation strategy the plugin wires.
///
/// The plugin defaults to [`TenantStrategy::Schema`] — every existing call site
/// and test keeps the shape (one database, one schema per tenant)
/// byte-for-byte. Opt into [`TenantStrategy::Database`] with
/// [`TenantsPlugin::strategy`] for a *database per tenant* (stronger isolation,
/// the operator provisions each tenant's database/pool).
///
/// The two modes are mutually exclusive in the [`TenantRouter`]:
/// - **Schema** routes via `schema_for_table` (qualify tenant tables to the
///   tenant's PG schema; `db_for_read`/`db_for_write` keep the default pool).
/// - **Database** routes via `db_for_read`/`db_for_write` (pick the tenant's
///   pool by alias; `schema_for_table` returns `None` — separate databases need
///   no schema qualification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TenantStrategy {
    /// One database, one Postgres schema per tenant. Zero extra round-trips
    /// (the SQL builder schema-qualifies tenant tables). **Default.**
    #[default]
    Schema,
    /// One database (pool) per tenant. Stronger isolation; the operator
    /// provisions the database and the pool, then onboards it via
    /// [`TenantsPlugin::register_tenant_database`]. In this mode a tenant's
    /// `schema_name` field names the *pool alias / database*, not a PG schema.
    Database,
}

/// The tenant registry. Lives in `public` (a SHARED app) — every tenant
/// resolution reads it without a tenant context, so it stays in the shared
/// schema. One row per provisioned tenant.
///
/// `#[derive(Model)]` snake-cases the struct to the table name `tenant`; the
/// generated column-constant module is therefore [`tenant`].
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, umbral::orm::Model)]
#[umbral(table = "tenant")]
pub struct Tenant {
    pub id: i64,
    /// The Postgres schema this tenant's tables live in. Unique, and validated
    /// as a safe PG identifier ([`Schema::new`]) at create time.
    #[umbral(unique)]
    pub schema_name: String,
    /// Human-friendly display name.
    pub name: String,
    /// The resolution key: the `Host` subdomain or explicit header value that
    /// maps an inbound request to this tenant (e.g. `acme.example.com` or just
    /// `acme`). Unique.
    #[umbral(unique)]
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
    Write(umbral::orm::WriteError),
    /// A read error querying the registry.
    Query(sqlx::Error),
    /// The core schema-migrate helper failed (e.g. SQLite pool, or a DDL error
    /// applying a tenant migration).
    Migrate(umbral::migrate::MigrateError),
}

impl std::fmt::Display for TenantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TenantError::InvalidSchemaName(s) => write!(
                f,
                "umbral-tenants: `{s}` is not a valid Postgres schema identifier \
                 (must match ^[A-Za-z_][A-Za-z0-9_]*$, 1..=63 chars)"
            ),
            TenantError::Write(e) => write!(f, "umbral-tenants: write: {e}"),
            TenantError::Query(e) => write!(f, "umbral-tenants: query: {e}"),
            TenantError::Migrate(e) => write!(f, "umbral-tenants: migrate: {e}"),
        }
    }
}

impl std::error::Error for TenantError {}

impl From<umbral::orm::WriteError> for TenantError {
    fn from(e: umbral::orm::WriteError) -> Self {
        TenantError::Write(e)
    }
}
impl From<sqlx::Error> for TenantError {
    fn from(e: sqlx::Error) -> Self {
        TenantError::Query(e)
    }
}
impl From<umbral::migrate::MigrateError> for TenantError {
    fn from(e: umbral::migrate::MigrateError) -> Self {
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

/// Alias-prefix returned in database-per-tenant mode when a tenant's pool is
/// **not** registered. It is deliberately never a real alias, so the query
/// terminal aborts (fails closed) instead of silently borrowing the *default*
/// database and commingling un-onboarded tenants there (TEN-2). The tenant key
/// is appended so the panic/error names the offending tenant for the operator.
const UNROUTED_TENANT_PREFIX: &str = "__umbral_unrouted_tenant__";

/// Resolve the SHARED app-label set from the plugin's config. When `tenant_apps`
/// is set (the recommended safe-by-default mode), the shared set is **every
/// registered plugin EXCEPT the tenant apps** — so built-ins and external
/// plugins are shared without being named. The `tenants` registry is forced
/// shared regardless. Otherwise the explicit `shared_apps` list is used.
///
/// Reads the live plugin registry (`plugin_order`), so it is meaningful once the
/// app is built — call it at `on_ready` or command-run time, not at config time.
fn resolve_shared_app_set(
    shared_apps: &[String],
    tenant_apps: Option<&HashSet<String>>,
) -> HashSet<String> {
    match tenant_apps {
        Some(tenant) => {
            let mut shared: HashSet<String> = umbral::migrate::plugin_order()
                .into_iter()
                .filter(|p| !tenant.contains(p))
                .collect();
            shared.insert("tenants".to_string()); // the registry is always shared
            shared
        }
        None => shared_apps.iter().cloned().collect(),
    }
}

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
    /// Opt-in TENANT apps — the safer inverse of [`Self::shared_apps`]. When
    /// `Some`, THESE are the per-tenant apps and **every other registered app is
    /// shared** (built-ins, external plugins, anything you forgot). Wins over
    /// `shared_apps`. Resolved against the live plugin registry, so it only
    /// needs the names of the apps you own that are tenant-specific.
    tenant_apps: Option<HashSet<String>>,
    /// If set, the resolver extracts the left-most `Host` label as the tenant
    /// key when the host ends in `.<base>` (e.g. base `example.com` →
    /// `acme.example.com` resolves tenant `acme`).
    subdomain_base: Option<String>,
    /// If set, this request header (case-insensitive) carries an explicit
    /// tenant key. An explicit header **wins** over the subdomain.
    ///
    /// **Off by default** (opt-in via [`Self::tenant_header`]). A client-set
    /// header is untrusted input: with shared auth/sessions there is no
    /// binding between the caller's principal and the tenant it names, so an
    /// enabled header lets any authenticated user select any tenant (TEN-1).
    /// Enable it only behind a trusted proxy that strips/sets it, never on a
    /// subdomain-isolated deployment.
    tenant_header: Option<String>,
    /// What to do when no active tenant matches. Default:
    /// [`MissingTenant::FallThroughToPublic`].
    on_missing: MissingTenant,
    /// Schema-per-tenant (default) vs database-per-tenant. Threaded into the
    /// [`TenantRouter`] at `on_ready`.
    strategy: TenantStrategy,
    /// Optional server-side membership guard (audit_2 C3). When set, a request
    /// that resolves to an existing tenant is additionally checked: the guard
    /// decides whether THIS caller may act under that tenant, and the request
    /// fails closed (404) if not. `None` (default) keeps the pre-C3 behavior —
    /// a resolved tenant is honored on trust, which is only safe on a
    /// subdomain-isolated deployment behind a proxy that controls the signal.
    membership: Option<Arc<dyn TenantMembership>>,
}

/// A server-side check that the caller may act under a resolved tenant
/// (audit_2 C3). `umbral-tenants` resolves a tenant KEY from the request
/// (header / subdomain), confirms the tenant exists, then — if a guard is
/// installed via [`TenantsPlugin::membership`] — asks it whether this caller is
/// bound to that tenant, failing closed (404, no enumeration oracle) when not.
///
/// The guard extracts the caller's identity however the app authenticates
/// (session cookie, bearer, mTLS header), so the tenant plugin stays decoupled
/// from any specific auth plugin. A typical impl reads the session user id and
/// checks it against a `user_id → tenant` membership table.
#[async_trait::async_trait]
pub trait TenantMembership: Send + Sync + std::fmt::Debug {
    /// Return `true` iff the request (identified via `headers`) may act under
    /// the tenant identified by `tenant_key` (the resolved domain key). **Fail
    /// closed:** return `false` when the caller is anonymous, not a member, or
    /// the check errors — a `false` rejects the request.
    async fn allows(&self, headers: &axum::http::HeaderMap, tenant_key: &str) -> bool;
}

impl Default for TenantsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TenantsPlugin {
    /// A new plugin with safe defaults: the [`DEFAULT_SHARED_APPS`] set, **no**
    /// header-based tenant resolution (opt in with [`Self::tenant_header`]), no
    /// subdomain base, and [`MissingTenant::FallThroughToPublic`].
    ///
    /// Header resolution is off by default on purpose (TEN-1): a client-supplied
    /// `X-Tenant` is untrusted and, with shared sessions, lets any authenticated
    /// user read/write any tenant. Turn it on only behind a trusted proxy that
    /// controls the header; prefer [`Self::subdomain_base`] otherwise.
    pub fn new() -> Self {
        Self {
            shared_apps: DEFAULT_SHARED_APPS.iter().map(|s| s.to_string()).collect(),
            tenant_apps: None,
            subdomain_base: None,
            tenant_header: None,
            on_missing: MissingTenant::default(),
            strategy: TenantStrategy::default(),
            membership: None,
        }
    }

    /// Install a server-side [`TenantMembership`] guard (audit_2 C3). Once set,
    /// a request that resolves to an existing tenant is only served if the guard
    /// confirms the caller is bound to that tenant; otherwise it fails closed
    /// (404). This is the binding that stops any authenticated user from acting
    /// under any tenant merely by naming it — enable it whenever the tenant
    /// signal is client-influenceable (an `X-Tenant` header, or shared sessions
    /// across subdomains).
    pub fn membership(mut self, guard: Arc<dyn TenantMembership>) -> Self {
        self.membership = Some(guard);
        self
    }

    /// Declare the **TENANT** apps — the apps whose tables are per-tenant
    /// (created in each tenant's schema, isolated). EVERY OTHER registered app
    /// is **shared** (its tables stay in `public`): built-ins (`auth`,
    /// `sessions`, …), external plugins, and anything you didn't list.
    ///
    /// This is the safer inverse of [`Self::shared_apps`]. With `shared_apps`
    /// you must enumerate *every* shared app — forget one and it silently
    /// becomes tenant (its tables fragment into every schema). With
    /// `tenant_apps` you list only the handful of apps you own that are
    /// tenant-specific; forgetting one leaves it shared (public), which is the
    /// safe failure. The `tenants` registry is forced shared regardless.
    ///
    /// **Prefer this** over `shared_apps`. If both are set, `tenant_apps` wins.
    pub fn tenant_apps<I, S>(mut self, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tenant_apps = Some(apps.into_iter().map(Into::into).collect());
        self
    }

    /// Type-safe sibling of [`Self::tenant_apps`]: declare a tenant app by the
    /// **plugin itself**, so its label comes from [`Plugin::name`] and a mistyped
    /// string can never desync the tenant set from the plugin's real name —
    /// the compiler checks the type, and the name is the single source of truth.
    /// Chain one per tenant app:
    ///
    /// ```ignore
    /// TenantsPlugin::new()
    ///     .tenant_app(&ExplorerPlugin)   // -> "explorer", from ExplorerPlugin::name()
    ///     .tenant_app(&LedgerPlugin);
    /// ```
    ///
    /// Merges with any [`Self::tenant_apps`] call; like it, wins over
    /// `shared_apps`. Prefer this in multi-dev projects where a stringly-typed
    /// app label is easy to fat-finger.
    pub fn tenant_app<P: Plugin + ?Sized>(mut self, plugin: &P) -> Self {
        self.tenant_apps
            .get_or_insert_with(HashSet::new)
            .insert(plugin.name().to_string());
        self
    }

    /// Pick the isolation strategy. Default [`TenantStrategy::Schema`] (one DB,
    /// schema per tenant). [`TenantStrategy::Database`] routes each tenant to
    /// its own database/pool — onboard those via
    /// [`Self::register_tenant_database`].
    pub fn strategy(mut self, strategy: TenantStrategy) -> Self {
        self.strategy = strategy;
        self
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

    /// Opt into an explicit tenant-key request header (wins over the subdomain).
    /// **Off by default** — a client-set header is untrusted (see TEN-1 / the
    /// [`tenant_header`](Self#structfield.tenant_header) note). Enable it ONLY
    /// when a trusted reverse proxy sets/strips this header; on a
    /// subdomain-isolated deployment leave it off so a caller can't override the
    /// proxy-pinned subdomain by sending the header.
    pub fn tenant_header(mut self, header: impl Into<String>) -> Self {
        self.tenant_header = Some(header.into());
        self
    }

    /// Explicitly disable header-based tenant resolution (the default). Use it
    /// to turn the header back off after an earlier [`Self::tenant_header`] call,
    /// or to document intent at the call site on a subdomain-only deployment.
    pub fn no_tenant_header(mut self) -> Self {
        self.tenant_header = None;
        self
    }

    /// Set the missing-tenant policy. Default:
    /// [`MissingTenant::FallThroughToPublic`].
    pub fn on_missing_tenant(mut self, policy: MissingTenant) -> Self {
        self.on_missing = policy;
        self
    }

    /// The set of SHARED **app labels** (used to filter migrations: the tenant
    /// apps are every plugin NOT in here). Resolves [`Self::tenant_apps`] when
    /// set (shared = every registered app except the tenant ones), else returns
    /// the explicit [`Self::shared_apps`] list.
    pub fn shared_app_set(&self) -> HashSet<String> {
        resolve_shared_app_set(&self.shared_apps, self.tenant_apps.as_ref())
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
        for plugin in umbral::migrate::plugin_order() {
            if !shared.contains(&plugin) {
                continue;
            }
            for meta in umbral::migrate::models_for_plugin(&plugin) {
                tables.insert(meta.table.clone());
            }
        }
        tables
    }

    /// Provision a tenant: validate the schema name, insert the [`Tenant`] row
    /// in `public` (via the ORM), then `CREATE SCHEMA` + migrate the tenant apps
    /// into it (via the core [`umbral::migrate::run_for_schema`] helper).
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
        let migrated = umbral::migrate::run_for_schema(&schema, &self.shared_app_set()).await?;
        tracing::info!(
            schema = %schema.as_str(),
            domain = %saved.domain,
            migrated,
            "umbral-tenants: provisioned tenant"
        );
        Ok(saved)
    }

    /// Onboard a tenant in **database-per-tenant** mode
    /// ([`TenantStrategy::Database`]) — the sibling of [`Self::create_tenant`].
    ///
    /// `alias` is the tenant's pool alias **and** its `schema_name` field (in
    /// Database mode `schema_name` names the pool/database, not a PG schema);
    /// it's validated as a safe identifier the same way. Steps:
    ///
    /// 1. Insert the [`Tenant`] registry row into the **default** database (the
    ///    registry lives with the app, not inside any tenant DB — there's no
    ///    tenant context here, so the router routes `tenant` to default).
    /// 2. Register `pool` under `alias` at runtime
    ///    ([`umbral::db::register_tenant_pool`]) so the [`TenantRouter`] can route
    ///    that tenant's queries to it.
    /// 3. Migrate the **tenant apps** (every plugin not in `shared_apps`) into
    ///    that pool ([`umbral::migrate::migrate_apps_into_pool`]) — the tenant
    ///    database gets its own `umbral_migrations` ledger.
    ///
    /// The operator provisions the actual database and opens the pool
    /// (`CREATE DATABASE` can't run in a transaction and is an ops concern); the
    /// framework owns routing, the registry row, and migration. First-write-wins
    /// on the pool registry: re-onboarding the same alias keeps the original
    /// pool.
    pub async fn register_tenant_database(
        &self,
        name: impl Into<String>,
        alias: impl Into<String>,
        domain: impl Into<String>,
        pool: DbPool,
    ) -> Result<Tenant, TenantError> {
        let alias = alias.into();
        // Validate up front: the alias doubles as schema_name, so it must be a
        // safe identifier (it names a pool, never reaches SQL as a schema, but
        // keeping the same gate means a tenant can be migrated to Schema mode
        // later without renaming).
        Schema::new(alias.clone()).ok_or_else(|| TenantError::InvalidSchemaName(alias.clone()))?;

        // 1. Registry row in the default DB.
        let row = Tenant {
            id: 0,
            schema_name: alias.clone(),
            name: name.into(),
            domain: domain.into(),
            is_active: true,
            created_at: Utc::now(),
        };
        let saved = Tenant::objects().create(row).await?;

        // 2. Register the runtime pool under the alias.
        umbral::db::register_tenant_pool(alias.clone(), pool);

        // 3. Migrate the tenant apps into the tenant's own database.
        let migrated =
            umbral::migrate::migrate_apps_into_pool(&alias, &self.shared_app_set()).await?;
        tracing::info!(
            alias = %alias,
            domain = %saved.domain,
            migrated,
            "umbral-tenants: onboarded tenant database"
        );
        Ok(saved)
    }
}

/// The tenant [`DatabaseRouter`]. Its routing seam depends on the configured
/// [`TenantStrategy`] — the two modes are mutually exclusive:
///
/// **Schema mode** (`schema_for_table` is the whole policy):
/// - a SHARED table → `None` (stays in `public`),
/// - a tenant-owned table under a tenant context → the tenant's schema,
/// - a tenant-owned table with **no** tenant context → `None` (public — the
///   bare-domain / background path).
///   `db_for_read`/`db_for_write` keep the defaults (one database).
///
/// **Database mode** (`db_for_read`/`db_for_write` pick the pool):
/// - a SHARED table, or no tenant ctx → the default alias,
/// - a tenant-owned table under a tenant ctx whose **alias is registered** →
///   the tenant's pool alias (its `schema_name`),
/// - a tenant-owned table under a tenant ctx whose alias is **not yet
///   onboarded** → an unroutable sentinel alias, so the query FAILS CLOSED
///   rather than silently borrowing the default database (TEN-2).
///   `schema_for_table` returns `None` (separate databases need no schema
///   qualification).
#[derive(Debug, Clone)]
pub struct TenantRouter {
    shared_tables: Arc<HashSet<String>>,
    strategy: TenantStrategy,
}

impl TenantRouter {
    /// Build a schema-mode router from the SHARED table-name set (the tables
    /// that stay in `public`). Kept for back-compat; [`Self::with_strategy`] is
    /// the general constructor.
    pub fn new(shared_tables: HashSet<String>) -> Self {
        Self::with_strategy(shared_tables, TenantStrategy::Schema)
    }

    /// Build a router for an explicit [`TenantStrategy`].
    pub fn with_strategy(shared_tables: HashSet<String>, strategy: TenantStrategy) -> Self {
        Self {
            shared_tables: Arc::new(shared_tables),
            strategy,
        }
    }
}

impl DatabaseRouter for TenantRouter {
    fn schema_for_table(&self, ctx: &RouteContext, table: &str) -> Option<Schema> {
        // Database mode: separate databases, no schema qualification ever.
        if self.strategy == TenantStrategy::Database {
            return None;
        }
        // Schema mode. SHARED table → public, regardless of tenant context.
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

    fn db_for_read(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        self.db_for(model, ctx)
    }

    fn db_for_write(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        self.db_for(model, ctx)
    }
}

impl TenantRouter {
    /// Shared read/write pool resolution for database mode. In schema mode this
    /// always falls back to the model's static alias (the trait default), so
    /// schema mode's pool selection is byte-identical to `DefaultRouter`.
    fn db_for(&self, model: &ModelMeta, ctx: &RouteContext) -> Alias {
        // The model's static alias (per-model `Model::DATABASE` / per-plugin
        // `Plugin::database()`, folded into the registry at build), else
        // `"default"` — exactly what the `DatabaseRouter` trait default resolves
        // to. Schema mode keeps this verbatim, so its pool selection is
        // byte-identical to `DefaultRouter`.
        let default = || match umbral::migrate::model_alias(&model.name) {
            Some(a) => Alias::new(a),
            None => Alias::new("default"),
        };
        if self.strategy != TenantStrategy::Database {
            return default();
        }
        // SHARED model (the Tenant registry, auth, …) → default DB, always.
        if self.shared_tables.contains(&model.table) {
            return default();
        }
        match ctx.tenant() {
            // Route to the tenant's pool — but only once its database has been
            // onboarded (register_tenant_database).
            Some(key) if umbral::db::pool_alias_registered(key.as_str()) => {
                Alias::new(key.as_str())
            }
            // A tenant IS active in the registry (so the middleware scoped a ctx
            // for it) but its pool was never registered — a provisioning gap or a
            // restart that lost the runtime pool registry. FAIL CLOSED: route to
            // an unroutable sentinel so the query aborts, never to `default()`,
            // which would silently commingle this tenant's rows in the shared DB
            // (TEN-2). Returning `default()` here trades a loud failure for a
            // silent isolation break — exactly backwards.
            Some(key) => Alias::new(format!("{UNROUTED_TENANT_PREFIX}:{}", key.as_str())),
            // No tenant context at all (bare-domain / background work) → default.
            None => default(),
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

/// The tenant the current request is scoped to, or `None` outside a tenant
/// context (bare-domain / public path / background work). Reads the ambient
/// [`RouteContext`] the resolution middleware established — by this point the
/// tenant has been resolved AND (when a [`TenantMembership`] guard is installed)
/// bound to the caller server-side, so a handler can trust it as the authorized
/// tenant rather than re-reading client input (audit_2 C3).
pub fn current_tenant() -> Option<TenantKey> {
    umbral::db::route_context().tenant().cloned()
}

/// Read the `Host` header as a string (forwarded `X-Forwarded-Host` is left to
/// a reverse proxy / the host-guard layer; here we read the literal `Host`).
fn host_header(headers: &http::HeaderMap) -> Option<&str> {
    headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
}

impl Plugin for TenantsPlugin {
    fn name(&self) -> &'static str {
        "tenants"
    }

    fn models(&self) -> Vec<ModelMeta> {
        vec![ModelMeta::for_::<Tenant>()]
    }

    fn commands(&self) -> Vec<Box<dyn umbral::cli::PluginCommand>> {
        vec![Box::new(MigrateSchemasCommand {
            shared_apps: self.shared_apps.clone(),
            tenant_apps: self.tenant_apps.clone(),
            strategy: self.strategy,
        })]
    }

    fn wrap_router(&self, router: axum::Router) -> axum::Router {
        // The resolution middleware. We capture the resolution config by value
        // (the closures must be 'static) into a small Arc'd config.
        let cfg = Arc::new(ResolverConfig {
            tenant_header: self.tenant_header.clone(),
            subdomain_base: self.subdomain_base.clone(),
            on_missing: self.on_missing,
            membership: self.membership.clone(),
        });
        router.layer(axum::middleware::from_fn_with_state(
            cfg,
            tenant_resolution_middleware,
        ))
    }

    fn on_ready(
        &self,
        _ctx: &umbral::plugin::AppContext,
    ) -> Result<(), umbral::plugin::PluginError> {
        // Fail FAST on a non-Postgres database. Schema-per-tenant isolates
        // tenants with Postgres schemas — without them there is no isolation, so
        // refuse to boot rather than silently misbehave later inside a migrate
        // or a schema-qualified query. (`TenantStrategy::Database` routes through
        // explicit per-tenant pools, so its backend is the operator's choice.)
        if self.strategy == TenantStrategy::Schema {
            let backend = umbral::db::pool_dispatched().backend_name();
            if backend != "postgres" {
                return Err(format!(
                    "umbral-tenants (schema-per-tenant) requires a Postgres database, but the \
                     default pool is `{backend}`. Postgres schemas are how tenants are isolated; \
                     point UMBRAL_DATABASE_URL at a Postgres before registering TenantsPlugin."
                )
                .into());
            }
        }

        // Install the TenantRouter now that the model registry is published, so
        // `shared_table_set()` sees every plugin's tables. First-write-wins: if
        // the app also called `App::builder().router(...)` (installed during
        // build, before on_ready), that one already won — document "don't also
        // set .router(...)".
        let router = TenantRouter::with_strategy(self.shared_table_set(), self.strategy);
        umbral::db::install_router_from_plugin(Arc::new(router));
        Ok(())
    }
}

/// Resolution-middleware config captured into the `from_fn` state.
#[derive(Clone)]
struct ResolverConfig {
    tenant_header: Option<String>,
    subdomain_base: Option<String>,
    on_missing: MissingTenant,
    membership: Option<Arc<dyn TenantMembership>>,
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
        // No tenant key in the request at all — this is the genuine bare-domain
        // / marketing path, so it honors the missing-tenant policy (public by
        // default). A *present* key is handled below and fails closed.
        return apply_missing(cfg.on_missing, next, req).await;
    };

    // Look the tenant up by domain, in public. ORM-only.
    let found = Tenant::objects()
        .filter(tenant::DOMAIN.eq(&key) & tenant::IS_ACTIVE.eq(true))
        .first()
        .await;

    match found {
        Ok(Some(t)) => {
            // audit_2 C3: the tenant exists — now bind it to the CALLER. If a
            // membership guard is installed, the request may proceed under this
            // tenant only when the guard confirms the caller belongs to it.
            // Fail closed with the SAME 404 as an unknown tenant so a non-member
            // can't distinguish "tenant doesn't exist" from "you're not a
            // member" (no enumeration oracle). Without a guard, behavior is
            // unchanged (honored on trust — safe only under subdomain isolation).
            if let Some(rejection) =
                check_membership(cfg.membership.as_ref(), req.headers(), &key).await
            {
                return rejection;
            }
            let ctx = RouteContext::new().with_tenant(TenantKey::new(t.schema_name));
            umbral::db::route_context_scope(ctx, next.run(req)).await
        }
        // A tenant key WAS supplied but matches no active tenant. FAIL CLOSED
        // with 404 regardless of `on_missing` (TEN-3): a present-but-bogus key
        // signals tenant intent, so serving it under `public` would silently run
        // one tenant's request against the shared/public context. `on_missing`
        // still governs only the no-key bare-domain path above.
        Ok(None) => reject_unknown_tenant(),
        // Lookup error → fail closed too (never fall through to public on an
        // error), with a generic body so no DB detail leaks. Detail is logged.
        Err(e) => {
            tracing::error!(key = %key, "umbral-tenants: tenant lookup failed: {e}");
            use axum::response::IntoResponse;
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "tenant resolution failed",
            )
                .into_response()
        }
    }
}

/// audit_2 C3: consult the membership guard (if any) for a resolved-and-existing
/// tenant. Returns `Some(reject)` when the guard denies the caller — a 404 that
/// matches an unknown tenant so a non-member can't tell "no such tenant" from
/// "not your tenant". `None` means proceed (no guard installed, or the guard
/// allowed). No DB access here → unit-testable without a live tenant DB.
async fn check_membership(
    guard: Option<&Arc<dyn TenantMembership>>,
    headers: &axum::http::HeaderMap,
    key: &str,
) -> Option<axum::response::Response> {
    let guard = guard?;
    if guard.allows(headers, key).await {
        return None;
    }
    tracing::warn!(
        key = %key,
        "umbral-tenants: caller is not a member of the resolved tenant; \
         rejecting (C3 membership guard)"
    );
    Some(reject_unknown_tenant())
}

/// Fail-closed response for a request that named a tenant which doesn't resolve
/// to an active one (TEN-3). A present tenant key is never served under the
/// `public` context.
fn reject_unknown_tenant() -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::NOT_FOUND, "tenant not found").into_response()
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

/// The `migrate_schemas` management command — the one-stop "migrate everything to
/// where it belongs". Two phases: (1) the SHARED apps into `public` (via
/// `run_shared`), then (2) the TENANT apps into every active [`Tenant`]'s schema
/// (idempotent). Resolves the shared/tenant split at run time against the live
/// registry, so `tenant_apps([...])` works.
struct MigrateSchemasCommand {
    shared_apps: Vec<String>,
    tenant_apps: Option<HashSet<String>>,
    strategy: TenantStrategy,
}

#[async_trait]
impl umbral::cli::PluginCommand for MigrateSchemasCommand {
    fn command(&self) -> clap::Command {
        clap::Command::new("migrate_schemas").about(
            "Migrate everything to where it belongs: the SHARED apps into public, \
             then the TENANT apps into every active tenant's schema. Idempotent — \
             the one command to run after `makemigrations`.",
        )
    }

    async fn run(&self, _matches: &clap::ArgMatches) -> Result<(), umbral::cli::CliError> {
        // Resolve the shared/tenant split now, against the live registry, so
        // `tenant_apps([...])` (shared = every other registered app) works.
        let shared = resolve_shared_app_set(&self.shared_apps, self.tenant_apps.as_ref());

        // Phase 1 — SHARED apps into `public` (the app/default DB). `run_shared`
        // (not the unfiltered `migrate`) keeps the tenant apps OUT of public.
        let shared_n = umbral::migrate::run_shared(&shared).await?;
        tracing::info!(
            applied = shared_n,
            "umbral-tenants migrate_schemas: shared apps → public"
        );

        // Phase 2 — TENANT apps into each tenant's schema. In Database strategy
        // the per-tenant databases are migrated when you onboard them with
        // `register_tenant_database`, so there is nothing schema-scoped here.
        if self.strategy == TenantStrategy::Database {
            tracing::info!(
                "umbral-tenants migrate_schemas: database strategy — tenant databases \
                 migrate at register_tenant_database time"
            );
            return Ok(());
        }

        let tenants = Tenant::objects()
            .filter(tenant::IS_ACTIVE.eq(true))
            .fetch()
            .await?;
        if tenants.is_empty() {
            tracing::info!("umbral-tenants migrate_schemas: no active tenants yet");
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
                        "umbral-tenants migrate_schemas: skipping tenant with invalid schema name"
                    );
                    continue;
                }
            };
            let n = umbral::migrate::run_for_schema(&schema, &shared).await?;
            total += n;
            tracing::info!(
                schema = %schema.as_str(),
                domain = %t.domain,
                applied = n,
                "umbral-tenants migrate_schemas: migrated tenant schema"
            );
        }
        tracing::info!(
            tenants = tenants.len(),
            applied = total,
            "umbral-tenants migrate_schemas: tenant schemas done"
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

    /// A membership guard that allows the request iff its `x-user` header equals
    /// a member id in the allowed set — a stand-in for a real session-user →
    /// tenant-membership lookup.
    #[derive(Debug)]
    struct HeaderMembership(&'static str);
    #[async_trait::async_trait]
    impl TenantMembership for HeaderMembership {
        async fn allows(&self, headers: &axum::http::HeaderMap, _tenant_key: &str) -> bool {
            headers.get("x-user").and_then(|v| v.to_str().ok()) == Some(self.0)
        }
    }

    fn headers_with_user(user: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(u) = user {
            h.insert("x-user", u.parse().unwrap());
        }
        h
    }

    #[tokio::test]
    async fn no_guard_proceeds() {
        // audit_2 C3: without a membership guard, resolution is unchanged.
        assert!(
            check_membership(None, &headers_with_user(None), "acme")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn member_proceeds_non_member_rejected_404() {
        let guard: Arc<dyn TenantMembership> = Arc::new(HeaderMembership("alice"));

        // Member → proceed.
        assert!(
            check_membership(Some(&guard), &headers_with_user(Some("alice")), "acme")
                .await
                .is_none(),
            "a member must be allowed under the tenant"
        );

        // Non-member and anonymous → fail closed with 404 (no enumeration oracle).
        for who in [Some("mallory"), None] {
            let resp = check_membership(Some(&guard), &headers_with_user(who), "acme")
                .await
                .expect("non-member must be rejected");
            assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn current_tenant_reads_the_scoped_context() {
        // Outside any scope → no tenant.
        assert!(current_tenant().is_none());
        // Inside a scoped RouteContext → the bound tenant.
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        umbral::db::route_context_scope(ctx, async {
            assert_eq!(
                current_tenant().map(|t| t.as_str().to_string()),
                Some("acme".to_string())
            );
        })
        .await;
        // Back to none after the scope.
        assert!(current_tenant().is_none());
    }

    /// Schema-per-tenant needs Postgres schemas — registering the plugin on a
    /// non-Postgres pool must FAIL at build (in `on_ready`), not silently later.
    #[tokio::test]
    async fn schema_strategy_rejects_non_postgres_pool_at_build() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite memory pool");
        let result = umbral::App::builder()
            .settings(umbral::Settings::from_env().expect("settings"))
            .database("default", pool)
            .plugin(TenantsPlugin::new()) // schema strategy (the default)
            .build();
        assert!(
            result.is_err(),
            "TenantsPlugin (schema strategy) must reject a non-Postgres default pool"
        );
        let msg = format!("{}", result.err().unwrap());
        assert!(
            msg.to_lowercase().contains("postgres"),
            "the build error should name Postgres, got: {msg}"
        );
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

    #[test]
    fn new_disables_tenant_header_by_default() {
        // TEN-1 partial: header-based tenant selection is untrusted, so it is
        // OFF by default. A default deployment must not honor `X-Tenant`.
        assert!(
            TenantsPlugin::new().tenant_header.is_none(),
            "the X-Tenant header must be opt-in, not on by default"
        );
    }

    #[test]
    fn tenant_header_opts_in_and_no_tenant_header_disables() {
        let on = TenantsPlugin::new().tenant_header("X-Tenant");
        assert_eq!(on.tenant_header.as_deref(), Some("X-Tenant"));
        let off = on.no_tenant_header();
        assert!(off.tenant_header.is_none());
    }

    #[test]
    fn header_is_ignored_when_resolution_is_disabled() {
        // With header resolution off (None), a client-set X-Tenant is ignored
        // and only the subdomain (if configured) can select a tenant.
        let mut h = HeaderMap::new();
        h.insert("x-tenant", "victim".parse().unwrap());
        h.insert(http::header::HOST, "acme.example.com".parse().unwrap());
        // No header configured → the header is NOT trusted; subdomain wins.
        assert_eq!(
            resolve_tenant_key(&h, None, Some("example.com")).as_deref(),
            Some("acme"),
        );
        // No header AND no subdomain base → no tenant at all.
        assert!(resolve_tenant_key(&h, None, None).is_none());
    }

    #[test]
    fn reject_unknown_tenant_is_404() {
        // TEN-3: a present-but-unresolved tenant key fails closed with 404.
        let resp = reject_unknown_tenant();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    // -- Database-mode (TenantStrategy::Database) router tests ----------------

    fn meta_for_table(table: &str) -> ModelMeta {
        // A minimal late-bound model meta; db_for only reads `name` + `table`.
        ModelMeta {
            name: format!("Model_{table}"),
            table: table.to_string(),
            ..ModelMeta::default()
        }
    }

    /// Register a throwaway in-memory sqlite pool under a unique alias so
    /// `pool_alias_registered(alias)` is true for it. Uses a process-unique
    /// alias to avoid the global pool-registry racing other tests (see the #30
    /// process-global-state convention).
    async fn register_unique_pool() -> String {
        let alias = format!(
            "tnt_test_db_{}_{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("sqlite mem pool");
        umbral::db::register_tenant_pool(alias.clone(), umbral::db::DbPool::Sqlite(pool));
        alias
    }

    #[tokio::test]
    async fn db_mode_routes_tenant_model_to_registered_alias() {
        let alias = register_unique_pool().await;
        let router = TenantRouter::with_strategy(shared_set(&["tenant"]), TenantStrategy::Database);
        let ctx = RouteContext::new().with_tenant(TenantKey::new(alias.clone()));
        let post = meta_for_table("post");
        // Tenant-owned model under a tenant ctx whose alias IS registered →
        // routes to the tenant's pool for both read and write.
        assert_eq!(router.db_for_write(&post, &ctx).as_str(), alias);
        assert_eq!(router.db_for_read(&post, &ctx).as_str(), alias);
        // schema_for_table is always None in Database mode.
        assert!(router.schema_for_table(&ctx, "post").is_none());
    }

    #[tokio::test]
    async fn db_mode_fails_closed_when_alias_not_registered() {
        // TEN-2: a tenant ctx whose pool was never registered must NOT fall back
        // to the default database (which would commingle un-onboarded tenants).
        // It routes to an unroutable sentinel so the terminal aborts.
        let router = TenantRouter::with_strategy(shared_set(&["tenant"]), TenantStrategy::Database);
        let ctx = RouteContext::new().with_tenant(TenantKey::new("never_onboarded_xyz"));
        let post = meta_for_table("post");
        for alias in [
            router.db_for_write(&post, &ctx),
            router.db_for_read(&post, &ctx),
        ] {
            assert_ne!(
                alias.as_str(),
                "default",
                "un-onboarded tenant must never fall back to the default DB"
            );
            assert!(
                alias.as_str().starts_with(UNROUTED_TENANT_PREFIX),
                "expected an unroutable sentinel alias, got {}",
                alias.as_str()
            );
            // The sentinel names the tenant for the operator, and is never a
            // registered pool (so the terminal fails closed).
            assert!(alias.as_str().contains("never_onboarded_xyz"));
            assert!(!umbral::db::pool_alias_registered(alias.as_str()));
        }
    }

    #[tokio::test]
    async fn db_mode_shared_model_always_default() {
        let alias = register_unique_pool().await;
        let router = TenantRouter::with_strategy(shared_set(&["tenant"]), TenantStrategy::Database);
        // SHARED model under a tenant ctx whose alias IS registered → still
        // default (the registry lives in the app DB).
        let ctx = RouteContext::new().with_tenant(TenantKey::new(alias));
        let tenant = meta_for_table("tenant");
        assert_eq!(router.db_for_write(&tenant, &ctx).as_str(), "default");
        assert_eq!(router.db_for_read(&tenant, &ctx).as_str(), "default");
    }

    #[tokio::test]
    async fn db_mode_no_tenant_ctx_is_default() {
        let router = TenantRouter::with_strategy(shared_set(&["tenant"]), TenantStrategy::Database);
        let ctx = RouteContext::new();
        let post = meta_for_table("post");
        assert_eq!(router.db_for_write(&post, &ctx).as_str(), "default");
        assert_eq!(router.db_for_read(&post, &ctx).as_str(), "default");
    }

    #[test]
    fn schema_mode_db_for_is_default_unchanged() {
        // In the default (Schema) strategy db_for_read/write never deviate from
        // the model's static alias — schema mode pool selection is byte-
        // identical to DefaultRouter.
        let router = TenantRouter::new(shared_set(&["tenant"]));
        let ctx = RouteContext::new().with_tenant(TenantKey::new("acme"));
        let post = meta_for_table("post");
        assert_eq!(router.db_for_write(&post, &ctx).as_str(), "default");
        assert_eq!(router.db_for_read(&post, &ctx).as_str(), "default");
    }
}
