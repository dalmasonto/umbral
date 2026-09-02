//! Runtime bootstrap for `#[umbral::main]` (gaps4 #60).
//!
//! `#[tokio::main]` is normally the way a Rust binary gets an async
//! `main`. Its own expansion (for the default, argument-less case) is
//! exactly:
//!
//! ```ignore
//! fn main() -> RetType {
//!     let body = async { /* your fn body */ };
//!     tokio::runtime::Builder::new_multi_thread()
//!         .enable_all()
//!         .build()
//!         .expect("Failed building the Runtime")
//!         .block_on(body)
//! }
//! ```
//!
//! [`block_on_main`] is that same expansion, lifted into a real
//! function instead of macro-generated inline code. `#[umbral::main]`
//! (in `umbral-macros`) calls it. Doing it this way — rather than
//! having the attribute literally emit `#[tokio::main]` — means the
//! generated code never spells the path `tokio::...`, so a crate using
//! `#[umbral::main]` does not need its own direct `tokio` dependency
//! (with or without tokio's `macros` feature) just to get an async
//! `main`. `umbral-core` already depends on `tokio` with
//! `rt-multi-thread` (see `Cargo.toml`); this function is the one place
//! that dependency is spent on behalf of the calling crate.

/// A boxed-error `Result` alias, short enough to write as a `main` return
/// type: `-> umbral::Result` instead of spelling out
/// `Result<(), Box<dyn std::error::Error + Send + Sync>>` (gaps4 #60).
///
/// `T` defaults to `()`, the overwhelmingly common case for a `main`
/// function, so the bare `umbral::Result` (no turbofish) covers the
/// `#[umbral::main] async fn main() -> umbral::Result { ... }` shape.
/// Reach for `umbral::Result<SomeType>` wherever a non-`main` function
/// wants the same "any error that's `Send + Sync + 'static`" story —
/// setup/bootstrap code, CLI command bodies, anywhere threading a
/// concrete error enum through isn't worth it.
///
/// `Send + Sync` matters because this is the type `#[tokio::main]`-style
/// mains use: the runtime's `block_on` return value has to be safe to
/// hand back across the async boundary, and downstream `?`-composed
/// errors (sqlx, io, etc.) all satisfy `Send + Sync` already.
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Build a multi-thread tokio runtime (`enable_all()`, the same defaults
/// `#[tokio::main]` uses) and block on `fut`, returning its output.
///
/// Not meant to be called directly from application code. Call sites
/// should apply `#[umbral::main]` to their `async fn main`; the macro
/// expands to a call to this function (re-exported `#[doc(hidden)]` as
/// `umbral::__rt::block_on_main`). Exposed `pub` here only so the
/// macro-generated code in a downstream crate can name it.
pub fn block_on_main<F>(fut: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("umbral::main: failed to build the tokio runtime")
        .block_on(fut)
}
