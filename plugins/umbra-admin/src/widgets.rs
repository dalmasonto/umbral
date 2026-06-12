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
    /// Donut chart — labeled slices summing to 100%. Best for
    /// low-cardinality breakdowns (status distribution, top N
    /// regions, mode share) where a bar chart's axes are
    /// overkill. 3-6 slices reads cleanly; past that switch
    /// to a bar.
    Donut,
    /// Radial gauge — one or more 0–100% tracks rendered as
    /// concentric arcs (ApexCharts `radialBar`). The everyday
    /// "progress toward a goal" tile: quota attainment, capacity
    /// used, completion rate, SLA. A single track reads as one big
    /// ring with the percent in the centre; 2–4 tracks compare
    /// related ratios (e.g. per-plan conversion).
    Radial,
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
            WidgetKind::Donut => "donut",
            WidgetKind::Radial => "radial",
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

/// One slice of a donut chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DonutSlice {
    pub label: String,
    pub value: f64,
    /// Optional explicit color (CSS hex / rgb / token name).
    /// `None` falls back to the chart's default palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Donut chart payload — categorical breakdown summing to 100%.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DonutPayload {
    pub slices: Vec<DonutSlice>,
}

impl DonutPayload {
    pub fn new(slices: Vec<DonutSlice>) -> Self {
        Self { slices }
    }

    /// Build slices from `(label, value)` tuples; the chart
    /// picks colors from its default palette.
    pub fn from_pairs<L: Into<String>>(pairs: impl IntoIterator<Item = (L, f64)>) -> Self {
        Self::new(
            pairs
                .into_iter()
                .map(|(label, value)| DonutSlice {
                    label: label.into(),
                    value,
                    color: None,
                })
                .collect(),
        )
    }
}

/// One arc of a [`RadialPayload`] gauge — a labeled 0–100% value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadialTrack {
    pub label: String,
    /// Percent in `[0, 100]`. The `RadialPayload` constructors clamp
    /// this so the arc never overruns the ring.
    pub value: f64,
    /// Optional explicit arc color (CSS hex / rgb / token name).
    /// `None` falls back to the chart's default palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Radial gauge payload — one or more 0–100% tracks rendered as
/// concentric arcs (ApexCharts `radialBar`). Use for "progress toward
/// a goal" metrics: quota attainment, capacity used, completion rate.
///
/// ```ignore
/// // One ring: 73% of the monthly sales goal.
/// RadialPayload::goal("Monthly goal", sales, target)
/// // Compare conversion across plans.
/// RadialPayload::from_pairs([("Free", 8.0), ("Pro", 21.5), ("Team", 34.0)])
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadialPayload {
    pub tracks: Vec<RadialTrack>,
}

impl RadialPayload {
    /// New payload from explicit tracks; each `value` is clamped to
    /// `[0, 100]` (non-finite -> 0).
    pub fn new(tracks: Vec<RadialTrack>) -> Self {
        Self {
            tracks: tracks
                .into_iter()
                .map(|t| RadialTrack {
                    value: clamp_percent(t.value),
                    ..t
                })
                .collect(),
        }
    }

    /// A single-track gauge — the common case (one big ring with the
    /// percent in the centre).
    pub fn single(label: impl Into<String>, percent: f64) -> Self {
        Self::new(vec![RadialTrack {
            label: label.into(),
            value: percent,
            color: None,
        }])
    }

    /// A single-track gauge whose percent is `current / target * 100`
    /// — the literal "progress toward a goal" shape. A non-positive
    /// `target` yields 0% (nothing to measure against).
    pub fn goal(label: impl Into<String>, current: f64, target: f64) -> Self {
        let pct = if target > 0.0 {
            current / target * 100.0
        } else {
            0.0
        };
        Self::single(label, pct)
    }

    /// Build tracks from `(label, percent)` tuples; the chart picks
    /// colors from its default palette. Each percent is clamped.
    pub fn from_pairs<L: Into<String>>(pairs: impl IntoIterator<Item = (L, f64)>) -> Self {
        Self::new(
            pairs
                .into_iter()
                .map(|(label, value)| RadialTrack {
                    label: label.into(),
                    value,
                    color: None,
                })
                .collect(),
        )
    }
}

