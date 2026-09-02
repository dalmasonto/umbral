//! Gap #75 — reverse-FK prefetch with **no declared field** on the
//! parent model.
//!
//! [`ReverseSet<C>`](crate::orm::ReverseSet) needs a `#[sqlx(skip)]
//! #[umbral(reverse_fk = "...")]` field on the parent struct because
//! `.prefetch_related(name)` writes its batched result INTO that field
//! (`HydrateRelated::set_reverse_fk_resolved_json`). Rust structs are
//! fixed at compile time — unlike Python/Django there is no instance
//! dict to stash an ad-hoc `_prefetched_cache` on, so without a
//! declared slot there is nowhere on `T` to put the answer.
//!
//! `QuerySet::prefetch_map::<C>()` sidesteps the problem by not writing
//! into `T` at all: it returns the parents AND the batched children
//! together as an explicit [`Prefetched<T, C>`] value. The child type
//! `C` is named at the call site (mirrors
//! [`ReverseRelations::reverse`](crate::orm::ReverseRelations::reverse)'s
//! per-instance `parent.reverse::<C>()> — same FK-discovery metadata,
//! batched instead of per-row) instead of a string field name, which is
//! what makes the "no field required" trick possible: the compiler
//! resolves `C::FIELDS` for us, so the FK-column lookup that would
//! otherwise need a runtime string→type table lookup is just a normal
//! generic-fn call.
//!
//! ## Example
//!
//! ```rust,ignore
//! // Developer has NO `ReverseSet<Achievement>` field.
//! let prefetched = Developer::objects()
//!     .filter(developer::ACTIVE.eq(true))
//!     .prefetch_map::<Achievement>()
//!     .fetch()
//!     .await?;
//! for dev in &prefetched.parents {
//!     for ach in prefetched.children_of(dev) {
//!         println!("{}: {}", dev.name, ach.title);
//!     }
//! }
//! ```
//!
//! Query budget: 1 (parents, with whatever `.filter`/`.order_by`/etc.
//! was chained) + 1 (children, `SELECT * FROM achievement WHERE
//! developer_id IN (...)`) — regardless of parent count. Same
//! no-N+1 guarantee as the declared-`ReverseSet` path; this reuses its
//! batch-fetch primitive ([`fetch_related_as_json_by_pk`]) unchanged,
//! only the destination of the results differs.
//!
//! ## Scope (v1, matches the declared-field path)
//!
//! Reverse-FK only. M2M / reverse-O2O without a declared field are not
//! covered here — `prefetch_map` assumes exactly one FK on `C` points
//! back at `T` (or the disambiguated one named via
//! [`PrefetchMapQuery::via`]).

use std::collections::HashMap;
use std::marker::PhantomData;

use serde_json::Value as JsonValue;

use crate::db::DbPool;
use crate::orm::model::HydrateRelated;
use crate::orm::reverse_accessor::discover_single_fk;
use crate::orm::{Model, ReverseError};

use super::hydration::{dedup_by_pk_key, fetch_related_as_json_by_pk, parent_pk_sql_type};
use super::{QuerySet, resolve_pool};

/// The batched result of [`QuerySet::prefetch_map`] /
/// [`crate::orm::Manager::prefetch_map`]: the fetched parents plus every
/// `C` row bucketed by which parent's FK it matched.
///
/// No struct mutation, no hidden cache — [`Self::children_of`] reads
/// straight out of the map this value owns.
#[derive(Debug)]
pub struct Prefetched<T, C> {
    /// The parent rows, in the order the main query returned them.
    pub parents: Vec<T>,
    by_parent: HashMap<String, Vec<C>>,
}

