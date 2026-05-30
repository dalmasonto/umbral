//! umbra internals: ORM, migrations, routing, DB backends, the Plugin trait.
//!
//! Do not depend on this crate directly. Use the `umbra` facade.
//!
//! Status: M0 shipped — Settings, db pool, web re-exports, App builder.

pub mod app;
pub mod backend;
pub mod check;
pub mod db;
pub mod inspect;
pub mod migrate;
pub mod orm;
pub mod plugin;
pub mod settings;
pub mod web;
