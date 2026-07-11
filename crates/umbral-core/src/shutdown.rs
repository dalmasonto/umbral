//! Process-wide graceful-shutdown coordination (Kikosi #5 — the zero-downtime
//! drain).
//!
//! `App::serve` already finishes in-flight requests on `SIGTERM` (axum's
//! `with_graceful_shutdown`). The missing half is telling the *load balancer* to
//! stop routing here **before** the process stops accepting connections.
//! Otherwise there is a window — from the signal until the LB's next readiness
//! probe — where new requests arrive at a server that is already refusing
//! connections, and they are dropped.
//!
//! The fix is a drain: on the signal, flip a process-wide flag that readiness
//! probes read (`umbral-health` `/readyz` → 503), keep serving for a short
//! delay so the LB observes the 503 and pulls this instance out of rotation,
//! then let the graceful shutdown proceed. This module is the flag; `App::serve`
//! and `AppBuilder::shutdown_drain` are the wiring.
//!
//! Lives in `umbral-core` (not `umbral-health`) because the shutdown path is in
//! core and the health plugin depends *inward* on the facade to read it — the
//! same direction as every other cross-cutting signal.

use std::sync::atomic::{AtomicBool, Ordering};

static DRAINING: AtomicBool = AtomicBool::new(false);

/// Whether the process has begun a graceful-shutdown drain.
///
/// Readiness probes return not-ready while this holds, so a load balancer stops
/// routing to this instance before it stops accepting connections. False for the
/// entire normal lifetime of the process; flips to true once and never back.
pub fn is_draining() -> bool {
    DRAINING.load(Ordering::SeqCst)
}

/// Mark the process as draining. Called by `App::serve` when a shutdown signal
/// arrives; a consumer driving its own shutdown (a custom `main`, a preStop
/// hook that hits an internal route) may call it directly. Idempotent.
pub fn begin_drain() {
    DRAINING.store(true, Ordering::SeqCst);
}
