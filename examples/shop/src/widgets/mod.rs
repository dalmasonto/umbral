//! Shop dashboard widgets — grouped by kind so each file stays
//! small and focused on one rendering shape.
//!
//! The submodules:
//!   - `aggregates`  — per-window helpers (sales/orders/customers
//!                     between, daily trail vectors). Foundation
//!                     for the cards + charts.
//!   - `cards`       — KPI tiles: Total Sales / Orders /
//!                     Customers / AOV.
//!   - `charts`      — line + donut: Daily Sales, Activity
//!                     (multi-series), Order Status.
//!   - `tables`      — Recent Orders.
//!   - `content`     — Posts + Subscribers (content plugin).
//!
//! Every widget builder is re-exported at the module root, so
//! `main.rs` calls them as `widgets::shop_total_sales_widget()`
//! without having to know which file owns each one.

pub mod aggregates;
pub mod cards;
pub mod charts;
pub mod content;
pub mod tables;

pub use cards::*;
pub use charts::*;
pub use content::*;
pub use tables::*;
