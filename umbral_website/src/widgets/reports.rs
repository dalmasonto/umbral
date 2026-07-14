//! The "Reports" custom admin view.
//!
//! A dedicated analytics page mounted at `/admin/custom-views/reports/`
//! that mirrors the dashboard's Composition / Trends / Gauges sections,
//! reusing the SAME widget builders (from the sibling modules) rather than
//! defining new ones. Each reused widget is re-keyed `rpt_*` so its Reports
//! copy doesn't collide with the dashboard's `pd_*` copy in the admin's
//! global widget catalog — same data fn, independently-keyed cell.
//!
//! Wired in `main.rs` via `AdminPlugin::default().view(widgets::reports_view())`.
//! Design: docs/superpowers/specs/2026-07-01-admin-views-namespace-and-website-reports.md

use umbral_admin::{AdminView, Widget, WidgetSection};

use super::{
    activity_chart, audit_coverage_radial, plugins_by_maturity, shipped_kpi, source_mix_donut,
    status_maturity_heatmap, status_mix_donut, submissions_bar, submissions_chart,
};

/// Override a reused widget's key so its Reports copy is distinct from the
/// dashboard's copy in the admin's global (key → widget) catalog.
fn rekey(mut widget: Widget, key: &'static str) -> Widget {
    widget.key = key;
    widget
}

/// The "Reports" analytics view (`/admin/custom-views/reports/`), grouped
/// under "Insights" in the sidebar.
pub fn reports_view() -> AdminView {
    AdminView::new("reports", "Reports")
        .with_icon("bar-chart-3")
        .with_group("Insights")
        .section(
            WidgetSection::new("Composition")
                .subtitle("How the directory breaks down by source, status, and maturity")
                .widget(rekey(source_mix_donut(), "rpt_source_mix_donut"))
                .widget(rekey(status_mix_donut(), "rpt_status_mix_donut"))
                .widget(rekey(submissions_bar(), "rpt_submissions_bar"))
                .widget(rekey(
                    status_maturity_heatmap(),
                    "rpt_status_maturity_heatmap",
                )),
        )
        .section(
            WidgetSection::new("Trends")
                .subtitle("Submissions + discussion activity over the last week")
                .widget(
                    rekey(submissions_chart(), "rpt_submissions_chart").with_default_period("7d"),
                )
                .widget(rekey(activity_chart(), "rpt_activity_chart").with_default_period("7d")),
        )
        .section(
            WidgetSection::new("Gauges & rankings")
                .subtitle("Audit coverage gauge, maturity breakdown, and a shipped KPI")
                .widget(rekey(audit_coverage_radial(), "rpt_audit_coverage_radial"))
                .widget(rekey(plugins_by_maturity(), "rpt_plugins_by_maturity"))
                .widget(rekey(shipped_kpi(), "rpt_shipped_kpi")),
        )
}
