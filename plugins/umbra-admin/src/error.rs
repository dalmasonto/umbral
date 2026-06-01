//! `AdminError` — the single enum every admin handler returns to.
//!
//! Maps cleanly onto an HTTP status via `IntoResponse`. Carries a string
//! payload for the diagnostic message; the response body never leaks the
//! underlying `sqlx::Error` text (just a generic "database error") so the
//! error message can't divulge schema details.

use umbra::web::{IntoResponse, Response, StatusCode};

#[derive(Debug)]
pub(crate) enum AdminError {
    NotFound(String),
    Render(String),
    Sqlx(sqlx::Error),
    BadInput(String),
}

impl From<sqlx::Error> for AdminError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            AdminError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
            AdminError::Render(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
            AdminError::Sqlx(e) => {
                tracing::error!(error = %e, "admin: database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
            }
            AdminError::BadInput(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    }
}
