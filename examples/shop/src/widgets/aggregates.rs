//! Per-window aggregate helpers — sum sales / count orders /
//! count customers across a (start, end) window, plus daily-bucket
//! versions used by sparklines + line charts.
//!
//! Pulled out of `main.rs` so widgets that need them (cards,
//! charts) can compose without dragging the full handler module
//! along. Each helper fans one ORM call (sometimes one filter +
//! one aggregate) and unwraps to a zero default — widgets that
//! call these on a fresh DB get sensible empty-state numbers
//! instead of bubbling an error all the way up.

use chrono::{DateTime, Duration, Utc};
use ecommerce::models::{Customer, Order, customer, order};

/// Sum order grand_total for orders placed in a window.
pub async fn sales_between(start: DateTime<Utc>, end: DateTime<Utc>) -> f64 {
    let rows = Order::objects()
        .filter(order::PLACED_AT.gte(start))
        .filter(order::PLACED_AT.lt(end))
        .fetch()
        .await
        .unwrap_or_default();
    rows.iter()
        .filter_map(|o| o.grand_total.parse::<f64>().ok())
        .sum()
}

/// Count orders placed in a window.
pub async fn orders_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    Order::objects()
        .filter(order::PLACED_AT.gte(start))
        .filter(order::PLACED_AT.lt(end))
        .count()
        .await
        .unwrap_or(0)
}

/// Count customer rows created in a window.
pub async fn customers_between(start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    Customer::objects()
        .filter(customer::CREATED_AT.gte(start))
        .filter(customer::CREATED_AT.lt(end))
        .count()
        .await
        .unwrap_or(0)
}

/// Daily sales totals for the last `days` days, oldest-first.
/// Feeds card sparklines + line charts.
pub async fn daily_sales_trail(days: i64) -> Vec<f64> {
    let now = Utc::now();
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let end = now - Duration::days(back);
        let start = end - Duration::days(1);
        out.push(sales_between(start, end).await);
    }
    out
}

/// Daily order counts for the last `days` days, oldest-first.
pub async fn daily_orders_trail(days: i64) -> Vec<f64> {
    let now = Utc::now();
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let end = now - Duration::days(back);
        let start = end - Duration::days(1);
        out.push(orders_between(start, end).await as f64);
    }
    out
}

/// Daily customer-signup counts for the last `days` days, oldest-first.
pub async fn daily_customers_trail(days: i64) -> Vec<f64> {
    let now = Utc::now();
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let end = now - Duration::days(back);
        let start = end - Duration::days(1);
        out.push(customers_between(start, end).await as f64);
    }
    out
}
