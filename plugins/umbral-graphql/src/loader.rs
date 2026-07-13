//! Row loading, batched.
//!
//! # The N+1 problem, which is the whole reason DataLoader exists
//!
//! ```graphql
//! { posts(limit: 100) { title author { username } } }
//! ```
//!
//! The naive resolver runs 1 query for the posts and then 100 more — one per post — for
//! the authors. It is correct, it passes every test you would think to write, and it melts
//! the database the first time somebody asks for a page of results. A GraphQL API without
//! batching is a loaded gun pointed at your own server, and the client is holding it: they
//! choose the query shape, so they choose your query count.
//!
//! So every relation goes through a loader that COALESCES the ids requested within one
//! resolution pass into a single `WHERE pk IN (...)`. 100 posts → 1 author query.
//!
//! The batching is per-request (`Loaders` is built fresh for each GraphQL request and put
//! in the context), so one request's cache can never serve another request's rows — which
//! would be a cross-user data leak, not an optimisation.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every database read this plugin performs. Test-only instrumentation — the N+1 claim in
/// the module docs is worthless unless something checks it, and "it feels fast" is not a
/// test. `batching_test` asserts a 3-post list with 2 authors costs exactly 2 reads
/// (1 list + 1 batched author lookup), not 4.
#[doc(hidden)]
pub static DB_READS: AtomicUsize = AtomicUsize::new(0);

use async_graphql::dataloader::{DataLoader, Loader};
use serde_json::Value as Json;
use umbral::migrate::ModelMeta;
use umbral::orm::DynQuerySet;

/// A batch key: which table, and which primary key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PkKey {
    pub table: String,
    pub id: String,
}

/// A batch key for a child list: which table, which FK column, which parent id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ChildKey {
    pub table: String,
    pub fk_col: String,
    pub parent_id: String,
}

pub struct RowLoader;

impl Loader<PkKey> for RowLoader {
    type Value = Json;
    type Error = Arc<async_graphql::Error>;

    async fn load(&self, keys: &[PkKey]) -> Result<HashMap<PkKey, Json>, Self::Error> {
        let mut out = HashMap::new();
        // Group by table: one query per TABLE, not per key. `keys` here is already the
        // coalesced set async-graphql collected across the whole resolution pass.
        let mut by_table: HashMap<&str, Vec<&PkKey>> = HashMap::new();
        for k in keys {
            by_table.entry(k.table.as_str()).or_default().push(k);
        }

        for (table, ks) in by_table {
            let Some(meta) = meta_for(table) else {
                continue;
            };
            let pk = pk_name(&meta);
            let ids: Vec<String> = ks.iter().map(|k| k.id.clone()).collect();

            let rows = fetch_where_in(&meta, &pk, &ids)
                .await
                .map_err(|e| Arc::new(async_graphql::Error::new(e)))?;

            for row in rows {
                let id = row
                    .get(&pk)
                    .map(crate::schema_id_string)
                    .unwrap_or_default();
                out.insert(
                    PkKey {
                        table: table.to_string(),
                        id,
                    },
                    row,
                );
            }
        }
        Ok(out)
    }
}

pub struct ChildLoader;

impl Loader<ChildKey> for ChildLoader {
    type Value = Vec<Json>;
    type Error = Arc<async_graphql::Error>;

    async fn load(&self, keys: &[ChildKey]) -> Result<HashMap<ChildKey, Vec<Json>>, Self::Error> {
        let mut out: HashMap<ChildKey, Vec<Json>> = HashMap::new();
        // Group by (table, fk_col) so all parents' children come back in ONE query.
        let mut groups: HashMap<(&str, &str), Vec<&ChildKey>> = HashMap::new();
        for k in keys {
            groups
                .entry((k.table.as_str(), k.fk_col.as_str()))
                .or_default()
                .push(k);
        }

        for ((table, fk_col), ks) in groups {
            let Some(meta) = meta_for(table) else {
                continue;
            };
            let ids: Vec<String> = ks.iter().map(|k| k.parent_id.clone()).collect();

            let rows = fetch_where_in(&meta, fk_col, &ids)
                .await
                .map_err(|e| Arc::new(async_graphql::Error::new(e)))?;

            // Seed every requested key so a parent with no children resolves to [] rather
            // than to null. An absent list and an empty list are different answers.
            for k in &ks {
                out.entry((*k).clone()).or_default();
            }
            for row in rows {
                let parent = row
                    .get(fk_col)
                    .map(crate::schema_id_string)
                    .unwrap_or_default();
                out.entry(ChildKey {
                    table: table.to_string(),
                    fk_col: fk_col.to_string(),
                    parent_id: parent,
                })
                .or_default()
                .push(row);
            }
        }
        Ok(out)
    }
}

