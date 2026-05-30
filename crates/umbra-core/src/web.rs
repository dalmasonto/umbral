//! The web layer. At M0 this is a thin re-export of axum's primitives.
//!
//! Later milestones will add umbra-specific wrappers (named routes for
//! `reverse()`, middleware registration through the Plugin contract, etc.)
//! while keeping the underlying axum API accessible.

pub use axum::Router;
pub use axum::extract::{Form, Json, Path, Query};
pub use axum::http::StatusCode;
pub use axum::response::{IntoResponse, Json as JsonResponse};
pub use axum::routing::{delete, get, head, options, patch, post, put};
