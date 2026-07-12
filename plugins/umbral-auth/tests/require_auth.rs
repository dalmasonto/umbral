//! `RequireAuth` — the gate lives in the signature (gaps3 #37).
//!
//! A hand-written `fn require_auth(&identity) -> Result<i64, _>` helper is a gate
//! you have to *remember to call*: a handler that forgets it still compiles, still
//! routes, and still runs — wide open. An extractor is a gate you cannot write the
//! handler without. That difference is the entire point, and it's why this is
//! worth having even though the helper "works".

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use tower::ServiceExt;
use umbral_auth::RequireAuth;

async fn whoami(RequireAuth(uid): RequireAuth) -> String {
    uid.to_string()
}

fn app() -> axum::Router {
    axum::Router::new().route("/whoami", get(whoami))
}

/// An anonymous request never reaches the handler body.
#[tokio::test]
async fn an_anonymous_request_is_rejected_before_the_handler_runs() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/whoami")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("request");
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the extractor must 401 an anonymous caller — the handler body is unreachable",
    );
}
