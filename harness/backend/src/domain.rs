//! Shared query + serialization logic.
//!
//! Everything here is pure application behaviour: no tracing, no auth
//! resolution, no request extractors. The binaries' handlers own those, which
//! is what lets `bench-prod` measure the same queries `bench-instrumented`
//! decomposes.

// Populated in Task 3.
