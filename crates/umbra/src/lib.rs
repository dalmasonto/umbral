//! A Django-shape web framework for Rust.
//!
//! `umbra` is the stable surface for users and plugin authors. Code in
//! user crates and third-party plugins should `use umbra::prelude::*;`
//! and never depend on `umbra-core` or `umbra-macros` directly. The
//! visibility rule is set out in `arch.md §2.1`; the contract that plugin
//! authors write against lives in `docs/specs/02-plugin-contract.md`.
//!
//! The internal crate split (`umbra-core`, `umbra-macros`, …) can be
//! refactored freely as long as the names exposed here keep their shape.
//!
//! Status: M0. Surfaces `Settings`, the db pool accessors, the web
//! re-exports, and the `App` builder.

pub mod prelude {
    //! Common imports for handlers, models, and plugin authors.
    //!
    //! `use umbra::prelude::*;` pulls in the items most code reaches for:
    //! the `App` builder, `Settings`, the router-construction surface, the
    //! standard extractors, and the response traits. Power-user items live
    //! on the facade itself rather than in the prelude: for example, the
    //! raw pool accessors are reached as `umbra::db::pool()` so they do
    //! not pollute the prelude with bare names like `pool`.

    pub use crate::db::{DatabaseRouter, RouteContext, TenantKey};
    pub use crate::middleware::Middleware;
    pub use crate::orm::{
        ChoiceField, Choices, F, FColExt, FileField, ForeignKey, ImageField, M2M, Masked, Model,
        MultiChoice, OneToOne, Q, ReverseRelations,
    };
    pub use crate::plugin::{AppContext, Plugin, StaticDir};
    pub use crate::routes::Routes;
    // The `Storage` trait so plugin authors can implement a custom
    // file-bytes backend via `use umbra::prelude::*`. The ambient
    // accessors (`storage`/`try_storage`/`set_storage`/`storage_opt`) stay on the
    // `umbra::storage` module — power-user surface, not bare names.
    pub use crate::storage::Storage;
    pub use crate::web::{
        Form, IntoResponse, Json, JsonResponse, Path, Query, Router, StreamingResponse, delete,
        get, patch, post, put,
    };
    pub use crate::{App, AppBuilder, Environment, Settings};
    // `models![Product, Order, Customer]` — type-safe shorthand
    // for "the TABLE strings of these models." Brought into the
    // prelude because it pairs with the rest of the model APIs.
    pub use crate::models;
}

/// The `async_trait` attribute macro, re-exported so plugin authors can
/// write `#[umbra::async_trait] impl Middleware for ...` (feature #68)
/// without declaring a direct `async-trait` dependency. The desugared
/// trait is dyn-compatible (`Arc<dyn Middleware>`), which a native
/// `async fn` in a trait is not.
pub use async_trait::async_trait;

/// Resolve a list of `Model` types to their `TABLE` strings.
/// Use anywhere an API takes table names — admin config, model
/// allowlists, anywhere — so a `#[umbra(table = "...")]` rename
/// or a struct rename propagates without chasing string
/// references through downstream config.
///
/// ```rust,ignore
/// use umbra::prelude::*;
/// use ecommerce::models::{Product, Order, Customer};
///
/// AdminPlugin::default()
///     .dashboard_models_only(&models![Product, Order, Customer])
/// ```
///
/// Each type must implement `umbra::orm::Model` (every
/// `#[derive(Model)]` struct does). Expansion is a plain array
/// literal `[T::TABLE, U::TABLE, ...]` so you can pass it as
/// `&models![...]` to any `&[&str]`-accepting API.
#[macro_export]
macro_rules! models {
    ($($model:ty),+ $(,)?) => {
        [$(<$model as $crate::orm::Model>::TABLE),+]
    };
}

/// Re-export of `serde_json` for use in macro-generated code.
///
/// The `#[derive(Model)]` macro emits `::umbra::_serde_json::from_value`
/// in `HydrateRelated::hydrate_fk` bodies. Routing through this re-export
/// means user crates don't need a direct `serde_json` dep for the generated
/// code to compile (umbra already depends on it).
#[doc(hidden)]
pub use serde_json as _serde_json;

