//! Runtime helpers the `#[derive(Form)]`-generated `validate` /
//! `render_html` call. Keeps SQL + ORM access in core (never in
//! plugin code) and the emitted macro tokens terse.

use crate::forms::{PkKind, ValidationErrors};

/// Resolve a target table's PK kind from the registry, defaulting to
/// BigInt before the registry is populated (tests build the registry
/// in App::build, so this only matters pre-boot).
pub fn pk_kind_for_table(table: &str) -> PkKind {
    match crate::migrate::pk_meta_for_table(table).map(|(_, ty)| ty) {
        Some(crate::orm::SqlType::Uuid) => PkKind::Uuid,
        Some(crate::orm::SqlType::Text) => PkKind::Text,
        _ => PkKind::BigInt,
    }
}

/// Check that `value` is one of the compile-time choice `values`.
/// On a miss, push a field-keyed error. Empty value on a nullable
/// field is the caller's responsibility (it passes `nullable`).
pub fn validate_choice_member(
    field: &str,
    value: &str,
    values: &[&'static str],
    nullable: bool,
    errs: &mut ValidationErrors,
) {
    if value.is_empty() {
        if !nullable {
            errs.add(field, format!("{field} is required"));
        }
        return;
    }
    if !values.iter().any(|v| *v == value) {
        errs.add(field, format!("{field} is not a valid choice"));
    }
}

/// Build `(value, label)` option pairs from a `ChoiceField`'s
/// parallel `VALUES` / `LABELS` slices.
pub fn choice_options(values: &[&'static str], labels: &[&'static str]) -> Vec<(String, String)> {
    values
        .iter()
        .zip(labels.iter())
        .map(|(v, l)| ((*v).to_string(), (*l).to_string()))
        .collect()
}

/// Verify a row with PK == `id` exists in `target_table`, through the
/// ORM (never raw SQL). On a miss, push a field-keyed error. Empty id
/// on a nullable field is a no-op (the caller checks requiredness).
/// Registry / pool failures are swallowed as a miss — a form can't
/// validate against a DB that isn't up.
pub async fn validate_fk_exists(
    field: &str,
    id: &str,
    target_table: &str,
    nullable: bool,
    errs: &mut ValidationErrors,
) {
    if id.is_empty() {
        if !nullable {
            errs.add(field, format!("{field} is required"));
        }
        return;
    }
    let Some(meta) = crate::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == target_table)
    else {
        // Target not registered — can't verify; leave it to the DB FK.
        return;
    };
    let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
        return;
    };
    let exists = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
        .filter_eq_string(&pk_col, id)
        .count()
        .await
        .map(|n| n > 0)
        .unwrap_or(false);
    if !exists {
        errs.add(field, format!("{field}: no matching record"));
    }
}
