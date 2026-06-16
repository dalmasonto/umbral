//! Post-fetch relation hydration. Each function here is called from
//! a QuerySet terminal (fetch / first / get) after the main rows
//! decode and walks one of three relation paths:
//!
//!   - [`hydrate_select_related`] (+ [`hydrate_select_related_nested`])
//!     — `.select_related("author")` / `.select_related("author__manager")`.
//!     Batched IN query per hop, fills `ForeignKey<U>.resolved` via
//!     `HydrateRelated::hydrate_fk`.
//!   - [`hydrate_prefetch_related`] — `.prefetch_related("tags")` for
//!     M2M and `.prefetch_related("comment_set")` for reverse-FK.
//!     Routes to [`hydrate_reverse_fk_for_field`] when the field
//!     matches `Model::REVERSE_FK_RELATIONS`.
//!
//! All hydration is batched: one query per relation field regardless
//! of parent count. No N+1.
//!
//! Query budgets:
//!   - `.select_related("a", "b")` → 1 (main) + 2 (one IN per FK)
//!   - `.select_related("a__b__c")` → 1 (main) + 3 (one IN per hop)
//!   - `.prefetch_related("tags", "comment_set")` → 1 (main) + 2

use std::collections::HashMap;

use sea_query::{PostgresQueryBuilder, SqliteQueryBuilder};
use sea_query_binder::SqlxBinder;
use serde_json::Value as JsonValue;

use crate::db::DbPool;
use crate::orm::{HydrateRelated, Model};

use super::{backend_pg, backend_sqlite};

/// Fetch related rows for each FK field name in `sr_fields` and
/// hydrate `HydrateRelated::hydrate_fk` on each main row.
///
/// Routes any `__`-containing name to [`hydrate_select_related_nested`]
/// for chain traversal (`author__manager` etc.). Single-hop paths
/// keep the original simpler shape: collect FK ids → batched IN
/// query → bucket by id → hydrate.
///
/// Generic parameters:
/// - `T`: the main model type. Bound on `HydrateRelated` so we can
///   call `fk_id_for` and `hydrate_fk` on each row.
pub(super) async fn hydrate_select_related<T: Model + HydrateRelated>(
    rows: &mut [T],
    sr_fields: &[String],
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    for field_name in sr_fields {
        // Nested traversal: `select_related("author__manager")` walks
        // the hop chain (author → manager → ...) one batched query
        // per hop, embedding each level's row into the prior level's
        // JSON. Recursive `ForeignKey<T>::Deserialize` (post-#42)
        // then unpacks the full chain into `resolved` slots at every
        // depth in one `hydrate_fk` call on the root parent.
        if field_name.contains("__") {
            hydrate_select_related_nested::<T>(rows, field_name, pool).await?;
            continue;
        }
        // Single-hop path: the original behaviour kept byte-for-byte.
        let field_spec = T::FIELDS
            .iter()
            .find(|f| f.name == field_name.as_str())
            .ok_or_else(|| {
                sqlx::Error::Protocol(format!(
                    "umbra::orm::select_related: unknown field `{field_name}` on model `{}`",
                    T::NAME
                ))
            })?;
        let fk_target = field_spec.fk_target.ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbra::orm::select_related: field `{field_name}` on `{}` is not a foreign key",
                T::NAME
            ))
        })?;

        // PK lift Pass D: collect FK values as `serde_json::Value`
        // (was `Vec<i64>`). The macro's `fk_id_for` now returns the
        // FK's PK in whatever JSON shape the target uses — i64
        // targets stay as `Number`, String / UUID targets land as
        // `String`. Dedup goes through `pk_json_key` because
        // `serde_json::Value` isn't `Hash`.
        let mut ids: Vec<JsonValue> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            if let Some(v) = row.fk_id_for(field_name.as_str()) {
                if !v.is_null() {
                    ids.push(v);
                }
            }
        }
        if ids.is_empty() {
            continue;
        }
        dedup_by_pk_key(&mut ids);

        // PK lift Pass E: O(1) lookup via the cached
        // `pk_meta_for_table`. Falls back to `"id"` when the
        // registry isn't initialised (low-level tests that drive
        // the QuerySet without `App::build` — the legacy behaviour,
        // byte-identical for every integer-PK target).
        //
        // `registered_models()` PANICS on an uninitialised registry, so
        // it must be guarded — otherwise this select_related path crashes
        // exactly the registry-less tests the comment above promises to
        // support. When the registry is absent, `target_meta` stays
        // `None`: `soft_delete` defaults off and the PK resolves through
        // the (also registry-safe) `pk_meta_for_table` / `"id"` fallback.
        let target_meta = if crate::migrate::is_initialised() {
            crate::migrate::registered_models()
                .into_iter()
                .find(|m| m.table == fk_target)
        } else {
            None
        };
        let target_soft_delete = target_meta.as_ref().is_some_and(|m| m.soft_delete);
        let (target_pk_col, target_pk_ty) = target_meta
            .as_ref()
            .and_then(|m| m.pk_column().map(|c| (c.name.clone(), c.ty)))
            .or_else(|| crate::migrate::pk_meta_for_table(fk_target))
            .unwrap_or_else(|| ("id".to_string(), crate::orm::SqlType::BigInt));
        let related_rows = fetch_related_as_json_by_pk(
            fk_target,
            &target_pk_col,
            target_pk_ty,
            target_soft_delete,
            &ids,
            pool,
        )
        .await?;
        let id_to_json: HashMap<String, JsonValue> = related_rows
            .into_iter()
            .filter_map(|obj| {
                let map = obj.as_object()?;
                let pk_val = map.get(target_pk_col.as_str())?;
                Some((pk_json_key(pk_val), obj.clone()))
            })
            .collect();

        for row in rows.iter_mut() {
            if let Some(fk_val) = row.fk_id_for(field_name.as_str()) {
                if let Some(resolved_json) = id_to_json.get(&pk_json_key(&fk_val)) {
                    row.hydrate_fk(field_name.as_str(), resolved_json);
                }
            }
        }
    }
    Ok(())
}