/// Re-export of `sea_query` for use in macro-generated code.
///
/// The `#[derive(Model)]` macro emits `::umbra::_sea_query::Value` in the
/// `write_pending_m2m` body (form-staged M2M junction writes). Routing
/// through this re-export means user crates don't need a direct
/// `sea-query` dep for the generated code to compile.
#[doc(hidden)]
pub use umbra_core::_sea_query;

/// Re-export of `sqlx` for use in macro-generated code.
///
/// `#[derive(Choices)]` emits `sqlx::Type` / `Encode` / `Decode` impls
/// behind `::umbra::_sqlx::*`, so user crates don't need their own
/// direct `sqlx` dependency for the generated code to compile.
#[doc(hidden)]
pub use sqlx as _sqlx;

pub use umbra_core::app::{App, AppBuilder, BuildError};
pub use umbra_core::settings::{Environment, Settings};

/// The authentication identity contract (gaps2 #76).
///
/// [`Identity`] and [`Authentication`] live in `umbra-core` so that
/// `umbra-auth` and `umbra-rest` both depend *inward* on core.
/// Re-exported here so plugin authors and app code reach them via
/// `umbra::auth::Identity` / `umbra::auth::Authentication`, and so
/// `umbra-auth` can drop its `umbra-rest` dependency entirely.
pub mod auth {
    pub use umbra_core::auth_contract::{
        Authentication, ChainAuthentication, FnAuthentication, Identity, NoAuthentication,
        parse_basic_credentials,
    };
}

/// CORS configuration for [`AppBuilder::cors`]. See
/// [`umbra_core::cors`] for the full surface.
pub mod cors {
    pub use umbra_core::cors::CorsConfig;
}

/// Feature #74 — per-model JSON fixture load / dump for tests
/// and dev seeding. Plain JSON arrays of row objects; hand-
/// editable, diff-friendly. See [`umbra_core::fixtures`] for the
/// full API surface.
pub mod fixtures {
    pub use umbra_core::fixtures::{FixtureError, dump_fixture, load_fixture};
}

/// Gap 106: IANA timezone-aware datetime marshalling.
///
/// Re-exports the helpers the admin form layer uses to render
/// stored UTC values as wall-clock local time, and to interpret
/// naive form inputs in the configured project tz. The active tz
/// comes from [`Settings::time_zone`]; `None` falls back to UTC
/// (the historical behaviour).
pub mod timezone {
    pub use umbra_core::timezone::{active_tz, naive_local_to_utc, tz_or_utc, utc_to_naive_local};
}

/// Settings accessors — `get()` returns the live `Settings` published
/// at `App::build` time. Used by plugin code that needs to branch on
/// `environment` / `bind_addr` etc. (e.g. umbra-email checking whether
/// to warn about console-backend usage in prod).
///
/// `get_opt()` is the non-panicking variant; returns `None` when
/// `App::build()` has not been called yet. Useful in plugin helpers that
/// run at route-build time (before boot) and need to check the environment
/// without forcing the app to be fully initialised.
pub mod settings {
    pub use umbra_core::settings::{get, get_opt};
}

pub mod db {
    //! Database pool accessors and transaction helpers.
    //!
    //! `connect` opens a new pool dispatched on the URL scheme,
    //! returning a [`DbPool`] enum (sqlite or postgres). For callers
    //! that want a typed sqlite pool directly, use [`connect_sqlite`].
    //!
    //! `pool` / `pool_for` keep returning [`sqlx::SqlitePool`]
    //! through Phase 1 of the Postgres rollout so existing plugin
    //! code continues to compile unchanged. The dispatched
    //! versions ([`pool_dispatched`] and [`pool_for_dispatched`])
    //! hand back a `&DbPool` for code that's ready to branch on the
    //! backend.
    //!
    //! ## Transactions
    //!
    //! Use [`transaction`] to run a multi-statement business operation
    //! that commits on `Ok` and rolls back on `Err`:
    //!
    //! ```rust,ignore
    //! use umbra::db::transaction;
    //!
    //! let order = transaction(|tx| Box::pin(async move {
    //!     let o = Order::objects().on_tx(tx).create(new_order).await?;
    //!     Stock::objects()
    //!         .on_tx(tx)
    //!         .filter(stock::SKU.eq(sku))
    //!         .update_values(delta)
    //!         .await?;
    //!     Ok::<_, MyError>(o)
    //! })).await?;
    //! ```

