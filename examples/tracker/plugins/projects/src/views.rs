//! HTTP handlers for the `projects` plugin.
//!
//! Each handler is an axum handler — return anything that implements
//! `IntoResponse` (`Html<String>`, `Json<T>`, `&'static str`, a
//! `Result<_, (StatusCode, String)>`, …). Read this app's data through
//! the ORM (`models::*::objects()`), never raw SQL.
//!
//! Routes that reach these handlers are declared in `urls.rs`.

/// Sample landing handler. `GET /projects/` hits this; rewire the path in
/// `urls.rs`.
pub async fn index() -> &'static str {
    "Hello from the projects plugin"
}
