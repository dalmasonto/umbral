//! Dashboard widget system for umbra-admin.
//!
//! Plugins register widgets via `AdminPlugin::register_widget`. Each widget
//! has a `key`, `title`, `kind`, `default_span`, optional `permission`, and
//! an async data function. The admin dashboard renders a 12-column grid of
//! the user's saved layout (defaulting to all permitted widgets).
//!
//! ## Registration shape
//!
//! ```rust,ignore
//! admin.register_widget(Widget {
//!     key:          "umbra_total_models",
//!     title:        "Total Models".to_string(),
//!     kind:         WidgetKind::Kpi,
//!     default_span: Span { cols: 3, rows: 1 },
//!     permission:   None,
//!     data:         WidgetDataFn::new(|_user| async move {
//!         WidgetPayload::Kpi(KpiPayload {
//!             value:     "42".to_string(),
//!             unit:      None,
//!             delta:     None,
//!             sparkline: None,
//!         })
//!     }),
//! });
//! ```
//!
//! ## Endpoint contract
//!
//! - `GET /admin/api/dashboard/catalog` — `[{key, title, kind, default_span}]`
//! - `GET /admin/api/dashboard/layout`  — user's saved layout or default
//! - `PUT /admin/api/dashboard/layout`  — save user's layout
//! - `GET /admin/api/dashboard/widgets/{key}/data` — typed payload JSON

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use umbra_auth::AuthUser;

// =========================================================================
// Span
// =========================================================================

/// Grid span in the 12-column dashboard grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// Number of columns to occupy (1–12).
    pub cols: u8,
    /// Number of rows to occupy (1–N).
    pub rows: u8,
}

impl Default for Span {
    fn default() -> Self {
        Self { cols: 3, rows: 1 }
    }
}

// =========================================================================
// WidgetKind
// =========================================================================

/// The visual kind of a dashboard widget. Drives how the payload is rendered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WidgetKind {
    /// Simple single-value KPI (legacy, kept for backwards compat).
    Kpi,
    /// Shop-style summary card: title + icon + small unit / subtitle
    /// + large humanized value + optional growth-vs-previous-period.
    /// The everyday "Total sales / Orders / Customers" tile.
    Card,
    Line,
    Bar,
    Table,
    Feed,
}

impl WidgetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WidgetKind::Kpi => "kpi",
            WidgetKind::Card => "card",
            WidgetKind::Line => "line",
            WidgetKind::Bar => "bar",
            WidgetKind::Table => "table",
            WidgetKind::Feed => "feed",
        }
    }
}

// =========================================================================
// Typed payloads
// =========================================================================

/// KPI card payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KpiPayload {
    /// The primary metric value (displayed large).
    pub value: String,
    /// Optional unit label, e.g. "rows" or "MB".
    pub unit: Option<String>,
    /// Optional delta percentage; positive = up, negative = down.
    pub delta: Option<f64>,
    /// Optional sparkline data points (values only; x is implicit index).
    pub sparkline: Option<Vec<f64>>,
}

// =========================================================================
// Card payload — the everyday "summary tile" widget.
// =========================================================================

/// Summary card payload. Renders as:
///
/// ```text
/// ┌──────────────────────────────────────────┐
/// │ TITLE                          [icon]    │  ← title row (from Widget)
/// │                                          │
/// │ USD                       12,438.20      │  ← unit (sm, left) + value (lg, right)
/// │                                          │
/// │ This month        ↑ 12.3% vs last month  │  ← subtitle + growth
/// └──────────────────────────────────────────┘
/// ```
///
/// Build with [`CardPayload::new`] + the chained setters; pass to
/// [`WidgetPayload::Card`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardPayload {
    /// Formatted primary value, e.g. "12,438.20" or "12.4K". Use
    /// [`humanize_number`] for the K/M/B/T compaction.
    pub value: String,
    /// Optional unit / context label shown on the left side of the
    /// value row, e.g. "USD", "rows", "today".
    pub unit: Option<String>,
    /// Optional Lucide icon name (e.g. "dollar-sign", "shopping-cart").
    /// Rendered via the data-lucide attribute the wrapper already
    /// initializes.
    pub icon: Option<String>,
    /// Optional caption below the value, e.g. "This month".
    pub subtitle: Option<String>,
    /// Percentage delta vs. the previous period, signed:
    /// `+12.3` = up 12.3%, `-4.1` = down 4.1%. The renderer picks
    /// the arrow + color from the sign.
    pub delta_percent: Option<f64>,
    /// Optional comparison label, e.g. "vs last month".
    pub delta_label: Option<String>,
    /// Optional trend trail — a flat series of N points the
    /// renderer plots as a fade-right sparkline under the value.
    /// X is implicit (evenly spaced); Y autoscales between
    /// min/max. Pair with `growth(...)` so the pill matches the
    /// trail visually. Keep the series small (7–30 points) —
    /// anything denser turns into noise at sparkline scale.
    pub sparkline: Option<Vec<f64>>,
}

