//! Donut + line chart widgets for the directory dashboard.
//!
//! - `source_mix_donut`: official / community / experimental /
//!   deprecated split, in canonical order.
//! - `status_mix_donut`: lifecycle status split.
//! - `submissions_chart`: single-series daily new-plugin line with
//!   period chips.
//! - `activity_chart`: new plugins + new discussion notes overlaid
//!   on one timeline.

use umbral_admin::{
    BarPayload, ChartPoint, DonutPayload, HeatmapPayload, LinePayload, Series, Span, Widget,
    WidgetDataFn, WidgetKind, WidgetPayload,
};

use super::aggregates::{
    daily_comments_trail, daily_plugins_trail, plugin_counts, status_maturity_grid,
    weekly_plugins_trail,
};

/// Order a `(value, count)` set by a canonical key list, appending any
/// unrecognised buckets at the end so nothing is silently dropped.
fn ordered_pairs(mut counts: Vec<(String, f64)>, order: &[&str]) -> Vec<(String, f64)> {
    let mut map: std::collections::HashMap<String, f64> = counts.drain(..).collect();
    let mut pairs: Vec<(String, f64)> = Vec::new();
    for k in order {
        if let Some(v) = map.remove(*k) {
            pairs.push(((*k).to_string(), v));
        }
    }
    pairs.extend(map);
    pairs
}

/// Where the directory's plugins come from — official vs community vs
/// the long tail.
pub fn source_mix_donut() -> Widget {
    Widget {
        key: "pd_source_mix_donut",
        title: "Plugins by Source".to_string(),
        kind: WidgetKind::Donut,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let counts = plugin_counts("source").await;
            let pairs = ordered_pairs(
                counts,
                &["official", "community", "experimental", "deprecated"],
            );
            WidgetPayload::Donut(DonutPayload::from_pairs(pairs))
        }),
    }
}

/// Lifecycle status mix across the directory.
pub fn status_mix_donut() -> Widget {
    Widget {
        key: "pd_status_mix_donut",
        title: "Plugins by Status".to_string(),
        kind: WidgetKind::Donut,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let counts = plugin_counts("status").await;
            let pairs = ordered_pairs(
                counts,
                &[
                    "shipped",
                    "usable",
                    "experimental",
                    "in_progress",
                    "planned",
                    "deprecated",
                ],
            );
            WidgetPayload::Donut(DonutPayload::from_pairs(pairs))
        }),
    }
}

/// Build dated x-axis labels for the last `days` days, oldest-first.
fn day_labels(days: i64) -> Vec<String> {
    let now = chrono::Utc::now();
    (0..days)
        .rev()
        .map(|back| {
            let day = now - chrono::Duration::days(back);
            day.format("%b %-d").to_string()
        })
        .collect()
}

/// Daily new-plugin submissions — single-series area line with a
/// `?period=` toggle (7d / 30d / 90d).
pub fn submissions_chart() -> Widget {
    Widget {
        key: "pd_submissions_chart",
        title: "Submissions".to_string(),
        kind: WidgetKind::Line,
        default_span: Span { cols: 8, rows: 3 },
        permission: None,
        default_period: Some("7d"),
        filters: Vec::new(),
        data: WidgetDataFn::with_params(|_user, params| async move {
            let days = params.period_days().unwrap_or(7);
            let trail = daily_plugins_trail(days).await;
            let points: Vec<ChartPoint> = day_labels(days)
                .into_iter()
                .zip(trail)
                .map(|(x, y)| ChartPoint { x, y })
                .collect();
            WidgetPayload::Line(LinePayload {
                series: vec![Series {
                    name: "Plugins".to_string(),
                    points,
                }],
                x_type: "date".to_string(),
            })
        }),
    }
}

/// Directory activity — new plugins + new discussion notes per day on a
/// shared timeline.
pub fn activity_chart() -> Widget {
    Widget {
        key: "pd_activity_chart",
        title: "Activity".to_string(),
        kind: WidgetKind::Line,
        default_span: Span { cols: 8, rows: 3 },
        permission: None,
        default_period: Some("7d"),
        filters: Vec::new(),
        data: WidgetDataFn::with_params(|_user, params| async move {
            let days = params.period_days().unwrap_or(7);
            let (plugins, comments) =
                tokio::join!(daily_plugins_trail(days), daily_comments_trail(days));
            let labels = day_labels(days);
            let mk_series = |name: &str, values: Vec<f64>| Series {
                name: name.to_string(),
                points: labels
                    .iter()
                    .zip(values)
                    .map(|(x, y)| ChartPoint { x: x.clone(), y })
                    .collect(),
            };
            WidgetPayload::Line(LinePayload {
                series: vec![
                    mk_series("New plugins", plugins),
                    mk_series("Discussion notes", comments),
                ],
                x_type: "date".to_string(),
            })
        }),
    }
}

/// Weekly submission volume — new plugins per week over the last 8
/// weeks, as a bar chart (coarser companion to the daily line).
pub fn submissions_bar() -> Widget {
    Widget {
        key: "pd_submissions_bar",
        title: "Submissions by week".to_string(),
        kind: WidgetKind::Bar,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let weeks = 8;
            let trail = weekly_plugins_trail(weeks).await;
            let now = chrono::Utc::now();
            let points: Vec<ChartPoint> = trail
                .into_iter()
                .enumerate()
                .map(|(i, y)| {
                    let back = weeks - 1 - i as i64;
                    let wk = now - chrono::Duration::weeks(back);
                    ChartPoint {
                        x: wk.format("%b %-d").to_string(),
                        y,
                    }
                })
                .collect();
            WidgetPayload::Bar(BarPayload {
                series: vec![Series {
                    name: "Plugins".to_string(),
                    points,
                }],
                x_type: "category".to_string(),
            })
        }),
    }
}

/// Status × maturity heatmap — how the directory's plugins distribute
/// across the lifecycle-status (rows) by maturity (columns) grid. Reads
/// as a single `GROUP BY status, maturity`, zero-filled.
pub fn status_maturity_heatmap() -> Widget {
    Widget {
        key: "pd_status_maturity_heatmap",
        title: "Status × maturity".to_string(),
        kind: WidgetKind::Heatmap,
        default_span: Span { cols: 8, rows: 3 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            // Canonical orders so the grid axes stay stable regardless of
            // which combinations currently have rows.
            let statuses = [
                "shipped",
                "usable",
                "experimental",
                "in_progress",
                "planned",
                "deprecated",
            ];
            let maturities = ["stable", "beta", "alpha", "design"];
            let grid = status_maturity_grid(&statuses, &maturities).await;
            WidgetPayload::Heatmap(HeatmapPayload::from_grid(statuses, maturities, grid))
        }),
    }
}
