//! Pagination — the envelope shape REST list endpoints return rows in.
//!
//! The trait [`Pagination`] has two halves:
//!
//! - [`Pagination::extract_request`] reads URL query parameters
//!   (`?page=2`, `?limit=20&offset=40`, etc.) and produces a
//!   [`PageRequest`] the handler uses to build the actual database
//!   query (LIMIT / OFFSET).
//! - [`Pagination::paginate`] takes the fetched rows + the total
//!   matching-row count + the original `PageRequest` and returns the
//!   `serde_json::Value` the client sees.
//!
//! Three built-ins ship:
//!
//! - [`NoPagination`] — the default. Returns every row in a
//!   `{ results: [...], count: N }` envelope. Same shape as before
//!   pagination existed; doesn't run a separate COUNT query.
//! - [`PageNumberPagination`] — page-number shape.
//!   `?page=N&page_size=M`. Envelope carries `count`,
//!   `total_pages`, `current_page`, `page_size`, `next`, `previous`,
//!   `results`.
//! - [`LimitOffsetPagination`] — REST classic. `?limit=N&offset=M`.
//!   Envelope carries `count`, `next`, `previous` (as offset deltas),
//!   `limit`, `offset`, `results`.
//!
//! Custom shapes implement the trait directly.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

/// What the handler should ask the database for: a window of rows.
///
/// Built by [`Pagination::extract_request`] from URL query
/// parameters; consumed by the list handler to add `LIMIT` / `OFFSET`
/// clauses, and echoed back to [`Pagination::paginate`] so the
/// envelope can carry page numbers / next-page links / etc.
#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    /// Maximum rows to return. `u64::MAX` means "no limit"
    /// — what `NoPagination` uses.
    pub limit: u64,
    /// Rows to skip from the start of the result set.
    pub offset: u64,
    /// For `PageNumberPagination` only. Lets `paginate` build
    /// next/previous page numbers without recomputing from
    /// limit/offset.
    pub page: Option<u64>,
}

impl PageRequest {
    /// `(u64::MAX, 0)` — the "give me everything" request used by
    /// [`NoPagination`] and as the safe default when query-param
    /// parsing fails.
    pub fn all() -> Self {
        Self {
            limit: u64::MAX,
            offset: 0,
            page: None,
        }
    }
}

/// Which query parameters the pagination backend reads from the request.
/// Used by `umbral-openapi` to emit the correct `parameters` entries on
/// list endpoints rather than always advertising `page`/`page_size`.
///
/// Custom [`Pagination`] implementors that don't override [`Pagination::style`]
/// return [`PaginationStyle::Custom`], which causes the OpenAPI plugin to emit
/// no pagination params (safe: we don't know what the custom backend reads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationStyle {
    /// No query params — every row is returned. Maps to [`NoPagination`].
    None,
    /// `?page=N&page_size=M`. Maps to [`PageNumberPagination`].
    PageNumber,
    /// `?limit=N&offset=M`. Maps to [`LimitOffsetPagination`].
    LimitOffset,
    /// A custom implementor; query params are unknown to the framework.
    Custom,
}

/// A JSON scalar a pagination envelope field / query param carries.
/// Framework-neutral so both codegen consumers (the OpenAPI spec and
/// the TypeScript client) can map it to their own type system —
/// `number`/`string`/`boolean` in TS, `integer`/`string`/`boolean` in
/// JSON Schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationScalar {
    /// A JSON string.
    String,
    /// A JSON number.
    Number,
    /// A JSON boolean.
    Boolean,
}

/// One field a custom paginator declares — an envelope key it returns,
/// or a query parameter it reads. Named so codegen can emit it typed.
#[derive(Debug, Clone)]
pub struct PaginationField {
    /// The wire name — the JSON key (`"next_cursor"`) or query param
    /// (`"cursor"`).
    pub name: String,
    /// The scalar type the value carries.
    pub ty: PaginationScalar,
    /// Whether the value can be `null` (envelope fields) / omitted.
    pub nullable: bool,
}