/// Clamp a percentage into `[0, 100]`; non-finite -> 0.
fn clamp_percent(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
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
    /// Optional "View all →" link in the widget header. Populated
    /// via [`Self::view_all_for`] (auto-resolves the admin URL
    /// from a `Model` type) or set explicitly when the target
    /// isn't a managed admin model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_all_url: Option<String>,
}

impl TablePayload {
    /// New payload from columns + rows; no `view_all` link.
    pub fn new(columns: Vec<TableColumn>, rows: Vec<serde_json::Value>) -> Self {
        Self {
            columns,
            rows,
            view_all_url: None,
        }
    }

    /// Auto-resolve the "View all" link from a `Model` type — the
    /// admin's changelist URL for that table. Mirrors the pattern
    /// used by `models![T, U, V]`: rename the struct's
    /// `#[umbra(table = "...")]` and the link follows automatically.
    ///
    /// ```rust,ignore
    /// WidgetPayload::Table(
    ///     TablePayload::new(columns, rows)
    ///         .view_all_for::<Order>()
    /// )
    /// // → "View all →" links to {admin_base}/order/
    /// ```
    pub fn view_all_for<T: umbra::orm::Model>(mut self) -> Self {
        self.view_all_url = Some(format!(
            "{}/{}/",
            crate::branding::current().base_path,
            T::TABLE,
        ));
        self
    }

    /// Explicit URL override — use when the link target isn't a
    /// managed admin model (an external dashboard, a custom route).
    pub fn view_all_url(mut self, url: impl Into<String>) -> Self {
        self.view_all_url = Some(url.into());
        self
    }
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
    /// Optional "View all →" link in the widget header. Same
    /// shape as [`TablePayload::view_all_url`] — auto-resolve
    /// from a `Model` via [`Self::view_all_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_all_url: Option<String>,
}

impl FeedPayload {
    /// New payload from items; no `view_all` link.
    pub fn new(items: Vec<FeedItem>) -> Self {
        Self {
            items,
            view_all_url: None,
        }
    }

    /// Auto-resolve the "View all" link from a `Model` type. The
    /// recent-signups feed for instance:
    ///
    /// ```rust,ignore
    /// WidgetPayload::Feed(
    ///     FeedPayload::new(items).view_all_for::<AuthUser>()
    /// )
    /// // → "View all →" links to {admin_base}/auth_user/
    /// ```
    pub fn view_all_for<T: umbra::orm::Model>(mut self) -> Self {
        self.view_all_url = Some(format!(
            "{}/{}/",
            crate::branding::current().base_path,
            T::TABLE,
        ));
        self
    }

    pub fn view_all_url(mut self, url: impl Into<String>) -> Self {
        self.view_all_url = Some(url.into());
        self
    }
}

/// Union of all widget payloads. The JSON discriminant is the variant name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WidgetPayload {
    Kpi(KpiPayload),
    Card(CardPayload),
    Line(LinePayload),
    Bar(BarPayload),
    Donut(DonutPayload),
    Radial(RadialPayload),
    Table(TablePayload),
    Feed(FeedPayload),
}

// =========================================================================
// WidgetDataFn
// =========================================================================

/// Per-request parameters a widget's data closure can read.
/// Sourced from the query string on
/// `GET /admin/api/dashboard/widgets/<key>/data?<params>`.
///
/// Defaults are all `None` — closures that don't care can use
/// `WidgetDataFn::new(|user| ...)` and ignore params entirely.
/// Closures that DO care use `WidgetDataFn::with_params` and
/// branch on `params.period` / `params.start` / `params.end`.
#[derive(Debug, Clone, Default)]
pub struct WidgetParams {
    /// Period preset like `"7d"`, `"30d"`, `"90d"`. The
    /// rendering side emits chips that pass this through.
    pub period: Option<String>,
    /// Explicit ISO start date (`YYYY-MM-DD`) — overrides
    /// `period` when both are present.
    pub start: Option<String>,
    /// Explicit ISO end date (`YYYY-MM-DD`).
    pub end: Option<String>,
    /// Catch-all for any other widget-specific query params
    /// — `?model=order` for a future per-model filter, etc.
    /// Closures read by `params.raw.get("...")`.
    pub raw: std::collections::HashMap<String, String>,
}

