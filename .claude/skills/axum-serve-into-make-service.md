---
name: axum-serve-into-make-service
description: Use when no-keep-alive (fresh-connection) HTTP throughput is low and scales DOWN with route count, while keep-alive throughput is fine. axum::serve(listener, router) pays an O(routes) finalization PER CONNECTION; serve via router.into_make_service() instead.
---

# axum::serve clones+finalizes the router per connection (O routes)

## Context

Symptom: an umbral app benchmarked with `ab` (no `-k`) tops out around ~1000 req/s on a trivial handler, while raw axum does tens of thousands. The number is the *same* for a static string, a JSON body, a DB read, and a DB write — so it is NOT the handler, serde, or the database. Turn on keep-alive (`ab -k`) and the same endpoint jumps to 100k+ req/s.

Root cause is in axum's `serve`, not umbral. `axum::serve(listener, router)` drives the `Router` itself as the connection-maker. Its per-connection `call` (axum-0.8.9 `src/routing/mod.rs`, `impl Service<IncomingStream> for Router<()>`) is:

```rust
fn call(&mut self, _req: IncomingStream) -> Self::Future {
    // turns everything into `Route` eagerly rather than per request
    std::future::ready(Ok(self.clone().with_state(())))
}
```

`router.clone()` is O(1) (the inner is `Arc`-backed — 16 ns even at 1000 routes). But `.with_state(())` **finalizes every route eagerly** — O(route-count). axum does this once per connection so per-request routing is cheap; with keep-alive it amortizes over thousands of requests. WITHOUT keep-alive (one connection per request) it runs on every request, so throughput collapses as the route table grows. A full admin + REST surface has hundreds of routes, so the tax is large.

## Approach

Serve via `into_make_service()`, whose per-connection `call` is just `self.svc.clone()` (the O(1) Arc bump) with no `with_state`:

```rust
// crates/umbral-core/src/app.rs — App::serve
axum::serve(listener, self.router.into_make_service()).await
```

Routing then finalizes lazily per request. Measured on the shop (hundreds of routes), fresh-connection throughput on a static handler went **1,042 → 37,353 req/s (~36x)**; DB endpoints rose to the SQLite ceiling (~10k). Keep-alive was unchanged-to-slightly-faster (no regression). No `ConnectInfo` regression — the direct `axum::serve(l, router)` path never provided it either (that needs `into_make_service_with_connect_info`).

## Diagnosing this class of problem

1. Bench the same endpoint with and without `ab -k`. A huge keep-alive gap ⇒ per-connection cost, not per-request.
2. Confirm it scales with route count: serve a bare axum router with 1 vs N routes, bench fresh connections. (Pure axum isolates it from your framework.)
3. **Measure `clone()` with `std::hint::black_box`** — `let _ = r.clone()` in `--release` is dead-code-eliminated and falsely reads ~15 ns. The real per-connection cost here was `with_state`, not `clone`, which black-box'd clone timing made obvious (clone stayed 16 ns; only serving degraded).

## Pitfalls

- `let _ = router.clone()` microbenchmarks are optimized away in release. Always `black_box` both the input and the result.
- Don't blame the layers/plugins first: a plugin-less umbral app matched bare axum (34k fresh), and an 11-plugin app's `router.clone()` was still ~15 ns — the cost was axum's per-connection `with_state`, invisible until you serve a many-route router.
- This only bites no-keep-alive clients (ab without `-k`, naïve scripts, some LBs). Real browsers/HTTP-2/SDKs keep connections alive. It is still worth fixing for "fast by default."

## See also

- `crates/umbral-core/src/app.rs` — `App::serve`.
- axum source: `~/.cargo/registry/src/*/axum-0.8.9/src/routing/mod.rs` (the `Service<IncomingStream>` impl).
