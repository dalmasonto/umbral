//! Template-rendered list-view pagination — a `Paginator`/`Page` pair with
//! Django `django.core.paginator` parity.
//!
//! This is the page-of-rows helper for **server-rendered (Jinja) list
//! views**, distinct from REST's JSON pagination. It lives in `umbra-core`
//! (an ORM-adjacent core utility, exactly like Django's
//! `django.core.paginator`) so any handler can paginate a [`QuerySet`]
//! without registering a plugin:
//!
//! ```rust,ignore
//! let paginator = Paginator::new(Post::objects().order_by(post::ID.asc()), 10);
//! let page = paginator.page(n).await?;          // strict: errors out of range
//! // or paginator.page_clamped(n).await — forgiving: clamps to [1, num_pages]
//! render("posts/list.html", context! { page => page.context(), base_query })
//! ```
//!
//! The [`Paginator`] holds the queryset and counts once + slices per page
//! via the queryset's *by-value* `limit`/`offset`/`count`/`fetch`. Because
//! `QuerySet<T>: Clone`, each terminal operates on a fresh clone, so the
//! paginator never consumes the queryset and can serve many pages.
//!
//! For the nav markup, [`Page::elided_page_range`] produces the windowed
//! `1 … 4 5 [6] 7 8 … 20` shape (mirroring the admin's prior-art elision),
//! and [`Page::context`] yields a [`PageContext`] that serializes straight
//! into a template so `{% include "_pagination.html" %}` renders the nav
//! from `{{ page }}`.

use std::fmt;

use serde::Serialize;

use crate::orm::queryset::QuerySet;
use crate::orm::{HydrateRelated, Model};

/// Error raised when a requested page number is invalid for the paginator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaginationError {
    /// The page number is `< 1` or `> num_pages` (Django's `InvalidPage`).
    InvalidPage {
        /// The page number that was requested.
        requested: i64,
        /// The highest valid page number for this paginator.
        num_pages: i64,
    },
    /// A database error occurred while counting or fetching a slice.
    Db(String),
}

/// Alias matching Django's `EmptyPage`/`PageNotAnInteger` umbrella name so
/// call-sites and docs can refer to a `PageError`.
pub type PageError = PaginationError;

impl fmt::Display for PaginationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaginationError::InvalidPage {
                requested,
                num_pages,
            } => write!(
                f,
                "invalid page number {requested}: valid pages are 1..={num_pages}"
            ),
            PaginationError::Db(msg) => write!(f, "pagination query failed: {msg}"),
        }
    }
}

impl std::error::Error for PaginationError {}

impl From<sqlx::Error> for PaginationError {
    fn from(e: sqlx::Error) -> Self {
        PaginationError::Db(e.to_string())
    }
}

/// Paginates a [`QuerySet`] into fixed-size pages.
///
/// Holds the queryset by value and clones it per terminal, so a single
/// paginator counts once and serves any number of [`Page`]s without
/// consuming the underlying query. `per_page` is clamped to `>= 1`.
#[derive(Debug, Clone)]
pub struct Paginator<T> {
    queryset: QuerySet<T>,
    per_page: usize,
}