/// PK lift — local alias for the canonical [`crate::orm::pk_key`]. Kept as
/// a thin delegate so the existing `dedup_by_pk_key` call site reads the
/// same while there's one source of truth for the `n:`/`s:`/`o:`
/// shape-namespacing.
fn pk_json_key(v: &JsonValue) -> String {
    crate::orm::pk_key(v)
}

/// PK lift Pass D — dedup a `Vec<Value>` of PK ids by stable string
/// key. `serde_json::Value` isn't `Hash`, so the standard
/// sort+dedup doesn't apply. Used for the IN-list dedup in both
/// `hydrate_select_related` and `hydrate_select_related_nested`.
fn dedup_by_pk_key(ids: &mut Vec<JsonValue>) {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    ids.retain(|v| seen.insert(pk_json_key(v)));
}

/// The SQL type of `T`'s primary-key column. Used by the reverse-FK /
/// reverse-OneToOne hydrators to bind the parent PK values in the child's
/// FK-column `IN (...)` list as the right type — so a `Uuid` parent PK
/// binds as a native `uuid` on Postgres, not text. Falls back to `BigInt`
/// (the historical i64 default) when the PK column can't be resolved.
fn parent_pk_sql_type<T: Model>() -> crate::orm::SqlType {
    crate::migrate::ModelMeta::for_::<T>()
        .fields
        .iter()
        .find(|c| c.primary_key)
        .map(|c| c.ty)
        .unwrap_or(crate::orm::SqlType::BigInt)
}

