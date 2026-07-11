//! umbral internals: ORM, migrations, routing, DB backends, the Plugin trait.
//!
//! Do not depend on this crate directly. Use the `umbral` facade.
//!
//! Status: M0 shipped — Settings, db pool, web re-exports, App builder.

pub mod api_error;
pub mod app;
pub mod auth_contract;
pub mod backend;
pub mod backup;
pub mod check;
pub mod cli;
pub mod cors;
pub mod db;
pub mod errors;
pub mod fixtures;
pub mod forms;
pub(crate) mod hosts;
pub mod inspect;
pub mod middleware;
pub mod migrate;
pub mod orm;
pub mod pagination;
pub mod plugin;
pub mod ratelimit;
pub mod routes;
pub mod settings;
pub mod shutdown;
pub mod signals;
pub mod slash;
pub mod static_files;
pub mod storage;
pub mod templates;
pub mod timezone;
pub mod typegen;
pub mod web;

/// Top-level transaction helper. Sugar for `umbral_core::db::transaction`.
///
/// Exposes `umbral_core::transaction(|tx| async { ... })` at the crate root
/// so the facade re-export becomes `umbral::transaction(...)`.
pub use db::{transaction, transaction_pg, transaction_sqlite};

/// Re-export of `sea_query` for use in macro-generated code.
///
/// The `#[derive(Model)]` macro emits `::umbral::_sea_query::Value` in the
/// `HydrateRelated::write_pending_m2m` body (form-staged M2M junction
/// writes). Routing through this re-export means user crates don't need a
/// direct `sea-query` dep for the generated code to compile.
#[doc(hidden)]
pub use sea_query as _sea_query;
