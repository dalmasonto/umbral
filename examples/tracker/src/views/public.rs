//! Public views — pages anyone can hit, no auth required.
//!
//! Handlers return `Result<_, ApiError>` and use a bare `?`. `ApiError`
//! converts from sqlx / template errors, logs the real cause server-side,
//! and returns an opaque 500 — so a SQL fragment never reaches the browser.

use umbral::prelude::*;
use umbral::templates::context;

/// Home page. Renders the welcome template. No models required — the
/// task-tracker models live in the `projects` plugin, and the admin,
/// REST, and GraphQL surfaces are where you exercise them.
pub async fn home() -> Result<Html<String>, ApiError> {
    let body = umbral::templates::render("home.html", &context!())?;
    Ok(Html(body))
}
