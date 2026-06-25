//! Django-style admin **inlines**: edit a child model's reverse-FK rows
//! directly on the parent's change form (add new / edit existing /
//! delete), saved atomically with the parent.
//!
//! This module owns three things:
//!
//!   1. **Render data** ([`InlineView`] / [`InlineRow`]) — the structured
//!      list fed into `admin/form.html`. Built by [`build_inline_views`],
//!      which resolves each declared inline's child [`ModelMeta`], builds
//!      the child's editable [`FormField`]s (excluding the FK and PK),
//!      fetches existing children (on edit), and pads with `extra` blank
//!      rows.
//!   2. **Parsing** ([`parse_inline_rows`]) — pull a single inline's
//!      submitted rows out of the flat `(field, value)` body using the
//!      formset naming scheme `inline-<child_table>-<i>-<field>`.
//!   3. **Atomic save** ([`save_inlines_in_tx`]) — for each inline, walk
//!      its parsed rows and INSERT / UPDATE / DELETE each child through
//!      the transaction-aware `DynQuerySet::*_in_tx` terminals, so the
//!      whole parent + children save commits or rolls back as one unit.
//!
//! ## Formset naming scheme
//!
//! For an inline whose child table is `<name>`:
//!
//! ```text
//! inline-<name>-TOTAL          = number of rows rendered (management count)
//! inline-<name>-<i>-id         = child PK ("" for a new row)
//! inline-<name>-<i>-<field>    = a child field value
//! inline-<name>-<i>-DELETE     = present ("on") to delete an existing child
//! ```
//!
//! `<i>` runs `0..TOTAL`. A row with no `id` and every field blank is a
//! skipped "extra" row (no write). A row with an `id` + `DELETE` is
//! deleted; with an `id` and no `DELETE` is updated; with no `id` and at
//! least one non-blank field is inserted with its FK set to the parent.

use std::collections::HashMap;

use serde::Serialize;
use umbral::migrate::{Column, ModelMeta};
use umbral::orm::{DynQuerySet, SqlType};

use crate::config::{AdminConfig, InlineModel};
use crate::error::AdminError;
use crate::view::{FormField, form_fields_for};

/// One inline section ready for the template: its child table name, the
/// layout kind, the editable field descriptors, and the per-row values.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InlineView {
    /// Child table name — the `<name>` in the formset naming scheme.
    pub name: String,
    /// Human label (the child model's display name).
    pub label: String,
    /// `"tabular"` or `"stacked"`.
    pub kind: &'static str,
    /// Whether each row carries a DELETE checkbox.
    pub can_delete: bool,
    /// The editable child columns (FK + PK excluded). Used as the
    /// table header in tabular mode and the field list per row.
    pub fields: Vec<FormField>,
    /// Existing + blank rows, in render order.
    pub rows: Vec<InlineRow>,
    /// Management count = `rows.len()` (the `-TOTAL` hidden input).
    pub total: usize,
}

/// One child row on an inline: its PK (empty for a new/blank row) and a
/// per-field copy of the editable fields carrying this row's values.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct InlineRow {
    /// Child PK as a string. Empty = a new (blank or to-be-added) row.
    pub id: String,
    /// The editable fields, prefilled with this row's values. Mirrors
    /// `InlineView.fields` in shape so the template can render either.
    pub fields: Vec<FormField>,
}

/// Build the editable child fields for an inline: the same
/// [`form_fields_for`] list the standalone child form would render,
/// minus the FK column (set to the parent automatically) and any field
/// not on the inline's `list_display` (when one is configured).
fn child_form_fields(
    child: &ModelMeta,
    inline: &InlineModel,
    prefill: Option<&HashMap<String, String>>,
) -> Vec<FormField> {
    // form_fields_for already drops the PK, noform, auto_now/_add, and
    // the password field. We additionally drop the FK column and honour
    // the inline's readonly_fields + list_display whitelist.
    let mut fields = form_fields_for(child, prefill, None);
    fields.retain(|f| f.name != inline.fk_field);
    if !inline.list_display.is_empty() {
        fields.retain(|f| inline.list_display.iter().any(|d| d == &f.name));
    }
    for f in &mut fields {
        if inline.readonly_fields.iter().any(|r| r == &f.name) {
            f.readonly = true;
        }
    }
    fields
}