impl PaginationField {
    /// A non-nullable field of the given scalar.
    pub fn new(name: impl Into<String>, ty: PaginationScalar) -> Self {
        Self {
            name: name.into(),
            ty,
            nullable: false,
        }
    }

    /// A nullable field (envelope keys that can come back `null`, like a
    /// `next_cursor` at the end of the stream).
    pub fn nullable(name: impl Into<String>, ty: PaginationScalar) -> Self {
        Self {
            name: name.into(),
            ty,
            nullable: true,
        }
    }
}

/// A custom paginator's wire shape, declared to codegen. The framework
/// knows the shape of the three built-in styles from [`PaginationStyle`];
/// this is how a [`PaginationStyle::Custom`] implementor tells the OpenAPI
/// plugin and the generated TypeScript client what its envelope and query
/// params actually are, so they can be emitted *typed* instead of as an
/// opaque escape hatch.
///
/// `results: T[]` is implicit and always present; `envelope` lists the
/// keys *beyond* `results`. `params` lists the query parameters the
/// paginator reads (each becomes a typed builder method on the client's
/// query, e.g. `.cursor(...)`).
#[derive(Debug, Clone, Default)]
pub struct PaginationSchema {
    /// Envelope keys beyond `results` — e.g. `next_cursor`, `count`.
    pub envelope: Vec<PaginationField>,
    /// Query params the paginator reads — e.g. `cursor`, `page_size`.
    pub params: Vec<PaginationField>,
}

/// The pagination contract. Implementors are stored on the plugin as
/// `Arc<dyn Pagination>` so the list handler can dispatch through a
/// trait object — register custom shapes per-app at builder time.
///
/// Object-safe by design: methods take `&self` and return owned
/// values, no generics.
pub trait Pagination: Send + Sync + 'static {
    /// Parse the request's query parameters and return the
    /// `PageRequest` the database query should use. Implementors
    /// pick sensible defaults when params are missing or malformed —
    /// don't return errors here; the user shouldn't get a 400 just
    /// because they typo'd `?paeg=2`.
    fn extract_request(&self, params: &HashMap<String, String>) -> PageRequest;

    /// Wrap the fetched rows in the envelope the client sees.
    /// `total_rows` is the count of all rows that match the filter
    /// (with no LIMIT / OFFSET applied); used to compute total pages
    /// and to decide whether there's a "next" link. The handler skips
    /// the extra COUNT query when [`Self::needs_total`] returns false.
    fn paginate(&self, rows: Vec<Map<String, Value>>, total_rows: i64, req: &PageRequest) -> Value;

    /// Whether `paginate` needs `total_rows` to be a real count.
    /// Default is `true`. [`NoPagination`] overrides to `false` so
    /// the handler skips the extra `SELECT COUNT(*)` round-trip when
    /// there's no point — the envelope just embeds `rows.len()`.
    fn needs_total(&self) -> bool {
        true
    }

    /// Identifies which query parameters this backend reads so that
    /// `umbral-openapi` can emit the correct `parameters` entries on list
    /// endpoints. Override in custom implementations to advertise the
    /// right params (or return [`PaginationStyle::Custom`] to suppress
    /// the generated params block).
    ///
    /// Built-in impls each return the appropriate variant; the default
    /// here is [`PaginationStyle::Custom`] so an unaware custom impl
    /// doesn't accidentally advertise wrong parameters.
    fn style(&self) -> PaginationStyle {
        PaginationStyle::Custom
    }

    /// Declare the paginator's wire shape to codegen (the OpenAPI spec
    /// and the generated TypeScript client). Return `Some` from a
    /// [`PaginationStyle::Custom`] implementor to have its envelope keys
    /// and query params emitted *typed*; the default `None` leaves a
    /// custom paginator as an opaque shape (permissive envelope + a
    /// generic `.param(...)` escape hatch on the client).
    ///
    /// The three built-in styles return `None` here — their shape is
    /// already known to codegen from [`Self::style`], so they don't need
    /// to restate it.
    fn schema(&self) -> Option<PaginationSchema> {
        None
    }
}

// =========================================================================
// Built-in 1: NoPagination — the default.
// =========================================================================

