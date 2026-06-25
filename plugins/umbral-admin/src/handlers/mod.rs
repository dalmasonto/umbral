//! HTTP handler functions, grouped by feature.
//!
//! Each child module owns the handlers for one slice of the admin's
//! surface area; the routes table in `lib.rs` calls them. Submodules
//! are private — handlers are reached only by the router.

pub(crate) mod actions;
pub(crate) mod crud;
pub(crate) mod dashboard;
pub(crate) mod fk_picker;
pub(crate) mod history;
pub(crate) mod inline_edit;
pub(crate) mod list;
pub(crate) mod palette;
pub(crate) mod prefs;
pub(crate) mod sheet;
pub(crate) mod upload;

pub(crate) use actions::descriptors_for;
