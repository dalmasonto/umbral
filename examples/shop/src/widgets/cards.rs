//! Four facing-page tiles for the admin dashboard. Each follows
//! the same shape: build a `Widget` with kind `Card`, hand it an
//! async data closure that hits the ORM, hands back a
//! `CardPayload` built with chained setters
//! (`.unit().icon().subtitle().growth(...)`).
//!
//! Numbers go through `humanize_number` so "12,438" → "12.4K"
//! when the column gets narrow; `format_thousands` keeps full
//! digits with commas for the "verify-this" AOV tile.

use ecommerce::models::Customer;
use umbral_admin::{CardPayload, Span, Widget, WidgetDataFn, WidgetKind, WidgetPayload};

use super::aggregates::{daily_orders_trail, daily_sales_trail, orders_between, sales_between};

pub fn shop_total_sales_widget() -> Widget {
    Widget {
        key: "shop_total_sales",
        title: "Total Sales".to_string(),
        kind: WidgetKind::Card,
        // rows: 2 (= 240px in a 120px auto-row grid). Cards with a
        // sparkline need ~220px to fit icon row + body + 48px trail
        // without clipping; 240px gives bottom padding too.
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let now = chrono::Utc::now();
            let month_ago = now - chrono::Duration::days(30);
            let two_months_ago = now - chrono::Duration::days(60);

            let current = sales_between(month_ago, now).await;
            let previous = sales_between(two_months_ago, month_ago).await;
            let trail = daily_sales_trail(30).await;

            WidgetPayload::Card(
                CardPayload::new(umbral_admin::humanize_number(current))
                    .unit("USD")
                    .icon("dollar-sign")
                    .subtitle("Last 30 days")
                    .growth(current, previous)
                    .delta_label("vs prior 30d".to_string())
                    .sparkline(trail),
            )
        }),
    }
}

pub fn shop_orders_widget() -> Widget {
    Widget {
        key: "shop_orders",
        title: "Orders".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let now = chrono::Utc::now();
            let month_ago = now - chrono::Duration::days(30);
            let two_months_ago = now - chrono::Duration::days(60);

            let current = orders_between(month_ago, now).await;
            let previous = orders_between(two_months_ago, month_ago).await;
            let trail = daily_orders_trail(30).await;

            WidgetPayload::Card(
                CardPayload::new(umbral_admin::humanize_number(current as f64))
                    .unit("total")
                    .icon("shopping-cart")
                    .subtitle("Last 30 days")
                    .growth(current as f64, previous as f64)
                    .delta_label("vs prior 30d".to_string())
                    .sparkline(trail),
            )
        }),
    }
}

pub fn shop_customers_widget() -> Widget {
    Widget {
        key: "shop_customers",
        title: "Customers".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let total = Customer::objects().count().await.unwrap_or(0);
            WidgetPayload::Card(
                CardPayload::new(umbral_admin::humanize_number(total as f64))
                    .unit("total")
                    .icon("users")
                    .subtitle("All time"),
            )
        }),
    }
}

pub fn shop_avg_order_value_widget() -> Widget {
    Widget {
        key: "shop_avg_order_value",
        title: "Avg Order Value".to_string(),
        kind: WidgetKind::Card,
        default_span: Span { cols: 3, rows: 2 },
        permission: None,
        default_period: None,
        filters: Vec::new(),
        data: WidgetDataFn::new(|_user| async move {
            let now = chrono::Utc::now();
            let month_ago = now - chrono::Duration::days(30);
            let two_months_ago = now - chrono::Duration::days(60);

            let cur_sales = sales_between(month_ago, now).await;
            let cur_orders = orders_between(month_ago, now).await.max(1) as f64;
            let cur_aov = cur_sales / cur_orders;

            let prev_sales = sales_between(two_months_ago, month_ago).await;
            let prev_orders = orders_between(two_months_ago, month_ago).await.max(1) as f64;
            let prev_aov = prev_sales / prev_orders;

            // Daily AOV trail — sales/orders per day, last 30 days.
            let sales_trail = daily_sales_trail(30).await;
            let orders_trail = daily_orders_trail(30).await;
            let aov_trail: Vec<f64> = sales_trail
                .iter()
                .zip(orders_trail.iter())
                .map(|(s, o)| if *o > 0.0 { s / o } else { 0.0 })
                .collect();

            WidgetPayload::Card(
                CardPayload::new(umbral_admin::format_thousands(cur_aov))
                    .unit("USD")
                    .icon("trending-up")
                    .subtitle("Per order, 30d")
                    .growth(cur_aov, prev_aov)
                    .delta_label("vs prior 30d".to_string())
                    .sparkline(aov_trail),
            )
        }),
    }
}
