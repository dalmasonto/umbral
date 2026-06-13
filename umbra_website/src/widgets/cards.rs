//! Four facing-page KPI tiles for the directory dashboard. Same
//! shape as the shop's `cards.rs`: each builds a `Widget` of kind
//! `Card` with an async closure that hits the ORM and returns a
//! `CardPayload` assembled with chained setters.
//!
//! Numbers run through `humanize_number` so a wide count collapses
//! to "12.4K" when the column gets narrow.

use plugin_directory::models::{self as pd, plugin};
use umbra_admin::{CardPayload, KpiPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload};

use super::aggregates::{
    comments_between, daily_comments_trail, daily_plugins_trail, plugins_between,
    visible_comments_total,
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
            // Headline is the all-time visible-notes count; the 30d window
            // drives the growth pill + sparkline.
            let total = visible_comments_total().await;

            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(total as f64))
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

/// Featured plugins — the curated set surfaced on the public landing
/// page. (Replaces a GitHub-stars tile: stars are a maintainer-synced
/// metric the directory must never fabricate, so a stars widget would
/// read 0 until a real sync lands.)
pub fn featured_card() -> Widget {
    Widget {
        key: "pd_featured",
        title: "Featured".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let featured = pd::Plugin::objects()
                .filter(plugin::FEATURED.eq(true))
                .count()
                .await
                .unwrap_or(0);
            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(featured as f64))
                    .unit("plugins")
                    .icon("star")
                    .subtitle("Surfaced on the landing page"),
            )
        }),
    }
}

/// Shipped plugins as a compact KPI tile (the `Kpi` widget kind) — the
/// count of plugins at the `shipped` lifecycle status, with a 14-day
/// submissions sparkline.
pub fn shipped_kpi() -> Widget {
    Widget {
        key: "pd_shipped_kpi",
        title: "Shipped".to_string(),
        kind: WidgetKind::Kpi,
        default_span: Span { cols: 4, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let shipped = pd::Plugin::objects()
                .filter(plugin::STATUS.eq("shipped"))
                .count()
                .await
                .unwrap_or(0);
            let trail = daily_plugins_trail(14).await;
            WidgetPayload::Kpi(KpiPayload {
                value: umbra_admin::humanize_number(shipped as f64),
                unit: Some("shipped".to_string()),
                delta: None,
                sparkline: Some(trail),
            })
        }),
    }
}
