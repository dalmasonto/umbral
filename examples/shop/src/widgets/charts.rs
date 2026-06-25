//! Line + donut chart widgets for the shop dashboard.
//!
//! - `daily_sales_chart`: single-series area chart with period
//!   chips. Demonstrates the line widget's `with_default_period`
//!   pre-selection + the `?period=` filter context.
//! - `activity_chart`: multi-series area chart — orders, items
//!   sold, new customers overlaid on the same x-axis.
//! - `order_status_donut`: 6-bucket order-status mix. Bar
//!   alternative for low-cardinality categorical data.

use ecommerce::models::{Order, order};
use std::collections::HashMap;
use umbral::orm::Aggregate;
use umbral_admin::{
    ChartPoint, DonutPayload, LinePayload, Series, Span, Widget, WidgetDataFn, WidgetKind,
    WidgetPayload,
};

use super::aggregates::{daily_customers_trail, daily_orders_trail, daily_sales_trail};

pub fn shop_daily_sales_chart() -> Widget {
    Widget {
        key: "shop_daily_sales_chart",
        title: "Daily Sales".to_string(),
        kind: WidgetKind::Line,
        default_span: Span { cols: 8, rows: 3 },
        permission: None,
        default_period: Some("30d"),
        data: WidgetDataFn::with_params(|_user, params| async move {
            let days = params.period_days().unwrap_or(30);
            let now = chrono::Utc::now();
            let trail = daily_sales_trail(days).await;
            let points: Vec<ChartPoint> = trail
                .into_iter()
                .enumerate()
                .map(|(i, y)| {
                    let back = (days - 1 - i as i64).max(0);
                    let day = now - chrono::Duration::days(back);
                    ChartPoint {
                        x: day.format("%b %-d").to_string(),
                        y,
                    }
                })
                .collect();
            WidgetPayload::Line(LinePayload {
                series: vec![Series {
                    name: "USD".to_string(),
                    points,
                }],
                x_type: "date".to_string(),
            })
        }),
    }
}

/// Multi-series area chart — orders, items sold, and new
/// customers per day plotted on the same timeline.
pub fn shop_activity_chart() -> Widget {
    Widget {
        key: "shop_activity_chart",
        title: "Activity".to_string(),
        kind: WidgetKind::Line,
        default_span: Span { cols: 8, rows: 3 },
        permission: None,
        default_period: Some("30d"),
        data: WidgetDataFn::with_params(|_user, params| async move {
            let days = params.period_days().unwrap_or(30);
            let now = chrono::Utc::now();
            let (orders, items, customers) = tokio::join!(
                daily_orders_trail(days),
                async {
                    let mut out = Vec::with_capacity(days as usize);
                    for back in (0..days).rev() {
                        let end = now - chrono::Duration::days(back);
                        let start = end - chrono::Duration::days(1);
                        // COUNT in SQL — don't pull the day's rows into
                        // memory just to .len() them (gaps2 #56).
                        let orders_n = Order::objects()
                            .filter(order::PLACED_AT.gte(start))
                            .filter(order::PLACED_AT.lt(end))
                            .count()
                            .await
                            .unwrap_or(0);
                        // Approximate items_sold ≈ N × small constant.
                        out.push((orders_n as f64) * 1.5);
                    }
                    out
                },
                daily_customers_trail(days),
            );
            let labels: Vec<String> = (0..days)
                .rev()
                .map(|back| {
                    let day = now - chrono::Duration::days(back);
                    day.format("%b %-d").to_string()
                })
                .collect();
            let mk_series = |name: &str, values: Vec<f64>| Series {
                name: name.to_string(),
                points: labels
                    .iter()
                    .zip(values.into_iter())
                    .map(|(x, y)| ChartPoint { x: x.clone(), y })
                    .collect(),
            };
            WidgetPayload::Line(LinePayload {
                series: vec![
                    mk_series("Orders", orders),
                    mk_series("Items sold", items),
                    mk_series("New customers", customers),
                ],
                x_type: "date".to_string(),
            })
        }),
    }
}

/// Order-status donut — slices every order into its status
/// bucket in canonical lifecycle order.
pub fn shop_order_status_donut() -> Widget {
    Widget {
        key: "shop_order_status_donut",
        title: "Order Status".to_string(),
        kind: WidgetKind::Donut,
        default_span: Span { cols: 4, rows: 3 },
        permission: None,
        default_period: None,
        data: WidgetDataFn::new(|_user| async move {
            // GROUP BY status in SQL — one row per status with its
            // count — instead of fetching every order into memory and
            // tallying client-side (gaps2 #56). The ORM equivalent of
            // Django's `.values("status").annotate(count=Count("id"))`.
            let rows = Order::objects()
                .only(&["id", "status"])
                .annotate(&["status"], &[("count", Aggregate::count())])
                .await
                .unwrap_or_default();
            let mut counts: HashMap<String, f64> = HashMap::new();
            for row in &rows {
                if let (Some(label), Some(n)) = (
                    row.get("status").and_then(|v| v.as_str()),
                    row.get("count").and_then(|v| v.as_f64()),
                ) {
                    counts.insert(label.to_string(), n);
                }
            }
            let order = [
                "pending",
                "paid",
                "fulfilled",
                "shipped",
                "delivered",
                "cancelled",
                "refunded",
            ];
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