/// No pagination. Returns every matching row.
///
/// Envelope:
///
/// ```json
/// {
///   "results": [ ... ],
///   "count": 1234
/// }
/// ```
///
/// `count` here is `rows.len()` — there's no separate COUNT query.
/// This is the pre-pagination v1 behaviour, kept as the default so
/// existing apps don't change envelope on upgrade.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPagination;

impl Pagination for NoPagination {
    fn extract_request(&self, _params: &HashMap<String, String>) -> PageRequest {
        PageRequest::all()
    }

    fn paginate(
        &self,
        rows: Vec<Map<String, Value>>,
        _total_rows: i64,
        _req: &PageRequest,
    ) -> Value {
        let count = rows.len();
        json!({
            "results": rows,
            "count": count,
        })
    }

    fn needs_total(&self) -> bool {
        false
    }

    fn style(&self) -> PaginationStyle {
        PaginationStyle::None
    }
}

// =========================================================================
// Built-in 2: PageNumberPagination — page-number shape.
// =========================================================================

/// `?page=N&page_size=M` page-number shape.
///
/// Envelope:
///
/// ```json
/// {
///   "count": 1234,
///   "total_pages": 25,
///   "current_page": 2,
///   "page_size": 50,
///   "next": 3,
///   "previous": 1,
///   "results": [ ... ]
/// }
/// ```
///
/// `next` / `previous` are page numbers (integers), or `null` at
/// the ends. They're not URLs — the client knows the path and adds
/// the page number itself. Returning page numbers keeps the envelope
/// host-agnostic (no need to know the canonical base URL).
#[derive(Debug, Clone, Copy)]
pub struct PageNumberPagination {
    /// Default rows-per-page when the client doesn't pass
    /// `?page_size=...`. 50 is the common default.
    pub page_size: u64,
    /// Hard ceiling on `?page_size=...`. Stops a curious client
    /// from asking for `?page_size=1000000` and DoSing the API.
    pub max_page_size: u64,
    /// Lets the client override the page size via
    /// `?page_size=...`. Default true; set false to lock everyone
    /// to `self.page_size`.
    pub allow_client_page_size: bool,
}

impl PageNumberPagination {
    /// New paginator with the given default `page_size`. Sets
    /// `max_page_size` to `page_size * 4` as a reasonable ceiling
    /// (override `.max_page_size` if you want different).
    pub fn new(page_size: u64) -> Self {
        let page_size = page_size.max(1);
        Self {
            page_size,
            max_page_size: page_size.saturating_mul(4),
            allow_client_page_size: true,
        }
    }

    /// Set the hard ceiling on client-requested page size.
    pub fn with_max_page_size(mut self, max: u64) -> Self {
        self.max_page_size = max.max(self.page_size);
        self
    }

    /// Disable client-side `?page_size=...` overrides.
    pub fn lock_page_size(mut self) -> Self {
        self.allow_client_page_size = false;
        self
    }
}

impl Default for PageNumberPagination {
    fn default() -> Self {
        Self::new(50)
    }
}

impl Pagination for PageNumberPagination {
    fn extract_request(&self, params: &HashMap<String, String>) -> PageRequest {
        let page = params
            .get("page")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&p| p > 0)
            .unwrap_or(1);
        let page_size = if self.allow_client_page_size {
            params
                .get("page_size")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|&n| n > 0)
                .map(|n| n.min(self.max_page_size))
                .unwrap_or(self.page_size)
        } else {
            self.page_size
        };
        PageRequest {
            limit: page_size,
            offset: page.saturating_sub(1).saturating_mul(page_size),
            page: Some(page),
        }
    }

    fn paginate(&self, rows: Vec<Map<String, Value>>, total_rows: i64, req: &PageRequest) -> Value {
        let current_page = req.page.unwrap_or(1);
        let page_size = req.limit.max(1);
        let total_pages = if total_rows <= 0 {
            0
        } else {
            ((total_rows as u64).div_ceil(page_size)).max(1)
        };
        let next: Value = if current_page < total_pages {
            json!(current_page + 1)
        } else {
            Value::Null
        };
        let previous: Value = if current_page > 1 {
            json!(current_page - 1)
        } else {
            Value::Null
        };
        json!({
            "count": total_rows,
            "total_pages": total_pages,
            "current_page": current_page,
            "page_size": page_size,
            "next": next,
            "previous": previous,
            "results": rows,
        })
    }

    fn style(&self) -> PaginationStyle {
        PaginationStyle::PageNumber
    }
}

