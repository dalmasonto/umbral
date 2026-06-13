//! Four facing-page KPI tiles for the directory dashboard. Same
//! shape as the shop's `cards.rs`: each builds a `Widget` of kind
//! `Card` with an async closure that hits the ORM and returns a
//! `CardPayload` assembled with chained setters.
//!
//! Numbers run through `humanize_number` so a wide count collapses
//! to "12.4K" when the column gets narrow.

use plugin_directory::models::{self as pd, plugin};
use umbra_admin::{CardPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload};

use super::aggregates::{
    comments_between, daily_comments_trail, daily_plugins_trail, plugin_total, plugins_between,
};

/// Total plugins in the directory (all non-deleted rows), with a
/// 30-day submissions sparkline + month-over-month growth.
pub fn total_plugins_card() -> Widget {
    Widget {
        key: "pd_total_plugins",
        title: "Total Plugins".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let now = chrono::Utc::now();
            let month_ago = now - chrono::Duration::days(30);
            let two_months_ago = now - chrono::Duration::days(60);

            let total = pd::Plugin::objects().count().await.unwrap_or(0);
            let current = plugins_between(month_ago, now).await;
            let previous = plugins_between(two_months_ago, month_ago).await;
            let trail = daily_plugins_trail(30).await;

            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(total as f64))
                    .unit("listed")
                    .icon("package")
                    .subtitle("All sources")
                    .growth(current as f64, previous as f64)
                    .delta_label("new vs prior 30d".to_string())
                    .sparkline(trail),
            )
        }),
    }
}

/// Submissions still awaiting moderation — a queue-depth KPI the
/// admin acts on. No sparkline: it's a standing count, not a flow.
pub fn pending_review_card() -> Widget {
    Widget {
        key: "pd_pending_review",
        title: "Pending Review".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let pending = pd::Plugin::objects()
                .filter(plugin::MODERATION.eq("pending"))
                .count()
                .await
                .unwrap_or(0);
            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(pending as f64))
                    .unit("in queue")
                    .icon("clock")
                    .subtitle("Awaiting moderation"),
            )
        }),
    }
}

/// Visible discussion notes across every plugin thread, with a 30-day
/// activity sparkline + growth.
pub fn discussion_notes_card() -> Widget {
    Widget {
        key: "pd_discussion_notes",
        title: "Discussion Notes".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let now = chrono::Utc::now();
            let month_ago = now - chrono::Duration::days(30);
            let two_months_ago = now - chrono::Duration::days(60);

            let current = comments_between(month_ago, now).await;
            let previous = comments_between(two_months_ago, month_ago).await;
            let trail = daily_comments_trail(30).await;
            // The card's headline is the visible all-time total; the
            // 30d window drives the growth pill + sparkline.
            let total: f64 = trail.iter().sum::<f64>().max(current as f64);

            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(total))
                    .unit("notes")
                    .icon("message-square")
                    .subtitle("Last 30 days")
                    .growth(current as f64, previous as f64)
                    .delta_label("vs prior 30d".to_string())
                    .sparkline(trail),
            )
        }),
    }
}

/// Total GitHub stars summed across the directory — a single SQL
/// `SUM`. Maintainer-synced, so no growth pill (it's a snapshot).
pub fn total_stars_card() -> Widget {
    Widget {
        key: "pd_total_stars",
        title: "GitHub Stars".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let stars = plugin_total("github_stars").await;
            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(stars))
                    .unit("stars")
                    .icon("star")
                    .subtitle("Across the directory"),
            )
        }),
    }
}