impl<T> Paginator<T>
where
    T: Model
        + Clone
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + HydrateRelated,
{
    /// Build a paginator over `queryset`, `per_page` rows per page.
    ///
    /// `per_page` is clamped to a minimum of 1 (Django raises on 0; we take
    /// the forgiving route and treat 0 as 1 so a misconfigured page size
    /// never divides by zero).
    pub fn new(queryset: QuerySet<T>, per_page: usize) -> Self {
        Self {
            queryset,
            per_page: per_page.max(1),
        }
    }

    /// Rows per page (always `>= 1`).
    pub fn per_page(&self) -> usize {
        self.per_page
    }

    /// Total number of rows across every page.
    pub async fn count(&self) -> Result<i64, PaginationError> {
        Ok(self.queryset.clone().count().await?)
    }

    /// Number of pages. Always `>= 1` (an empty queryset still has one
    /// empty page — Django's behavior).
    pub async fn num_pages(&self) -> Result<i64, PaginationError> {
        let count = self.count().await?;
        Ok(num_pages_for(count, self.per_page))
    }

    /// Fetch the slice for `number`, **erroring** on an out-of-range page
    /// (Django's strict `Paginator.page`). Page numbers are 1-based.
    pub async fn page(&self, number: i64) -> Result<Page<T>, PaginationError> {
        let count = self.count().await?;
        let num_pages = num_pages_for(count, self.per_page);
        if number < 1 || number > num_pages {
            return Err(PaginationError::InvalidPage {
                requested: number,
                num_pages,
            });
        }
        self.build_page(number, count, num_pages).await
    }

    /// Fetch the slice for `number`, **clamping** out-of-range requests into
    /// `[1, num_pages]` (the forgiving variant — handy when `?page=N` comes
    /// straight from an untrusted querystring).
    pub async fn page_clamped(&self, number: i64) -> Result<Page<T>, PaginationError> {
        let count = self.count().await?;
        let num_pages = num_pages_for(count, self.per_page);
        let number = number.clamp(1, num_pages);
        self.build_page(number, count, num_pages).await
    }

    /// Shared slice fetch for [`Self::page`]/[`Self::page_clamped`]. `number`
    /// is assumed already validated/clamped into `[1, num_pages]`.
    async fn build_page(
        &self,
        number: i64,
        count: i64,
        num_pages: i64,
    ) -> Result<Page<T>, PaginationError> {
        let per_page = self.per_page as u64;
        let offset = (number - 1) as u64 * per_page;
        let object_list = self
            .queryset
            .clone()
            .limit(per_page)
            .offset(offset)
            .fetch()
            .await?;
        Ok(Page {
            object_list,
            number,
            per_page: self.per_page,
            total_count: count,
            num_pages,
        })
    }
}

/// `ceil(count / per_page)`, clamped to a minimum of 1 even for an empty
/// queryset (Django: an empty paginator reports `num_pages == 1`).
fn num_pages_for(count: i64, per_page: usize) -> i64 {
    if count <= 0 {
        return 1;
    }
    let per_page = per_page.max(1) as i64;
    // Manual ceil: `i64::div_ceil` is unstable on this toolchain. `count` and
    // `per_page` are both `> 0` here, so `(count + per_page - 1) / per_page`
    // is the standard non-overflowing ceil for positive operands.
    (count + per_page - 1) / per_page
}

/// A single page of paginated rows.
///
/// All the page-relative helpers (`has_next`, `start_index`, …) mirror
/// Django's `Page` semantics 1-for-1. [`Page::context`] derives the
/// serializable [`PageContext`] a template renders the nav from.
#[derive(Debug, Clone)]
pub struct Page<T> {
    /// The rows on this page.
    pub object_list: Vec<T>,
    /// This page's 1-based number.
    pub number: i64,
    /// Rows per page (the paginator's clamped `per_page`).
    pub per_page: usize,
    /// Total rows across all pages.
    pub total_count: i64,
    /// Total page count (`>= 1`).
    pub num_pages: i64,
}

impl<T> Page<T> {
    /// Is there a page after this one?
    pub fn has_next(&self) -> bool {
        self.number < self.num_pages
    }

    /// Is there a page before this one?
    pub fn has_previous(&self) -> bool {
        self.number > 1
    }

    /// Is there any other page besides this one?
    pub fn has_other_pages(&self) -> bool {
        self.has_next() || self.has_previous()
    }

    /// The next page number, or `None` on the last page.
    pub fn next_page_number(&self) -> Option<i64> {
        self.has_next().then(|| self.number + 1)
    }

    /// The previous page number, or `None` on the first page.
    pub fn previous_page_number(&self) -> Option<i64> {
        self.has_previous().then(|| self.number - 1)
    }