// =========================================================================
// Built-in 3: LimitOffsetPagination — REST classic.
// =========================================================================

/// `?limit=N&offset=M` shape. The REST-API classic.
///
/// Envelope:
///
/// ```json
/// {
///   "count": 1234,
///   "limit": 50,
///   "offset": 100,
///   "next": 150,
///   "previous": 50,
///   "results": [ ... ]
/// }
/// ```
///
/// `next` / `previous` are offset values (or `null` at the ends), not
/// URLs. Same host-agnostic rationale as PageNumberPagination's
/// page-numbers approach.
#[derive(Debug, Clone, Copy)]
pub struct LimitOffsetPagination {
    /// Default `limit` when the client doesn't pass `?limit=...`.
    pub default_limit: u64,
    /// Hard ceiling on client-requested `?limit=...`.
    pub max_limit: u64,
}

impl LimitOffsetPagination {
    /// New paginator with the given default limit.
    pub fn new(default_limit: u64) -> Self {
        let default_limit = default_limit.max(1);
        Self {
            default_limit,
            max_limit: default_limit.saturating_mul(4),
        }
    }

    /// Override the hard ceiling on `?limit=...`.
    pub fn with_max_limit(mut self, max: u64) -> Self {
        self.max_limit = max.max(self.default_limit);
        self
    }
}

impl Default for LimitOffsetPagination {
    fn default() -> Self {
        Self::new(50)
    }
}