/// Nested `select_related("a__b__c")` traversal. Walks the hop chain
/// through `crate::migrate::registered_models()` (rather than the
/// typed `T::FIELDS` after hop 1, since deeper hops live on the
/// related model whose type isn't in scope here), runs ONE batched
/// `IN (...)` query per hop, and embeds each level's row into the
/// prior level's JSON. The root parent then sees one
/// `hydrate_fk(first_hop, fully_nested_json)` call and the recursive
/// `ForeignKey<T>::Deserialize` (post-#42) unpacks every depth into
/// its `resolved` slot.
///
/// Query budget = `1 + len(hops)` round-trips. No N+1 — each hop is
/// one batched query across every parent (and every dedup'd parent of
/// prior hops). So `select_related("a__b__c")` on N parents takes
/// 1 (main) + 3 (hops) = 4 queries regardless of N.
pub(super) async fn hydrate_select_related_nested<T: Model + HydrateRelated>(
    rows: &mut [T],
    path: &str,
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    let hops: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
    if hops.is_empty() {
        return Ok(());
    }
    let registered = crate::migrate::registered_models();

    // Resolve every hop's (from_table, field, to_table) trio up
    // front so a typo in any hop surfaces before any SQL runs.
    let mut current_table = T::TABLE;
    let mut hop_targets: Vec<&str> = Vec::with_capacity(hops.len());
    for hop in &hops {
        let meta = registered
            .iter()
            .find(|m| m.table == current_table)
            .ok_or_else(|| {
                sqlx::Error::Protocol(format!(
                    "umbra::orm::select_related: model for table `{current_table}` is not registered \
                     (needed for nested traversal of `{path}`)"
                ))
            })?;
        let col = meta.fields.iter().find(|c| c.name == *hop).ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbra::orm::select_related: unknown field `{hop}` on table `{current_table}` \
                 (full path `{path}`)"
            ))
        })?;
        let target = col.fk_target.as_deref().ok_or_else(|| {
            sqlx::Error::Protocol(format!(
                "umbra::orm::select_related: field `{hop}` on table `{current_table}` is not a \
                 foreign key (full path `{path}`)"
            ))
        })?;
        hop_targets.push(target);
        current_table = target;
    }

    // PK lift Pass D: resolve each hop target's PK column name
    // (was hardcoded `"id"`) so codename / slug / UUID-keyed
    // targets bind against the right column.
    let hop_target_pk: Vec<(String, crate::orm::SqlType)> = hop_targets
        .iter()
        .filter_map(|t| {
            registered
                .iter()
                .find(|m| &m.table == t)
                .and_then(|m| m.pk_column().map(|c| (c.name.clone(), c.ty)))
        })
        .collect();
    if hop_target_pk.len() != hops.len() {
        // A target meta lookup failed mid-chain. Same shape the
        // dynamic-side hydrator falls back with — skip the chain
        // rather than crash.
        return Ok(());
    }
    let hop_target_soft_delete: Vec<bool> = hop_targets
        .iter()
        .map(|t| {
            registered
                .iter()
                .find(|m| &m.table == t)
                .is_some_and(|m| m.soft_delete)
        })
        .collect();

    // Phase 1: fetch each level's rows top-down, one batched IN
    // query per hop. `levels[i]` holds the rows at depth i (before
    // any nesting is embedded), keyed for later lookup by PK key.
    let first_field = hops[0];
    let mut ids: Vec<JsonValue> = rows
        .iter()
        .filter_map(|r| {
            let v = r.fk_id_for(first_field)?;
            if v.is_null() { None } else { Some(v) }
        })
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    dedup_by_pk_key(&mut ids);
    let mut levels: Vec<Vec<JsonValue>> = Vec::with_capacity(hops.len());
    levels.push(
        fetch_related_as_json_by_pk(
            hop_targets[0],
            &hop_target_pk[0].0,
            hop_target_pk[0].1,
            hop_target_soft_delete[0],
            &ids,
            pool,
        )
        .await?,
    );

    for hop_idx in 1..hops.len() {
        let hop_field = hops[hop_idx];
        let hop_target = hop_targets[hop_idx];
        let prev_lvl = &levels[hop_idx - 1];
        let mut next_ids: Vec<JsonValue> = prev_lvl
            .iter()
            .filter_map(|r| {
                let v = r.as_object()?.get(hop_field)?;
                if v.is_null() { None } else { Some(v.clone()) }
            })
            .collect();
        if next_ids.is_empty() {
            // The chain bottoms out: the prior level has only NULL
            // for this hop. Subsequent hops would also be empty;
            // stop here. Earlier levels still embed below.
            break;
        }
        dedup_by_pk_key(&mut next_ids);
        levels.push(
            fetch_related_as_json_by_pk(
                hop_target,
                &hop_target_pk[hop_idx].0,
                hop_target_pk[hop_idx].1,
                hop_target_soft_delete[hop_idx],
                &next_ids,
                pool,
            )
            .await?,
        );
    }

    // Phase 2: bottom-up embed. For each level from the second-to-
    // last down to the first, embed the next level's matching row
    // into the corresponding `hop_field` slot. By the time we hit
    // levels[0], its rows carry the full nested chain.
    if levels.len() > 1 {
        for i in (0..levels.len() - 1).rev() {
            let next_pk_col = &hop_target_pk[i + 1].0;
            let next_by_pk: HashMap<String, JsonValue> = levels[i + 1]
                .iter()
                .filter_map(|obj| {
                    let map = obj.as_object()?;
                    let pk_val = map.get(next_pk_col.as_str())?;
                    Some((pk_json_key(pk_val), obj.clone()))
                })
                .collect();
            let hop_field = hops[i + 1];
            for row in levels[i].iter_mut() {
                let Some(map) = row.as_object_mut() else {
                    continue;
                };
                let Some(fk_val) = map.get(hop_field) else {
                    continue;
                };
                if fk_val.is_null() {
                    continue;
                }
                if let Some(next_json) = next_by_pk.get(&pk_json_key(fk_val)) {
                    map.insert(hop_field.to_string(), next_json.clone());
                }
            }
        }
    }

    // Phase 3: hydrate root parents with the fully-nested level-0
    // rows. Recursive ForeignKey<T>::Deserialize unpacks the chain
    // into resolved slots at every depth.
    let first_pk_col = &hop_target_pk[0].0;
    let first_by_pk: HashMap<String, JsonValue> = levels
        .into_iter()
        .next()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|obj| {
            let map = obj.as_object()?;
            let pk_val = map.get(first_pk_col.as_str())?;
            Some((pk_json_key(pk_val), obj.clone()))
        })
        .collect();
    for row in rows.iter_mut() {
        if let Some(fk_val) = row.fk_id_for(first_field) {
            if let Some(json) = first_by_pk.get(&pk_json_key(&fk_val)) {
                row.hydrate_fk(first_field, json);
            }
        }
    }
    Ok(())
}

