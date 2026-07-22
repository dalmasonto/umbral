//! URL conf for the `projects` plugin — the route table.
//! `router()` returns the axum `Router` that
//! `Plugin::routes()` in lib.rs hands back to the framework.
//!
//! Convention: `/<name>/...` for HTML pages, `/api/<name>/...` for JSON.
//! Map each path to a handler in `views.rs` so this file reads as the
//! single index of everything the plugin serves.

use umbral::web::{Router, get};

use crate::views;

/// Build this plugin's route table. Add one `.route(path, method(handler))`
/// line per endpoint.
pub fn router() -> Router {
    Router::new().route("/projects/", get(views::index))
}
