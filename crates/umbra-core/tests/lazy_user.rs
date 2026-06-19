//! The lazy current-user channel: the resolver closure runs only when a
//! template actually reads `user`, and at most once per request scope.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use umbra_core::templates::{LazyUser, with_current_user_lazy};

// Build a minijinja Value standing in for a serialized user.
fn user_value(is_staff: bool) -> minijinja::Value {
    let mut m = serde_json::Map::new();
    m.insert("is_authenticated".into(), serde_json::Value::Bool(true));
    m.insert("is_staff".into(), serde_json::Value::Bool(is_staff));
    minijinja::Value::from_serialize(serde_json::Value::Object(m))
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_does_not_run_when_user_is_not_rendered() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let lazy = LazyUser::new(move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            user_value(true)
        }
    });

    // Inside the scope, render a template that NEVER references `user`.
    let out = with_current_user_lazy(lazy, async {
        umbra_core::templates::render_str("hello {{ name }}", &serde_json::json!({"name": "ada"}))
    })
    .await
    .expect("render");

    assert_eq!(out, "hello ada");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "resolver must NOT run when user unused");
}

#[tokio::test(flavor = "multi_thread")]
async fn resolver_runs_once_across_two_renders_that_read_user() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let lazy = LazyUser::new(move || {
        let c = c.clone();
        async move {
            c.fetch_add(1, Ordering::SeqCst);
            user_value(true)
        }
    });

    let out = with_current_user_lazy(lazy, async {
        let a = umbra_core::templates::render_str("{{ user.is_staff }}", &serde_json::json!({})).unwrap();
        let b = umbra_core::templates::render_str("{{ user.is_staff }}", &serde_json::json!({})).unwrap();
        format!("{a}-{b}")
    })
    .await;

    assert_eq!(out, "true-true");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "resolver memoized: runs exactly once");
}
