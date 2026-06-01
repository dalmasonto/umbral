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

    pub use crate::orm::{F, FColExt, ForeignKey, Model, Q};
    pub use crate::plugin::{AppContext, Plugin};
    pub use crate::web::{
        Form, IntoResponse, Json, JsonResponse, Path, Query, Router, delete, get, patch, post, put,
    };
    pub use crate::{App, AppBuilder, Environment, Settings};
}

/// Re-export of `serde_json` for use in macro-generated code.
///
/// The `#[derive(Model)]` macro emits `::umbra::_serde_json::from_value`
/// in `HydrateRelated::hydrate_fk` bodies. Routing through this re-export
/// means user crates don't need a direct `serde_json` dep for the generated
/// code to compile (umbra already depends on it).
#[doc(hidden)]
pub use serde_json as _serde_json;

pub use umbra_core::app::{App, AppBuilder, BuildError};
pub use umbra_core::settings::{Environment, Settings};

/// Settings accessors — `get()` returns the live `Settings` published
/// at `App::build` time. Used by plugin code that needs to branch on
/// `environment` / `bind_addr` etc. (e.g. umbra-email checking whether
/// to warn about console-backend usage in prod).
pub mod settings {
    pub use umbra_core::settings::get;
}

pub mod db {
    //! Database pool accessors.
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

    pub use umbra_core::db::{
        DbPool, connect, connect_sqlite, pool, pool_dispatched, pool_for, pool_for_dispatched,
    };
}

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
        APP_PLUGIN_NAME, Column, DriftReport, MIGRATIONS_DIR, MigrateError, MigrationEntry,
        MigrationFile, MigrationRef, MigrationStatus, ModelMeta, Operation, Snapshot,
        detect_all_drift, detect_drift, diff, fake_apply, fake_apply_in, fake_initial,
        fake_initial_in, make, make_in, model_alias, models_for_plugin, plugin_order,
        record_applied, registered_models, registered_plugins, render_operation_for, run,
        run_checked, run_checked_in, run_in, show, show_in,
    };
}

pub mod plugin {
    //! The Plugin trait (M7), umbra's extension mechanism.
    //!
    //! Auth, sessions, admin, tasks, REST, and OpenAPI are all
    //! plugins; so is every third-party crate that ships models,
    //! routes, or commands. The trait is also re-exported from the
    //! prelude so `use umbra::prelude::*;` brings it in.

    pub use umbra_core::plugin::{AppContext, Plugin, PluginError};
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
        EmailFormat, Field, Form, InputKind, MaxLength, MinLength, Required, ValidationErrors,
        Validator,
    };

    /// The `#[derive(Form)]` proc-macro. Shares the `Form` name with
    /// the trait — Rust's type and macro namespaces are separate so
    /// both ride in on one import.
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

    pub use minijinja::context;
    pub use umbra_core::templates::{TemplateError, render};
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

    pub use umbra_core::orm::write::WriteError;
    pub use umbra_core::orm::{
        ArrayElement, F, FColExt, FExpr, FieldSpec, ForeignKey, GetError, HydrateRelated, Manager,
        Model, Post, PrimaryKey, Q, QuerySet, SqlType, TsVector, column, write,
    };

    /// The `#[derive(Model)]` proc macro. Shares the `Model` name with the
    /// trait — Rust's type and macro namespaces are separate, so both can
    /// coexist behind one import.
    pub use umbra_macros::Model;

    /// The typed column constants for the demo `Post` model.
    ///
    /// `umbra::orm::post::ID`, `::TITLE`, `::BODY`, `::PUBLISHED_AT`. `Post`
    /// itself lives at `umbra::orm::Post`. The model is a development
    /// fixture: M3's `#[derive(Model)]` retires it; users defining their
    /// own model produce their own column module from the derive.
    pub use umbra_core::orm::post::post;
}