/// Gap #44 — reverse-FK collection hydration. Runs one batched
/// `SELECT * FROM <target_table> WHERE <fk_column> IN (parent_pks)`,
/// groups rows by their `<fk_column>` value, and feeds each parent's
/// bucket to `HydrateRelated::set_reverse_fk_resolved_json`.
///
/// Query budget: 1 query per declared `prefetch_related("...")`
/// field regardless of parent count. Same no-N+1 guarantee the M2M
/// path has. PK-agnostic (PK lift): parents are collected via
/// [`HydrateRelated::pk_as_json`] and bucketed by [`crate::orm::pk_key`],
/// so i64-, String/slug-, and Uuid-PK parents all hydrate.
pub(super) async fn hydrate_reverse_fk_for_field<T: Model + HydrateRelated>(
    rows: &mut [T],
    spec: &crate::orm::model::ReverseFkRelationSpec,
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    // Parent PKs as JSON Values — i64 / String / Uuid all flow.
    let mut parent_pks: Vec<JsonValue> = rows.iter().filter_map(|r| r.pk_as_json()).collect();
    if parent_pks.is_empty() {
        // Set empty resolved on every parent so `comment_set.resolved()`
        // returns `Some(&[])` after prefetch (matches the "no children
        // found" shape — distinct from "not loaded").
        for r in rows.iter_mut() {
            r.set_reverse_fk_resolved_json(spec.field_name, Vec::new());
        }
        return Ok(());
    }
    dedup_by_pk_key(&mut parent_pks);
    // SELECT * FROM <target> WHERE <fk_col> IN (parent_pks). The child's FK
    // column is typed as the PARENT's PK, so bind the parent PK values as
    // that type (correct uuid binding on Postgres).
    let parent_pk_ty = parent_pk_sql_type::<T>();
    // Registry-safe: `registered_models()` panics before `App::build`, so
    // guard it. Registry-less tests (and the legacy integer-PK path) treat
    // an absent registry as "child is not soft-delete".
    let child_soft_delete = crate::migrate::is_initialised()
        && crate::migrate::registered_models()
            .into_iter()
            .find(|m| m.table == spec.target_table)
            .is_some_and(|m| m.soft_delete);
    let child_rows = fetch_related_as_json_by_pk(
        spec.target_table,
        spec.fk_column,
        parent_pk_ty,
        child_soft_delete,
        &parent_pks,
        pool,
    )
    .await?;
    // Bucket children by their fk_column value, keyed PK-agnostically.
    let mut by_parent: HashMap<String, Vec<JsonValue>> = HashMap::new();
    for row in child_rows {
        let key = row
            .as_object()
            .and_then(|m| m.get(spec.fk_column))
            .map(crate::orm::pk_key);
        if let Some(key) = key {
            by_parent.entry(key).or_default().push(row);
        }
    }
    // Populate each parent's ReverseSet — empty bucket → empty Vec
    // (matches the documented "loaded, no children" shape).
    for row in rows.iter_mut() {
        if let Some(pk) = row.pk_as_json() {
            let bucket = by_parent
                .remove(&crate::orm::pk_key(&pk))
                .unwrap_or_default();
            row.set_reverse_fk_resolved_json(spec.field_name, bucket);
        }
    }
    Ok(())
}

