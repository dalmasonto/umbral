//! The web layer. At M0 this is a thin re-export of axum's primitives.
//!
//! Later milestones will add umbra-specific wrappers (named routes for
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
