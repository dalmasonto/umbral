//! Row marshalling — turn `ModelMeta` + dynamic column lists into
//! parameterized SQL, bind form values, and decode result rows into
//! `HashMap<String, String>` for the templates.
//!
//! The read-side queries (`count_rows_filtered`, `fetch_rows_paged`)
//! now go through [`umbra::orm::DynQuerySet`] — the runtime-typed
//! Manager that lives in `umbra-core`. The write-side functions
//! (`insert_row`, `update_row`, the SQLite-row decoder `column_to_string`,
//! the form-value binder `bind_form_value`, the typed-NULL binder
//! `bind_null`) still hand-build SQL because the ORM extension's
//! write path is the next pass.

use std::collections::HashMap;

use sqlx::SqlitePool;
use umbra::migrate::{Column, ModelMeta};
use umbra::orm::DynQuerySet;

use crate::AdminError;
use crate::config::AdminConfig;

/// COUNT(*) for one filtered changelist query. Returns the total so
/// the Pagination footer can compute total_pages.
///
/// Backed by [`DynQuerySet`] — the search / filter clause comes from
/// the same builder the row fetch uses, so the count and the page
/// agree on what "filtered" means.
pub(crate) async fn count_rows_filtered(
    _pool: &SqlitePool,
    model: &ModelMeta,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
) -> Result<usize, AdminError> {
    let mut qs = DynQuerySet::for_meta(model);
    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        qs = qs.search(&c.search_fields, term);
    }
    if let Some((field, value)) = active_filter {
        qs = qs.filter_eq_string(field, value);
    }
    let count = qs.count().await?;
    Ok(count as usize)
}

/// Fetch one page of rows for the changelist. Phase 2's paginated
/// counterpart to `fetch_rows_filtered`.
///
/// Backed by [`DynQuerySet`]. `order_clause` carries the same
/// pre-built ORDER BY string the legacy path used (single
/// `"col" ASC|DESC` or comma-joined multi-column); we parse it back
/// out into `(col, descending)` pairs and feed each to
/// `order_by_col` so the ORM owns the rendering.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_rows_paged(
    _pool: &SqlitePool,
    model: &ModelMeta,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filter: Option<(&str, &str)>,
    limit: usize,
    offset: usize,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let mut qs = DynQuerySet::for_meta(model).select_cols(display_cols);
    if let Some(term) = search_term
        && let Some(c) = cfg
        && !c.search_fields.is_empty()
    {
        qs = qs.search(&c.search_fields, term);
    }
    if let Some((field, value)) = active_filter {
        qs = qs.filter_eq_string(field, value);
    }
    for (col, desc) in parse_order_clause(order_clause) {
        qs = qs.order_by_col(&col, desc);
    }
    qs = qs.limit(limit as u64).offset(offset as u64);
    Ok(qs.fetch_as_strings().await?)
}

/// Parse the legacy `"col" ASC, "col2" DESC` ORDER BY string back into
/// `(column_name, descending)` pairs. Whitespace tolerant; segments
/// that don't parse are silently dropped.
fn parse_order_clause(clause: &str) -> Vec<(String, bool)> {
    if clause.trim().is_empty() {
        return Vec::new();
    }
    clause
        .split(',')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Format: `"col" ASC` or `"col" DESC`.
            let (col_part, dir_part) = trimmed.rsplit_once(' ')?;
            let col = col_part.trim().trim_matches('"');
            if col.is_empty() {
                return None;
            }
            let descending = dir_part.trim().eq_ignore_ascii_case("DESC");
            Some((col.to_string(), descending))
        })
        .collect()
}

/// Fetch a single row by primary key, projected over `display_cols`.
///
/// Used by the detail / edit / sheet handlers — "one row, every column
/// the caller asks for." Goes through [`DynQuerySet`]. The signature
/// keeps the legacy positional shape (`_pool`, `_order_clause`,
/// `_search_term`, `_cfg`, `_active_filter`) so callers stay
/// unchanged; only `where_pk` and `display_cols` are read.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fetch_rows_filtered(
    _pool: &SqlitePool,
    model: &ModelMeta,
    where_pk: Option<(&str, &str)>,
    display_cols: &[String],
    _order_clause: &str,
    _search_term: Option<&str>,
    _cfg: Option<&AdminConfig>,
    _active_filter: Option<(&str, &str)>,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let mut qs = DynQuerySet::for_meta(model).select_cols(display_cols);
    if let Some((col, val)) = where_pk {
        qs = qs.filter_eq_string(col, val).limit(1);
    } else {
        qs = qs.limit(200);
    }
    Ok(qs.fetch_as_strings().await?)
}

/// INSERT one form submission. Handles `password_field` (hash + confirm
/// check) before delegating to [`DynQuerySet::insert_form`] and
/// respects the merged readonly set (config + sensitive-column
/// defaults) so the server can't be tricked into writing fields the
/// form was supposed to skip.
pub(crate) async fn insert_row(
    _pool: &SqlitePool,
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    let form_owned: HashMap<String, String>;
    let form = if let Some(pw_col) = cfg.and_then(|c| c.password_field.as_deref()) {
        if let Some(plaintext) = form.get(pw_col).filter(|v| !v.is_empty()) {
            let confirm_key = format!("{pw_col}_confirm");
            let confirm = form.get(&confirm_key).map(|s| s.as_str()).unwrap_or("");
            if plaintext != confirm {
                return Err(AdminError::BadInput("Passwords do not match.".to_string()));
            }
            let hash = umbra_auth::hash_password(plaintext)
                .map_err(|e| AdminError::BadInput(format!("password hashing failed: {e}")))?;
            let mut owned = form.clone();
            owned.insert(pw_col.to_string(), hash);
            form_owned = owned;
            &form_owned
        } else {
            form
        }
    } else {
        form
    };

    let skip = readonly_set(model, cfg);
    DynQuerySet::for_meta(model)
        .insert_form(form, &skip)
        .await?;
    Ok(())
}

/// UPDATE one row identified by its PK. Same readonly enforcement as
/// `insert_row` — fields can't be smuggled back in via the form.
pub(crate) async fn update_row(
    _pool: &SqlitePool,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
) -> Result<(), AdminError> {
    let skip = readonly_set(model, cfg);
    DynQuerySet::for_meta(model)
        .filter_eq_string(&pk.name, pk_value)
        .update_form(form, &skip)
        .await?;
    Ok(())
}

/// Compute the readonly set (config-supplied + sensitive-column
/// defaults) for write paths so insert / update reject the same
/// columns the form-building layer hid.
fn readonly_set(model: &ModelMeta, cfg: Option<&AdminConfig>) -> Vec<String> {
    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    if let Some(c) = cfg {
        c.effective_readonly_fields(&all_col_names)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        all_col_names
            .iter()
            .filter(|n| crate::config::is_sensitive_column(n))
            .map(|s| s.to_string())
            .collect()
    }
}