/// OneToOne reverse hydration. Discovers the back-pointing FK
/// column on the child at runtime by scanning the child's
/// `FIELDS` for a UNIQUE FK whose `fk_target` is this parent's
/// table. Exactly one match is required; 0 or 2+ matches surface
/// a loud error naming the candidates so the user can either:
///
///   - add `#[umbra(unique)]` to make a non-unique FK unique
///     (turning a one-to-many into a one-to-one), or
///   - rename one of the multiple FKs (the ambiguity is on them).
///
/// Once the column is resolved, the loader runs ONE batched
/// `SELECT * FROM <child_table> WHERE <fk_col> IN (parent_pks)`
/// — same shape as the ReverseSet path — and feeds each parent
/// the FIRST matching row (the UNIQUE constraint guarantees at
/// most one, but for safety the loader takes the first if the
/// DB ever has dupes from an unconstrained legacy column).
async fn hydrate_one_to_one_for_field<T: Model + HydrateRelated>(
    rows: &mut [T],
    spec: &crate::orm::model::OneToOneRelationSpec,
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    // Resolve the FK column on the child at runtime.
    let registered = crate::migrate::registered_models();
    let Some(child_meta) = registered.iter().find(|m| m.table == spec.target_table) else {
        return Err(sqlx::Error::Protocol(format!(
            "umbra::orm::prefetch_related: child model for table `{}` is not \
             registered (needed by OneToOne field `{}` on `{}`)",
            spec.target_table,
            spec.field_name,
            T::NAME,
        )));
    };
    let candidates: Vec<&str> = child_meta
        .fields
        .iter()
        .filter(|c| c.unique && c.fk_target.as_deref() == Some(T::TABLE))
        .map(|c| c.name.as_str())
        .collect();
    let fk_column = match candidates.len() {
        1 => candidates[0],
        0 => {
            return Err(sqlx::Error::Protocol(format!(
                "umbra::orm::prefetch_related: OneToOne field `{}` on `{}` \
                 has no back-link — `{}` needs a `#[umbra(unique)]` \
                 ForeignKey<{}> pointing back (none found)",
                spec.field_name,
                T::NAME,
                spec.target_name,
                T::NAME
            )));
        }
        _ => {
            return Err(sqlx::Error::Protocol(format!(
                "umbra::orm::prefetch_related: OneToOne field `{}` on `{}` \
                 is ambiguous — `{}` has multiple UNIQUE ForeignKey<{}> \
                 columns ({}). Rename one or use a typed ReverseSet field \
                 instead.",
                spec.field_name,
                T::NAME,
                spec.target_name,
                T::NAME,
                candidates.join(", "),
            )));
        }
    };

    // Parent PKs as JSON Values — PK-agnostic (i64 / String / Uuid).
    let mut parent_pks: Vec<JsonValue> = rows.iter().filter_map(|r| r.pk_as_json()).collect();
    if parent_pks.is_empty() {
        // Mark every parent as loaded-with-no-child so
        // `is_loaded()` flips even on empty parents.
        for r in rows.iter_mut() {
            r.set_one_to_one_resolved_json(spec.field_name, None);
        }
        return Ok(());
    }
    dedup_by_pk_key(&mut parent_pks);

    let parent_pk_ty = parent_pk_sql_type::<T>();
    let child_rows = fetch_related_as_json_by_pk(
        spec.target_table,
        fk_column,
        parent_pk_ty,
        child_meta.soft_delete,
        &parent_pks,
        pool,
    )
    .await?;
    // Index by parent PK key. Take FIRST per parent — the UNIQUE
    // constraint guarantees uniqueness, but if there are dupes
    // (legacy data, deferred constraint, race condition during
    // an in-flight migration) the loader doesn't crash; it
    // picks one deterministically.
    let mut by_parent: HashMap<String, JsonValue> = HashMap::new();
    for row in child_rows {
        let key = row
            .as_object()
            .and_then(|m| m.get(fk_column))
            .map(crate::orm::pk_key);
        if let Some(key) = key {
            by_parent.entry(key).or_insert(row);
        }
    }
    for row in rows.iter_mut() {
        if let Some(pk) = row.pk_as_json() {
            let child = by_parent.remove(&crate::orm::pk_key(&pk));
            row.set_one_to_one_resolved_json(spec.field_name, child);
        }
    }
    Ok(())
}

