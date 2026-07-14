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
    if !values.contains(&value) {
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

/// Fetch `(id, label)` option rows for a ModelChoice/ModelMultiChoice
/// `<select>` through the ORM. `label_field` overrides the label
/// column; default is the first non-PK text column (matches the admin's
/// fk_picker convention). Returns at most 1000 rows — a form `<select>`
/// with more candidates needs a search widget, not a flat list. Errors
/// → empty options (an unrenderable select beats a 500).
pub async fn fetch_model_options(
    target_table: &str,
    label_field: Option<&str>,
) -> Vec<(String, String)> {
    let Some(meta) = crate::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == target_table)
    else {
        return Vec::new();
    };
    let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
        return Vec::new();
    };
    let label_col = label_field
        .map(|s| s.to_string())
        .or_else(|| {
            meta.fields
                .iter()
                .find(|c| c.ty == crate::orm::SqlType::Text && c.name != pk_col)
                .map(|c| c.name.clone())
        })
        .unwrap_or_else(|| pk_col.clone());
    // fetch_as_json returns Vec<serde_json::Map<String, Value>> — each
    // row is already a Map, no .as_object() needed.
    let rows = crate::orm::dynamic::DynQuerySet::for_meta(&meta)
        .select_cols(&[pk_col.clone(), label_col.clone()])
        .limit(1000)
        .fetch_as_json()
        .await
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|obj| {
            let id = json_scalar_to_string(obj.get(&pk_col)?);
            let label = obj
                .get(&label_col)
                .map(json_scalar_to_string)
                .unwrap_or_else(|| id.clone());
            Some((id, label))
        })
        .collect()
}

/// Stringify a JSON scalar for option values/labels.
fn json_scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn pk_string_to_sea_value(id: &str, ty: crate::orm::SqlType) -> Option<sea_query::Value> {
    match ty {
        crate::orm::SqlType::SmallInt | crate::orm::SqlType::Integer => id
            .parse::<i32>()
            .ok()
            .map(|n| sea_query::Value::Int(Some(n))),
        crate::orm::SqlType::BigInt | crate::orm::SqlType::ForeignKey => id
            .parse::<i64>()
            .ok()
            .map(|n| sea_query::Value::BigInt(Some(n))),
        // A UUID binds as a UUID, not as its string form (gaps3 #79).
        //
        // Everything else in the ORM already treats a `SqlType::Uuid` column as a typed
        // uuid: `write::json_to_sea_value` binds `SeaValue::Uuid`, `filter_eq` /
        // `filter_in_strings` bind a parsed `Uuid`, and `m2m.rs` reads `child_id` back with
        // `try_get::<uuid::Uuid>`. This function was the one place that bound the string.
        //
        // The values it produces are the junction's `child_id`s, so the junction was storing
        // the uuid's TEXT form while the child table stored the uuid itself (sqlx encodes a
        // `Uuid` as a BLOB on SQLite). The junction a migration emits carries
        // `child_id REFERENCES <child>(<pk>)`, and TEXT can never match that — so an M2M
        // whose CHILD has a Uuid PK failed with `FOREIGN KEY constraint failed` on every
        // real schema. It only ever "worked" against hand-written test tables that had no
        // foreign key.
        //
        // A malformed uuid yields `None`, which the caller reports as a missing reference —
        // the same answer it gives for an id that does not exist, which is what a
        // non-uuid id IS for a uuid-keyed table.
        crate::orm::SqlType::Uuid => uuid::Uuid::parse_str(id)
            .ok()
            .map(|u| sea_query::Value::Uuid(Some(Box::new(u)))),
        crate::orm::SqlType::Text => Some(sea_query::Value::String(Some(Box::new(id.to_string())))),
        _ => Some(sea_query::Value::String(Some(Box::new(id.to_string())))),
    }
}

/// Split a submitted M2M value into ids. The form layer joins repeated
/// keys with `,`; we also accept whitespace. Empty pieces are dropped.
pub fn parse_multi_ids(raw: &str) -> Vec<String> {
    raw.split([',', ' ', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Verify every id in `ids` exists in `target_table`; on any miss, push
/// a field-keyed error. Returns the parsed sea_query PK values for the
/// ids that exist (used to stage the pending junction write). When any
/// id is missing the caller treats the whole submission as invalid
/// (atomicity) — errs is non-empty so the create never runs.
///
/// A *genuine miss* (the existence query ran, an id wasn't in the result
/// set) records a per-field "id has no matching record" error. A *real
/// database error* (the query itself failed) is a different animal: it's
/// NOT the user's fault and every submitted id is unverifiable, not
/// missing. Flagging all of them "no matching record" would be a lie that
/// masks the real failure (gaps2 #48). So a DB error is logged via
/// `tracing::error!` and surfaced as a single NON-field error
/// ("could not validate references (database error)"); we fail closed
/// (errs non-empty → the create never runs) but honestly.
pub async fn validate_multi_fk_exists(
    field: &str,
    ids: &[String],
    target_table: &str,
    errs: &mut ValidationErrors,
) -> Vec<sea_query::Value> {
    // Empty / optional M2M submitted nothing → no DB hit at all.
    if ids.is_empty() {
        return Vec::new();
    }
    let Some(meta) = crate::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == target_table)
    else {
        return Vec::new();
    };
    let Some(pk_col) = meta.pk_column().map(|c| c.name.clone()) else {
        return Vec::new();
    };
    // ONE batched query — `SELECT <pk> FROM <target> WHERE <pk> IN
    // (...)`. NOT one count() per id: a list of M selected ids costs a
    // single round-trip, never M (no N+1). The set-difference below
    // finds the missing ids.
    let rows = match crate::orm::dynamic::DynQuerySet::for_meta(&meta)
        .select_cols(&[pk_col.clone()])
        .filter_in_strings(&pk_col, ids)
        .fetch_as_json()
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            // Do NOT swallow the error into an empty result set: that
            // would flag EVERY submitted id "no matching record" — a
            // bogus per-field error masking a real DB failure. Log the
            // real error and record one honest non-field error instead.
            tracing::error!(
                field = %field,
                target_table = %target_table,
                error = %e,
                "M2M reference validation query failed; failing closed without flagging ids as missing",
            );
            errs.add_non_field(format!(
                "could not validate {field} references (database error)"
            ));
            return Vec::new();
        }
    };
    let found: std::collections::HashSet<String> = rows
        .into_iter()
        .filter_map(|r| r.get(&pk_col).map(json_scalar_to_string))
        .collect();
    let pk_ty = meta
        .fields
        .iter()
        .find(|c| c.name == pk_col)
        .map(|c| c.ty)
        .unwrap_or(crate::orm::SqlType::BigInt);
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if found.contains(id) {
            match pk_string_to_sea_value(id, pk_ty) {
                Some(value) => out.push(value),
                None => errs.add(
                    field,
                    format!("{field}: id {id} is not a valid primary key"),
                ),
            }
        } else {
            errs.add(field, format!("{field}: id {id} has no matching record"));
        }
    }
    out
}