    /// 1-based index of this page's first row within the full result set.
    /// Returns 0 when the page is empty (Django returns 0 for an empty
    /// page).
    pub fn start_index(&self) -> i64 {
        if self.total_count == 0 {
            return 0;
        }
        (self.number - 1) * self.per_page as i64 + 1
    }

    /// 1-based index of this page's last row within the full result set.
    /// On the final page this is `total_count`; on an empty set it's 0.
    pub fn end_index(&self) -> i64 {
        if self.total_count == 0 {
            return 0;
        }
        (self.number * self.per_page as i64).min(self.total_count)
    }

    /// The windowed page range for nav rendering: `on_each_side` numbers
    /// either side of the current page, `on_ends` numbers pinned to each
    /// end, and [`PageItem::Ellipsis`] markers where the run is elided.
    ///
    /// Mirrors Django's `Paginator.get_elided_page_range`. For 20 pages on
    /// page 6 with `(on_each_side, on_ends) = (2, 1)` this yields
    /// `1 … 4 5 [6] 7 8 … 20`.
    pub fn elided_page_range(&self, on_each_side: i64, on_ends: i64) -> Vec<PageItem> {
        elided_range(self.number, self.num_pages, on_each_side, on_ends)
    }

    /// Derive the serializable template view of this page (the nav uses a
    /// default `(on_each_side, on_ends)` of `(3, 1)`, matching Django's
    /// default elided range).
    pub fn context(&self) -> PageContext {
        self.context_with(3, 1)
    }

    /// [`Self::context`] with an explicit elision window.
    pub fn context_with(&self, on_each_side: i64, on_ends: i64) -> PageContext {
        PageContext {
            number: self.number,
            num_pages: self.num_pages,
            total_count: self.total_count,
            per_page: self.per_page,
            has_next: self.has_next(),
            has_previous: self.has_previous(),
            next_page_number: self.next_page_number(),
            previous_page_number: self.previous_page_number(),
            start_index: self.start_index(),
            end_index: self.end_index(),
            page_range: self
                .elided_page_range(on_each_side, on_ends)
                .into_iter()
                .map(PageItemContext::from)
                .collect(),
        }
    }
}

/// One entry in a windowed page range: either a concrete page number or an
/// ellipsis gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageItem {
    /// A concrete, linkable page number.
    Number(i64),
    /// An elided run of pages, rendered as `…`.
    Ellipsis,
}

/// Build the windowed range `[1 .. on_ends] … [n-on_each_side .. n+on_each_side] … [last-on_ends .. last]`.
///
/// Free function so it's unit-testable without a `Page`/DB. Collapses an
/// ellipsis to nothing when the gap is a single missing page (Django shows
/// the number rather than a `…` that hides exactly one page).
fn elided_range(current: i64, num_pages: i64, on_each_side: i64, on_ends: i64) -> Vec<PageItem> {
    let on_each_side = on_each_side.max(0);
    let on_ends = on_ends.max(0);

    // Small enough to show every page: no elision.
    if num_pages <= (on_each_side + on_ends) * 2 + 1 {
        return (1..=num_pages).map(PageItem::Number).collect();
    }

    let mut items = Vec::new();

    // Left end + left ellipsis.
    let left_window_start = current - on_each_side;
    if left_window_start > on_ends + 1 {
        for p in 1..=on_ends {
            items.push(PageItem::Number(p));
        }
        // Only emit `…` if it actually hides >1 page; otherwise show the
        // lone page it would have hidden.
        if left_window_start > on_ends + 2 {
            items.push(PageItem::Ellipsis);
        } else {
            items.push(PageItem::Number(on_ends + 1));
        }
    } else {
        for p in 1..left_window_start.max(1) {
            items.push(PageItem::Number(p));
        }
    }

    // Central window around the current page.
    let window_start = left_window_start.max(1);
    let window_end = (current + on_each_side).min(num_pages);
    for p in window_start..=window_end {
        items.push(PageItem::Number(p));
    }

    // Right ellipsis + right end.
    let right_window_end = current + on_each_side;
    if right_window_end < num_pages - on_ends {
        if right_window_end < num_pages - on_ends - 1 {
            items.push(PageItem::Ellipsis);
        } else {
            items.push(PageItem::Number(num_pages - on_ends));
        }
        for p in (num_pages - on_ends + 1)..=num_pages {
            items.push(PageItem::Number(p));
        }
    } else {
        for p in (right_window_end + 1)..=num_pages {
            items.push(PageItem::Number(p));
        }
    }

    items
}

