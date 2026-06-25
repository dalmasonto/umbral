//! Integration coverage for umbral-signals.
//!
//! The signals registry is process-wide; cargo runs `#[test]`s in
//! one binary in parallel by default. The `TEST_LOCK` mutex
//! serializes every test in this file so each one runs against a
//! freshly-cleared registry with no interleaved subscribes from
//! sibling tests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use tokio::sync::Mutex as TokioMutex;
use umbral_signals::{clear_for_tests, emit, subscribe, subscribe_async};

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[tokio::test]
async fn sync_subscribe_then_emit_calls_the_handler() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    subscribe("user_logged_in", move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });

    let n = emit("user_logged_in", json!({"user_id": 42})).await;
    assert_eq!(n, 1, "one handler should have received the signal");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn emit_passes_the_payload_to_the_handler() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let c = captured.clone();
    subscribe("any_event", move |payload| {
        *c.lock().unwrap() = payload.clone();
    });

    emit("any_event", json!({"key": "value", "n": 7})).await;
    let v = captured.lock().unwrap();
    assert_eq!(v["key"], "value");
    assert_eq!(v["n"], 7);
}

#[tokio::test]
async fn multiple_subscribers_all_receive_the_signal_in_order() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let log = Arc::new(Mutex::new(Vec::<usize>::new()));
    for i in 0..3 {
        let l = log.clone();
        subscribe("ordered", move |_| {
            l.lock().unwrap().push(i);
        });
    }
    let n = emit("ordered", json!({})).await;
    assert_eq!(n, 3);
    assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
}

#[tokio::test]
async fn emit_on_unknown_signal_returns_zero() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let n = emit("no_subscribers", json!({})).await;
    assert_eq!(n, 0);
}

#[tokio::test]
async fn async_subscribe_runs_the_future_to_completion() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let flag = Arc::new(AtomicUsize::new(0));
    let f = flag.clone();
    subscribe_async("background_work", move |_payload| {
        let f = f.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            f.store(99, Ordering::SeqCst);
        }
    });

    let n = emit("background_work", json!({})).await;
    assert_eq!(n, 1);
    assert_eq!(
        flag.load(Ordering::SeqCst),
        99,
        "emit should have awaited the async handler"
    );
}

#[tokio::test]
async fn sync_and_async_handlers_coexist_on_one_signal() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let sync_counter = Arc::new(AtomicUsize::new(0));
    let async_counter = Arc::new(AtomicUsize::new(0));

    let sc = sync_counter.clone();
    subscribe("mixed", move |_| {
        sc.fetch_add(1, Ordering::SeqCst);
    });
    let ac = async_counter.clone();
    subscribe_async("mixed", move |_| {
        let ac = ac.clone();
        async move {
            ac.fetch_add(1, Ordering::SeqCst);
        }
    });

    let n = emit("mixed", json!({})).await;
    assert_eq!(n, 2);
    assert_eq!(sync_counter.load(Ordering::SeqCst), 1);
    assert_eq!(async_counter.load(Ordering::SeqCst), 1);
}
