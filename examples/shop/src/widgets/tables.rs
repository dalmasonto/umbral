//! Recent-Orders table widget. Five most-recent orders with a
//! "View all →" link to the admin's order changelist —
//! `view_all_for::<Order>()` resolves the URL from the model
//! type so a `#[umbral(table = "...")]` rename propagates
//! without chasing strings.

use ecommerce::models::{Order, order};
use umbral_admin::{
    Span, TableColumn, TablePayload, Widget, WidgetDataFn, WidgetKind, WidgetPayload,
};

pub fn shop_recent_orders_table() -> Widget {
    Widget {
        key: "shop_recent_orders",
        title: "Recent Orders".to_string(),
        kind: WidgetKind::Table,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            let columns = vec![
                TableColumn {
                    key: "number".to_string(),
                    label: "Order".to_string(),
                },
                TableColumn {
                    key: "status".to_string(),
                    label: "Status".to_string(),
                },
                TableColumn {
                    key: "grand_total".to_string(),
                    label: "Total".to_string(),
                },
            ];
            let rows: Vec<serde_json::Value> = Order::objects()
                .order_by(order::PLACED_AT.desc())
                .limit(5)
                .fetch()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|o| {
                    serde_json::json!({
                        "number":      o.number,
                        "status":      format!("{:?}", o.status).to_lowercase(),
                        "grand_total": format!("${}", o.grand_total),
                    })
                })
                .collect();
            WidgetPayload::Table(TablePayload::new(columns, rows).view_all_for::<Order>())
        }),
    }
}
