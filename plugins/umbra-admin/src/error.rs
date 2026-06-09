//! `AdminError` — the single enum every admin handler returns to.
//!
//! Maps cleanly onto an HTTP status via `IntoResponse`. Carries a string
//! payload for the diagnostic message; the response body never leaks the
//! underlying `sqlx::Error` text (just a generic "database error") so the
//! error message can't divulge schema details.

use umbra::orm::write::WriteError;
use umbra::web::{IntoResponse, Response, StatusCode};

#[derive(Debug)]
pub(crate) enum AdminError {
    NotFound(String),
    Render(String),
    Sqlx(sqlx::Error),
    /// gaps2 #12: structured umbra-validator failure with per-field
    /// + non-field error accessors. Lets `sanitise_form_error`
    /// render the specific message (FK target missing, validator
    /// rule failure, required-field miss) instead of flattening
    /// every write-time failure to "database error".
    Write(WriteError),
    BadInput(String),
}

impl From<sqlx::Error> for AdminError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sqlx(e)
    }
}

impl From<WriteError> for AdminError {
    fn from(e: WriteError) -> Self {
        Self::Write(e)
    }
}

impl From<umbra::orm::DynError> for AdminError {
    fn from(e: umbra::orm::DynError) -> Self {
        // gaps2 #12: route the two-arm DynError to the matching
        // AdminError variant so the structured `WriteError` keeps
        // its per-field map all the way to the form template.
        match e {
            umbra::orm::DynError::Write(w) => Self::Write(w),
            umbra::orm::DynError::Sqlx(s) => Self::Sqlx(s),
        }
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
            AdminError::Write(e) => {
                // Display impl carries the human-readable
                // "umbra::orm::write: <message>" form already; the
                // form-submit path uses `sanitise_form_error` to
                // render per-field instead of this generic
                // response, so this arm is only hit when an admin
                // handler returns a `WriteError` outside the form
                // submit flow (rare).
                tracing::error!(error = %e, "admin: validator error");
                (StatusCode::BAD_REQUEST, e.to_string()).into_response()
            }
            AdminError::BadInput(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    }
}
