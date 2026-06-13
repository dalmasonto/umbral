//! umbra internals: ORM, migrations, routing, DB backends, the Plugin trait.
//!
//! Do not depend on this crate directly. Use the `umbra` facade.
//!
//! Status: M0 shipped — Settings, db pool, web re-exports, App builder.

pub mod app;
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
pub mod plugin;
pub mod routes;
pub mod settings;
pub mod signals;
pub mod slash;
pub mod static_files;
pub mod storage;
pub mod templates;
pub mod timezone;
pub mod web;

/// Top-level transaction helper. Sugar for `umbra_core::db::transaction`.
///
/// Exposes `umbra_core::transaction(|tx| async { ... })` at the crate root
/// so the facade re-export becomes `umbra::transaction(...)`.
pub use db::{transaction, transaction_pg, transaction_sqlite};

/// Re-export of `sea_query` for use in macro-generated code.
///
/// The `#[derive(Model)]` macro emits `::umbra::_sea_query::Value` in the
/// `HydrateRelated::write_pending_m2m` body (form-staged M2M junction
/// writes). Routing through this re-export means user crates don't need a
/// direct `sea-query` dep for the generated code to compile.
#[doc(hidden)]
pub use sea_query as _sea_query;
