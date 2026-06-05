//! Gap 38 — actor task-local on signal payloads.
//!
//! Every ORM signal payload gains an `"actor"` key whose value is the
//! `serde_json::Value` set by the nearest enclosing `with_actor(...)`
//! scope. When no scope is active the value is `Null`. The scope is a
//! tokio task-local, so concurrent requests on the same async runtime
//! see their own actor without interference.

use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Value, json};
use tokio::sync::Mutex as TokioMutex;
use umbra_core::signals::{clear_for_tests, current_actor, emit, subscribe, with_actor};

/// Process-wide serialiser. The signal registry is global; without this
/// the tests in this binary race their handler registrations against
/// one another.
fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[tokio::test]
async fn current_actor_is_null_when_no_scope_is_active() {
    let _guard = test_lock().lock().await;
    assert_eq!(current_actor(), Value::Null);
}

#[tokio::test]
async fn with_actor_sets_current_actor_for_the_duration() {
    let _guard = test_lock().lock().await;
    let actor = json!({ "id": 7, "username": "alice" });
    let observed = with_actor(actor.clone(), async { current_actor() }).await;
    assert_eq!(observed, actor);
    assert_eq!(
        current_actor(),
        Value::Null,
        "scope must reset after the future completes"
    );
}

#[tokio::test]
async fn nested_with_actor_shadows_then_restores() {
    let _guard = test_lock().lock().await;
    let outer = json!({ "id": 1, "username": "outer" });
    let inner = json!({ "id": 2, "username": "inner" });
    let (observed_inner, observed_outer) = with_actor(outer.clone(), async {
        let i = with_actor(inner.clone(), async { current_actor() }).await;
        (i, current_actor())
    })
    .await;
    assert_eq!(observed_inner, inner);
    assert_eq!(observed_outer, outer);
}

#[tokio::test]
async fn emit_includes_actor_when_scope_is_active() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let captured: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let c = captured.clone();
    subscribe("actor_event", move |payload| {
        *c.lock().unwrap() = payload.clone();
    });

    let actor = json!({ "id": 42 });
    with_actor(actor.clone(), async {
        emit("actor_event", json!({ "thing": "hello" })).await;
    })
    .await;

    let v = captured.lock().unwrap();
    assert_eq!(v["actor"], actor, "actor should be embedded in the payload");
    assert_eq!(v["thing"], "hello", "original payload fields must survive");
}

#[tokio::test]
async fn emit_actor_is_null_outside_with_actor() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let captured: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let c = captured.clone();
    subscribe("no_actor_event", move |payload| {
        *c.lock().unwrap() = payload.clone();
    });

    emit("no_actor_event", json!({ "thing": "hello" })).await;

    let v = captured.lock().unwrap();
    assert_eq!(v["actor"], Value::Null);
    assert_eq!(v["thing"], "hello");
}

#[tokio::test]
async fn emit_preserves_explicit_actor_in_payload() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let captured: Arc<Mutex<Value>> = Arc::new(Mutex::new(Value::Null));
    let c = captured.clone();
    subscribe("explicit_actor_event", move |payload| {
        *c.lock().unwrap() = payload.clone();
    });

    // If the caller already put an `actor` key in the payload, the
    // task-local should not clobber it. The explicit one wins.
    let explicit = json!({ "id": 99, "source": "system-task" });
    with_actor(json!({ "id": 1 }), async {
        emit("explicit_actor_event", json!({ "actor": explicit.clone() })).await;
    })
    .await;

    let v = captured.lock().unwrap();
    assert_eq!(v["actor"], explicit);
}

#[tokio::test]
async fn concurrent_tasks_see_their_own_actor() {
    let _guard = test_lock().lock().await;
    let actor_a = json!({ "id": "A" });
    let actor_b = json!({ "id": "B" });
    let (a, b) = tokio::join!(
        with_actor(actor_a.clone(), async {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            current_actor()
        }),
        with_actor(actor_b.clone(), async {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            current_actor()
        }),
    );
    assert_eq!(a, actor_a);
    assert_eq!(b, actor_b);
}
