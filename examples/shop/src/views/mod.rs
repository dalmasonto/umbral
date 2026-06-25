//! HTTP handlers split by concern. Submodules:
//!
//!   - `public`  — storefront pages anyone can hit (home,
//!                 product list, product detail).
//!   - `account` — auth-gated views (dashboard, /me,
//!                 staff_only demo).
//!
//! Every handler is re-exported at the module root so `main.rs`
//! references them as `views::home`, `views::dashboard`, etc.
//! without caring which file owns each one.

pub mod account;
pub mod public;

pub use account::*;
pub use public::*;

use umbral::web::StatusCode;

/// Convert any displayable error into a 500 response. Shared by
/// every handler that bubbles a `?` through `.map_err(...)`.
pub(crate) fn internal_error<E: std::fmt::Display>(err: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}
