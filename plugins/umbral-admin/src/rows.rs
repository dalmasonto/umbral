//! Row marshalling — turn `ModelMeta` + dynamic column lists into
//! parameterized SQL via [`umbral::orm::DynQuerySet`] and decode result
//! rows into `HashMap<String, String>` for the templates.
//!
//! Read and write helpers both route through `DynQuerySet`, which picks
//! up the ambient backend-aware pool installed by `App::build()`. No
//! helper here takes a pool argument — that was a transitional shape
//! and would panic on Postgres because `umbral::db::pool()` is the
//! SQLite-only accessor.

use std::collections::HashMap;

use umbral::migrate::{Column, ModelMeta};
use umbral::orm::{DynQuerySet, SqlType};

use crate::AdminError;
use crate::config::AdminConfig;

/// Apply the active-filter slice to a [`DynQuerySet`], honouring the
/// admin's three filter shapes:
///   - column eq: `?filter_status=published` → `WHERE status = ?`
///   - column in: `?filter_brand=1,2,3` → `WHERE brand IN (?, ?, ?)`
///   - M2M any: `?filter_tags=1,2,3` → `WHERE id IN (SELECT parent_id
///     FROM <junction> WHERE child_id IN (?, ?, ?))`
///
/// The comma is the URL-level multi-select separator (the FK / choice
/// / M2M dialogs all serialise their pill arrays this way). M2M
/// dispatch is keyed by `model.m2m_relations` membership so a regular
/// scalar column with the same name (impossible by derive contract, but
/// cheap to be defensive about) would still fall through to the
/// column path.
fn apply_active_filters<'a>(
    mut qs: DynQuerySet<'a>,
    model: &ModelMeta,
    active_filters: &[(String, String)],
) -> DynQuerySet<'a> {
    for (field, value) in active_filters {
        let parts: Vec<String> = if value.contains(',') {
            value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            vec![value.clone()]
        };
        if model.m2m_relations.iter().any(|r| &r.field_name == field) {
            qs = qs.filter_m2m_contains_any(field, &parts);
        } else if parts.len() > 1 {
            qs = qs.filter_in_strings(field, &parts);
        } else {
            // Single value — same byte-identical path as the pre-
            // multi-select implementation. Keeps any per-type
            // affinity rules in filter_eq_string intact.
            qs = qs.filter_eq_string(field, value);
        }
    }
    qs
}

/// COUNT(*) for one filtered changelist query. Returns the total so
/// the Pagination footer can compute total_pages.
///
/// Backed by [`DynQuerySet`] — the search / filter clause comes from
/// the same builder the row fetch uses, so the count and the page
/// agree on what "filtered" means.
pub(crate) async fn count_rows_filtered(
    model: &ModelMeta,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filters: &[(String, String)],
    trash: bool,
) -> Result<usize, AdminError> {
    let mut qs = DynQuerySet::for_meta(model);
    // gaps2 #35: in the trash view, count only soft-deleted rows.
    // `only_deleted()` is a no-op on a non-soft-delete model.
    if trash {
        qs = qs.only_deleted();
    }
    if let Some(term) = search_term {
        // Pass cfg.search_fields when present; empty slice means
        // "search every searchable column" (DynQuerySet::search default).
        let restrict: &[String] = cfg.map(|c| c.search_fields.as_slice()).unwrap_or(&[]);
        qs = qs.search(restrict, term);
    }
    qs = apply_active_filters(qs, model, active_filters);
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
    model: &ModelMeta,
    display_cols: &[String],
    order_clause: &str,
    search_term: Option<&str>,
    cfg: Option<&AdminConfig>,
    active_filters: &[(String, String)],
    limit: usize,
    offset: usize,
    trash: bool,
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let mut qs = DynQuerySet::for_meta(model).select_cols(display_cols);
    // gaps2 #35: trash view shows only soft-deleted rows.
    if trash {
        qs = qs.only_deleted();
    }
    if let Some(term) = search_term {
        let restrict: &[String] = cfg.map(|c| c.search_fields.as_slice()).unwrap_or(&[]);
        qs = qs.search(restrict, term);
    }
    qs = apply_active_filters(qs, model, active_filters);
    for (col, desc) in parse_order_clause(order_clause) {
        qs = qs.order_by_col(&col, desc);
    }
    qs = qs.limit(limit as u64).offset(offset as u64);
    let mut rows = qs.fetch_as_strings().await?;
    apply_max_length_truncation(model, &mut rows);
    Ok(rows)
}