impl WidgetParams {
    /// Build from a `?key=value&...` query string. Recognised
    /// keys (`period`, `start`, `end`) populate the typed
    /// fields; the rest land in `raw`.
    pub fn from_query<S: AsRef<str>>(query: S) -> Self {
        let mut out = Self::default();
        for pair in query.as_ref().split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let value = urlencoding_decode(v);
            match k {
                "period" => out.period = Some(value),
                "start" => out.start = Some(value),
                "end" => out.end = Some(value),
                _ => {
                    out.raw.insert(k.to_string(), value);
                }
            }
        }
        out
    }

    /// Number of days the `period` preset represents. `"7d"`
    /// → 7, `"30d"` → 30, `"90d"` → 90. None for unrecognised /
    /// missing values so callers fall back to a default.
    pub fn period_days(&self) -> Option<i64> {
        let p = self.period.as_deref()?;
        let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }
}

/// Minimal `%XX` → byte decoder; avoids pulling a query-string
/// crate just for the four chars we need (`+` → space, `%2F` → `/`,
/// etc.). Anything malformed passes through unchanged.
fn urlencoding_decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push(char::from((h as u8) * 16 + l as u8));
                    i += 3;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

pub(crate) type DataFuture = Pin<Box<dyn Future<Output = WidgetPayload> + Send + 'static>>;
pub(crate) type DataFnInner =
    Arc<dyn Fn(AuthUser, WidgetParams) -> DataFuture + Send + Sync + 'static>;

/// Wrapper around the async data closure. Build via
/// [`WidgetDataFn::new`] (closure ignores per-request params) or
/// [`WidgetDataFn::with_params`] (closure reads `WidgetParams` to
/// honour period / date-range filters from the request URL).
#[derive(Clone)]
pub struct WidgetDataFn(pub(crate) DataFnInner);

impl WidgetDataFn {
    /// Create from any `async fn(AuthUser) -> WidgetPayload` —
    /// per-request params are dropped on the floor. Use when the
    /// widget renders the same thing regardless of UI controls
    /// (KPI counts, registry sizes, etc.).
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(AuthUser) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WidgetPayload> + Send + 'static,
    {
        Self(Arc::new(move |user, _params| Box::pin(f(user))))
    }

    /// Create from `async fn(AuthUser, WidgetParams) ->
    /// WidgetPayload`. Use for filterable widgets — the line
    /// chart reads `params.period` to switch between 7d / 30d /
    /// 90d views, a future table widget might read
    /// `params.raw.get("status")` for status filtering, etc.
    pub fn with_params<F, Fut>(f: F) -> Self
    where
        F: Fn(AuthUser, WidgetParams) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = WidgetPayload> + Send + 'static,
    {
        Self(Arc::new(move |user, params| Box::pin(f(user, params))))
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
    /// Default period preset used by line/bar/etc. widgets that
    /// carry a period-chip strip — `"7d"`, `"30d"`, `"90d"`. When
    /// `Some`, the handler pre-fills `WidgetParams.period` from
    /// this value on first load (no `?period=` in the URL), so
    /// the matching chip renders highlighted AND the data
    /// closure receives the same period via `params.period_days()`.
    /// `None` falls back to whatever the template / data closure
    /// chooses as its fallback.
    pub default_period: Option<&'static str>,
}

impl Widget {
    /// Override the default grid span. Lets a caller resize a
    /// builtin (or any pre-built widget) at registration time
    /// without having to re-construct the whole struct literal:
    ///
    /// ```rust,ignore
    /// .register_widget(builtin_total_models_widget().with_span(6, 2))
    /// .register_widget(builtin_recent_users_widget().with_span(6, 2))
    /// ```
    ///
    /// `cols` is clamped at the 12-col grid; `rows` is whatever
    /// the dashboard's `auto-rows-[...]` accepts (1 = 120px).
    pub fn with_span(mut self, cols: u8, rows: u8) -> Self {
        self.default_span = Span { cols, rows };
        self
    }