impl CardPayload {
    /// New card with just a primary value. Caller picks the format
    /// — strings stay as-is, numbers should be pre-humanized with
    /// [`humanize_number`] / [`format_thousands`].
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            unit: None,
            icon: None,
            subtitle: None,
            delta_percent: None,
            delta_label: None,
            sparkline: None,
        }
    }

    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Compute the delta automatically from current + previous raw
    /// numbers. Skips the delta when `previous` is zero (no baseline
    /// to grow from) or non-finite — the renderer just won't show
    /// the growth row in that case.
    pub fn growth(mut self, current: f64, previous: f64) -> Self {
        if previous.is_finite() && previous != 0.0 && current.is_finite() {
            self.delta_percent = Some(((current - previous) / previous.abs()) * 100.0);
        }
        self
    }

    /// Explicit delta percent (signed) + label. Use when you've
    /// computed the percentage yourself or want a custom label.
    pub fn delta(mut self, percent: f64, label: impl Into<String>) -> Self {
        self.delta_percent = Some(percent);
        self.delta_label = Some(label.into());
        self
    }

    /// Standalone label for the delta — pairs with [`Self::growth`]
    /// for the common case "auto-compute the percent but customize
    /// the comparison label" (e.g. `"vs prior 30d"`).
    pub fn delta_label(mut self, label: impl Into<String>) -> Self {
        self.delta_label = Some(label.into());
        self
    }

    /// Attach a trend trail rendered as a fade-right sparkline
    /// under the value. Pass 7–30 raw numbers (daily totals,
    /// hourly counts, etc.); the renderer autoscales and colors
    /// the stroke to match [`Self::delta_percent`]'s sign.
    pub fn sparkline(mut self, points: impl IntoIterator<Item = f64>) -> Self {
        self.sparkline = Some(points.into_iter().collect());
        self
    }
}

/// Humanize a number into a compact display string:
///
/// | input            | output     |
/// |------------------|------------|
/// | `42.0`           | `"42"`     |
/// | `1_234.5`        | `"1,234.50"` |
/// | `12_438.2`       | `"12.4K"`  |
/// | `1_500_000.0`    | `"1.50M"`  |
/// | `2_700_000_000.` | `"2.70B"`  |
///
/// Suitable for card values where horizontal space is scarce.
pub fn humanize_number(n: f64) -> String {
    if !n.is_finite() {
        return "—".to_string();
    }
    let abs = n.abs();
    let sign = if n < 0.0 { "-" } else { "" };
    if abs < 1000.0 {
        // Two decimals when there's a fractional part; integer otherwise.
        if (abs.fract() - 0.0).abs() < f64::EPSILON {
            return format!("{sign}{}", abs as i64);
        }
        return format!("{sign}{:.2}", abs);
    }
    if abs < 1_000_000.0 {
        if abs < 10_000.0 {
            // Keep the thousands separator at the low end of the K
            // range — "9,876" reads better than "9.9K" for amounts a
            // user is likely to mentally verify against the data.
            return format_thousands(n);
        }
        return format!("{sign}{:.1}K", abs / 1_000.0);
    }
    if abs < 1_000_000_000.0 {
        return format!("{sign}{:.2}M", abs / 1_000_000.0);
    }
    if abs < 1_000_000_000_000.0 {
        return format!("{sign}{:.2}B", abs / 1_000_000_000.0);
    }
    format!("{sign}{:.2}T", abs / 1_000_000_000_000.0)
}

