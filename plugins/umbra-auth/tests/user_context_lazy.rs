//! `user_context_layer` must resolve the user LAZILY: a response that never
//! renders the template `user` (e.g. JSON) triggers zero identity work; an
//! HTML response that renders `user` triggers exactly one resolution.
//!
//! ## Why the original no-cookie test proved nothing
//!
//! The original test sent a request with NO cookie. `current_user()` →
//! `current_user_id_str()` → `current_session()` → `cookie_from_headers()`
//! returns `None` and short-circuits to `Ok(None)` BEFORE any DB query.
//! So BOTH the old eager layer and the new lazy layer perform zero DB work
//! for a cookieless request → the test passed for either implementation.
//!
//! ## How the new test distinguishes
//!
//! The rigorous proof:
//! - Send a request **with a session cookie** but with **no ambient DB pool**.
//! - The lazy layer: installs a `LazyUser` resolver, calls `next.run(req)`,
//!   the JSON handler returns without touching `user`, the resolver is never
//!   invoked, `pool_dispatched()` is never called → **200 OK**.
//! - An eager layer: calls `current_user().await` before `next.run(req)`,
//!   which calls `read_session()` → `Session::objects().filter(...).first()`
//!   → `pool_dispatched()` → **PANIC** ("db pool not initialised").
//!
//! On a bare `axum::Router::new()...oneshot(req).await`, panics propagate
//! through the future and fail the test. This is the observable.
//!
//! The mechanistic proof that the `LazyUser` resolver itself is memoized and
//! zero-invocations on no-`user`-access lives in `umbra-core`'s
//! `tests/lazy_user.rs::resolver_does_not_run_when_user_is_not_rendered`.
//! That test uses an `Arc<AtomicUsize>` counter to prove zero calls with
//! complete certainty. Together the two tests cover both the mechanism and
//! the end-to-end middleware wiring.

use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use tower::ServiceExt;

use umbra_auth::user_context_layer;

// A JSON handler that never touches templates / `user`.
async fn json_handler() -> &'static str {
    "{\"ok\":true}"
}

/// Smoke test: no-cookie request always skips resolution in both eager and
/// lazy implementations, so this test documents behavior without gating
/// the change. Kept here as a baseline; the gating test is below.
#[tokio::test(flavor = "multi_thread")]
async fn no_cookie_json_request_returns_ok() {
    let app = axum::Router::new()
        .route("/json", get(json_handler))
        .layer(axum::middleware::from_fn(user_context_layer));

    let resp = app
        .oneshot(Request::builder().uri("/json").body(Body::empty()).unwrap())
        .await
        .expect("request");
    assert_eq!(resp.status(), http::StatusCode::OK);
}

/// **Gating test** — distinguishes the lazy implementation from an eager one.
///
/// Observable: a request bearing a session cookie that reaches a JSON handler
/// (which never accesses `user`) must complete with 200 OK even though no
/// ambient DB pool is configured.
///
/// Why this gates the change:
/// - LAZY: `user_context_layer` installs the resolver but never calls it
///   because the JSON handler returns without reading any template. The
///   `pool_dispatched()` call inside `read_session()` is never reached.
///   Result: 200 OK → test PASSES.
/// - EAGER (e.g. revert `user_context_layer` to call `current_user().await`
///   before `next.run(req)`): `current_user()` → `current_session()` →
///   `read_session()` → `Session::objects()...first().await` →
///   `pool_dispatched()` → PANIC ("db pool not initialised").
///   A panic in the axum middleware async fn propagates through
///   `oneshot().await`, failing the test. Result: test FAILS → RED.
///
/// Red-on-eager / green-on-lazy verified manually: see task-2-report.md.
#[tokio::test(flavor = "multi_thread")]
async fn cookie_bearing_json_request_does_not_touch_pool() {
    // No App::build(), no pool configured — pool_dispatched() would panic
    // if called. The lazy middleware must NOT call it for this request.
    let app = axum::Router::new()
        .route("/json", get(json_handler))
        .layer(axum::middleware::from_fn(user_context_layer));

    // A plausible session cookie value (raw token format: UUID v4).
    // The eager path would try to hash this, look it up, and call pool_dispatched().
    // The lazy path never reaches that code.
    let fake_token = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/json")
                .header(
                    http::header::COOKIE,
                    format!("umbra_session={fake_token}"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("lazy layer must not panic: pool_dispatched() must not be called");

    assert_eq!(resp.status(), http::StatusCode::OK);
    // Confirm the handler's JSON body came through unmodified.
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(&body[..], b"{\"ok\":true}");
}