    pub use umbra_core::db::route_context::scope as route_context_scope;
    pub use umbra_core::db::{
        Alias, DatabaseRouter, DbPool, DefaultRouter, RouteContext, RouteOp, Schema, TenantKey,
        Transaction, TxFuture, begin, begin_pg, begin_sqlite, close, connect, connect_sqlite, ping,
        pool, pool_dispatched, pool_for, pool_for_dispatched, registered_aliases, route_context,
        router, transaction, transaction_pg, transaction_sqlite,
    };
}

/// Run an async closure inside a database transaction against the ambient pool.
///
/// Sugar for `umbra::db::transaction(...)`. See that function for full docs.
///
/// ```rust,ignore
/// let order = umbra::transaction(|tx| Box::pin(async move {
///     let o = Order::objects().on_tx(tx).create(new_order).await?;
///     Stock::objects().on_tx(tx).filter(...).update_values(...).await?;
///     Ok::<_, MyError>(o)
/// })).await?;
/// ```
pub use umbra_core::db::{transaction, transaction_pg, transaction_sqlite};

pub mod backend {
    //! The database backend abstraction (M4).
    //!
    //! `active()` returns the backend that `App::build()` matched
    //! against `settings.database_url`. Plugin code consults this when
    //! it needs to gate Postgres-only behaviour or render dialect-aware
    //! SQL. See `docs/specs/05-backends-and-system-check.md`.

    pub use umbra_core::backend::{
        BackendDetectError, BackendFeature, DatabaseBackend, PostgresBackend, SqliteBackend,
        active, detect,
    };
}

pub mod check {
    //! The boot-time system check framework (M4).
    //!
    //! Plugins return their own checks from `Plugin::system_checks()`
    //! (M7); the framework's built-in checks live in `umbra-core` and
    //! get walked alongside in phase 4 of `App::build()`.

    pub use umbra_core::check::{
        CheckContext, CheckLocation, Severity, SystemCheck, SystemCheckFinding, framework_checks,
        run_all,
    };
}

pub mod migrate {
    //! The migration engine (M5).
    //!
    //! Implements the declare → migrate → change → migrate loop. Users
    //! register models with `App::builder().model::<T>()`; `make`
    //! generates a JSON migration file from the diff against the
    //! latest snapshot; `run` applies pending migrations against the
    //! ambient pool inside one transaction per file.

    pub use umbra_core::migrate::{
        APP_PLUGIN_NAME, ClassifiedOp, Column, DriftReport, M2MRelation, MIGRATIONS_DIR,
        MigrateError, MigrationEntry, MigrationFile, MigrationRef, MigrationStatus, ModelMeta,
        OpSafety, Operation, Snapshot, check_pending_safety, check_pending_safety_in,
        classify_operation, detect_all_drift, detect_drift, diff, fake_apply, fake_apply_in,
        fake_initial, fake_initial_in, fk_effective_type, make, make_in, model_alias,
        model_meta_for_table, models_for_plugin, pk_meta_for_table, plugin_order, record_applied,
        registered_api_endpoints, registered_models, registered_plugins, render_operation_for, run,
        run_checked, run_checked_in, run_in, show, show_in, table_alias,
    };
}

pub mod routes {
    //! Route registry — snapshot of every declared URL path the
    //! framework knows about, grouped by plugin.
    //!
    //! Populated at `App::build()` time from the user binary's
    //! `Routes` (via `AppBuilder::routes`) plus each plugin's
    //! `Plugin::route_paths()` contribution. Read by
    //! the dev-mode default 404 page to surface the route list when a
    //! request misses every match.

