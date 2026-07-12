//! Model-level audit trail — `#[umbral(audited)]` (gaps3 #54).
//!
//! Every write to an audited model records a row in `umbral_audit`: **who**
//! changed **which row**, **when**, and **which fields changed, from what to
//! what**.
//!
//! # Not to be confused with `AdminAuditLog`
//!
//! `umbral-admin` has an `AdminAuditLog` that looks like this and is not. That
//! one is Django's `LogEntry`: it records only writes made *through the admin
//! UI*, stores a free-text summary rather than a field-level diff, and produces
//! **no row at all** for a write from REST, a background task, or
//! `Model::objects().save()`. This module is the real thing — it hooks the ORM,
//! so every write path is covered no matter who initiated it.
//!
//! # Why it hooks the write paths and not the signals
//!
//! The obvious implementation is a signal subscriber. It does not work. The
//! typed path's `post_save` carries the full serialized row, but the **dynamic**
//! path's carries only `{ids, created}` — no row data — and admin and REST run
//! entirely on `DynQuerySet`. An audit trail built on signals would hold rich
//! history for code-driven writes and PK-only stubs for the writes humans
//! actually make. A log that *looks* complete and isn't is worse than no log,
//! because it is the one you would testify from.
//!
//! # The cost, paid only by audited models
//!
//! An UPDATE or DELETE records what the row looked like *before*, and the ORM
//! keeps no pre-image. So an audited update/delete reads the affected rows first
//! — one extra SELECT. Models without `#[umbral(audited)]` pay nothing.

use serde_json::{Map, Value, json};

use crate::migrate::{Column, ModelMeta};
use crate::orm::SqlType;

/// The table every audit row lands in.
pub const AUDIT_TABLE: &str = "umbral_audit";

/// What happened to the row.
pub const CREATE: &str = "create";
pub const UPDATE: &str = "update";
pub const DELETE: &str = "delete";

/// The audit table's schema, as a `ModelMeta`.
///
/// Hand-built rather than `#[derive(Model)]` because the derive emits `::umbral::`
/// paths, which don't resolve inside `umbral-core` (the same reason
/// `orm/post.rs` hand-writes its impl). A `ModelMeta` is all that's needed: the
/// migration engine creates the table from it, and `DynQuerySet::for_meta` writes
/// rows through it. No typed model required.
///
/// `row_pk` and `actor` are TEXT on purpose. The audited model may be keyed by
/// i64, String or Uuid, and the user model independently so — the audit table
/// must not care about either.
pub fn audit_meta() -> ModelMeta {
    let col = |name: &str, ty: SqlType, nullable: bool| Column {
        name: name.to_string(),
        ty,
        nullable,
        ..Column::default()
    };
    ModelMeta {
        view: None,
        materialized: false,
        name: "UmbralAudit".to_string(),
        table: AUDIT_TABLE.to_string(),
        fields: vec![
            Column {
                name: "id".to_string(),
                ty: SqlType::BigInt,
                primary_key: true,
                ..Column::default()
            },
            col("table_name", SqlType::Text, false),
            col("row_pk", SqlType::Text, false),
            col("action", SqlType::Text, false),
            // NULL for a background job, the CLI, a migration, an anonymous
            // request. We record "nobody was authenticated", never a guess.
            col("actor", SqlType::Text, true),
            col("at", SqlType::Timestamptz, false),
            // JSON: {"field": {"from": <old>, "to": <new>}, ...}
            col("changes", SqlType::Text, false),
        ],
        ordering: vec![("id".to_string(), true)],
        ..ModelMeta::default()
    }
}

