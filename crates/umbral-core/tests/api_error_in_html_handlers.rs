//! `ApiError` has to work in an HTML handler, or nobody uses it (gaps3 #57).
//!
//! The #29 audit found ~72 hand-written `internal_error` / `err500` call sites for APIs
//! that already shipped, and concluded the bottleneck was discovery. Partly true — but
//! building this turned up two hard reasons `ApiError` did not fit an HTML handler, and
//! a reason not to use it is worth more than any amount of documentation telling you to:
//!
//! 1. `umbral::templates::render(...)` — the single most common fallible line in an HTML
//!    handler — had no `From` impl, so `?` did not compile. `ApiError` literally could
//!    not be that handler's error type.
//! 2. Its JSON body was printed verbatim as the message on the styled error page, so a
//!    browser got `{"code":"database_error","error":"internal server error"}`.
//!
//! Both are fixed, and the hand-rolled alternative they pushed people toward —
//! `(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())` — hands the raw database error
//! to the browser. The ugly page was steering people into an information leak.

use umbral::web::ApiError;

/// `?` on a template error must compile and become an opaque 500. This test is mostly a
/// COMPILE-time assertion: if the `From` impl regresses, this file stops building.
#[test]
fn a_template_error_converts_with_a_bare_question_mark() {
    fn handler() -> Result<String, ApiError> {
        // A template that does not exist — the error type is what matters here.
        let body = umbral::templates::render("no_such_template.html", &())?;
        Ok(body)
    }

    let err = handler().expect_err("missing template must be an error");
    assert!(
        matches!(err, ApiError::Internal(_)),
        "a broken template is a bug in the app, not the request: {err:?}"
    );
}

/// A database error must NOT put the cause on the wire. This is the property the
/// hand-rolled `internal_error` helper gets wrong, in every example that defined one.
#[tokio::test]
async fn a_database_error_is_opaque_to_the_client_but_logged() {
    use umbral::web::IntoResponse;

    let leaky = sqlx::Error::Protocol("no such table: shop_product".to_string());
    let resp = ApiError::from(leaky).into_response();

    assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&bytes);

    assert!(
        !body.contains("shop_product") && !body.contains("no such table"),
        "the database's error text reached the client: {body}\n\
         This is exactly what `(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())` does."
    );
    assert!(body.contains("internal server error"), "{body}");
}

/// `Identity::pk::<T>()` replaces the `.parse::<i64>()` the doc-comment used to teach.
#[test]
fn identity_hands_back_a_typed_primary_key() {
    let id = umbral::auth::Identity {
        user_id: "42".to_string(),
        is_staff: true,
        is_superuser: false,
        extras: Default::default(),
    };
    assert_eq!(id.pk::<i64>().expect("42 is an i64"), 42);
    assert_eq!(id.pk::<String>().expect("always"), "42");

    // A key that cannot be its own type is a misconfiguration, and the error says which
    // value and which type — not just "parse failed".
    let bad = umbral::auth::Identity {
        user_id: "not-a-number".to_string(),
        ..id
    };
    let msg = bad.pk::<i64>().expect_err("must fail").to_string();
    assert!(msg.contains("not-a-number") && msg.contains("i64"), "{msg}");
}