    pub use umbra_core::routes::{
        RouteRegistry, RouteSpec, Routes, get, init, init_openapi, init_openapi_spec_url,
        registered_openapi_paths, registered_openapi_spec_url,
    };
}

pub mod plugin {
    //! The Plugin trait (M7), umbra's extension mechanism.
    //!
    //! Auth, sessions, admin, tasks, REST, and OpenAPI are all
    //! plugins; so is every third-party crate that ships models,
    //! routes, or commands. The trait is also re-exported from the
    //! prelude so `use umbra::prelude::*;` brings it in.

    pub use umbra_core::plugin::{
        ApiEndpoint, AppContext, Plugin, PluginError, StaticDir, StaticFile,
        block_on_ready,
    };
}

pub mod middleware {
    //! Framework request/response middleware (feature #68).
    //!
    //! Implement [`Middleware`]'s `before_request` / `after_response`
    //! hooks for the common "look at every request / response" case;
    //! register it with `AppBuilder::middleware` or contribute it from a
    //! plugin via `Plugin::middleware`. The trait is also re-exported
    //! from the prelude. [`MiddlewareStack`] is the collected, ordered
    //! set `App::build` installs as one axum layer.

    pub use umbra_core::middleware::{Middleware, MiddlewareStack};
}

pub mod static_files {
    //! The unified static-asset pipeline's request → file resolution.
    //!
    //! Plugins contribute on-disk source dirs via
    //! [`Plugin::static_dirs`](crate::plugin::Plugin::static_dirs); the
    //! framework mounts one handler at `settings.static_url` that serves
    //! `/static/<namespace>/<rest>` live-from-source in dev and from
    //! `settings.static_root` in prod.
    //!
    //! Power-user surface — most code never names these types directly.
    //! [`serve_file`] is the framework's single file-serving function
    //! (Content-Type, ETag, range, conditional requests via
    //! `tower_http::ServeFile`); a plugin that needs to serve a file off
    //! disk should route through it rather than hand-rolling MIME / range
    //! handling. [`resolve_under_root`] is the path-traversal-safe
    //! resolver behind the handler.

    pub use umbra_core::static_files::{
        CollectError, CollectSummary, CollectedNamespace, LocalStorage, MANIFEST_FILENAME,
        MissingSourceDir, PublishedStatic, StaticContribution, StaticError, StaticHandlerState,
        StaticNamespaceCollision, StaticRegistry, StaticStorage, collect_into, collect_into_with,
        collect_static, content_hash, hashed_name, load_manifest, manifest_loaded, manifest_lookup,
        publish_static, published_static, resolve_under_root, serve_file, static_handler,
    };
}

pub mod storage {
    //! The file-bytes storage backend and its ambient registry.
    //!
    //! [`Storage`] is the backend-agnostic seam for file uploads — the
    //! file counterpart to the DB pool. The filesystem impl (`FsStorage`)
    //! ships in the `umbra-media` plugin, which registers it as the
    //! ambient default in `on_ready`; future `FileField` / `ImageField`
    //! and the admin resolve uploads through [`storage`] without naming
    //! a backend.
    //!
    //! Plugin authors implementing a custom backend get the [`Storage`]
    //! trait from the prelude (`use umbra::prelude::*`). The ambient
    //! accessors ([`storage`], [`try_storage`], [`storage_opt`], [`set_storage`]) are
    //! power-user surface, reached as `umbra::storage::storage()` so they
    //! don't pollute the prelude with bare names.

    pub use umbra_core::storage::{
        ByteStream, Storage, StorageError, StoredFile, cap_stream, is_cap_exceeded, set_storage,
        storage, storage_opt, try_storage,
    };