impl Pagination for LimitOffsetPagination {
    fn extract_request(&self, params: &HashMap<String, String>) -> PageRequest {
        let limit = params
            .get("limit")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&n| n > 0)
            .map(|n| n.min(self.max_limit))
            .unwrap_or(self.default_limit);
        let offset = params
            .get("offset")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        PageRequest {
            limit,
            offset,
            page: None,
        }
    }

    fn paginate(&self, rows: Vec<Map<String, Value>>, total_rows: i64, req: &PageRequest) -> Value {
        let next_offset = req.offset.saturating_add(req.limit);
        let next: Value = if (next_offset as i64) < total_rows {
            json!(next_offset)
        } else {
            Value::Null
        };
        let previous: Value = if req.offset > 0 {
            json!(req.offset.saturating_sub(req.limit))
        } else {
            Value::Null
        };
        json!({
            "count": total_rows,
            "limit": req.limit,
            "offset": req.offset,
            "next": next,
            "previous": previous,
            "results": rows,
        })
    }

    fn style(&self) -> PaginationStyle {
        PaginationStyle::LimitOffset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_from(items: &[(&str, &str)]) -> HashMap<String, String> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // -------- NoPagination --------

    #[test]
    fn no_pagination_does_not_need_total() {
        assert!(!NoPagination.needs_total());
    }

    #[test]
    fn no_pagination_extracts_all() {
        let req = NoPagination.extract_request(&params_from(&[]));
        assert_eq!(req.limit, u64::MAX);
        assert_eq!(req.offset, 0);
        assert!(req.page.is_none());
    }

    #[test]
    fn no_pagination_envelope_has_results_and_count() {
        let rows = vec![Map::new(), Map::new(), Map::new()];
        let v = NoPagination.paginate(rows, 0, &PageRequest::all());
        assert_eq!(v["count"], 3);
        assert_eq!(v["results"].as_array().unwrap().len(), 3);
    }

    // -------- PageNumberPagination --------

    #[test]
    fn page_number_extract_parses_page_and_page_size() {
        let p = PageNumberPagination::new(50);
        let req = p.extract_request(&params_from(&[("page", "3"), ("page_size", "10")]));
        assert_eq!(req.limit, 10);
        assert_eq!(req.offset, 20);
        assert_eq!(req.page, Some(3));
    }

    #[test]
    fn page_number_extract_defaults_when_missing() {
        let p = PageNumberPagination::new(25);
        let req = p.extract_request(&params_from(&[]));
        assert_eq!(req.limit, 25);
        assert_eq!(req.offset, 0);
        assert_eq!(req.page, Some(1));
    }

    #[test]
    fn page_number_extract_clamps_to_max() {
        let p = PageNumberPagination::new(50).with_max_page_size(100);
        let req = p.extract_request(&params_from(&[("page_size", "9999")]));
        assert_eq!(req.limit, 100);
    }

    #[test]
    fn page_number_extract_ignores_invalid_values() {
        let p = PageNumberPagination::new(50);
        let req = p.extract_request(&params_from(&[("page", "0"), ("page_size", "junk")]));
        assert_eq!(req.page, Some(1));
        assert_eq!(req.limit, 50);
    }

    #[test]
    fn page_number_extract_respects_lock_page_size() {
        let p = PageNumberPagination::new(50).lock_page_size();
        let req = p.extract_request(&params_from(&[("page_size", "9999")]));
        assert_eq!(req.limit, 50);
    }

    #[test]
    fn page_number_envelope_shape() {
        let p = PageNumberPagination::new(10);
        let rows = vec![Map::new(); 10];
        let req = PageRequest {
            limit: 10,
            offset: 10,
            page: Some(2),
        };
        let v = p.paginate(rows, 25, &req);
        assert_eq!(v["count"], 25);
        assert_eq!(v["total_pages"], 3);
        assert_eq!(v["current_page"], 2);
        assert_eq!(v["page_size"], 10);
        assert_eq!(v["next"], 3);
        assert_eq!(v["previous"], 1);
        assert_eq!(v["results"].as_array().unwrap().len(), 10);
    }

    #[test]
    fn page_number_envelope_null_links_at_ends() {
        let p = PageNumberPagination::new(10);
        // First page → no previous.
        let v = p.paginate(
            vec![Map::new(); 10],
            25,
            &PageRequest {
                limit: 10,
                offset: 0,
                page: Some(1),
            },
        );
        assert_eq!(v["previous"], Value::Null);
        assert_eq!(v["next"], 2);
        // Last page → no next.
        let v = p.paginate(
            vec![Map::new(); 5],
            25,
            &PageRequest {
                limit: 10,
                offset: 20,
                page: Some(3),
            },
        );
        assert_eq!(v["next"], Value::Null);
        assert_eq!(v["previous"], 2);
    }

    // -------- LimitOffsetPagination --------

    #[test]
    fn limit_offset_extract_parses_limit_and_offset() {
        let p = LimitOffsetPagination::new(50);
        let req = p.extract_request(&params_from(&[("limit", "20"), ("offset", "40")]));
        assert_eq!(req.limit, 20);
        assert_eq!(req.offset, 40);
        assert!(req.page.is_none());
    }

    #[test]
    fn limit_offset_extract_defaults_when_missing() {
        let p = LimitOffsetPagination::new(25);
        let req = p.extract_request(&params_from(&[]));
        assert_eq!(req.limit, 25);
        assert_eq!(req.offset, 0);
    }

    #[test]
    fn limit_offset_extract_clamps_limit() {
        let p = LimitOffsetPagination::new(50).with_max_limit(100);
        let req = p.extract_request(&params_from(&[("limit", "9999")]));
        assert_eq!(req.limit, 100);
    }

    #[test]
    fn limit_offset_envelope_shape() {
        let p = LimitOffsetPagination::new(20);
        let rows = vec![Map::new(); 20];
        let req = PageRequest {
            limit: 20,
            offset: 40,
            page: None,
        };
        let v = p.paginate(rows, 100, &req);
        assert_eq!(v["count"], 100);
        assert_eq!(v["limit"], 20);
        assert_eq!(v["offset"], 40);
        assert_eq!(v["next"], 60);
        assert_eq!(v["previous"], 20);
    }
}
