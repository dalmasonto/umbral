//! The web layer. At M0 this is a thin re-export of axum's primitives.
//!
//! Later milestones will add umbral-specific wrappers (named routes for
//! `reverse()`, middleware registration through the Plugin contract, etc.)
//! while keeping the underlying axum API accessible.

pub mod multipart;
pub mod streaming;

pub use axum::Router;
pub use axum::extract::{Form, Json, Path, Query, Request};
pub use axum::http::{HeaderMap, StatusCode, header};
pub use axum::response::{Html, IntoResponse, Json as JsonResponse, Redirect, Response};
pub use axum::routing::{delete, get, head, options, patch, post, put};
pub use streaming::StreamingResponse;

use std::sync::OnceLock;

/// Process-wide API base path, published by the REST plugin during build
/// (before router assembly) so other plugins — notably umbral-auth — can
/// mount their JSON routes under the same prefix without a Cargo dependency
/// on umbral-rest. Defaults to "/api" when no REST plugin set it.
static API_BASE: OnceLock<String> = OnceLock::new();

/// Read the configured API base path. Returns "/api" until [`set_api_base`]
/// runs. Trailing slashes are not normalized here — callers append "/auth"
/// etc. directly, and the REST plugin publishes its own normalized base.
pub fn api_base() -> String {
    API_BASE
        .get()
        .cloned()
        .unwrap_or_else(|| "/api".to_string())
}

/// Publish the API base path. First call wins (mirrors the REST plugin's
/// own `CONFIG` OnceLock); subsequent calls are ignored. The REST plugin
/// calls this in an early build phase.
pub fn set_api_base(base: impl Into<String>) {
    let _ = API_BASE.set(base.into());
}

#[cfg(test)]
mod api_base_tests {
    use super::*;
    #[test]
    fn api_base_defaults_to_api_then_takes_first_set() {
        // Default before any set.
        assert_eq!(api_base(), "/api");
        set_api_base("/v2");
        assert_eq!(api_base(), "/v2");
        // First-set-wins: a later set is ignored.
        set_api_base("/v3");
        assert_eq!(api_base(), "/v2");
    }
}
