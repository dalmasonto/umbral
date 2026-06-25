//! Regression test: a panicking async signal subscriber must be isolated the
//! same way a panicking sync subscriber is. Before the fix, the panic would
//! unwind through `emit()` into the ORM write that fired the signal and kill
//! the entire request. After the fix:
//!   (a) `emit()` returns normally — the call does NOT propagate the panic.
//!   (b) subsequent async subscribers in the same dispatch still fire
//!       (isolation does not abort the whole fan-out).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use serde_json::json;
use tokio::sync::Mutex as TokioMutex;
use umbral_core::signals::{clear_for_tests, emit, subscribe_async};

fn test_lock() -> &'static TokioMutex<()> {
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| TokioMutex::new(()))
}

/// A panicking async subscriber must not propagate the panic through `emit`,
/// and a second async subscriber registered after the panicking one must still
/// fire (isolation does not abort the whole fan-out).
#[tokio::test]
async fn panicking_async_handler_does_not_propagate_and_sibling_still_fires() {
    let _guard = test_lock().lock().await;
    clear_for_tests();

    // Subscriber 1: panics unconditionally.
    subscribe_async("async_boom", |_| async move {
        panic!("async handler blew up");
    });

    // Subscriber 2: records that it ran.
    let survivor = Arc::new(AtomicUsize::new(0));
    let s = survivor.clone();
    subscribe_async("async_boom", move |_| {
        let s = s.clone();
        async move {
            s.fetch_add(1, Ordering::SeqCst);
        }
    });

    // (a) emit must NOT panic/propagate — if it does, the test itself panics.
    let n = emit("async_boom", json!({})).await;
    assert_eq!(
        n, 2,
        "both subscribers are counted even though one panicked"
    );

    // (b) the non-panicking sibling subscriber still ran.
    assert_eq!(
        survivor.load(Ordering::SeqCst),
        1,
        "the sibling async subscriber must still fire after a panicking one"
    );
}

/// The registry must remain usable after an async subscriber panics — the
/// mutex must not be poisoned and a subsequent subscribe+emit must dispatch
/// normally.
#[tokio::test]
async fn registry_usable_after_async_subscriber_panics() {
    let _guard = test_lock().lock().await;
    clear_for_tests();

    subscribe_async("async_boom2", |_| async move {
        panic!("boom");
    });
    // Absorb the panic — this is the assertion that emit doesn't propagate.
    emit("async_boom2", json!({})).await;

    // Now register a fresh handler on a new signal and confirm it fires.
    clear_for_tests();
    let after = Arc::new(AtomicUsize::new(0));
    let a = after.clone();
    subscribe_async("after_async_panic", move |_| {
        let a = a.clone();
        async move {
            a.fetch_add(1, Ordering::SeqCst);
        }
    });
    emit("after_async_panic", json!({})).await;
    assert_eq!(
        after.load(Ordering::SeqCst),
        1,
        "registry still usable after an async handler panicked"
    );
}
