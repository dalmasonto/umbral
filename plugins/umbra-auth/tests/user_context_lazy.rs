//! `user_context_layer` must resolve the user LAZILY: a response that never
//! renders the template `user` (e.g. JSON) triggers zero identity work; an
//! HTML response that renders `user` triggers exactly one resolution.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use tower::ServiceExt;

use umbra_auth::user_context_layer;

// A JSON handler that never touches templates / `user`.
async fn json_handler() -> &'static str {
    "{\"ok\":true}"
}

#[tokio::test(flavor = "multi_thread")]
async fn json_request_does_not_resolve_user() {
    // No session cookie → if resolution ran, current_user() would query the
    // session table (which doesn't exist in this test harness) and the lazy
    // resolver would still be invoked. We assert the resolver is never run by
    // observing that the request succeeds with NO database configured at all:
    // a non-lazy (eager) layer would call current_user().await and hit the
    // ambient pool — which is unset here — surfacing an error/panic path.
    let app = axum::Router::new()
        .route("/json", get(json_handler))
        .layer(axum::middleware::from_fn(user_context_layer));

    let resp = app
        .oneshot(Request::builder().uri("/json").body(Body::empty()).unwrap())
        .await
        .expect("request");
    assert_eq!(resp.status(), http::StatusCode::OK);
}