/// Validate that `fk_field` exists on the child and points at the
/// parent table. Returns the resolved FK [`Column`] on success, or `None`
/// (with a `tracing::warn!`) when the declaration is bogus — the caller
/// skips that inline rather than 500-ing.
fn validate_inline<'c>(parent: &ModelMeta, child: &'c ModelMeta, inline: &InlineModel) -> Option<&'c Column> {
    let fk = child.fields.iter().find(|c| c.name == inline.fk_field);
    let Some(fk) = fk else {
        tracing::warn!(
            parent = %parent.table,
            child = %child.table,
            fk_field = %inline.fk_field,
            "admin inline: declared fk_field is not a column on the child; skipping inline"
        );
        return None;
    };
    if !matches!(fk.ty, SqlType::ForeignKey) {
        tracing::warn!(
            parent = %parent.table,
            child = %child.table,
            fk_field = %inline.fk_field,
            "admin inline: fk_field is not a ForeignKey column; skipping inline"
        );
        return None;
    }
    // fk_target is the table the FK points at. Tolerate a None target
    // (older metadata) but warn on an explicit mismatch.
    if let Some(target) = &fk.fk_target {
        if target != &parent.table {
            tracing::warn!(
                parent = %parent.table,
                child = %child.table,
                fk_field = %inline.fk_field,
                fk_target = %target,
                "admin inline: fk_field points at a different table than the parent; skipping inline"
            );
            return None;
        }
    }
    Some(fk)
}

/// Build every inline section for a parent change form.
///
/// `parent_pk` is `Some(pk)` on the edit form (existing children get
/// fetched and prefilled) and `None` on the create form (only the blank
/// `extra` rows render — there's no parent id yet).
pub(crate) async fn build_inline_views(
    parent: &ModelMeta,
    parent_pk: Option<&str>,
    cfg: Option<&AdminConfig>,
) -> Result<Vec<InlineView>, AdminError> {
    let Some(cfg) = cfg else {
        return Ok(Vec::new());
    };
    if cfg.inlines.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for inline in &cfg.inlines {
        let Some((_, child)) = crate::discovery::find_model(&inline.model) else {
            tracing::warn!(
                parent = %parent.table,
                child = %inline.model,
                "admin inline: child table is not a registered model; skipping inline"
            );
            continue;
        };
        if validate_inline(parent, &child, inline).is_none() {
            continue;
        }

        // Column descriptors (shape only — no values) for the header /
        // blank-row template.
        let header_fields = child_form_fields(&child, inline, None);

        let mut rows: Vec<InlineRow> = Vec::new();

        // Existing children, one prefilled row each (edit form only).
        if let Some(pk) = parent_pk {
            let child_pk_col = child.fields.iter().find(|c| c.primary_key);
            let existing = DynQuerySet::for_meta(&child)
                .filter_eq_string(&inline.fk_field, pk)
                .fetch_as_strings()
                .await?;
            for row in existing {
                let id = child_pk_col
                    .and_then(|c| row.get(&c.name))
                    .cloned()
                    .unwrap_or_default();
                rows.push(InlineRow {
                    id,
                    fields: child_form_fields(&child, inline, Some(&row)),
                });
            }
        }

        // Blank "add another" rows.
        for _ in 0..inline.extra {
            rows.push(InlineRow {
                id: String::new(),
                fields: header_fields.clone(),
            });
        }

        let total = rows.len();
        out.push(InlineView {
            name: child.table.clone(),
            label: child.display.clone(),
            kind: inline.kind.as_str(),
            can_delete: inline.can_delete,
            fields: header_fields,
            rows,
            total,
        });
    }
    Ok(out)
}

/// Rebuild the inline render views from a *submitted* body (the
/// repopulate-on-error path) instead of from the DB. Keeps the user's
/// in-flight inline edits when the parent save fails and the form
/// re-renders.
pub(crate) fn build_inline_views_from_submitted(
    parent: &ModelMeta,
    cfg: Option<&AdminConfig>,
    pairs: &[(String, String)],
) -> Vec<InlineView> {
    let Some(cfg) = cfg else { return Vec::new() };
    let mut out = Vec::new();
    for inline in &cfg.inlines {
        let Some((_, child)) = crate::discovery::find_model(&inline.model) else {
            continue;
        };
        if validate_inline(parent, &child, inline).is_none() {
            continue;
        }
        let header_fields = child_form_fields(&child, inline, None);
        let parsed = parse_inline_rows(&child.table, pairs);
        let mut rows: Vec<InlineRow> = Vec::with_capacity(parsed.len());
        for p in &parsed {
            rows.push(InlineRow {
                id: p.id.clone(),
                fields: child_form_fields(&child, inline, Some(&p.values)),
            });
        }
        // Guarantee at least one blank row to add against, mirroring the
        // fresh-render `extra` padding.
        if rows.is_empty() {
            for _ in 0..inline.extra.max(1) {
                rows.push(InlineRow {
                    id: String::new(),
                    fields: header_fields.clone(),
                });
            }
        }
        let total = rows.len();
        out.push(InlineView {
            name: child.table.clone(),
            label: child.display.clone(),
            kind: inline.kind.as_str(),
            can_delete: inline.can_delete,
            fields: header_fields,
            rows,
            total,
        });
    }
    out
}

/// A single parsed child row from the submitted body.
#[derive(Debug)]
pub(crate) struct ParsedInlineRow {
    /// Child PK; empty for a new row.
    pub id: String,
    /// Whether the DELETE checkbox was set.
    pub delete: bool,
    /// `field → value` for this row (FK excluded; it's set on save).
    pub values: HashMap<String, String>,
}

