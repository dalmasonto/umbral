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

// There is deliberately no `internal_error` helper here any more.
//
// Every handler below returns `Result<_, umbral::web::ApiError>` and uses a bare `?`.
// The old helper did `(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())`, which hands
// the raw error to whoever asked for the page — a missing table or a SQL fragment,
// printed to the browser. `ApiError` logs the real cause server-side and returns an
// opaque 500, which is what an example should be teaching.