/// The per-request loaders. Built fresh for each GraphQL request.
#[derive(Clone)]
pub struct Loaders {
    pub rows: Arc<DataLoader<RowLoader>>,
    pub children: Arc<DataLoader<ChildLoader>>,
}

impl Loaders {
    pub fn new() -> Self {
        Self {
            rows: Arc::new(DataLoader::new(RowLoader, tokio::spawn)),
            children: Arc::new(DataLoader::new(ChildLoader, tokio::spawn)),
        }
    }

    pub async fn load_by_pk(
        &self,
        meta: &ModelMeta,
        id: String,
    ) -> async_graphql::Result<Option<Json>> {
        let key = PkKey {
            table: meta.table.clone(),
            id,
        };
        self.rows
            .load_one(key)
            .await
            .map_err(|e| async_graphql::Error::new(e.message.clone()))
    }

    pub async fn load_children(
        &self,
        child: &ModelMeta,
        fk_col: &str,
        parent_id: String,
    ) -> async_graphql::Result<Vec<Json>> {
        let key = ChildKey {
            table: child.table.clone(),
            fk_col: fk_col.to_string(),
            parent_id,
        };
        self.children
            .load_one(key)
            .await
            .map(|o| o.unwrap_or_default())
            .map_err(|e| async_graphql::Error::new(e.message.clone()))
    }
}

impl Default for Loaders {
    fn default() -> Self {
        Self::new()
    }
}

pub fn meta_for(table: &str) -> Option<ModelMeta> {
    umbral::migrate::registered_models()
        .into_iter()
        .find(|m| m.table == table)
}

pub fn pk_name(meta: &ModelMeta) -> String {
    meta.pk_column()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "id".to_string())
}

/// `SELECT * FROM t WHERE col IN (...)` — the batched read every loader lands on.
///
/// Values are coerced per the column's declared type (`typed_eq_condition`), so a `String`
/// pk whose value is numeric is bound as text and an i64 FK is bound as an i64. Binding a
/// string against an INTEGER column works on SQLite by affinity and ERRORS on Postgres —
/// the gaps3 #59 trap.
async fn fetch_where_in(meta: &ModelMeta, col: &str, ids: &[String]) -> Result<Vec<Json>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut any = sea_or(meta, col, ids);
    if any.is_none() {
        // Not one id could be coerced to the column's type — match nothing. NEVER fall
        // through to an unfiltered query: that would return the whole table (gaps3 #56).
        any = Some(umbral::orm::never_matches());
    }
    DB_READS.fetch_add(1, Ordering::Relaxed);
    let rows = DynQuerySet::for_meta(meta)
        .filter_condition(any.expect("set above"))
        .limit(crate::MAX_LIMIT)
        .fetch_as_json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(Json::Object).collect())
}

/// `col IN (ids)` as an OR of typed equalities, skipping ids that cannot be that column's
/// type at all.
fn sea_or(meta: &ModelMeta, col: &str, ids: &[String]) -> Option<sea_query::Condition> {
    let mut cond = sea_query::Condition::any();
    let mut matched = false;
    for id in ids {
        if let Some(c) = umbral::orm::typed_eq_condition(meta, col, id) {
            cond = cond.add(c);
            matched = true;
        }
    }
    matched.then_some(cond)
}

/// A plain list read for a top-level `Query.posts(...)`.
pub async fn fetch_list(
    meta: &ModelMeta,
    limit: u64,
    offset: u64,
) -> async_graphql::Result<Vec<Json>> {
    DB_READS.fetch_add(1, Ordering::Relaxed);
    let rows = DynQuerySet::for_meta(meta)
        .limit(limit)
        .offset(offset)
        .fetch_as_json()
        .await
        .map_err(|e| async_graphql::Error::new(e.to_string()))?;
    Ok(rows.into_iter().map(Json::Object).collect())
}

/// Read one row by primary key, fresh and redacted.
///
/// Not batched, and deliberately not going through the DataLoader: a subscription event is
/// its own moment in time, and a cached row would serve a subscriber the state from whenever
/// the cache was filled rather than the state that just changed.
pub async fn load_one_json(
    meta: &ModelMeta,
    pk: &str,
) -> Result<Option<serde_json::Map<String, Json>>, String> {
    let pk_col = pk_name(meta);
    DB_READS.fetch_add(1, Ordering::Relaxed);
    DynQuerySet::for_meta(meta)
        .filter_eq_string(&pk_col, pk)
        .first_as_json()
        .await
        .map_err(|e| e.to_string())
}
