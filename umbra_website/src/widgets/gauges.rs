//! Radial gauge + progress bar-list for the directory dashboard —
//! the plugin-directory analogue of the shop's `gauges.rs`.
//!
//! - `audit_coverage_radial`: the share of plugins that have reached
//!   any reviewed audit state. One ring, percent in the centre.
//! - `top_plugins_progress`: the directory's most-starred plugins as
//!   a ranked bar list, each bar sized against the leader.

use plugin_directory::models::{self as pd};
use umbra_admin::{
    ProgressPayload, RadialPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload,
};

use super::aggregates::plugin_counts;

/// Radial gauge — the fraction of plugins whose `audit_status` is any
/// reviewed state (self / umbra / third-party reviewed), as a percent.
pub fn audit_coverage_radial() -> Widget {
    Widget {
        key: "pd_audit_coverage_radial",
        title: "Audit coverage".to_string(),
        kind: WidgetKind::Radial,
        default_span: Span { cols: 4, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let counts = plugin_counts("audit_status").await;
            let total: f64 = counts.iter().map(|(_, n)| *n).sum();
            let reviewed: f64 = counts
                .iter()
                .filter(|(s, _)| {
                    matches!(
                        s.as_str(),
                        "self_reviewed" | "umbra_reviewed" | "third_party_reviewed"
                    )
                })
                .map(|(_, n)| *n)
                .sum();
            let pct = if total > 0.0 {
                reviewed / total * 100.0
            } else {
                0.0
            };
            WidgetPayload::Radial(RadialPayload::single("Reviewed", pct))
        }),
    }
}

/// Progress bar-list — the top plugins by GitHub stars, ranked
/// descending, each bar sized against the leader. Plugins with no
/// synced star count are excluded (never shown as a zero bar).
pub fn top_plugins_progress() -> Widget {
    Widget {
        key: "pd_top_plugins_progress",
        title: "Top plugins by stars".to_string(),
        kind: WidgetKind::Progress,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            // Small dataset: pull (name, stars) for every plugin, then
            // rank in memory — the ranking needs the values anyway, and
            // doing it here sidesteps nullable-column NULLS-FIRST
            // ordering quirks across backends.
            let rows = pd::Plugin::objects()
                .only(&["id", "name", "github_stars"])
                .fetch()
                .await
                .unwrap_or_default();
            let mut pairs: Vec<(String, f64)> = rows
                .into_iter()
                .filter_map(|p| match p.github_stars {
                    Some(n) if n > 0 => Some((p.name, n as f64)),
                    _ => None,
                })
                .collect();
            pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            pairs.truncate(8);
            WidgetPayload::Progress(ProgressPayload::from_pairs(pairs))
        }),
    }
}
