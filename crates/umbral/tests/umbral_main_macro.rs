//! Compile + run coverage for `#[umbral::main]` (gaps4 #60).
//!
//! The macro is applied here to non-`main`-named `async fn`s so the test
//! can call the *generated* synchronous fn directly and assert on its
//! return value — exactly the same shape a real `fn main()` gets called
//! in by the OS, just without needing a separate binary + subprocess to
//! observe it.
//!
//! What this pins:
//!   - `#[umbral::main] async fn f() -> umbral::Result { ... }` expands
//!     to a plain `fn f() -> umbral::Result` that runs the body on a
//!     real tokio runtime (an inner `.await` genuinely executes) and
//!     returns `Ok`.
//!   - The same shape propagates `Err` the way the hand-written
//!     `#[tokio::main] async fn main() -> Result<(), Box<dyn
//!     std::error::Error + Send + Sync>>` always did.
//!   - The no-return-type shape (`async fn main() { ... }`) is accepted
//!     too and its body runs to completion.
//!   - None of this requires a direct `tokio` dependency in the calling
//!     crate: this test file never names `tokio`, and
//!     `crates/umbral/Cargo.toml` has no `tokio` dev-dependency —
//!     `#[umbral::main]` reaches the runtime only through the
//!     `umbral::__rt` re-export, per its doc comment in
//!     `umbral-macros`.

#[umbral::main]
async fn returns_result_ok() -> umbral::Result {
    async fn add(a: i32, b: i32) -> i32 {
        a + b
    }
    // Proves a real async executor is driving this body, not just a
    // synchronous call dressed up as async.
    let sum = add(2, 3).await;
    assert_eq!(sum, 5);
    Ok(())
}

#[umbral::main]
async fn returns_result_err() -> umbral::Result {
    async fn fails() -> Result<(), &'static str> {
        Err("boom")
    }
    fails()
        .await
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    Ok(())
}

#[umbral::main]
async fn returns_unit() {
    async fn touch(flag: &std::sync::atomic::AtomicBool) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    static FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    touch(&FLAG).await;
    assert!(FLAG.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn umbral_main_ok_shape_runs_and_returns_ok() {
    // `returns_result_ok` is now a plain synchronous fn — the macro
    // stripped `async` and wrapped the original body in a tokio
    // runtime. Call it directly, same as the OS would call a real
    // `fn main() -> umbral::Result`.
    let result: umbral::Result = returns_result_ok();
    assert!(result.is_ok());
}

#[test]
fn umbral_main_err_shape_propagates_the_error() {
    let result: umbral::Result = returns_result_err();
    let err = result.expect_err("fails() should have propagated Err through `?`");
    assert_eq!(err.to_string(), "boom");
}

#[test]
fn umbral_main_unit_shape_runs_to_completion() {
    // No return type, no panic: the bare `async fn main() { .. }` shape
    // works too, and the assert inside its body actually ran.
    returns_unit();
}