/// Serializable template view of a [`Page`].
///
/// A handler renders the nav by passing this (via [`Page::context`]) into a
/// template as `page`; the bundled `_pagination.html` partial reads exactly
/// these fields. `page_range` is the elided window as serializable items.
#[derive(Debug, Clone, Serialize)]
pub struct PageContext {
    /// 1-based current page number.
    pub number: i64,
    /// Total number of pages.
    pub num_pages: i64,
    /// Total rows across all pages.
    pub total_count: i64,
    /// Rows per page.
    pub per_page: usize,
    /// Whether a next page exists.
    pub has_next: bool,
    /// Whether a previous page exists.
    pub has_previous: bool,
    /// Next page number (null on the last page).
    pub next_page_number: Option<i64>,
    /// Previous page number (null on the first page).
    pub previous_page_number: Option<i64>,
    /// 1-based index of the first row on this page.
    pub start_index: i64,
    /// 1-based index of the last row on this page.
    pub end_index: i64,
    /// The elided nav window.
    pub page_range: Vec<PageItemContext>,
}

/// Serializable form of a [`PageItem`]: a number entry renders `{n}` (with
/// `ellipsis: false`); an ellipsis entry renders `{ellipsis: true}` (with
/// `n: null`). A template branches on `item.ellipsis`.
#[derive(Debug, Clone, Serialize)]
pub struct PageItemContext {
    /// The page number, or `null` for an ellipsis.
    pub n: Option<i64>,
    /// Whether this entry is an elided `…` gap.
    pub ellipsis: bool,
}

impl From<PageItem> for PageItemContext {
    fn from(item: PageItem) -> Self {
        match item {
            PageItem::Number(n) => PageItemContext {
                n: Some(n),
                ellipsis: false,
            },
            PageItem::Ellipsis => PageItemContext {
                n: None,
                ellipsis: true,
            },
        }
    }
}

/// Rebuild a querystring, replacing (or inserting) `key`'s value with
/// `value`, preserving every other parameter and their order.
///
/// The fiddly bit behind a pagination nav: a `?sort=name` filter has to
/// survive every `?page=N` link, so the partial writes
/// `{{ querystring_with(base_query, "page", item.n) }}`. The returned string
/// has no leading `?`; the template prepends one.
///
/// - An empty/`None`-ish `current_query` yields just `key=value`.
/// - The replaced/inserted key is value-encoded; existing untouched pairs
///   pass through verbatim (they were already encoded in the inbound URL).
pub fn querystring_with(current_query: &str, key: &str, value: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut replaced = false;

    for pair in current_query.trim_start_matches('?').split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (pair.to_string(), String::new()),
        };
        if k == key {
            pairs.push((k, encode_component(value)));
            replaced = true;
        } else {
            pairs.push((k, v));
        }
    }

    if !replaced {
        pairs.push((key.to_string(), encode_component(value)));
    }

    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Minimal percent-encoding for a querystring value: spaces and the handful
