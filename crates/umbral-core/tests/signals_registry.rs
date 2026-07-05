//! Tests for the bare name-keyed signal registry in `umbral_core::signals`.
//!
//! These tests cover the registry mechanics (subscribe, emit, clear) in
//! isolation, without any model or ORM involvement. The registry is
//! process-wide; the `TEST_LOCK` serialises every test in this file so
//! they run against a fresh registry with no interference from parallel
//! sibling tests.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use tokio::sync::Mutex as TokioMutex;
use umbral_core::signals::{clear_for_tests, emit, subscribe, subscribe_async};

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

#[tokio::test]
async fn sync_subscribe_then_emit_calls_handler() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    subscribe("test_event", move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    });
    let n = emit("test_event", json!({})).await;
    assert_eq!(n, 1);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn emit_passes_payload_to_handler() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let captured = Arc::new(Mutex::new(serde_json::Value::Null));
    let c = captured.clone();
    subscribe("payload_event", move |payload| {
        *c.lock().unwrap() = payload.clone();
    });
    emit("payload_event", json!({ "key": "hello", "n": 7 })).await;
    let v = captured.lock().unwrap();
    assert_eq!(v["key"], "hello");
    assert_eq!(v["n"], 7);
}

#[tokio::test]
async fn multiple_sync_subscribers_all_fire_in_order() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let log = Arc::new(Mutex::new(Vec::<usize>::new()));
    for i in 0..3usize {
        let l = log.clone();
        subscribe("ordered_event", move |_| {
            l.lock().unwrap().push(i);
        });
    }
    let n = emit("ordered_event", json!({})).await;
    assert_eq!(n, 3);
    assert_eq!(*log.lock().unwrap(), vec![0, 1, 2]);
}

#[tokio::test]
async fn emit_unknown_signal_returns_zero() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let n = emit("no_handlers_here", json!({})).await;
    assert_eq!(n, 0);
}

#[tokio::test]
async fn async_handler_is_awaited_to_completion() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let flag = Arc::new(AtomicUsize::new(0));
    let f = flag.clone();
    subscribe_async("async_event", move |_| {
        let f = f.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            f.store(42, Ordering::SeqCst);
        }
    });
    let n = emit("async_event", json!({})).await;
    assert_eq!(n, 1);
    assert_eq!(flag.load(Ordering::SeqCst), 42);
}

#[tokio::test]
async fn sync_and_async_handlers_coexist() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let sync_count = Arc::new(AtomicUsize::new(0));
    let async_count = Arc::new(AtomicUsize::new(0));
    {
        let sc = sync_count.clone();
        subscribe("mixed_event", move |_| {
            sc.fetch_add(1, Ordering::SeqCst);
        });
    }
    {
        let ac = async_count.clone();
        subscribe_async("mixed_event", move |_| {
            let ac = ac.clone();
            async move {
                ac.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
    let n = emit("mixed_event", json!({})).await;
    assert_eq!(n, 2);
    assert_eq!(sync_count.load(Ordering::SeqCst), 1);
    assert_eq!(async_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn clear_for_tests_removes_all_handlers() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    subscribe("clear_test_event", move |_| {
        f.fetch_add(1, Ordering::SeqCst);
    });
    // Confirm it fires before clear.
    emit("clear_test_event", json!({})).await;
    assert_eq!(fired.load(Ordering::SeqCst), 1);
    // Clear and confirm it no longer fires.
    clear_for_tests();
    emit("clear_test_event", json!({})).await;
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "handler should not fire after clear"
    );
}

#[tokio::test]
async fn panicking_sync_handler_does_not_brick_the_registry() {
    // Regression for the mutex-poisoning bug (gaps: BROKEN-3): a sync
    // handler that panics used to poison the registry lock, so every
    // *subsequent* emit — hence every ORM write that fires a signal —
    // would panic forever. The panic must be caught and the registry
    // must keep working: this emit returns normally, and a fresh
    // subscribe + emit on a clean registry still dispatches.
    let _guard = test_lock().lock().await;
    clear_for_tests();

    subscribe("boom", |_| panic!("handler blew up"));
    let survivor = Arc::new(AtomicUsize::new(0));
    let s = survivor.clone();
    subscribe("boom", move |_| {
        s.fetch_add(1, Ordering::SeqCst);
    });

    // Emitting must not propagate the panic, and the non-panicking
    // sibling handler still runs.
    let n = emit("boom", json!({})).await;
    assert_eq!(n, 2, "both handlers are counted even though one panicked");
    assert_eq!(
        survivor.load(Ordering::SeqCst),
        1,
        "the sibling handler runs after the panicking one"
    );

    // The lock is not poisoned: a brand-new subscribe + emit works.
    clear_for_tests();
    let after = Arc::new(AtomicUsize::new(0));
    let a = after.clone();
    subscribe("after_panic", move |_| {
        a.fetch_add(1, Ordering::SeqCst);
    });
    emit("after_panic", json!({})).await;
    assert_eq!(
        after.load(Ordering::SeqCst),
        1,
        "registry still usable after a handler panicked"
    );
}

/// audit_2 observability #10: a hung async subscriber must NOT stall the ORM
/// write path forever. `emit` bounds each subscriber with a timeout and
/// cancels it on expiry. Under `start_paused` tokio auto-advances to the
/// timeout, so this returns promptly instead of blocking for an hour.
#[tokio::test(start_paused = true)]
async fn slow_async_subscriber_is_timed_out_not_hung() {
    let _guard = test_lock().lock().await;
    clear_for_tests();
    subscribe_async("obs10_slow", move |_| async move {
        // Would hang far past the emit timeout.
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    });
    // If the timeout weren't enforced this would never return.
    let n = emit("obs10_slow", json!({})).await;
    assert_eq!(n, 1, "the timed-out subscriber is still counted");
}
