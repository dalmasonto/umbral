//! Postgres-specific helpers extracted out of the queryset module's
//! main body. Paired with [`super::backend_sqlite`] — the SQL logic
//! is identical; only the `sqlx::Row` concrete type differs.

use serde_json::Value as JsonValue;
use sqlx::Column as _;
use sqlx::Row as _;

use crate::orm::{HydrateRelated, Model};

/// Convert a Postgres row to a `serde_json::Value::Object`. See the
/// note on [`super::backend_sqlite::row_to_json`] — same
/// `Option<T>`-first cascade so NULL columns map to `JsonValue::Null`
/// rather than the type's default (`0`, `false`, `""`).
pub(super) fn row_to_json(row: &sqlx::postgres::PgRow) -> JsonValue {
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

/// Postgres counterpart to
/// [`super::backend_sqlite::hydrate_joined_rels`]. See that function
/// for the algorithm; the only difference is the row type.
pub(super) fn hydrate_joined_rels<T: Model + HydrateRelated>(
    t: &mut T,
    row: &sqlx::postgres::PgRow,
    join_fields: &[String],
) -> Result<(), sqlx::Error> {
    let registered = crate::migrate::registered_models();
    for field_name in join_fields {
        let Some(fk_field) = T::FIELDS.iter().find(|f| f.name == field_name.as_str()) else {
            continue;
        };
        let Some(related_table) = fk_field.fk_target else {
            continue;
        };
        let Some(related_meta) = registered.iter().find(|m| m.table == related_table) else {
            continue;
        };
        let Some(pk_col) = related_meta.fields.iter().find(|c| c.primary_key) else {
            continue;
        };
        let pk_alias = format!("{}__{}", field_name, pk_col.name);
        let pk_is_null = row
            .try_get::<Option<i64>, _>(pk_alias.as_str())
            .map(|v| v.is_none())
            .unwrap_or(true);
        if pk_is_null {
            continue;
        }
        let mut obj = serde_json::Map::with_capacity(related_meta.fields.len());
        for col in &related_meta.fields {
            let alias = format!("{}__{}", field_name, col.name);
            let val = crate::orm::dynamic::decode_pg_to_json_aliased(row, col, &alias)?;
            obj.insert(col.name.clone(), val);
        }
        t.hydrate_fk(field_name, &serde_json::Value::Object(obj));
    }
    Ok(())
}

/// Postgres counterpart of
/// [`super::backend_sqlite::extract_m2m_child_json`]. See that
/// function for the algorithm — only the row type differs.
pub(super) fn extract_m2m_child_json(
    row: &sqlx::postgres::PgRow,
    field_name: &str,
    child_meta: &crate::migrate::ModelMeta,
) -> Result<Option<JsonValue>, sqlx::Error> {
    let Some(pk_col) = child_meta.fields.iter().find(|c| c.primary_key) else {
        return Ok(None);
    };
    let pk_alias = format!("{field_name}__{}", pk_col.name);
    let pk_null = row
        .try_get::<Option<i64>, _>(pk_alias.as_str())
        .map(|v| v.is_none())
        .unwrap_or(true);
    if pk_null {
        return Ok(None);
    }
    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
    for col in &child_meta.fields {
        let alias = format!("{field_name}__{}", col.name);
        let val = crate::orm::dynamic::decode_pg_to_json_aliased(row, col, &alias)?;
        obj.insert(col.name.clone(), val);
    }
    Ok(Some(JsonValue::Object(obj)))
}

/// Decode a primary-key column to JSON. Postgres preserves integer
/// widths (i16 / i32 / i64), so we dispatch per SmallInt / Integer /
/// BigInt instead of folding everything into i64 like the SQLite
/// counterpart.
pub(super) fn pk_to_json(
    row: &sqlx::postgres::PgRow,
    col_name: &str,
    ty: crate::orm::SqlType,
) -> Result<JsonValue, sqlx::Error> {
    use crate::orm::SqlType::*;
    use serde_json::json;
    Ok(match ty {
        SmallInt => json!(row.try_get::<i16, _>(col_name)?),
        Integer => json!(row.try_get::<i32, _>(col_name)?),
        BigInt | ForeignKey => json!(row.try_get::<i64, _>(col_name)?),
        Text => json!(row.try_get::<String, _>(col_name)?),
        Uuid => json!(row.try_get::<uuid::Uuid, _>(col_name)?.to_string()),
        _ => JsonValue::Null,
    })
}

/// Postgres aggregate result decoder. See
/// [`super::backend_sqlite::decode_agg`] for the algorithm. The only
/// difference: Postgres preserves integer widths, so SUM/MAX/MIN on a
/// SmallInt comes back as `i16` (not coerced to `i64` like SQLite).
pub(super) fn decode_agg(
    row: &sqlx::postgres::PgRow,
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
            Some(SmallInt) => row
                .try_get::<Option<i16>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Integer) => row
                .try_get::<Option<i32>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(BigInt | ForeignKey) => row
                .try_get::<Option<i64>, _>(name)?
                .map_or(JsonValue::Null, |n| json!(n)),
            Some(Real) => row
                .try_get::<Option<f32>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f as f64)),
            Some(Double) => row
                .try_get::<Option<f64>, _>(name)?
                .map_or(JsonValue::Null, |f| json!(f)),
            _ => row
                .try_get::<Option<String>, _>(name)?
                .map_or(JsonValue::Null, JsonValue::String),
        },
    })
}