/// Gap 19: post-fetch hydration for `prefetch_related` names.
///
/// For each requested M2M field, runs one query joining the child
/// table to the junction:
///
///   SELECT j.parent_id AS __parent_id, child.<col1>, child.<col2>, ...
///   FROM <child_table> child
///   INNER JOIN <junction> j ON child.<child_pk> = j.child_id
///   WHERE j.parent_id IN (<parent_ids>)
///
/// Each result row decodes its child columns to a `serde_json::Value`
/// object (using the child ModelMeta's column types — same machinery
/// as `values()`). Rows are bucketed by parent_id; each parent in
/// `rows` then receives the matching bucket via
/// `HydrateRelated::set_m2m_resolved_json`.
///
/// V1 scope: i64 parent PK only (parents whose `pk_i64()` returns
/// `None` are skipped). Reverse-FK names (post-#44) route through
/// [`hydrate_reverse_fk_for_field`] before the M2M lookup. Unknown
/// names error loudly with a hint pointing at the right method.
pub(super) async fn hydrate_prefetch_related<T: Model + HydrateRelated>(
    rows: &mut [T],
    prefetch_fields: &[String],
    pool: &DbPool,
) -> Result<(), sqlx::Error> {
    for field_name in prefetch_fields {
        // Try M2M first.
        let m2m_spec = T::M2M_RELATIONS
            .iter()
            .find(|s| s.field_name == field_name.as_str());
        // If not M2M, try reverse-FK (gap #44 — needs a ReverseSet<C>
        // field declared on the parent with `#[umbra(reverse_fk =
        // "<fk_col>")]`).
        let rfk_spec = T::REVERSE_FK_RELATIONS
            .iter()
            .find(|s| s.field_name == field_name.as_str());
        if let Some(spec) = rfk_spec {
            hydrate_reverse_fk_for_field::<T>(rows, spec, pool).await?;
            continue;
        }
        // OneToOne reverse: zero-config. The FK column on the
        // child isn't named at macro time — discover it at
        // runtime by scanning the child's FIELDS for the UNIQUE
        // FK pointing back at this parent's table.
        let o2o_spec = T::ONE_TO_ONE_RELATIONS
            .iter()
            .find(|s| s.field_name == field_name.as_str());
        if let Some(spec) = o2o_spec {
            hydrate_one_to_one_for_field::<T>(rows, spec, pool).await?;
            continue;
        }
        let spec = match m2m_spec {
            Some(s) => s,
            None => {
                let is_fk = T::FIELDS
                    .iter()
                    .any(|f| f.name == field_name.as_str() && f.fk_target.is_some());
                let hint = if is_fk {
                    format!(
                        " — `{field_name}` is a foreign key, use `.select_related(...)` \
                         or `.join_related(...)` instead"
                    )
                } else {
                    " — no M2M, ReverseSet, or OneToOne field with that name on this model"
                        .to_string()
                };
                return Err(sqlx::Error::Protocol(format!(
                    "umbra::orm::prefetch_related: unknown field `{field_name}` on model `{}`{hint}",
                    T::NAME
                )));
            }
        };
        let junction_table = format!("{}_{}", T::TABLE, spec.field_name);

        // Look up the child model's ModelMeta via the migrate
        // registry so we can iterate its columns at decode time.
        let registered: Vec<crate::migrate::ModelMeta> = crate::migrate::registered_models();
        let child_meta = match registered
            .into_iter()
            .find(|m| m.table == spec.target_table)
        {
            Some(m) => m,
            None => continue,
        };
        let child_pk_col = match child_meta.fields.iter().find(|c| c.primary_key) {
            Some(c) => c.name.clone(),
            None => continue,
        };

        // Collect parent PKs (PK-agnostic) from the main rows.
        let mut parent_pks: Vec<JsonValue> = rows.iter().filter_map(|r| r.pk_as_json()).collect();
        if parent_pks.is_empty() {
            // Still need to set empty resolved on every parent so
            // `tags.resolved()` returns `Some(&[])` after prefetch,
            // matching the documented "empty Vec, not None" contract.
            for r in rows.iter_mut() {
                r.set_m2m_resolved_json(field_name.as_str(), Vec::new());
            }
            continue;
        }
        dedup_by_pk_key(&mut parent_pks);

        // A Column descriptor for the aliased junction `parent_id` carrying
        // the PARENT model's PK SqlType — lets us (a) bind the IN-list and
        // (b) decode `__parent_id` back through the shape-aware helpers, so
        // i64 / String / Uuid parents all work on both backends.
        let parent_id_col = {
            let meta = crate::migrate::ModelMeta::for_::<T>();
            match meta.fields.into_iter().find(|c| c.primary_key) {
                Some(mut c) => {
                    c.name = "__parent_id".to_string();
                    c
                }
                None => continue, // no PK column — can't bucket
            }
        };
        // Convert the parent PK Values to the junction column's SQL type
        // for the `IN (...)` predicate.
        let parent_seavals: Vec<sea_query::SimpleExpr> = parent_pks
            .iter()
            .filter_map(|v| {
                crate::orm::write::json_to_sea_value(
                    parent_id_col.ty,
                    v,
                    false,
                    "__parent_id",
                    None,
                )
                .ok()
                .map(sea_query::SimpleExpr::Value)
            })
            .collect();

        // Build the SELECT joining child + junction.
        let mut q = sea_query::Query::select();
        q.expr_as(
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("parent_id"),
            )),
            sea_query::Alias::new("__parent_id"),
        );
        for col in &child_meta.fields {
            q.expr_as(
                sea_query::Expr::col((
                    sea_query::Alias::new("c"),
                    sea_query::Alias::new(col.name.as_str()),
                )),
                sea_query::Alias::new(col.name.as_str()),
            );
        }
        q.from_as(
            sea_query::Alias::new(child_meta.table.as_str()),
            sea_query::Alias::new("c"),
        )
        .join_as(
            sea_query::JoinType::InnerJoin,
            sea_query::Alias::new(&junction_table),
            sea_query::Alias::new("j"),
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("child_id"),
            ))
            .equals((
                sea_query::Alias::new("c"),
                sea_query::Alias::new(child_pk_col.as_str()),
            )),
        )
        .and_where(
            sea_query::Expr::col((
                sea_query::Alias::new("j"),
                sea_query::Alias::new("parent_id"),
            ))
            .is_in(parent_seavals),
        );
        if child_meta.soft_delete {
            q.and_where(
                sea_query::Expr::col((
                    sea_query::Alias::new("c"),
                    sea_query::Alias::new("deleted_at"),
                ))
                .is_null(),
            );
        }

        // Execute and group by parent_id, keyed PK-agnostically. The
        // `__parent_id` value decodes through the same shape-aware helper
        // as the child columns, using the parent PK's SqlType.
        let mut buckets: HashMap<String, Vec<JsonValue>> = HashMap::new();
        match pool {
            DbPool::Sqlite(p) => {
                let (sql, vals) = q.build_sqlx(SqliteQueryBuilder);
                let raw_rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, vals)
                    .fetch_all(p)
                    .await?;
                for raw in &raw_rows {
                    let parent_json = crate::orm::dynamic::decode_to_json(raw, &parent_id_col)?;
                    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
                    for col in &child_meta.fields {
                        let v = crate::orm::dynamic::decode_to_json(raw, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    buckets
                        .entry(crate::orm::pk_key(&parent_json))
                        .or_default()
                        .push(JsonValue::Object(obj));
                }
            }
            DbPool::Postgres(p) => {
                let (sql, vals) = q.build_sqlx(PostgresQueryBuilder);
                let raw_rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, vals)
                    .fetch_all(p)
                    .await?;
                for raw in &raw_rows {
                    let parent_json = crate::orm::dynamic::decode_pg_to_json(raw, &parent_id_col)?;
                    let mut obj = serde_json::Map::with_capacity(child_meta.fields.len());
                    for col in &child_meta.fields {
                        let v = crate::orm::dynamic::decode_pg_to_json(raw, col)?;
                        obj.insert(col.name.clone(), v);
                    }
                    buckets
                        .entry(crate::orm::pk_key(&parent_json))
                        .or_default()
                        .push(JsonValue::Object(obj));
                }
            }
        }

        // Hand each parent its bucket. Parents without children
        // still get an empty Vec so .resolved() returns Some(&[])
        // consistently after prefetch.
        for row in rows.iter_mut() {
            let bucket = match row.pk_as_json() {
                Some(pk) => buckets.remove(&crate::orm::pk_key(&pk)).unwrap_or_default(),
                None => Vec::new(),
            };
            row.set_m2m_resolved_json(field_name.as_str(), bucket);
        }
    }
    Ok(())
}

