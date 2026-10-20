# Security hot path — old-vs-new diagnostic A/B

**Date:** 2026-10-21 · **Branch:** `perf/security-hotpath` · **Commits:** `053f5cd0` (security plugin), `0df99287` (ORM metadata)

## What changed

Both commits are behavior-preserving hot-path work:

- **`053f5cd0`** — the response-header bundle was ten separate tower layers (one per header, each walking the header map) and is now one precomputed `(name, value)` list applied in a single pass with the same `if_not_present` semantics. The `Cookie` header and its extracted CSRF/session values are borrowed instead of cloned into `String`s; only the write-form path clones the two values it must keep across `req.into_parts()`. CSRF-exempt prefixes are precomputed as `(bare, bare + "/")` pairs. The signing secret resolves once into a `OnceLock` (a `None` result is still not cached, preserving the build-order rule). The mint builds one buffer instead of two hex `String`s plus `format!`, and the safe path no longer clones the token to keep a copy for the cookie.
- **`0df99287`** — `model_meta_for_table` / `pk_meta_for_table` clone their cached entry on every call. New crate-internal borrowing variants over the same `OnceLock` caches are used by the `select_related` prefetch and FK type hints. Public APIs keep their signatures (10 dependents; no break).

Security posture is unchanged: the CSRF token compare is still constant-time, and a new test locks in that a handler-set security header still wins over the plugin's default.

## Method

**Diagnostic, not a capacity result.** This uses the v1 in-process instrumented harness (`harness/backend`), which the bench README explicitly labels a diagnostic tool. No number here goes through `record.sh`.

- **Old binary:** built from `origin/main` at `d40d93c8` (the `bench-rig-v2` worktree). **New binary:** built from this branch. Same harness source in both; the only difference is the two framework commits above.
- **Envelope:** 200 VUs, 20 s per scenario, fresh SQLite database per arm (2 000 users / 20 000 posts, seeded by `serve`).
- **Scenarios:** `static` = `GET /api/ping` — `/api` is CSRF-exempt, so this isolates the header/private-cache path; `readers` = the journey mix (`/api/feed`, `/u/{id}`, `/api/users/{id}/profile`, `/`) — HTML routes exercise the full CSRF path.
- **Two alternating passes** (OLD, NEW, OLD, NEW). The `sessions` row is the control: its code did not change.
- The probe bracket measures each plugin's own before+after work, excluding the inner handler/DB.

## Results

| pass | arm | scenario | requests | security avg | sessions avg |
|---|---|---|---|---|---|
| 1 | OLD | static | 252 427 | 18.06 µs | 7.92 µs |
| 1 | NEW | static | 244 033 | 15.37 µs | 9.53 µs |
| 1 | OLD | readers | 2 282 | 36.92 µs | 22.39 µs |
| 1 | NEW | readers | 2 282 | 31.27 µs | 21.67 µs |
| 2 | OLD | static | 247 404 | 18.00 µs | 8.50 µs |
| 2 | NEW | static | 251 841 | 12.70 µs | 7.01 µs |
| 2 | OLD | readers | 2 282 | 33.20 µs | 19.85 µs |
| 2 | NEW | readers | 2 282 | 28.12 µs | 19.32 µs |

Means:

| scenario | OLD | NEW | delta |
|---|---|---|---|
| static | 18.03 µs | 14.04 µs | **−22.1%** |
| readers | 35.06 µs | 29.70 µs | **−15.3%** |
| sessions control (static) | 8.21 µs | 8.27 µs | +0.7% |
| sessions control (readers) | 21.12 µs | 20.50 µs | −2.9% |

Both passes agree in direction for both scenarios; the unchanged sessions row is flat, which is the control.

## Read this honestly

The silo layer ledger puts `security (headers + CSRF)` at **40.6 µs of the 1 088 µs request (3.7%)**. A ~4–5 µs saving here is roughly **0.4% of request time** — real, but well below what a 5-minute silo ABAB can resolve against the host's noise floor. **Do not quote it as a capacity number.** The route to the 25 000 RPS target remains the database bucket (83% of request time), not middleware.

## Caveats

- v1 harness: in-process load generator, `DevAuth`, short runs — diagnostic only.
- The `readers` sample is 2 282 requests per arm (journey think times), so its mean is coarser than `static`'s ~250 000. The `static` result is the tighter one.
- Fresh DBs per arm; seeding is outside the measured window.
- Host thermal drift is uncontrolled in a 20 s run; alternating arms cancels a linear drift but not a step change.