/// The fields that actually changed, as `{"field": {"from": .., "to": ..}}`.
///
/// Only *changed* fields are recorded. Writing every column on every update
/// makes the log enormous and unreadable — the question an audit answers is
/// "what changed", and a diff that includes the 40 fields that didn't buries it.
fn diff(before: Option<&Map<String, Value>>, after: Option<&Map<String, Value>>) -> Value {
    let mut out = Map::new();
    match (before, after) {
        (None, Some(a)) => {
            for (k, v) in a {
                out.insert(k.clone(), json!({ "from": Value::Null, "to": v }));
            }
        }
        (Some(b), None) => {
            for (k, v) in b {
                out.insert(k.clone(), json!({ "from": v, "to": Value::Null }));
            }
        }
        (Some(b), Some(a)) => {
            for (k, new) in a {
                let old = b.get(k).unwrap_or(&Value::Null);
                if old != new {
                    out.insert(k.clone(), json!({ "from": old, "to": new }));
                }
            }
        }
        (None, None) => {}
    }
    Value::Object(out)
}

/// Record one write against one row. A no-op unless the model is `audited`.
///
/// `before` / `after` are the row's full column maps; the entry stores only the
/// difference. The actor comes from the ambient request context — `None` when
/// nobody is authenticated, which is the honest answer for a job or the CLI.
///
/// Deliberately best-effort: an audit failure logs loudly but does not fail the
/// user's write. Losing the log entry is bad; rolling back a legitimate business
/// write because the log table was full is worse, and this is not a
/// compliance-grade WORM store.
pub async fn record(
    meta: &ModelMeta,
    row_pk: &str,
    action: &str,
    before: Option<&Map<String, Value>>,
    after: Option<&Map<String, Value>>,
) {
    if !meta.audited {
        return;
    }
    let changes = diff(before, after);
    // An update that changed nothing is not an event worth a row.
    if action == UPDATE && changes.as_object().is_some_and(Map::is_empty) {
        return;
    }

    let mut body = Map::new();
    body.insert("table_name".into(), json!(meta.table));
    body.insert("row_pk".into(), json!(row_pk));
    body.insert("action".into(), json!(action));
    body.insert(
        "actor".into(),
        match crate::db::route_context::current_user_id() {
            Some(u) => json!(u),
            None => Value::Null,
        },
    );
    body.insert("at".into(), json!(chrono::Utc::now()));
    body.insert("changes".into(), json!(changes.to_string()));

    let audit = audit_meta();
    if let Err(e) = crate::orm::dynamic::DynQuerySet::for_meta(&audit)
        .insert_json(&body)
        .await
    {
        tracing::error!(
            table = %meta.table,
            row_pk = %row_pk,
            action = %action,
            "umbral: failed to write audit row: {e:?}",
        );
    }
}

/// Record one entry per affected row, for a write that matched several.
pub async fn record_many(
    meta: &ModelMeta,
    action: &str,
    rows: Vec<(
        String,
        Option<Map<String, Value>>,
        Option<Map<String, Value>>,
    )>,
) {
    if !meta.audited {
        return;
    }
    for (pk, before, after) in rows {
        record(meta, &pk, action, before.as_ref(), after.as_ref()).await;
    }
}

/// `pk IN (…)` for the rows an audited write touched.
///
/// The after-image is re-read **by primary key**, not by re-running the caller's
/// filter: an update that changes a column the filter matched on (`SET title='b'
/// WHERE title='a'`) would find nothing the second time, and the audit row would
/// claim the update deleted the data.
pub fn pk_in_condition(meta: &ModelMeta, pks: &[Value]) -> Option<sea_query::Condition> {
    use sea_query::{Alias, Expr};
    let pk = meta.pk_column()?;
    if pks.is_empty() {
        return None;
    }
    let vals: Vec<sea_query::Value> = pks
        .iter()
        .filter_map(|v| crate::orm::write::json_to_sea_value(pk.ty, v, false, &pk.name, None).ok())
        .collect();
    if vals.is_empty() {
        return None;
    }
    Some(sea_query::Condition::all().add(Expr::col(Alias::new(&pk.name)).is_in(vals)))
}

/// The value of `row`'s primary key, stringified — the audit table's `row_pk`.
pub fn pk_of(meta: &ModelMeta, row: &Map<String, Value>) -> String {
    meta.pk_column()
        .and_then(|pk| row.get(&pk.name))
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}