/// of delimiter characters that would otherwise break parsing. Keeps the
/// helper dependency-free; the common pagination values (`page` numbers,
/// `sort` column names) are already URL-safe.
fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '&' => out.push_str("%26"),
            '=' => out.push_str("%3D"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            '%' => out.push_str("%25"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nums(items: &[PageItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                PageItem::Number(n) => n.to_string(),
                PageItem::Ellipsis => "…".to_string(),
            })
            .collect()
    }

    #[test]
    fn num_pages_math() {
        assert_eq!(num_pages_for(23, 10), 3);
        assert_eq!(num_pages_for(20, 10), 2);
        assert_eq!(num_pages_for(21, 10), 3);
        // Empty set: still one page (Django behavior).
        assert_eq!(num_pages_for(0, 10), 1);
        assert_eq!(num_pages_for(-5, 10), 1);
        // per_page clamps to >= 1.
        assert_eq!(num_pages_for(5, 0), 5);
    }

    fn page_of(number: i64, per_page: usize, total: i64, num_pages: i64) -> Page<()> {
        Page {
            object_list: Vec::new(),
            number,
            per_page,
            total_count: total,
            num_pages,
        }
    }

    #[test]
    fn start_end_index_semantics() {
        // 23 rows, 10 per page, 3 pages.
        let p1 = page_of(1, 10, 23, 3);
        assert_eq!(p1.start_index(), 1);
        assert_eq!(p1.end_index(), 10);
        assert!(!p1.has_previous());
        assert!(p1.has_next());
        assert_eq!(p1.next_page_number(), Some(2));
        assert_eq!(p1.previous_page_number(), None);

        let p3 = page_of(3, 10, 23, 3);
        assert_eq!(p3.start_index(), 21);
        assert_eq!(p3.end_index(), 23);
        assert!(p3.has_previous());
        assert!(!p3.has_next());
        assert_eq!(p3.next_page_number(), None);
        assert_eq!(p3.previous_page_number(), Some(2));
    }

    #[test]
    fn empty_page_indices_are_zero() {
        let p = page_of(1, 10, 0, 1);
        assert_eq!(p.start_index(), 0);
        assert_eq!(p.end_index(), 0);
        assert!(!p.has_other_pages());
    }

    #[test]
    fn elided_range_shows_all_when_small() {
        let r = elided_range(2, 5, 2, 1);
        assert_eq!(nums(&r), vec!["1", "2", "3", "4", "5"]);
    }

    #[test]
    fn elided_range_windows_middle_with_both_ellipses() {
        // 20 pages, page 6, on_each_side=2, on_ends=1 -> 1 … 4 5 6 7 8 … 20
        let r = elided_range(6, 20, 2, 1);
        assert_eq!(
            nums(&r),
            vec!["1", "…", "4", "5", "6", "7", "8", "…", "20"]
        );
        assert!(r.contains(&PageItem::Ellipsis));
    }

    #[test]
    fn elided_range_near_start_only_right_ellipsis() {
        // page 2 of 20: left side fully shown, right elided.
        let r = elided_range(2, 20, 2, 1);
        assert_eq!(nums(&r), vec!["1", "2", "3", "4", "…", "20"]);
    }

    #[test]
    fn elided_range_near_end_only_left_ellipsis() {
        let r = elided_range(19, 20, 2, 1);
        assert_eq!(nums(&r), vec!["1", "…", "17", "18", "19", "20"]);
    }

    #[test]
    fn querystring_replaces_page_preserving_others() {
        // Other params survive; page is replaced in place.
        assert_eq!(
            querystring_with("page=1&sort=name", "page", "3"),
            "page=3&sort=name"
        );
        // Inserted when absent.
        assert_eq!(querystring_with("sort=name", "page", "2"), "sort=name&page=2");
        // Empty current query -> just the pair.
        assert_eq!(querystring_with("", "page", "5"), "page=5");
        // Leading `?` tolerated.
        assert_eq!(querystring_with("?page=1", "page", "2"), "page=2");
        // Value with a space gets encoded.
        assert_eq!(
            querystring_with("page=1", "sort", "first name"),
            "page=1&sort=first%20name"
        );
    }
}
