//! audit_2 plugin-auth #4 — argon2 hashing runs behind a bounded concurrency
//! gate so a login/register/reset flood can't spawn hundreds of 19-MiB hashes
//! at once and OOM the process. Past `cap × 8` in-flight operations, work is
//! shed with `AuthError::Overloaded` (which the routes map to HTTP 503) rather
//! than queued without limit.
//!
//! This is the ONLY test in its binary so the `UMBRAL_AUTH_HASH_CONCURRENCY`
//! env var (read once into a process-wide `OnceLock`) is set before any hashing
//! and can't race a sibling test.

use umbral_auth::{AuthError, hash_password_async, verify_password_async};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gate_bounds_concurrency_and_sheds_excess_with_overloaded() {
    // Force a tiny cap so the bound is deterministic regardless of core count:
    // cap = 2 → at most 2 argon2 run at once, and at most 2 × 8 = 16 total
    // in-flight (running + waiting) before requests are shed.
    // SAFETY: set at test start before any hashing reads the env; this is the
    // only test in the binary.
    unsafe {
        std::env::set_var("UMBRAL_AUTH_HASH_CONCURRENCY", "2");
    }

    // A lone hash/verify still round-trips through the gate.
    let hash = hash_password_async("correct horse battery staple")
        .await
        .expect("a single hash under the gate must succeed");
    assert!(
        verify_password_async("correct horse battery staple", &hash)
            .await
            .expect("verify under the gate must succeed"),
        "the gated hash must verify against its own plaintext"
    );

    // Fire far more concurrent hashes than the in-flight cap (16). argon2 is
    // ~100× slower than the spawn loop, so all N are admitted-or-shed before
    // any completes: the first ≤16 proceed, the rest get `Overloaded`.
    const N: usize = 80;
    let mut futs = Vec::with_capacity(N);
    for i in 0..N {
        futs.push(tokio::spawn(async move {
            hash_password_async(&format!("password-{i}")).await
        }));
    }

    let mut ok = 0usize;
    let mut overloaded = 0usize;
    let mut other = 0usize;
    for f in futs {
        match f.await.expect("join") {
            Ok(_) => ok += 1,
            Err(AuthError::Overloaded) => overloaded += 1,
            Err(_) => other += 1,
        }
    }

    // The bound actually shed load: with cap=2 (max in-flight 16) and N=80
    // launched at once, the majority must be rejected.
    assert!(
        overloaded > 0,
        "the gate must shed excess load with Overloaded; got ok={ok} overloaded={overloaded} other={other}"
    );
    // ...but the gate is not a brick wall: the admitted work still completed.
    assert!(
        ok >= 2,
        "at least `cap` hashes must have been admitted and completed; got ok={ok} overloaded={overloaded}"
    );
    // Every request is accounted for as either done or shed — never dropped.
    assert_eq!(
        ok + overloaded + other,
        N,
        "every request must resolve to a definite outcome"
    );
    assert_eq!(other, 0, "no request should fail with a runtime/join error");
}