/// Format a number with thousands separators and (when fractional)
/// two decimal places. Use for values where the full digits matter
/// (currency totals, audit counts) — for compact display use
/// [`humanize_number`].
pub fn format_thousands(n: f64) -> String {
    if !n.is_finite() {
        return "—".to_string();
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let abs = n.abs();
    let int_part = abs.trunc() as u128;
    let frac_part = abs - abs.trunc();

    // Insert commas every 3 digits, right-to-left.
    let int_str = int_part.to_string();
    let bytes = int_str.as_bytes();
    let mut grouped = String::with_capacity(int_str.len() + int_str.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(*b as char);
    }

    if frac_part > 0.0 {
        format!("{sign}{grouped}.{:02}", (frac_part * 100.0).round() as u64)
    } else {
        format!("{sign}{grouped}")
    }
}

/// One data series for Line or Bar charts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub points: Vec<ChartPoint>,
}

/// X/Y data point. X is a string for flexible labeling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartPoint {
    pub x: String,
    pub y: f64,
}

/// Line chart payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinePayload {
    pub series: Vec<Series>,
    /// Describes what `x` represents; e.g. "date", "category".
    pub x_type: String,
}

/// Bar chart payload (same shape as Line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarPayload {
    pub series: Vec<Series>,
    pub x_type: String,
}

/// Table widget column descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableColumn {
    pub key: String,
    pub label: String,
}

/// Table widget payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePayload {
    pub columns: Vec<TableColumn>,
    pub rows: Vec<serde_json::Value>,
}

/// One item in an activity feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedItem {
    pub actor: String,
    pub verb: String,
    pub object: String,
    pub object_link: Option<String>,
    pub at: String,
}

/// Activity feed payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedPayload {
    pub items: Vec<FeedItem>,
}

/// Union of all widget payloads. The JSON discriminant is the variant name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WidgetPayload {
    Kpi(KpiPayload),
    Card(CardPayload),
    Line(LinePayload),
    Bar(BarPayload),
    Table(TablePayload),
    Feed(FeedPayload),
}

// =========================================================================
// WidgetDataFn
// =========================================================================

pub(crate) type DataFuture = Pin<Box<dyn Future<Output = WidgetPayload> + Send + 'static>>;
pub(crate) type DataFnInner = Arc<dyn Fn(AuthUser) -> DataFuture + Send + Sync + 'static>;

/// Wrapper around the async data closure. Build via [`WidgetDataFn::new`].
#[derive(Clone)]
pub struct WidgetDataFn(pub(crate) DataFnInner);

impl WidgetDataFn {
    /// Create from any `async fn(AuthUser) -> WidgetPayload`.
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(AuthUser) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WidgetPayload> + Send + 'static,
    {
        Self(Arc::new(move |user| Box::pin(f(user))))
    }
}

impl std::fmt::Debug for WidgetDataFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WidgetDataFn(<fn>)")
    }
}

// =========================================================================
// Widget
// =========================================================================

/// A registered dashboard widget.
///
/// Register via `AdminPlugin::register_widget(...)`.
#[derive(Debug, Clone)]
pub struct Widget {
    /// URL-safe unique key, e.g. `"umbra_total_models"`.
    pub key: &'static str,
    /// Human-readable title shown in the widget card header.
    pub title: String,
    /// Determines which renderer (KPI card, chart, table, feed).
    pub kind: WidgetKind,
    /// Default grid span when the user hasn't customized.
    pub default_span: Span,
    /// Optional permission codename. `None` = any staff user may see.
    pub permission: Option<&'static str>,
    /// Async function that computes and returns the payload.
    pub data: WidgetDataFn,
}

// =========================================================================
// WidgetInstance (user's saved layout entry)
// =========================================================================

/// One entry in a user's saved layout JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidgetInstance {
    pub key: String,
    pub span: Span,
}

// =========================================================================
// Widget catalog entry (API response shape)
// =========================================================================

/// Serialized catalog entry returned by `GET /admin/api/dashboard/catalog`.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogEntry {
    pub key: &'static str,
    pub title: String,
    pub kind: String,
    pub default_span: Span,
}
