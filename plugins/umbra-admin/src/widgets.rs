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
    Kpi,
    Line,
    Bar,
    Table,
    Feed,
}

impl WidgetKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            WidgetKind::Kpi => "kpi",
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