// PK lift Pass D — `fetch_related_as_json(table, &[i64], pool)` was
// retired here. Pass A introduced it as a thin delegate to keep the
// typed-side select_related (then i64-bound via Vec<i64>) running
// while the JSON-shape-aware helper landed. Pass D lifted the typed
// hydrator to use `Vec<serde_json::Value>` directly, so the i64
// shim has no callers — every consumer goes through
// `fetch_related_as_json_by_pk` now.

/// PK-shape-agnostic `SELECT * FROM "<table>" WHERE "<pk_col>" IN
/// (...)` — used by the dynamic ORM's `hydrate_select_related_into`
/// when the FK target's PK is a `String` / `Uuid` / arbitrary other
/// shape (e.g. `permissions_permission.codename`).
///
/// Inspects the first non-null id in `ids` to pick the bind type:
/// `Value::Number` → bind as `i64`, `Value::String` → bind as
/// `String`. Mixed shapes produce a loud sqlx::Error::Protocol so a
/// stale id list mixed with the new PK type surfaces immediately
/// instead of partially binding then silently mis-fetching.
///
/// `pk_col` is the SQL column name (e.g. `"id"` for integer PKs,
/// `"codename"` for the permissions table). The caller pulls it from
/// `target_meta.pk_column().name` — the framework's source of truth
/// for which column carries the PK.
pub(crate) async fn fetch_related_as_json_by_pk(
    table: &str,
    pk_col: &str,
    pk_ty: crate::orm::SqlType,
    soft_delete: bool,
    ids: &[JsonValue],
    pool: &DbPool,
) -> Result<Vec<JsonValue>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    // Convert each non-null id to a `sea_query::Value` using the column's
    // PK SqlType (NOT the JSON shape). This is what lets a `uuid::Uuid` PK
    // — which serialises to a String, indistinguishable from a slug —
    // bind as a native `uuid` on Postgres rather than as text (which PG's
    // uuid column rejects). `json_to_sea_value` already parses Uuid /
    // dates / etc. per type; sea-query then renders dialect-correct
    // placeholders and binds the typed values.
    let mut seavals: Vec<sea_query::Value> = Vec::with_capacity(ids.len());
    for id in ids {
        if id.is_null() {
            continue;
        }
        if let Ok(v) = crate::orm::write::json_to_sea_value(pk_ty, id, false, pk_col, None) {
            seavals.push(v);
        }
    }
    if seavals.is_empty() {
        return Ok(vec![]);
    }

    let mut q = sea_query::Query::select();
    q.column(sea_query::Asterisk)
        .from(sea_query::Alias::new(table))
        .and_where(
            sea_query::Expr::col(sea_query::Alias::new(pk_col))
                .is_in(seavals.into_iter().map(sea_query::SimpleExpr::Value)),
        );
    if soft_delete {
        q.and_where(sea_query::Expr::col(sea_query::Alias::new("deleted_at")).is_null());
    }

    match pool {
        DbPool::Sqlite(pool) => {
            let (sql, args) = q.build_sqlx(SqliteQueryBuilder);
            let rows = sqlx::query_with::<sqlx::Sqlite, _>(&sql, args)
                .fetch_all(pool)
                .await?;
            Ok(rows.iter().map(backend_sqlite::row_to_json).collect())
        }
        DbPool::Postgres(pool) => {
            let (sql, args) = q.build_sqlx(PostgresQueryBuilder);
            let rows = sqlx::query_with::<sqlx::Postgres, _>(&sql, args)
                .fetch_all(pool)
                .await?;
            Ok(rows.iter().map(backend_pg::row_to_json).collect())
        }
    }
}
