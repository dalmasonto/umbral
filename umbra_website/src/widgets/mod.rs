//! Directory dashboard widgets — grouped by rendering shape, mirroring
//! the shop example's `src/widgets/` layout but bound to the
//! plugin-directory data (plugins + discussion notes) instead of orders.
//!
//! The submodules:
//!
//! - `aggregates` — per-window + group-by helpers (counts between, daily
//!   trails, SUM totals). Foundation for the cards + charts.
//! - `cards` — KPI tiles: Total Plugins / Pending Review / Discussion
//!   Notes / GitHub Stars.
//! - `charts` — donut (source, status) + line (submissions, activity).
//! - `gauges` — radial (audit coverage) + progress (top plugins by stars).
//!
//! Every widget builder is re-exported at the module root, so `main.rs`
//! calls them as `widgets::total_plugins_card()` without knowing which
//! file owns each one.

pub mod aggregates;
pub mod cards;
pub mod charts;
pub mod gauges;

pub use cards::*;
pub use charts::*;
pub use gauges::*;
