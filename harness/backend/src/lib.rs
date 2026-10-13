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
