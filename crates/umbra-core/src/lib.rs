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
pub mod db;
pub mod errors;
pub mod forms;
pub mod inspect;
pub mod migrate;
pub mod orm;
pub mod plugin;
pub mod settings;
pub mod signals;
pub mod slash;
pub mod templates;
pub mod web;

/// Top-level transaction helper. Sugar for `umbra_core::db::transaction`.
///
/// Exposes `umbra_core::transaction(|tx| async { ... })` at the crate root
/// so the facade re-export becomes `umbra::transaction(...)`.
pub use db::{transaction, transaction_pg, transaction_sqlite};
