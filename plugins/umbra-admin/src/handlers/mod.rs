//! HTTP handler functions, grouped by feature.
//!
//! Each child module owns the handlers for one slice of the admin's
//! surface area; the routes table in `lib.rs` calls them. Submodules
//! are private — handlers are reached only by the router.

pub(crate) mod dashboard;
pub(crate) mod history;
pub(crate) mod palette;
pub(crate) mod prefs;