impl<T: HydrateRelated, C> Prefetched<T, C> {
    /// The children batched for this parent, or `&[]` when the parent
    /// has none (or its PK couldn't be read — see
    /// [`HydrateRelated::pk_as_json`]).
    ///
    /// `parent` need not be one of `self.parents` by identity — any
    /// value with the same PK resolves the same bucket — but the
    /// common call is iterating `self.parents` and reading each one's
    /// children in turn.
    pub fn children_of(&self, parent: &T) -> &[C] {
        parent
            .pk_as_json()
            .map(|pk| crate::orm::pk_key(&pk))
            .and_then(|key| self.by_parent.get(&key))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Builder returned by [`QuerySet::prefetch_map`]. Chaining stops here
/// until [`Self::fetch`] — the `via` builder mirrors
/// [`ReverseRelations::reverse_via`](crate::orm::ReverseRelations::reverse_via)'s
/// disambiguation escape hatch for when `C` has more than one FK to
/// `T`.
pub struct PrefetchMapQuery<T, C> {
    pub(super) qs: QuerySet<T>,
    pub(super) fk_col: Option<&'static str>,
    pub(super) _c: PhantomData<fn() -> C>,
}

impl<T: Model, C: Model> PrefetchMapQuery<T, C> {
    /// Name the FK column on `C` explicitly — needed when `C` has more
    /// than one `ForeignKey<T>` and automatic discovery would
    /// otherwise return [`ReverseError::Ambiguous`].
    pub fn via(mut self, fk_col: &'static str) -> Self {
        self.fk_col = Some(fk_col);
        self
    }

    /// Run the parent query (with whatever `.filter` / `.order_by` /
    /// `.select_related` / etc. was chained onto it beforehand), then
    /// batch-load `C` rows whose FK column points at one of the
    /// returned parents.
    ///
    /// One query for the parents, one `SELECT ... WHERE <fk> IN (...)`
    /// for the children — regardless of parent count. Errors loudly
    /// (same [`ReverseError`] messages as `.reverse::<C>()`) when `C`
    /// has zero or multiple FKs to `T` and `.via(...)` wasn't used, or
    /// when `.via(...)` named a column that isn't a FK to `T`.
    pub async fn fetch(self) -> Result<Prefetched<T, C>, sqlx::Error>
    where
        T: for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
            + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
            + HydrateRelated,
        C: for<'de> serde::Deserialize<'de> + HydrateRelated,
    {
        let fk_col = self.resolve_fk_col()?;
        let explicit_pool = self.qs.explicit_pool.clone();
        let parents = self.qs.fetch().await?;
        let pool = resolve_pool::<T>(explicit_pool, crate::db::RouteOp::Read);
        let by_parent = load_children::<T, C>(&parents, fk_col, &pool).await?;
        Ok(Prefetched { parents, by_parent })
    }

    /// Resolve which column on `C` is the FK back to `T`: the
    /// explicit `.via(...)` name (validated to exist and to actually
    /// target `T::TABLE`), or the single discovered candidate.
    fn resolve_fk_col(&self) -> Result<&'static str, sqlx::Error> {
        match self.fk_col {
            Some(col) => {
                let spec = C::FIELDS.iter().find(|f| f.name == col).ok_or_else(|| {
                    sqlx::Error::Protocol(
                        ReverseError::UnknownColumn {
                            child: C::NAME,
                            column: col.to_string(),
                        }
                        .to_string(),
                    )
                })?;
                if spec.fk_target != Some(T::TABLE) {
                    return Err(sqlx::Error::Protocol(
                        ReverseError::NotAForeignKey {
                            child: C::NAME,
                            column: col.to_string(),
                            parent_table: T::TABLE,
                        }
                        .to_string(),
                    ));
                }
                Ok(col)
            }
            None => discover_single_fk::<T, C>().map_err(|e| sqlx::Error::Protocol(e.to_string())),
        }
    }
}

/// The shared batch-fetch: parents → PKs → one `IN (...)` query on
/// `C::TABLE` → bucket by `fk_col` value, keyed PK-agnostically via
/// [`crate::orm::pk_key`]. Byte-for-byte the same query shape
/// [`super::hydration::hydrate_reverse_fk_for_field`] runs for the
/// declared-`ReverseSet` path; only the destination differs (a
/// returned map here, `HydrateRelated::set_reverse_fk_resolved_json`
/// there).
async fn load_children<T, C>(
    parents: &[T],
    fk_col: &'static str,
    pool: &DbPool,
) -> Result<HashMap<String, Vec<C>>, sqlx::Error>
where
    T: Model + HydrateRelated,
    C: Model + HydrateRelated + for<'de> serde::Deserialize<'de>,
{
    let mut parent_pks: Vec<JsonValue> = parents.iter().filter_map(|r| r.pk_as_json()).collect();
    if parent_pks.is_empty() {
        return Ok(HashMap::new());
    }
    dedup_by_pk_key(&mut parent_pks);

    let parent_pk_ty = parent_pk_sql_type::<T>();
    // Registry-safe: `registered_models()` panics before `App::build`.
    // Registry-less tests treat an absent registry as "child is not
    // soft-delete" — same guard `hydrate_reverse_fk_for_field` uses.
    let child_soft_delete = crate::migrate::is_initialised()
        && crate::migrate::registered_models()
            .into_iter()
            .find(|m| m.table == C::TABLE)
            .is_some_and(|m| m.soft_delete);

    let child_rows = fetch_related_as_json_by_pk(
        C::TABLE,
        fk_col,
        parent_pk_ty,
        child_soft_delete,
        &parent_pks,
        pool,
    )
    .await?;

    let mut by_parent: HashMap<String, Vec<C>> = HashMap::new();
    for row in child_rows {
        let key = row
            .as_object()
            .and_then(|m| m.get(fk_col))
            .map(crate::orm::pk_key);
        let Some(key) = key else { continue };
        // Forgive-and-continue on a per-row decode error — same
        // posture the macro-emitted `set_reverse_fk_resolved_json`
        // arm takes for the declared-field path.
        if let Ok(c) = serde_json::from_value::<C>(row) {
            by_parent.entry(key).or_default().push(c);
        }
    }
    Ok(by_parent)
}
