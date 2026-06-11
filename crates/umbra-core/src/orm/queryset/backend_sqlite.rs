//! SQLite-specific helpers extracted out of the queryset module's
//! main body. These functions only exist because the SQLite +
//! Postgres `sqlx::Row` types are distinct concrete types and don't
//! share a trait we could blanket-bound over — so paired with
//! [`super::backend_pg`], not because the SQL logic differs.
//!
//! Everything here is `pub(super)` so only [`super`] (queryset/mod.rs)
//! and its other submodules can reach in.

use serde_json::Value as JsonValue;
use sqlx::Column as _;
use sqlx::Row as _;

use crate::orm::{HydrateRelated, Model};

/// Convert a SQLite row to a `serde_json::Value::Object`. Reads every
/// column by index and maps the SQLite type to the closest JSON
/// primitive.
///
/// Type-cascade uses `Option<T>` for every decode so a NULL column
/// reliably maps to `JsonValue::Null` instead of getting silently
/// coerced to `0` / `false` / etc. The pre-#42 cascade used bare
/// `try_get::<i64>` which SQLite affinity coerces from NULL to 0 —
/// fine when the row never had nullable integer-shaped columns, but
/// wrong as soon as a nullable FK was in scope (the nested
/// `select_related` traversal hit this immediately).
pub(super) fn row_to_json(row: &sqlx::sqlite::SqliteRow) -> JsonValue {
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name().to_string();
        let ord = col.ordinal();
        let val: JsonValue = if let Ok(opt) = row.try_get::<Option<i64>, _>(ord) {
            opt.map_or(JsonValue::Null, |v| JsonValue::Number(v.into()))
        } else if let Ok(opt) = row.try_get::<Option<f64>, _>(ord) {
            opt.map_or(JsonValue::Null, |v| serde_json::json!(v))
        } else if let Ok(opt) = row.try_get::<Option<bool>, _>(ord) {
            opt.map_or(JsonValue::Null, JsonValue::Bool)
        } else if let Ok(opt) = row.try_get::<Option<String>, _>(ord) {
            opt.map_or(JsonValue::Null, JsonValue::String)
        } else {
            JsonValue::Null
        };
        map.insert(name, val);
    }
    JsonValue::Object(map)
}

/// JOIN-path hydration. For each `join_field` in `join_fields`,
/// pulls the `<field>__<col>` aliased columns out of the row into a
/// `serde_json::Value::Object` and calls
/// `HydrateRelated::hydrate_fk` to populate `ForeignKey<U>.resolved`.
///
/// LEFT JOIN miss → the related PK column comes back NULL → skip
/// hydration (the FK stays unresolved, matching the unloaded shape).
/// Unknown fields / unregistered related models / models without a
/// PK are silently skipped — the SQL build path emitted no JOIN for
/// them either, so the columns wouldn't be in the row.
pub(super) fn hydrate_joined_rels<T: Model + HydrateRelated>(
    t: &mut T,
    row: &sqlx::sqlite::SqliteRow,
    join_fields: &[String],
) -> Result<(), sqlx::Error> {
    let registered = crate::migrate::registered_models();
    for field_name in join_fields {
        // Resolve the (possibly nested) FK chain. The deepest hop's
        // columns are the only ones SELECTed — aliased by the full
        // dotted path — so we rebuild the nested object bottom-up and
        // hand the chain to `hydrate_fk`, whose macro body recursively
        // deserialises nested `ForeignKey<U>` slots.
        let Some(hops) = crate::orm::queryset::resolve_join_hops_for::<T>(field_name) else {
            continue;
        };
        let segs: Vec<&str> = field_name.split("__").collect();
        // Build the nested object bottom-up. Each level's columns are
        // aliased by its cumulative dotted prefix (`plugin`,
        // `plugin__author`). `deeper` is the already-built object for
        // the next level down; it nests under the current level's
        // onward FK-field key. A NULL PK at any level means that level
        // is a LEFT-JOIN miss: the deeper chain is dropped and (for
        // level 0) hydration is skipped entirely so the FK stays
        // unresolved — no bogus child from all-null columns.
        let mut deeper: Option<serde_json::Value> = None;
        let mut hop0_missing = false;
        for idx in (0..hops.len()).rev() {
            let hop = &hops[idx];
            let prefix = segs[..=idx].join("__");
            let Some(meta) = registered.iter().find(|m| m.table == hop.child_table) else {
                deeper = None;
                if idx == 0 {
                    hop0_missing = true;
                }
                continue;
            };
            let Some(pk_col) = meta.fields.iter().find(|c| c.primary_key) else {
                deeper = None;
                if idx == 0 {
                    hop0_missing = true;
                }
                continue;
            };
            let pk_alias = format!("{prefix}__{}", pk_col.name);
            let pk_is_null = row
                .try_get::<Option<i64>, _>(pk_alias.as_str())
                .map(|v| v.is_none())
                .unwrap_or(true);
            if pk_is_null {
                // This level missed; the deeper object can't attach to
                // anything, and the level above sees no object here.
                deeper = None;
                if idx == 0 {
                    hop0_missing = true;
                }
                continue;
            }
            let mut obj = serde_json::Map::with_capacity(meta.fields.len());
            for col in &meta.fields {
                let alias = format!("{prefix}__{}", col.name);
                let val = crate::orm::dynamic::decode_to_json_aliased(row, col, &alias)?;
                obj.insert(col.name.clone(), val);
            }
            // Nest the already-built deeper level under the onward
            // FK-field key (the NEXT segment in the path).
            if let Some(child) = deeper.take()
                && let Some(next_seg) = segs.get(idx + 1)
            {
                obj.insert((*next_seg).to_string(), child);
            }
            deeper = Some(serde_json::Value::Object(obj));
        }
        if hop0_missing {
            continue;
        }
        if let Some(nested) = deeper {
            t.hydrate_fk(segs[0], &nested);
        }
    }
    Ok(())
}

