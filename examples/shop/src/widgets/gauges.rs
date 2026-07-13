//! Demo widgets for the newer dashboard kinds — radial gauge,
//! heatmap, and progress bar-list — all driven by real order data so
//! they double as a worked example of each `WidgetPayload`.
//!
//! - `fulfillment_radial`: a single-ring gauge — the share of orders
//!   that have reached a fulfilled/shipped/delivered state.
//! - `sales_heatmap`: the last 28 daily-sales values reshaped into a
//!   4-week × 7-day calendar grid.
//! - `order_status_progress`: order-status counts as a ranked bar list
//!   (the same data the donut shows, in the "top N" shape).

use ecommerce::models::Order;
use umbral::orm::Aggregate;
use umbral_admin::{
    HeatmapPayload, ProgressPayload, RadialPayload, Span, Widget, WidgetDataFn, WidgetKind,
    WidgetPayload,
};

use super::aggregates::daily_sales_trail;

/// `GROUP BY status` once, returning `(status, count)` pairs — shared
/// by the radial + progress widgets so neither pulls every order into
/// memory.
async fn status_counts() -> Vec<(String, f64)> {
    let rows = Order::objects()
        .only(&["id", "status"])
        .annotate(&["status"], &[("count", Aggregate::count())])
        .await
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            match (
                row.get("status").and_then(|v| v.as_str()),
                row.get("count").and_then(|v| v.as_f64()),
            ) {
                (Some(s), Some(n)) => Some((s.to_string(), n)),
                _ => None,
            }
        })
        .collect()
}

/// Radial gauge — the share of orders that reached a
/// fulfilled/shipped/delivered state. One ring, percent in the centre.
pub fn shop_fulfillment_radial() -> Widget {
    Widget {
        key: "shop_fulfillment_radial",
        title: "Fulfillment rate".to_string(),
        kind: WidgetKind::Radial,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let counts = status_counts().await;
            let total: f64 = counts.iter().map(|(_, n)| *n).sum();
            let done: f64 = counts
                .iter()
                .filter(|(s, _)| matches!(s.as_str(), "fulfilled" | "shipped" | "delivered"))
                .map(|(_, n)| *n)
                .sum();
            let pct = if total > 0.0 {
                done / total * 100.0
            } else {
                0.0
            };
            WidgetPayload::Radial(RadialPayload::single("Fulfilled", pct))
        }),
    }
}

/// Heatmap — the last 28 days of sales reshaped into a 4-week × 7-day
/// calendar grid (week 1 oldest). `from_grid` pads/truncates, so a
/// short or long trail still renders a rectangular grid.
pub fn shop_sales_heatmap() -> Widget {
    Widget {
        key: "shop_sales_heatmap",
        title: "Daily sales — last 4 weeks".to_string(),
        kind: WidgetKind::Heatmap,
        default_span: Span { cols: 6, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let trail = daily_sales_trail(28).await;
            let weeks = ["Week 1", "Week 2", "Week 3", "Week 4"];
            let cols = ["D1", "D2", "D3", "D4", "D5", "D6", "D7"];
            let grid: Vec<Vec<f64>> = (0..weeks.len())
                .map(|w| {
                    (0..cols.len())
                        .map(|d| trail.get(w * 7 + d).copied().unwrap_or(0.0))
                        .collect()
                })
                .collect();
            WidgetPayload::Heatmap(HeatmapPayload::from_grid(weeks, cols, grid))
        }),
    }
}

/// Progress bar-list — order-status counts ranked descending, each bar
/// sized against the largest bucket.
pub fn shop_order_status_progress() -> Widget {
    Widget {
        key: "shop_order_status_progress",
        title: "Orders by status".to_string(),
        kind: WidgetKind::Progress,
        default_span: Span { cols: 3, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let mut pairs = status_counts().await;
            pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            WidgetPayload::Progress(ProgressPayload::from_pairs(pairs))
        }),
    }
}
