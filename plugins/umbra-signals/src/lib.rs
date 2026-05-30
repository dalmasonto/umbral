//! umbra-signals — in-process pub/sub for umbra plugins.
//!
//! Django's signals (`post_save`, `pre_delete`, custom application
//! events) in the Rust shape. Plugins emit named events; other
//! plugins or the application subscribe by name. Strictly
//! in-process v1 — no cross-process broker, no persistence, no
//! replay. Use [`umbra_tasks::enqueue`] for work that needs to
//! survive the process.
//!
//! ## Surface
//!
//! - [`emit(name, payload)`] — fire a named signal. Returns the
//!   number of listeners that received it.
//! - [`subscribe<F>(name, handler)`] — register a handler. The
//!   handler runs on the same task that calls `emit`, sync, in
//!   registration order. For async work the handler spawns a
//!   tokio task itself.
//! - [`subscribe_async<F, Fut>(name, handler)`] — same but the
//!   handler returns a Future; the emitter awaits all subscribers
//!   in series (matches Django's `dispatcher.send` semantics).
//! - [`SignalsPlugin`] — empty Plugin impl, exists so the app
//!   builder can name the dependency for ordering (`auth` ->
//!   `signals` for post-login signal plumbing).
//!
//! ## Payload shape
//!
//! Payloads are `serde_json::Value`. The flexibility cost (no
//! compile-time check that emitter and subscriber agree) is a
//! known tradeoff against the per-event-type generic gymnastics a
//! typed event bus would force. Document the payload shape in the
//! emitter's plugin docs.
//!
//! ## Deferred past v1
//!
//! - Typed events via `enum AppEvent { ... }` with associated type
//!   on the trait. Lands when the first real plugin chain needs
//!   compile-time event-type guarantees.
//! - Cross-process broadcast (Redis / NATS adapter). Lands when a
//!   horizontally-scaled deployment needs it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;
use umbra::prelude::*;

type SyncHandler = Box<dyn Fn(&Value) + Send + Sync + 'static>;
type AsyncHandler = Box<
    dyn Fn(&Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync
        + 'static,
>;

struct Registry {
    sync: HashMap<String, Vec<SyncHandler>>,
    r#async: HashMap<String, Vec<AsyncHandler>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            sync: HashMap::new(),
            r#async: HashMap::new(),
        }
    }
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::new()))
}

/// Register a sync handler for `name`. Multiple handlers per name
/// stack in registration order.
pub fn subscribe<F>(name: &str, handler: F)
where
    F: Fn(&Value) + Send + Sync + 'static,
{
    let mut reg = registry().lock().expect("signals registry poisoned");
    reg.sync
        .entry(name.to_string())
        .or_default()
        .push(Box::new(handler));
}

/// Register an async handler for `name`. The emitter awaits each
/// handler's future in series before returning.
pub fn subscribe_async<F, Fut>(name: &str, handler: F)
where
    F: Fn(&Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut reg = registry().lock().expect("signals registry poisoned");
    let wrapped: AsyncHandler = Box::new(move |payload| {
        let fut = handler(payload);
        Box::pin(fut)
    });
    reg.r#async
        .entry(name.to_string())
        .or_default()
        .push(wrapped);
}

/// Emit a named signal. Runs every sync handler then awaits every
/// async handler. Returns the total subscriber count that received
/// the event.
///
/// The async handlers are awaited serially. Concurrent dispatch
/// lands behind a feature flag once a real workload needs it.
pub async fn emit(name: &str, payload: Value) -> usize {
    // Take the lock once, run sync handlers under it, collect the
    // async futures, drop the lock, then await. Holding the lock
    // across `.await` would block other emitters / subscribes for
    // the duration of every future; the collect-then-drop shape
    // is the standard pattern.
    let (futures, total) = {
        let reg = registry().lock().expect("signals registry poisoned");
        let mut count = 0;
        if let Some(handlers) = reg.sync.get(name) {
            for h in handlers {
                h(&payload);
                count += 1;
            }
        }
        let mut futs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>> =
            Vec::new();
        if let Some(handlers) = reg.r#async.get(name) {
            for h in handlers {
                futs.push(h(&payload));
                count += 1;
            }
        }
        (futs, count)
    };
    for fut in futures {
        fut.await;
    }
    total
}

/// Test-only helper: drop every registered handler. The signals
/// registry is process-wide, so a `#[tokio::test]` that registers
/// handlers can interfere with other tests in the same binary;
/// calling `clear` at the top of each test isolates them.
#[doc(hidden)]
pub fn clear_for_tests() {
    let mut reg = registry().lock().expect("signals registry poisoned");
    reg.sync.clear();
    reg.r#async.clear();
}

/// The plugin. Carries no models, no routes, no system_checks.
/// Exists so other plugins can declare `dependencies = &["signals"]`
/// when they want to be sure the registry is alive before their
/// `on_ready` fires.
#[derive(Debug, Default)]
pub struct SignalsPlugin;

impl Plugin for SignalsPlugin {
    fn name(&self) -> &'static str {
        "signals"
    }
}
