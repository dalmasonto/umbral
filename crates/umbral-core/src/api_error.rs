//! `ApiError` — a handler-facing error any plain axum handler can return.
//!
//! Batteries-included apps write non-CRUD handlers that touch the ORM. Without
//! this, every one re-declares `fn err500<E: Display>(e) -> (StatusCode, String)`
//! and sprinkles `.map_err(err500)?` on every terminal (the single highest-volume
//! boilerplate observed in a live consumer). `ApiError` implements
//! `From<WriteError>` / `From<sqlx::Error>` / `From<DynError>` and `IntoResponse`,
//! so a handler returns `Result<T, ApiError>` and uses a bare `?`:
//!
//! ```ignore
//! use umbral::web::{ApiError, Json};
//!
//! async fn get_post(Path(id): Path<i64>) -> Result<Json<Post>, ApiError> {
//!     let post = Post::objects().filter(post::ID.eq(id)).first().await?   // sqlx::Error -> 500
//!         .ok_or_else(|| ApiError::not_found("no such post"))?;           // -> 404
//!     Ok(Json(post))
//! }
//! ```
//!
//! Safe by default (WEB-5): a database/internal error logs the real cause
//! server-side and hands the client an opaque 500 — table names, SQL fragments
//! and constraint internals never reach the wire. A `WriteError` that is a
//! *validation* failure (required field, FK-not-found, format rule, …) becomes a
//! 400 carrying the structured per-field error map.

use crate::orm::DynError;
use crate::orm::write::WriteError;
use crate::web::{IntoResponse, Json, Response, StatusCode};

/// A handler-facing error. Build explicit ones with the constructors, or let `?`
/// convert an ORM error (`sqlx::Error` / `WriteError` / `DynError`).
#[derive(Debug)]
pub enum ApiError {
    /// `404` — [`ApiError::not_found`].
    NotFound(String),
    /// `400` with a single message — [`ApiError::bad_request`].
    BadRequest(String),
    /// `400` carrying a structured validation failure (per-field + non-field).
    Validation(WriteError),
    /// `500` — a database error. Logged server-side; the client sees an opaque
    /// "internal server error" (WEB-5: never leak DB internals).
    Database(sqlx::Error),
    /// `500` — any other internal error — [`ApiError::internal`]. Logged; opaque.
    Internal(String),
}

impl ApiError {
    /// A `404 Not Found` with a client-visible message.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }
    /// A `400 Bad Request` with a client-visible message.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }
    /// A `500` whose message is logged server-side but never sent to the client.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e)
    }
}

/// gaps3 #57 — so `umbral::templates::render(...)?` works with a bare `?`.
///
/// Rendering a template is the single most common fallible line in an HTML handler, and
/// without this impl `ApiError` could not be that handler's error type at all. Which is
/// how every example ended up hand-rolling `fn internal_error(e) -> (StatusCode, String)`
/// — and that helper hands `e.to_string()` straight to the browser, so a missing table
/// or a bad column name is printed to whoever asked for the page.
///
/// A broken template is a bug in the app, never in the request: it becomes an opaque 500
/// with the real cause logged server-side, the same posture as a database error.
impl From<crate::templates::TemplateError> for ApiError {
    fn from(e: crate::templates::TemplateError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<WriteError> for ApiError {
    fn from(e: WriteError) -> Self {
        // A validation failure (required field, FK-not-found, format rule, …) is
        // a 400 the client can act on; a true infra/serialization failure is a
        // 500 they can't.
        if e.is_validation() {
            return Self::Validation(e);
        }
        match e {
            WriteError::Sqlx(s) => Self::Database(s),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<DynError> for ApiError {
    fn from(e: DynError) -> Self {
        match e {
            DynError::Write(w) => Self::from(w),
            DynError::Sqlx(s) => Self::from(s),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::NotFound(m) | ApiError::BadRequest(m) | ApiError::Internal(m) => {
                write!(f, "{m}")
            }
            ApiError::Validation(e) => write!(f, "{e}"),
            ApiError::Database(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// A single-message JSON error body: `{"error": "...", "code": "..."}`.
fn json_error(status: StatusCode, code: &str, error: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": error, "code": code })),
    )
        .into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::NotFound(msg) => json_error(StatusCode::NOT_FOUND, "not_found", &msg),
            ApiError::BadRequest(msg) => json_error(StatusCode::BAD_REQUEST, "bad_request", &msg),
            ApiError::Validation(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "code": e.code(),
                    "field_errors": e.field_errors(),
                    "non_field_errors": e.non_field_errors(),
                })),
            )
                .into_response(),
            ApiError::Database(e) => {
                tracing::error!(error = %e, "ApiError: database error");
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database_error",
                    "internal server error",
                )
            }
            ApiError::Internal(msg) => {
                tracing::error!(detail = %msg, "ApiError: internal error");
                json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error",
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::{IntoResponse, StatusCode};

    #[test]
    fn write_error_validation_becomes_a_400_with_the_field() {
        let e = WriteError::RequiredFieldMissing {
            field: "email".into(),
        };
        let api = ApiError::from(e);
        assert_eq!(api.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_bare_sqlx_error_becomes_an_opaque_500() {
        let api = ApiError::from(sqlx::Error::RowNotFound);
        assert_eq!(
            api.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn dynerror_routes_write_to_400_and_sqlx_to_500() {
        let v = ApiError::from(DynError::Write(WriteError::RequiredFieldMissing {
            field: "x".into(),
        }));
        assert_eq!(v.into_response().status(), StatusCode::BAD_REQUEST);
        let s = ApiError::from(DynError::Sqlx(sqlx::Error::RowNotFound));
        assert_eq!(
            s.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn explicit_constructors_carry_their_status() {
        assert_eq!(
            ApiError::not_found("nope").into_response().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            ApiError::bad_request("bad").into_response().status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            ApiError::internal("boom").into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