    /// Re-export of `async-trait` so a plugin author implementing
    /// [`Storage`] (an `#[async_trait]` trait) can annotate the impl as
    /// `#[umbra::storage::async_trait]` without adding a direct
    /// `async-trait` dependency to their crate. Mirrors
    /// `umbra::forms::async_trait`.
    pub use umbra_core::storage::async_trait_reexport as async_trait;
}

pub mod cli {
    //! Plugin-contributed CLI subcommands.
    //!
    //! Plugins implement [`PluginCommand`] and return values from
    //! `Plugin::commands()`; the user's binary calls [`dispatch`] with
    //! the App's plugin list to route args to the right handler. See
    //! `docs/specs/02-plugin-contract.md` and `umbra-core::cli` for
    //! the full design.
    //!
    //! ```ignore
    //! use umbra::cli::dispatch;
    //!
    //! #[tokio::main]
    //! async fn main() {
    //!     let app = umbra::App::builder()
    //!         .plugin(my_plugin::MyPlugin)
    //!         .build()
    //!         .unwrap();
    //!     dispatch(app.plugins(), std::env::args_os()).await.unwrap();
    //! }
    //! ```

    pub use umbra_core::cli::{CliError, DispatchOutcome, PluginCommand, dispatch};
}

pub mod forms {
    //! Form parsing, validation, and HTML rendering.
    //!
    //! Two layers:
    //!
    //! - **Primitives**: `Field`, `Validator` impls (`Required`,
    //!   `MinLength`, `MaxLength`, `EmailFormat`), `ValidationErrors`.
    //!   Build forms by hand when the macro doesn't fit.
    //! - **`#[derive(Form)]`**: lowers a struct + per-field
    //!   `#[form(min_length = N, email, password, optional, ...)]`
    //!   attrs into an `impl Form` that validates a `HashMap` into
    //!   the typed struct and renders the HTML.

    pub use umbra_core::forms::{
        EmailFormat, Field, Form, FormErrors, FormValidate, InputKind, MaxLength, MinLength,
        PkKind, Required, ValidationErrors, Validator,
    };

    /// Re-export of `async-trait` so the `#[derive(Form)]` macro can
    /// name `::umbra::forms::async_trait` on the `impl FormValidate`
    /// it emits — the trait is `#[async_trait]`, so its impls must be
    /// too. `#[doc(hidden)]`: an implementation detail of the derive,
    /// not a surface users call directly.
    #[doc(hidden)]
    pub use umbra_core::forms::async_trait_reexport as async_trait;

    /// The `#[derive(Form)]` proc-macro. Shares the `Form` name with
    /// the extractor struct — Rust's type and macro namespaces are
    /// separate so both ride in on one import. The derive emits
    /// `impl FormValidate for <Struct>`; the `Form<T>` extractor
    /// then calls into that impl via `FromRequest`.
    pub use umbra_macros::Form;
}

pub mod backup {
    //! Dump every registered model's rows to JSON; load them back.
    //!
    //! The `dump` / `load` pair is the upgrade-safety net: a user
    //! preparing for a breaking framework change runs `dumpdata`,
    //! migrates the schema (or the framework version), then runs
    //! `loaddata` to put their rows back. The receiving schema has
    //! to already exist — `loaddata` doesn't run migrations.
    //!
    //! `umbra-cli dumpdata --output backup.json` and `umbra-cli
    //! loaddata backup.json` are the CLI entry points.

    pub use umbra_core::backup::{
        BackupError, Dump, LoadReport, ModelDump, dump, dump_to_path, load, load_from_path,
    };
}

pub mod templates {
    //! Server-side HTML rendering via minijinja.
    //!
    //! Templates live under a project-level `templates/` directory
    //! (configurable on the builder via `AppBuilder::templates_dir`).
    //! Render with `umbra::templates::render(name, &ctx)`; the engine
    //! is published into a process-wide ambient handle by
    //! `App::build()`, same pattern as the DB pool.
    //!
    //! `minijinja::context!` is the most ergonomic context builder —
    //! it's re-exported here so a user crate doesn't need to depend on
    //! minijinja directly.

