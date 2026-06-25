//! Radial gauge + progress bar-list for the directory dashboard —
//! the plugin-directory analogue of the shop's `gauges.rs`.
//!
//! - `audit_coverage_radial`: the share of plugins that have reached
//!   any reviewed audit state. One ring, percent in the centre.
//! - `top_plugins_progress`: the directory's most-starred plugins as
//!   a ranked bar list, each bar sized against the leader.

use umbral_admin::{
    ProgressPayload, RadialPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload,
};

use super::aggregates::plugin_counts;

/// Radial gauge — the fraction of plugins whose `audit_status` is any
/// reviewed state (self / umbral / third-party reviewed), as a percent.
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
                        "self_reviewed" | "umbral_reviewed" | "third_party_reviewed"
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

/// Progress bar-list — how many plugins sit at each maturity level
/// (stable / beta / alpha / design), ranked descending with each bar
/// sized against the largest bucket.
///
/// (Replaces a top-plugins-by-stars ranking: stars are a maintainer-
/// synced metric the directory must never fabricate, so until a real
/// sync lands it has no data to rank. Maturity is always populated.)
pub fn plugins_by_maturity() -> Widget {
    Widget {
        key: "pd_plugins_by_maturity",
        title: "Plugins by maturity".to_string(),
        kind: WidgetKind::Progress,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let mut pairs = plugin_counts("maturity").await;
            pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            WidgetPayload::Progress(ProgressPayload::from_pairs(pairs))
        }),
    }
}