/// Walk every row and truncate cells whose column has a
/// `max_length > 0` hint. Appends an ellipsis (`…`) when truncation
/// happens so the user can see something was cut. UTF-8 safe: we step
/// by char count, not byte count.
fn apply_max_length_truncation(model: &ModelMeta, rows: &mut [HashMap<String, String>]) {
    for row in rows.iter_mut() {
        for col in &model.fields {
            if col.max_length == 0 {
                continue;
            }
            let limit = col.max_length as usize;
            if let Some(val) = row.get_mut(&col.name) {
                if val.chars().count() > limit {
                    let truncated: String = val.chars().take(limit).collect();
                    *val = format!("{truncated}…");
                }
            }
        }
    }
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
/// the caller asks for." Goes through [`DynQuerySet`].
pub(crate) async fn fetch_rows_filtered(
    model: &ModelMeta,
    where_pk: Option<(&str, &str)>,
    display_cols: &[String],
) -> Result<Vec<HashMap<String, String>>, AdminError> {
    let mut qs = DynQuerySet::for_meta(model).select_cols(display_cols);
    if let Some((col, val)) = where_pk {
        qs = qs.filter_eq_string(col, val).limit(1);
    } else {
        qs = qs.limit(200);
    }
    Ok(qs.fetch_as_strings().await?)
}

/// Transaction-aware INSERT of one form submission. Same password-hashing +
/// readonly enforcement, but runs the INSERT on the caller's open `tx`
/// so the parent write and its inline children commit (or roll back) as
/// one unit. Returns the new parent PK as a string.
pub(crate) async fn insert_row_in_tx(
    tx: &mut umbral::db::Transaction,
    model: &ModelMeta,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
    authorize_privileged: bool,
) -> Result<String, AdminError> {
    let form_owned: HashMap<String, String>;
    let form = if let Some(pw_col) = cfg.and_then(|c| c.password_field.as_deref()) {
        if let Some(plaintext) = form.get(pw_col).filter(|v| !v.is_empty()) {
            let confirm_key = format!("{pw_col}_confirm");
            let confirm = form.get(&confirm_key).map(|s| s.as_str()).unwrap_or("");
            if plaintext != confirm {
                return Err(AdminError::BadInput("Passwords do not match.".to_string()));
            }
            let hash = umbral_auth::hash_password_async(plaintext)
                .await
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
    let new_int_pk = allow_privileged_if(DynQuerySet::for_meta(model), model, authorize_privileged)
        .insert_form_in_tx(tx, form, &skip)
        .await?;
    let pk_col = model.fields.iter().find(|c| c.primary_key);
    Ok(match pk_col {
        Some(c) if !matches!(c.ty, SqlType::SmallInt | SqlType::Integer | SqlType::BigInt) => {
            form.get(&c.name).cloned().unwrap_or_default()
        }
        _ => new_int_pk.to_string(),
    })
}

/// Transaction-aware sibling of [`update_row`].
pub(crate) async fn update_row_in_tx(
    tx: &mut umbral::db::Transaction,
    model: &ModelMeta,
    pk: &Column,
    pk_value: &str,
    form: &HashMap<String, String>,
    cfg: Option<&AdminConfig>,
    authorize_privileged: bool,
) -> Result<(), AdminError> {
    let skip = readonly_set(model, cfg);
    allow_privileged_if(DynQuerySet::for_meta(model), model, authorize_privileged)
        .filter_eq_string(&pk.name, pk_value)
        .update_form_in_tx(tx, form, &skip)
        .await?;
    Ok(())
}

/// Opt a `DynQuerySet` into writing the model's `#[umbral(privileged)]` columns
/// when the acting admin user is authorized (a superuser). Without this the
/// write path default-denies those columns — the mass-assignment guard (audit_2
/// H3) that stops a plain staff user from self-promoting to `is_superuser` via
/// the admin form. A superuser legitimately manages staff/superuser status, so
/// their writes authorize every privileged column on the model.
fn allow_privileged_if<'a>(
    qs: DynQuerySet<'a>,
    model: &ModelMeta,
    authorize: bool,
) -> DynQuerySet<'a> {
    if !authorize {
        return qs;
    }
    let privileged: Vec<&str> = model
        .fields
        .iter()
        .filter(|c| c.privileged)
        .map(|c| c.name.as_str())
        .collect();
    if privileged.is_empty() {
        qs
    } else {
        qs.allow_privileged(&privileged)
    }
}

/// Compute the readonly set (config-supplied + sensitive-column
/// defaults) for write paths so insert / update reject the same
/// columns the form-building layer hid.
fn readonly_set(model: &ModelMeta, cfg: Option<&AdminConfig>) -> Vec<String> {
    let all_col_names: Vec<&str> = model.fields.iter().map(|c| c.name.as_str()).collect();
    let mut set: Vec<String> = if let Some(c) = cfg {
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
    };
    // Model-level write guards hold regardless of admin config. The
    // form-building layer only *hides* these columns; without echoing
    // them into the write skip-set a crafted POST writes them anyway —
    // classic mass assignment (gaps: WEB-2). `noform` blocks writes
    // entirely (documented); `noedit` renders disabled in the admin with
    // the promise it "can't change through the admin", so the admin must
    // enforce that too. REST keeps its own per-field rules (noedit is a
    // UX hint there by design; noform is stripped in insert/update_json).
    for col in &model.fields {
        if (col.noform || col.noedit) && !set.iter().any(|s| s == &col.name) {
            set.push(col.name.clone());
        }
    }
    set
}

#[cfg(test)]
mod readonly_set_tests {
    use super::*;

    /// Build a bare column with just the flags the readonly-set logic
    /// reads. Every other field is an inert default — the function under
    /// test only inspects `name`, `noform`, and `noedit`.
    fn col(name: &str, noform: bool, noedit: bool) -> Column {
        Column {
            name: name.into(),
            ty: SqlType::Text,
            primary_key: false,
            nullable: false,
            fk_target: None,
            noform,
            privileged: false,
            db_constraint: true,
            noedit,
            auto_user_add: false,
            auto_user: false,
            is_string_repr: false,
            max_length: 0,
            choices: vec![],
            choice_labels: vec![],
            default: String::new(),
            is_multichoice: false,
            unique: false,
            on_delete: umbral::orm::FkAction::NoAction,
            on_update: umbral::orm::FkAction::NoAction,
            index: false,
            auto_now_add: false,
            auto_now: false,
            trim: false,
            lowercase: false,
            case_insensitive: false,
            help: String::new(),
            example: String::new(),
            widget: None,
            supported_backends: Vec::new(),
            min: None,
            max: None,
            text_format: None,
            slug_from: None,
        }
    }

    fn meta(fields: Vec<Column>) -> ModelMeta {
        ModelMeta {
            name: "M".into(),
            table: "m".into(),
            fields,
            display: "M".into(),
            icon: "database".into(),
            database: None,
            singleton: false,
            unique_together: Vec::new(),
            indexes: Vec::new(),
            ordering: Vec::new(),
            m2m_relations: Vec::new(),
            soft_delete: false,
            audited: false,
            app_label: "app".into(),
        }
    }

    /// WEB-2 regression: a `noform` or `noedit` column must land in the
    /// admin write skip-set even with no `AdminConfig`, so a crafted POST
    /// can't mass-assign a field the form layer only *hid*. A plain
    /// editable column must NOT be in the set.
    #[test]
    fn noform_and_noedit_columns_are_always_readonly() {
        let m = meta(vec![
            col("title", false, false),   // editable
            col("locked", true, false),   // noform → hard write block
            col("username", false, true), // noedit → admin can't change
        ]);
        let skip = readonly_set(&m, None);
        assert!(
            skip.contains(&"locked".to_string()),
            "noform must be skipped: {skip:?}"
        );
        assert!(
            skip.contains(&"username".to_string()),
            "noedit must be skipped: {skip:?}"
        );
        assert!(
            !skip.contains(&"title".to_string()),
            "editable col must be writable: {skip:?}"
        );
    }
}