    //! Plugins add custom tags/filters by returning
    //! [`TemplateRegistrar`]s from `Plugin::template_registrars`; each is
    //! a closure over the minijinja [`Environment`] (re-exported here so a
    //! plugin crate doesn't depend on minijinja directly).

    pub use minijinja::{Environment, Value, context};
    #[doc(hidden)]
    pub use umbra_core::templates::render_str;
    pub use umbra_core::templates::{
        CURRENT_CSRF, CURRENT_USER, LazyUser, TemplateError, TemplateRegistrar, current_csrf,
        highlight_css, merge_ambient_context, merge_ambient_value, render, resolve_static_url,
        with_current_csrf, with_current_user, with_current_user_lazy,
    };
}

pub mod pagination {
    //! Template-rendered list-view pagination (Django `core.paginator`
    //! parity).
    //!
    //! A [`Paginator`] counts a [`QuerySet`](crate::orm::QuerySet) once and
    //! slices it into fixed-size [`Page`]s for server-rendered (Jinja) list
    //! views — distinct from REST's JSON pagination. No plugin to register:
    //! `Paginator::new(Post::objects().order_by(..), 10).page(n).await?`,
    //! then pass `page.context()` into a template that `{% include %}`s the
    //! bundled `_pagination.html` partial. The `{{ querystring_with(..) }}`
    //! template global (registered by the core engine) rebuilds the
    //! querystring so a `?sort=` filter survives every `?page=N` link.

    pub use umbra_core::pagination::{
        Page, PageContext, PageError, PageItem, PageItemContext, PaginationError, Paginator,
        querystring_with,
    };
}

pub mod ratelimit {
    //! In-memory sliding-window rate limiter — the primitive behind
    //! umbra-rest's API throttles ([`umbra_rest::throttle`]).
    //!
    //! Not a plugin: a [`RateLimiter`] is a standalone keyed counter a
    //! throttle (or any caller) wraps. Build a [`Rate`] from a DRF-style
    //! string (`Rate::parse("100/hour")`) and ask `limiter.check(key)` for
    //! a [`RateDecision`]. See `umbra-core`'s `ratelimit` module for the
    //! window semantics and the single-process / multi-instance caveats.

    pub use umbra_core::ratelimit::{Rate, RateDecision, RateLimiter};
}

pub mod signals {
    //! In-process signal registry.
    //!
    //! The generic name-keyed pub/sub that the ORM write paths call
    //! automatically for per-row lifecycle events. Use
    //! `umbra_signals::on_model::<M>()` from the `umbra-signals` plugin
    //! for the typed per-model API; reach `umbra::signals::subscribe`
    //! directly only for application-defined (non-model-tied) signals.

    pub use umbra_core::signals::{
        clear_for_tests, current_actor, emit, subscribe, subscribe_async, with_actor,
    };
}

pub mod inspect {
    //! `inspectdb` (M6): introspect an existing database into umbra
    //! models that drop straight into the M5 migration loop.
    //!
    //! M6 v1 ships SQLite introspection and a flat output (one
    //! `models.rs` plus `migrations/app/0001_initial.json`). The
    //! plugin-crate output, Postgres backend, FK / index detection,
    //! and the `--strip-prefix` / `--ignore-builtin` flags are
    //! deferred per `docs/specs/07-inspectdb.md`.

    pub use umbra_core::inspect::{
        INITIAL_MIGRATION_ID, INSPECTED_PLUGIN_NAME, InspectError, InspectOptions, InspectReport,
        IntrospectedColumn, IntrospectedSchema, IntrospectedTable, inspectdb, introspect_pool,
        introspect_pool_pg, render_initial_migration, render_models, write_outputs,
    };
}

pub mod web {
    //! The web layer: router, extractors, and response types.
    //!
    //! Re-exports `umbra-core`'s web surface (currently a curated slice of
    //! axum). Later milestones will add umbra-specific wrappers without
    //! changing the names here.

