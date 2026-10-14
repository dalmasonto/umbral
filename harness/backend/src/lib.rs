//! umbral-bench — the benchmark harness, as a library.
//!
//! Two binaries consume this crate:
//!   - `bench-instrumented` — the DIAGNOSTIC tool. Telemetry, probe plugins,
//!     the framework lab, and `DevAuth`. Answers "where did the time go".
//!   - `bench-prod` — the MEASURED application. Real session auth, real
//!     security, no instrumentation constructed at all. Answers "how fast is
//!     it". No number from the instrumented binary is ever quoted as a
//!     performance result.
//!
//! Query logic is shared in [`domain`]; the binaries differ only in how they
//! resolve the acting user and whether they record telemetry.

// Cycle B: the allocator is the one measured lever an application owns
// outright. A CPU profile at 15,000 VUs attributed >=17% of all four silo
// cores (lower bound) to glibc's allocator internals — this workload allocates
// constantly (JSON payload values, decoded rows, response headers). mimalloc
// is the standard drop-in; keeping it here (not in a bin target) gives both
// binaries the same allocator so diagnostics and measurements agree.
//
// `#[global_allocator]` in the library is deliberate: every binary that
// links this crate gets it, which is exactly the arms' contract.
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub mod api;
pub mod domain;
pub mod lab;
pub mod loadgen;
pub mod models;
pub mod probes;
pub mod prod_handlers;
pub mod seed;
pub mod telemetry;
pub mod views;
