//! Async password helpers: round-trip parity with the sync fns, and the
//! key regression — a CPU-bound argon2 burst offloaded to `spawn_blocking`
//! must NOT starve the async runtime. The starvation test would fail if
//! `hash_password_async` ran argon2 directly on the worker thread.

use std::time::{Duration, Instant};

#[tokio::test]
async fn async_hash_then_verify_round_trips() {
    let hash = umbra_auth::hash_password_async("correct horse battery staple")
        .await
        .expect("hashing succeeds");

    // Right password verifies true.
    assert!(
        umbra_auth::verify_password_async("correct horse battery staple", &hash)
            .await
            .expect("verify of correct password does not error"),
        "the right password verifies"
    );

    // Wrong password verifies false (and is NOT an error).
    assert!(
        !umbra_auth::verify_password_async("wrong password", &hash)
            .await
            .expect("verify of wrong password is Ok(false), not an error"),
        "the wrong password does not verify"
    );
}

#[tokio::test]
async fn async_helpers_match_the_sync_helpers() {
    // A hash made by the async helper verifies under the sync fn, and
    // vice-versa: both paths share the same argon2 parameters.
    let async_hash = umbra_auth::hash_password_async("shared-secret")
        .await
        .unwrap();
    assert!(umbra_auth::verify_password("shared-secret", &async_hash).unwrap());

    let sync_hash = umbra_auth::hash_password("shared-secret").unwrap();
    assert!(
        umbra_auth::verify_password_async("shared-secret", &sync_hash)
            .await
            .unwrap()
    );
}

/// The point of Fix 1: with a SINGLE worker thread, a burst of concurrent
/// argon2 hashes (each ~100ms of CPU) must run on the blocking pool, leaving
/// the lone async worker free to drive an unrelated tiny task promptly. If
/// hashing ran on the worker thread, the worker would be pinned for the
/// duration of the burst and the assert below would fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn argon2_burst_does_not_starve_the_single_worker() {
    // Several concurrent hashes, each landing on a separate spawn_blocking
    // thread (tokio's blocking pool is large by default).
    let hashes = (0..8)
        .map(|i| {
            tokio::spawn(async move {
                umbra_auth::hash_password_async(&format!("pw-{i}"))
                    .await
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();

    // The worker is free, so this tiny task completes promptly. yield_now
    // hands control back to the scheduler; the sleep is a real timer wait.
    let t0 = Instant::now();
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert!(
        t0.elapsed() < Duration::from_millis(300),
        "the single worker stayed responsive during the hash burst (elapsed {:?}); \
         if this failed, argon2 ran on the worker thread, not spawn_blocking",
        t0.elapsed()
    );

    // All hashes complete and are valid.
    for h in hashes {
        let hash = h.await.unwrap();
        assert!(hash.starts_with("$argon2"), "produced a PHC argon2 hash");
    }
}
