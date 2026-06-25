//! Table + activity-feed widgets for the directory dashboard — the
//! plugin-directory analogue of the shop's `tables.rs`.
//!
//! - `recent_plugins_table`: the five most-recently-listed plugins with
//!   a "View all →" link to the admin changelist.
//! - `recent_activity_feed`: the latest plugins as an activity stream.

use plugin_directory::models::{self as pd, plugin};
use umbral_admin::{
    FeedItem, FeedPayload, Span, TableColumn, TablePayload, Widget, WidgetDataFn, WidgetKind,
    WidgetPayload,
};

/// Recent-plugins table — five most-recently-listed plugins. The "View
/// all →" link resolves from the `Plugin` model type, so a table rename
/// propagates without chasing strings.
pub fn recent_plugins_table() -> Widget {
    Widget {
        key: "pd_recent_plugins",
        title: "Recently listed".to_string(),
        kind: WidgetKind::Table,
        default_span: Span { cols: 6, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let columns = vec![
                TableColumn {
                    key: "name".to_string(),
                    label: "Plugin".to_string(),
                },
                TableColumn {
                    key: "status".to_string(),
                    label: "Status".to_string(),
                },
                TableColumn {
                    key: "maturity".to_string(),
                    label: "Maturity".to_string(),
                },
            ];
            let rows: Vec<serde_json::Value> = pd::Plugin::objects()
                .order_by(plugin::CREATED_AT.desc())
                .limit(5)
                .fetch()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    serde_json::json!({
                        "name":     p.name,
                        "status":   format!("{:?}", p.status),
                        "maturity": format!("{:?}", p.maturity),
                    })
                })
                .collect();
            WidgetPayload::Table(TablePayload::new(columns, rows).view_all_for::<pd::Plugin>())
        }),
    }
}

/// Recent-activity feed — the latest plugins listed in the directory, as
/// an actor/verb/object stream linking back to each plugin page.
pub fn recent_activity_feed() -> Widget {
    Widget {
        key: "pd_recent_activity",
        title: "Recent activity".to_string(),
        kind: WidgetKind::Feed,
        default_span: Span { cols: 6, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let items: Vec<FeedItem> = pd::Plugin::objects()
                .order_by(plugin::CREATED_AT.desc())
                .limit(8)
                .fetch()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|p| FeedItem {
                    actor: p.author,
                    verb: "listed".to_string(),
                    object: p.name,
                    object_link: Some(format!("/plugins/{}", p.crate_name)),
                    at: p.created_at.format("%b %-d").to_string(),
                })
                .collect();
            WidgetPayload::Feed(FeedPayload::new(items).view_all_for::<pd::Plugin>())
        }),
    }
}