/// Extract one M2M child row from a JOIN'd row. `field_name` is the
/// full join path (`"tags"` or the chained `"tags__category"`); the
/// child columns are aliased by the FIRST segment, and any onward FK
/// chain by its cumulative dotted prefix. Returns `Some(JsonValue::Object)`
/// when the child PK column is non-null (= real match) — with the
/// onward FK object nested under its field key so `tag.category`
/// hydrates — or `None` when the LEFT JOIN missed (no junction row for
/// this parent — parent still appears once with all child cols NULL).
pub(super) fn extract_m2m_child_json<T: Model>(
    row: &sqlx::sqlite::SqliteRow,
    field_name: &str,
    child_meta: &crate::migrate::ModelMeta,
) -> Result<Option<JsonValue>, sqlx::Error> {
    let segs: Vec<&str> = field_name.split("__").collect();
    let m2m_seg = segs[0];
    let Some(pk_col) = child_meta.fields.iter().find(|c| c.primary_key) else {
        return Ok(None);
    };
    let pk_alias = format!("{m2m_seg}__{}", pk_col.name);
    let pk_null = row
        .try_get::<Option<i64>, _>(pk_alias.as_str())
        .map(|v| v.is_none())
        .unwrap_or(true);
    if pk_null {
        return Ok(None);
    }
    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
    for col in &child_meta.fields {
        let alias = format!("{m2m_seg}__{}", col.name);
        let val = crate::orm::dynamic::decode_to_json_aliased(row, col, &alias)?;
        obj.insert(col.name.clone(), val);
    }
    // Onward FK chain off the child (`tags__category` -> category).
    // Build the chain bottom-up from its cumulative-prefix aliases and
    // nest it under the child's onward FK key (overriding the raw FK
    // id that was just inserted above). A NULL onward PK leaves the FK
    // as its raw id — unresolved, the LEFT-miss shape.
    if let Some((_ct, _cpk, onward)) = crate::orm::queryset::resolve_m2m_chain::<T>(field_name) {
        let registered = crate::migrate::registered_models();
        let mut deeper: Option<JsonValue> = None;
        for i in (0..onward.len()).rev() {
            let hop = &onward[i];
            let seg_idx = i + 1;
            let prefix = segs[..=seg_idx].join("__");
            let Some(meta) = registered.iter().find(|m| m.table == hop.child_table) else {
                deeper = None;
                continue;
            };
            let Some(hpk) = meta.fields.iter().find(|c| c.primary_key) else {
                deeper = None;
                continue;
            };
            let hpk_alias = format!("{prefix}__{}", hpk.name);
            let hpk_null = row
                .try_get::<Option<i64>, _>(hpk_alias.as_str())
                .map(|v| v.is_none())
                .unwrap_or(true);
            if hpk_null {
                deeper = None;
                continue;
            }
            let mut hobj = serde_json::Map::with_capacity(meta.fields.len());
            for col in &meta.fields {
                let alias = format!("{prefix}__{}", col.name);
                let val = crate::orm::dynamic::decode_to_json_aliased(row, col, &alias)?;
                hobj.insert(col.name.clone(), val);
            }
            if let Some(child) = deeper.take()
                && let Some(next_seg) = segs.get(seg_idx + 1)
            {
                hobj.insert((*next_seg).to_string(), child);
            }
            deeper = Some(JsonValue::Object(hobj));
        }
        if let Some(top) = deeper
            && let Some(first_onward_seg) = segs.get(1)
        {
            obj.insert((*first_onward_seg).to_string(), top);
        }
    }
    Ok(Some(JsonValue::Object(obj)))
}

/// Decode a primary-key column to JSON. SQLite stores integers,
/// text, and UUIDs as TEXT-affinity values — the typed try_get
/// paths handle the read; the json! macro takes care of the rest.
pub(super) fn pk_to_json(
    row: &sqlx::sqlite::SqliteRow,
    col_name: &str,
    ty: crate::orm::SqlType,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use serde_json::json;
    Ok(match ty {
        SmallInt | Integer | BigInt | ForeignKey => json!(row.try_get::<i64, _>(col_name)?),
        Text => json!(row.try_get::<String, _>(col_name)?),
        Uuid => json!(row.try_get::<uuid::Uuid, _>(col_name)?.to_string()),
        _ => JsonValue::Null,
    })
}

/// Aggregate result decoder. COUNT always returns BIGINT, AVG always
/// returns DOUBLE (both backends agree). SUM/MAX/MIN inherit the
/// source column's type, so the decoder dispatches on the FieldSpec's
/// SqlType collected at terminal-build time.
pub(super) fn decode_agg(
    row: &sqlx::sqlite::SqliteRow,
    name: &str,
    agg: &crate::orm::Aggregate,
    source_ty: Option<crate::orm::SqlType>,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use crate::orm::aggregate::AggregateKind;
    use serde_json::json;
    Ok(match agg.kind() {
        AggregateKind::Count => json!(row.try_get::<i64, _>(name)?),
        AggregateKind::Avg => row
            .try_get::<Option<f64>, _>(name)?
            .map_or(JsonValue::Null, |f| json!(f)),
        AggregateKind::Sum | AggregateKind::Max | AggregateKind::Min => match source_ty {
            Some(SmallInt | Integer | BigInt | ForeignKey) => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Real | Double) => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f)),
            // Default to a string read for date/time/text/uuid; SQLite
            // stores them as TEXT, so a MIN/MAX comes back stringified.
            _ => row
                .try_get::<Option<String>, _>(name)?
                .map_or(JsonValue::Null, JsonValue::String),
        },
    })
}