impl ParsedInlineRow {
    /// True when this is an unsubmitted "extra" row — no id and every
    /// value blank. Such rows are skipped entirely on save.
    fn is_blank(&self) -> bool {
        self.id.is_empty() && self.values.values().all(|v| v.is_empty())
    }
}

/// Pull one inline's rows out of the flat `(field, value)` body, keyed
/// by the formset naming scheme. `inline_name` is the child table.
pub(crate) fn parse_inline_rows(
    inline_name: &str,
    pairs: &[(String, String)],
) -> Vec<ParsedInlineRow> {
    let total_key = format!("inline-{inline_name}-TOTAL");
    let total: usize = pairs
        .iter()
        .find(|(k, _)| k == &total_key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);

    // Last-wins map for quick lookup of `inline-<name>-<i>-<field>`.
    let prefix = format!("inline-{inline_name}-");
    let mut by_key: HashMap<&str, &str> = HashMap::new();
    for (k, v) in pairs {
        if k.starts_with(&prefix) {
            by_key.insert(k.as_str(), v.as_str());
        }
    }

    let mut rows = Vec::with_capacity(total);
    for i in 0..total {
        let id_key = format!("inline-{inline_name}-{i}-id");
        let delete_key = format!("inline-{inline_name}-{i}-DELETE");
        let id = by_key.get(id_key.as_str()).map(|s| s.to_string()).unwrap_or_default();
        let delete = by_key
            .get(delete_key.as_str())
            .map(|v| matches!(*v, "on" | "true" | "1"))
            .unwrap_or(false);
        let field_prefix = format!("inline-{inline_name}-{i}-");
        let mut values = HashMap::new();
        for (k, v) in &by_key {
            if let Some(rest) = k.strip_prefix(field_prefix.as_str()) {
                // Skip the structural keys.
                if rest == "id" || rest == "DELETE" {
                    continue;
                }
                values.insert(rest.to_string(), v.to_string());
            }
        }
        rows.push(ParsedInlineRow { id, delete, values });
    }
    rows
}

/// Persist every declared inline's children inside `tx`, with the FK set
/// to `parent_pk`. Any error propagates so the caller drops the tx
/// (rolling back the parent write too). Returns the child table name of
/// the first row that failed, for a friendly form-level message.
///
/// On a child write error the returned [`AdminError`] is wrapped with a
/// `BadInput` describing which inline/row failed (best-effort), so the
/// re-rendered form can surface it.
pub(crate) async fn save_inlines_in_tx(
    tx: &mut umbral::db::Transaction,
    parent: &ModelMeta,
    parent_pk: &str,
    cfg: Option<&AdminConfig>,
    pairs: &[(String, String)],
) -> Result<(), AdminError> {
    let Some(cfg) = cfg else { return Ok(()) };
    for inline in &cfg.inlines {
        let Some((_, child)) = crate::discovery::find_model(&inline.model) else {
            continue;
        };
        if validate_inline(parent, &child, inline).is_none() {
            continue;
        }
        let Some(child_pk_col) = child.fields.iter().find(|c| c.primary_key) else {
            continue;
        };
        // Skip readonly fields on writes. The FK is NOT skipped — we set
        // it explicitly to the parent PK below, and the parsed row's
        // `values` never carry the FK (it's excluded from the inline
        // form fields), so there's no mass-assignment surface to guard.
        let skip: Vec<String> = inline.readonly_fields.clone();

        let parsed = parse_inline_rows(&child.table, pairs);
        for (idx, row) in parsed.iter().enumerate() {
            // Delete: existing row + DELETE checked.
            if row.delete && !row.id.is_empty() {
                DynQuerySet::for_meta(&child)
                    .filter_eq_string(&child_pk_col.name, &row.id)
                    .delete_in_tx(tx)
                    .await
                    .map_err(|e| inline_err(&child.table, idx, e))?;
                continue;
            }
            // Blank extra row: nothing to do.
            if row.is_blank() {
                continue;
            }
            // Build the child form with the FK forced to the parent.
            let mut form = row.values.clone();
            form.insert(inline.fk_field.clone(), parent_pk.to_string());

            if row.id.is_empty() {
                // Insert a new child.
                DynQuerySet::for_meta(&child)
                    .insert_form_in_tx(tx, &form, &skip)
                    .await
                    .map_err(|e| inline_err(&child.table, idx, e))?;
            } else {
                // Update an existing child.
                DynQuerySet::for_meta(&child)
                    .filter_eq_string(&child_pk_col.name, &row.id)
                    .update_form_in_tx(tx, &form, &skip)
                    .await
                    .map_err(|e| inline_err(&child.table, idx, e))?;
            }
        }
    }
    Ok(())
}

/// Wrap a child write error with the inline name + row index so the
/// re-rendered parent form can name the offending row.
fn inline_err(child_table: &str, row_idx: usize, e: umbral::orm::DynError) -> AdminError {
    AdminError::BadInput(format!(
        "Inline `{child_table}` row {} could not be saved: {e}",
        row_idx + 1
    ))
}
