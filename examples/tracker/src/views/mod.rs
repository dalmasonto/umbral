//! HTTP handlers, split by concern — the re-export / discoverability
//! layer. Open this file and you see the whole web surface in a few
//! lines: one submodule per resource grouping.
//!
//! Submodules:
//!   - `public` — pages anyone can hit (home, JSON listings).
//!
//! Add `pub mod account;` here when auth-gated views land (dashboard,
//! /me, staff-only pages), then re-export it below so `main.rs` keeps
//! referencing handlers as `views::public::home` without caring which
//! file owns each one. This is a recommended convention, not a rule —
//! the router reads handlers directly, so you're free to restructure.

pub mod public;

// No `internal_error` helper, on purpose.
//
// Handlers return `Result<_, umbral::web::ApiError>` and use a bare `?`. ApiError
// converts from sqlx / WriteError / TemplateError, logs the real cause server-side, and
// returns an opaque 500 — so a missing table or a SQL fragment never reaches the browser.
// The `(StatusCode, String)` + `err.to_string()` pattern does the opposite.
