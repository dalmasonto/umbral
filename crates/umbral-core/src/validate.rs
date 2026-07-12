//! Request-body validation (gaps3 #29 item 4).
//!
//! `#[umbral(trim, lowercase, max_length = N, email, ...)]` has always worked on a
//! `Model` — but a great many request bodies are not models. They are DTOs: a
//! `#[derive(Deserialize)]` struct that gets checked, then turned into something else.
//! Until now those had no story at all, so every handler re-implemented the same four
//! rules by hand.
//!
//! ```ignore
//! #[derive(Deserialize, Validate)]
//! struct CreateGoal {
//!     #[umbral(trim, min_length = 1, max_length = 80)]
//!     scorer: String,
//!     #[umbral(choices = ["home", "away"])]
//!     side: String,
//!     #[umbral(min = 0, max = 120)]
//!     minute: i64,
//! }
//!
//! async fn create_goal(Valid(body): Valid<CreateGoal>) -> impl IntoResponse {
//!     // `body` is normalised AND checked. There is no path into this function
//!     // where it is not.
//! }
//! ```
//!
//! Two properties are doing the work here.
//!
//! **The vocabulary is the one you already know.** These are the *same* attribute
//! names, with the same meanings, as on a `Model` — and `email` / `url` / `slug` call
//! the very same [`validate_text_format`](crate::orm::validators::validate_text_format)
//! the ORM's write path calls. A string a DTO accepts is a string the model accepts.
//! Two validators that "both check emails" is how you end up with a row your own API
//! cannot round-trip.
//!
//! **The gate is in the signature.** `Valid<T>` is an extractor, so a handler that
//! forgot to validate does not compile into existence. A `validate()` helper you must
//! remember to call is a gate you can forget.

use serde::de::DeserializeOwned;

pub use crate::forms::ValidationErrors;

/// Normalise a value in place, then check it.
///
/// Derived with `#[derive(Validate)]`. Implement by hand only when the rules cannot be
/// spelled as attributes.
///
/// `&mut self` is not an accident: `trim` and `lowercase` **rewrite** the value, and a
/// validator that could only say no would leave every caller to do the normalising
/// itself — which is the boilerplate this exists to delete.
pub trait Validate: Sized {
    /// Rewrite this value into its normalised form and return every rule it breaks.
    ///
    /// Errors accumulate: a body with three bad fields reports all three, because
    /// making a user fix one mistake per round-trip is its own kind of bug.
    fn validate(&mut self) -> Result<(), ValidationErrors>;
}

/// Extractor: deserialize a JSON body, normalise it, validate it — or reject with the
/// same structured 400 the REST plugin emits for a model write.
///
/// ```ignore
/// async fn handler(Valid(body): Valid<CreateGoal>) -> impl IntoResponse { ... }
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct Valid<T>(pub T);

impl<T> std::ops::Deref for Valid<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Why a [`Valid<T>`] extraction failed.
#[derive(Debug)]
pub enum ValidRejection {
    /// The body was not valid JSON, or did not fit the struct.
    Malformed(String),
    /// The body parsed, but broke one or more rules.
    Invalid(ValidationErrors),
}

impl axum::response::IntoResponse for ValidRejection {
    fn into_response(self) -> axum::response::Response {
        use axum::Json;
        use http::StatusCode;

        match self {
            // A body that will not parse has no field-level shape to report — there is
            // no "which field" when the JSON itself is broken.
            ValidRejection::Malformed(msg) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "code": "malformed_body", "error": msg })),
            )
                .into_response(),

            // The shape below is byte-for-byte the one `umbral-rest` returns when a
            // model write fails validation: field errors flattened to the top level,
            // `non_field_errors` alongside, a stable `code`. A client should not have
            // to care whether the 400 it just got came from a viewset or a hand-written
            // handler — one API, one error format.
            ValidRejection::Invalid(errs) => {
                let mut body = serde_json::Map::new();
                body.insert("code".into(), serde_json::json!("validation_error"));
                for (field, messages) in errs.fields {
                    body.insert(field, serde_json::json!(messages));
                }
                if !errs.non_field.is_empty() {
                    body.insert("non_field_errors".into(), serde_json::json!(errs.non_field));
                }
                (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::Value::Object(body)),
                )
                    .into_response()
            }
        }
    }
}

impl<T, S> axum::extract::FromRequest<S> for Valid<T>
where
    T: DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = ValidRejection;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let axum::Json(mut value) = axum::Json::<T>::from_request(req, state)
            .await
            .map_err(|e| ValidRejection::Malformed(e.body_text()))?;
        value.validate().map_err(ValidRejection::Invalid)?;
        Ok(Valid(value))
    }
}

// ---------------------------------------------------------------------------
// Rule helpers. The derive expands to calls into these rather than inlining the
// logic, so a rule's behaviour lives in ONE place and the generated code stays
// small enough to read in a macro-expansion dump.
// ---------------------------------------------------------------------------

/// `#[umbral(min_length = N)]` — counts CHARACTERS, not bytes. `"é"` is one character
/// and two bytes; a byte-length limit would reject a name a user can legitimately have.
pub fn check_min_length(errs: &mut ValidationErrors, field: &str, value: &str, n: usize) {
    if value.chars().count() < n {
        errs.add(
            field,
            if n == 1 {
                "This field cannot be blank.".to_string()
            } else {
                format!("Must be at least {n} characters.")
            },
        );
    }
}

/// `#[umbral(max_length = N)]` — rejects rather than truncating. Silently cutting a
/// user's input to length is data loss that looks like success.
pub fn check_max_length(errs: &mut ValidationErrors, field: &str, value: &str, n: usize) {
    let len = value.chars().count();
    if len > n {
        errs.add(
            field,
            format!("Must be at most {n} characters (got {len})."),
        );
    }
}

/// `#[umbral(email)]` / `#[umbral(url)]` / `#[umbral(slug)]` — delegates to the ORM's
/// validator so a DTO and a model agree on what a valid email is.
pub fn check_text_format(errs: &mut ValidationErrors, field: &str, value: &str, format: &str) {
    if let Err(e) = crate::orm::validators::validate_text_format(format, value) {
        errs.add(field, e.to_string());
    }
}

/// `#[umbral(choices = ["home", "away"])]` — an enum-of-strings, without the enum.
pub fn check_choices(errs: &mut ValidationErrors, field: &str, value: &str, allowed: &[&str]) {
    if !allowed.contains(&value) {
        errs.add(field, format!("Must be one of: {}.", allowed.join(", ")));
    }
}

/// `#[umbral(min = N)]` / `#[umbral(max = N)]` on a numeric field.
///
/// Compares as `f64` so one helper covers every integer and float width. The bounds a
/// request body checks (an age, a minute of a match, a page size) are nowhere near the
/// magnitude where f64 loses integer precision.
pub fn check_min(errs: &mut ValidationErrors, field: &str, value: f64, n: f64) {
    if value < n {
        errs.add(field, format!("Must be at least {n}."));
    }
}

/// See [`check_min`].
pub fn check_max(errs: &mut ValidationErrors, field: &str, value: f64, n: f64) {
    if value > n {
        errs.add(field, format!("Must be at most {n}."));
    }
}
