//! Per-window + group-by aggregate helpers for the directory
//! dashboard — the plugin-directory analogue of the shop's
//! `aggregates.rs`.
//!
//! Every helper fans one ORM call and unwraps to a zero/empty
//! default, so a widget run against a fresh DB shows a sensible
//! empty state instead of bubbling an error to the dashboard.
//! Counts go through `.objects()`, which auto-injects
//! `WHERE deleted_at IS NULL` for these soft-delete models — no
//! manual deleted-row filtering needed.

use chrono::{DateTime, Duration, Utc};
use plugin_directory::models::{self as pd, plugin, plugin_comment};
use umbra::orm::Aggregate;

/// Count plugins created in a `[start, end)` window.
pub async fn plugins_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    pd::Plugin::objects()
        .filter(plugin::CREATED_AT.gte(start))
        .filter(plugin::CREATED_AT.lt(end))
        .count()
        .await
        .unwrap_or(0)
}

/// Count visible discussion notes created in a `[start, end)` window.
pub async fn comments_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    pd::PluginComment::objects()
        .filter(plugin_comment::MODERATION.eq("visible"))
        .filter(plugin_comment::CREATED_AT.gte(start))
        .filter(plugin_comment::CREATED_AT.lt(end))
        .count()
        .await
        .unwrap_or(0)
}

/// Daily new-plugin counts for the last `days` days, oldest-first.
/// Feeds card sparklines + the submissions line chart.
pub async fn daily_plugins_trail(days: i64) -> Vec<f64> {
    let now = Utc::now();
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let end = now - Duration::days(back);
        let start = end - Duration::days(1);
        out.push(plugins_between(start, end).await as f64);
    }
    out
}

/// Daily discussion-note counts for the last `days` days, oldest-first.
pub async fn daily_comments_trail(days: i64) -> Vec<f64> {
    let now = Utc::now();
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let end = now - Duration::days(back);
        let start = end - Duration::days(1);
        out.push(comments_between(start, end).await as f64);
    }
    out
}

/// `GROUP BY <column>` → `(value, count)` pairs in one query, instead
/// of pulling every row into memory to tally (gaps2 #56). Shared by the
/// source/status donuts + the audit radial. `column` must be a Plugin
/// column name (`"source"`, `"status"`, `"audit_status"`).
pub async fn plugin_counts(column: &'static str) -> Vec<(String, f64)> {
    let rows = pd::Plugin::objects()
        .only(&["id", column])
        .annotate(&[column], &[("count", Aggregate::count())])
        .await
        .unwrap_or_default();
    rows.iter()
        .filter_map(|row| {
            match (
                row.get(column).and_then(|v| v.as_str()),
                row.get("count").and_then(|v| v.as_f64()),
            ) {
                (Some(s), Some(n)) => Some((s.to_string(), n)),
                _ => None,
            }
        })
        .collect()
}

/// SQL `SUM(<column>)` over every non-deleted plugin — nulls (an
/// unsynced star/download count) are ignored by SUM, and an empty table
/// yields 0.0. `column` is `"github_stars"` or `"downloads"`.
pub async fn plugin_total(column: &'static str) -> f64 {
    pd::Plugin::objects()
        .aggregate(&[("total", Aggregate::sum(column))])
        .await
        .ok()
        .and_then(|v| v.get("total").and_then(|x| x.as_f64()))
        .unwrap_or(0.0)
}