    pub use umbra_core::slash::SlashRedirect;
    pub use umbra_core::web::multipart::{
        FilePart, MultipartError, MultipartForm, MultipartUploadError, is_multipart,
        parse_and_store_multipart, parse_multipart,
    };
    pub use umbra_core::web::*;
}

pub mod orm {
    //! The ORM: model trait, querysets, column types, and the `Model`
    //! derive.
    //!
    //! At M2 the trait `Model` is implemented by hand on user types; at
    //! M3 the same impl is generated by `#[derive(Model)]`. The QuerySet
    //! is generic over `T: Model`, so plugin authors and users get the
    //! full query API by implementing one trait, by hand or via derive.

    pub use umbra_core::orm::write::{SaveError, WriteError, slugify};
    pub use umbra_core::orm::{
        Aggregate, AggregateKind, ArrayElement, ChoiceField, CsvImportReport, DynError,
        DynQuerySet, Email, F, FColExt, FExpr, FieldSpec, FileField, FkAction, ForeignKey,
        GetError, HydrateRelated, ImageField, JoinKind, M2M, M2MRelationSpec, Manager, MaskError,
        MaskKeyring, Masked, Model, MultiChoice, OneToOne, OneToOneRelationSpec, Post, Predicate,
        PrimaryKey, Q, QuerySet, QuerySetTx, ReverseError, ReverseFkRelationSpec, ReverseRelations,
        ReverseSet, Search, SearchHit, Searchable, Slug, SqlType, TryForEachError, TsVector, Url,
        ValidatorError, column, decode_to_string, escape_like_literal, import_table_rows,
        load_junction_selection, pk_key, set_junction_dynamic, set_mask_keyring,
        validate_text_format, write,
    };

    /// The `#[derive(Model)]` proc macro. Shares the `Model` name with the
    /// trait — Rust's type and macro namespaces are separate, so both can
    /// coexist behind one import.
    pub use umbra_macros::Model;

    /// The `#[derive(Choices)]` proc macro for closed-set enum field
    /// types. Pair the enum derive with `#[umbra(choices)]` on the
    /// owning model field. See `umbra::orm::ChoiceField`.
    pub use umbra_macros::Choices;

    /// The typed column constants for the demo `Post` model.
    ///
    /// `umbra::orm::post::ID`, `::TITLE`, `::BODY`, `::PUBLISHED_AT`. `Post`
    /// itself lives at `umbra::orm::Post`. The model is a development
    /// fixture: M3's `#[derive(Model)]` retires it; users defining their
    /// own model produce their own column module from the derive.
    pub use umbra_core::orm::post::post;

    /// Runtime helpers the `#[derive(Form)]` macro emits calls to —
    /// choice membership, FK existence probes, async `<select>` option
    /// fetches, M2M id parsing. `#[doc(hidden)]`: an implementation
    /// detail of the Form derive, not a surface users call by hand.
    #[doc(hidden)]
    pub use umbra_core::orm::forms_runtime;
}

/// The `#[task]` attribute macro.
///
/// Marks an `async fn` as an umbra background task. Emits the original
/// function unchanged and a companion `register_<fn_name>()` function
/// that registers the handler with `umbra_tasks::register_handler`.
///
/// Generated code references `::umbra_tasks::register_handler` directly,
/// so user crates that apply `#[umbra::task]` must have `umbra-tasks` in
/// their `[dependencies]` (they would anyway, since using the tasks plugin
/// implies depending on `umbra-tasks`).
///
/// # Constraints
///
/// - Must be `async fn`.
/// - Must take exactly one parameter (the typed payload implementing
///   `serde::Deserialize`).
/// - Must return `Result<(), String>`.
///
/// # Example
///
/// ```ignore
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct WelcomeEmailPayload { user_id: i64 }
///
/// #[umbra::task]
/// async fn send_welcome(payload: WelcomeEmailPayload) -> Result<(), String> {
///     Ok(())
/// }
///
/// // At boot (Plugin::on_ready or main):
/// register_send_welcome();
/// ```
pub use umbra_macros::task;