    /// Pre-select a period chip on the widget — `"7d"`, `"30d"`,
    /// `"90d"`. On first load (no `?period=` in the URL), the
    /// handler stamps this into `WidgetParams.period` before
    /// calling the data closure, so the chip strip highlights
    /// the right preset AND the data fn computes the right
    /// window. Override on a per-request basis happens via the
    /// chip clicks (which send their own `?period=` query).
    ///
    /// ```ignore
    /// shop_daily_sales_chart().with_default_period("7d")
    /// // → first paint shows 7d highlighted, 7 days of data;
    /// //   clicking "30d" hands control to the URL state.
    /// ```
    pub fn with_default_period(mut self, period: &'static str) -> Self {
        self.default_period = Some(period);
        self
    }
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

// =========================================================================
// Sections — grouped widgets
// =========================================================================

/// A named group of widgets on the dashboard. Each section renders
/// as its own heading + (optional) subtitle + widget grid, so a
/// dashboard with 20 widgets reads as themed clusters rather than
/// one mega-grid.
///
/// Build with the chainable API:
///
/// ```rust,ignore
/// use umbra_admin::WidgetSection;
///
/// let sales = WidgetSection::new("Sales overview")
///     .subtitle("Daily KPIs across the storefront")
///     .widget(shop_total_sales_widget())
///     .widget(shop_orders_widget())
///     .widget(shop_avg_order_value_widget());
///
/// AdminPlugin::default().dashboard_section(sales);
/// ```
///
/// Register multiple sections by chaining `.dashboard_section(...)`.
/// Widgets registered via the legacy `.register_widget(...)` end up
/// in an implicit final section titled "Widgets" — so existing apps
/// keep working without code changes.
#[derive(Debug, Clone)]
pub struct WidgetSection {
    /// Heading shown above the section (e.g. "Sales overview").
    pub title: String,
    /// Optional descriptive line under the title — keep it short,
    /// it's not a paragraph.
    pub subtitle: Option<String>,
    /// Widgets in this section, rendered in registration order.
    pub widgets: Vec<Widget>,
}

impl WidgetSection {
    /// New empty section with just a title.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            widgets: Vec::new(),
        }
    }

    /// Add a one-line subtitle under the heading.
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Append one widget to the section.
    pub fn widget(mut self, w: Widget) -> Self {
        self.widgets.push(w);
        self
    }

    /// Append many widgets at once (handy for splatting a Vec).
    pub fn widgets(mut self, ws: impl IntoIterator<Item = Widget>) -> Self {
        self.widgets.extend(ws);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_kind_serializes_as_radial() {
        assert_eq!(WidgetKind::Radial.as_str(), "radial");
    }

    #[test]
    fn radial_single_builds_one_track() {
        let p = RadialPayload::single("Monthly goal", 73.0);
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].label, "Monthly goal");
        assert_eq!(p.tracks[0].value, 73.0);
        assert!(p.tracks[0].color.is_none());
    }

    #[test]
    fn radial_clamps_out_of_range_and_non_finite_percents() {
        // Over 100, under 0, and non-finite all clamp into [0, 100].
        assert_eq!(RadialPayload::single("over", 150.0).tracks[0].value, 100.0);
        assert_eq!(RadialPayload::single("under", -20.0).tracks[0].value, 0.0);
        // Non-finite (NaN, ±∞) is meaningless as a percent -> 0.
        assert_eq!(RadialPayload::single("nan", f64::NAN).tracks[0].value, 0.0);
        assert_eq!(
            RadialPayload::single("inf", f64::INFINITY).tracks[0].value,
            0.0,
        );
    }

    #[test]
    fn radial_goal_is_current_over_target() {
        assert_eq!(RadialPayload::goal("g", 73.0, 100.0).tracks[0].value, 73.0);
        // current > target clamps to 100 (overachieved, full ring).
        assert_eq!(
            RadialPayload::goal("g", 120.0, 100.0).tracks[0].value,
            100.0
        );
        // A non-positive target has nothing to measure against -> 0%.
        assert_eq!(RadialPayload::goal("g", 5.0, 0.0).tracks[0].value, 0.0);
    }

    #[test]
    fn radial_from_pairs_keeps_order_and_clamps() {
        let p = RadialPayload::from_pairs([("Free", 8.0), ("Pro", 150.0), ("Team", 34.0)]);
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.tracks[0].label, "Free");
        assert_eq!(p.tracks[1].value, 100.0); // clamped
        assert_eq!(p.tracks[2].label, "Team");
    }

    #[test]
    fn radial_payload_serializes_with_kind_tag() {
        let payload = WidgetPayload::Radial(RadialPayload::single("Quota", 42.0));
        let json = serde_json::to_value(&payload).expect("serialize");
        assert_eq!(json["kind"], "radial");
        assert_eq!(json["tracks"][0]["label"], "Quota");
        assert_eq!(json["tracks"][0]["value"], 42.0);
        // No explicit color -> the field is skipped entirely.
        assert!(json["tracks"][0].get("color").is_none());
    }
}
