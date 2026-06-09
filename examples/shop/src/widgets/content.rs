//! Content-plugin widgets — same Table / Donut / Card kinds the
//! ecommerce side uses, just pointed at the `content` plugin's
//! Post / Subscriber models. Demonstrates that adding a new
//! domain's section to the dashboard is a data-closure change,
//! not a framework change.

use content::models::{Post, Subscriber, post, subscriber};
use std::collections::HashMap;
use umbra_admin::{
    CardPayload, DonutPayload, Span, TableColumn, TablePayload, Widget, WidgetDataFn, WidgetKind,
    WidgetPayload,
};

/// Recent-posts table — five most-recent posts with a "View
/// all →" link to /admin/post/.
pub fn content_recent_posts_table() -> Widget {
    Widget {
        key: "content_recent_posts",
        title: "Recent Posts".to_string(),
        kind: WidgetKind::Table,
        default_span: Span { cols: 6, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let columns = vec![
                TableColumn {
                    key: "title".to_string(),
                    label: "Title".to_string(),
                },
                TableColumn {
                    key: "status".to_string(),
                    label: "Status".to_string(),
                },
                TableColumn {
                    key: "views".to_string(),
                    label: "Views".to_string(),
                },
            ];
            let rows: Vec<serde_json::Value> = Post::objects()
                .order_by(post::CREATED_AT.desc())
                .limit(5)
                .fetch()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    serde_json::json!({
                        "title":  p.title,
                        "status": format!("{:?}", p.status).to_lowercase(),
                        "views":  umbra_admin::humanize_number(p.view_count as f64),
                    })
                })
                .collect();
            WidgetPayload::Table(TablePayload::new(columns, rows).view_all_for::<Post>())
        }),
    }
}

/// Post-status donut — draft / scheduled / published in canonical
/// lifecycle order.
pub fn content_post_status_donut() -> Widget {
    Widget {
        key: "content_post_status_donut",
        title: "Post Status".to_string(),
        kind: WidgetKind::Donut,
        default_span: Span { cols: 3, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let posts = Post::objects().fetch().await.unwrap_or_default();
            let mut counts: HashMap<String, f64> = HashMap::new();
            for p in &posts {
                let label = format!("{:?}", p.status).to_lowercase();
                *counts.entry(label).or_insert(0.0) += 1.0;
            }
            let order = ["draft", "scheduled", "published"];
            let mut pairs: Vec<(String, f64)> = Vec::new();
            for k in order {
                if let Some(v) = counts.remove(k) {
                    pairs.push((k.to_string(), v));
                }
            }
            pairs.extend(counts.into_iter());
            WidgetPayload::Donut(DonutPayload::from_pairs(pairs))
        }),
    }
}

/// Newsletter-subscribers card — total + confirmed in the
/// subtitle. KPI-style tile at the standard 3×2 card span.
pub fn content_subscribers_card() -> Widget {
    Widget {
        key: "content_subscribers",
        title: "Subscribers".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let total = Subscriber::objects().count().await.unwrap_or(0);
            let confirmed = Subscriber::objects()
                .filter(subscriber::IS_CONFIRMED.eq(true))
                .count()
                .await
                .unwrap_or(0);
            WidgetPayload::Card(
                CardPayload::new(umbra_admin::humanize_number(total as f64))
                    .unit("total")
                    .icon("mail")
                    .subtitle(format!("{confirmed} confirmed")),
            )
        }),
    }
}
