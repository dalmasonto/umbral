//! Reusable nested-tree writer (gaps4 #77).
//!
//! umbral-rest has long owned the "one nested JSON document ↔ one object
//! graph" write: a parent row plus every declared reverse-FK child subtree,
//! written on ONE transaction with each child's FK auto-filled from its
//! parent's just-inserted (still-uncommitted) primary key, depth- and
//! node-bounded, and rolled back whole on any error. That orchestration was
//! private to the REST plugin and coupled to `RestPlugin`/an HTTP request, so
//! a non-REST caller — a CLI seeder, an AI-agent object-graph seeder — had to
//! hand-walk the tree (per-model `.create()` calls plus a manual M2M `.set()`
//! and a delete-first idempotency dance), re-implementing the same write less
//! safely (no single transaction).
//!
//! This module lifts the tree walk down to the ORM layer so ANY caller can
//! hand one nested document to one call. The walk itself is pure ORM
//! (`DynQuerySet::insert_json_in_tx` per node); the REST-only security gating
//! (hidden-field stripping, per-child create permission, object-scope) is a
//! pluggable [`NestedWriteGate`] hook rather than baked in. umbral-rest wires a
//! gate that enforces its checks; a direct caller uses [`NoGate`] and gets the
//! same atomic tree write with no gating.
//!
//! **M2M comes for free.** Each node is inserted via `insert_json_in_tx`, which
//! already mirrors any declared M2M relation fields left in the body into their
//! junction tables on the same transaction. So a nested document that carries
//! `{"tags": [1, 2]}` on a node writes those junction rows as part of the same
//! atomic tree — no separate `.set()` needed.
//!
//! See `arch.md` and `plugins/umbral-rest/src/lib.rs` (the REST wrappers).

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::db::Transaction;
use crate::migrate::{Column, ModelMeta};
use crate::orm::dynamic::{DynError, DynQuerySet};
use crate::orm::write::WriteError;

/// Max writable-nesting depth. A cyclic `.nested()` declaration (A→B→A) or a
/// self-referential one would otherwise recurse without bound; hitting this
/// fails with [`NestedError::MaxDepth`] rather than blowing the stack.
pub const MAX_NEST_DEPTH: usize = 16;

/// Default ceiling on the total child rows one nested write may create across
/// the whole tree, so a payload can never expand to an unbounded number of
/// statements on one transaction. A gate can lower this via
/// [`NestedWriteGate::max_nodes`]; depth is bounded separately by
/// [`MAX_NEST_DEPTH`].
pub const DEFAULT_MAX_NEST_NODES: usize = 1000;

/// A declared nested spec: for each parent table, the ordered list of
/// `(json_field, child_table)` pairs whose arrays in the body are written as
/// reverse-FK children. Grandchildren are discovered by looking up the child's
/// OWN table in the same map, so one entry declares exactly one level — no
/// magic. This mirrors `RestPlugin::nested`, but is a plain data structure any
/// caller can build.
pub type NestedSpec = HashMap<String, Vec<(String, String)>>;

/// Errors the tree walk itself raises (independent of any gate). A gate's
/// associated error must be `From<NestedError>` so these lift into the caller's
/// error type — for [`NoGate`] that is `NestedError` itself.
#[derive(Debug)]
pub enum NestedError {
    /// The document was malformed for the walk (a declared nested field was
    /// not an array, an item was not an object, a child table is unknown or has
    /// no/ambiguous FK to its parent, a row lacked a PK after insert, …).
    BadInput(String),
    /// An UPDATE (upsert) targeted a row that does not exist, or that exists
    /// but does not belong to the parent named in the tree — the cross-parent
    /// ownership guard. Kept distinct from [`BadInput`](NestedError::BadInput)
    /// so a gate (REST) can map it to a 404 rather than a 400.
    NotFound(String),
    /// The tree exceeded the node budget ([`DEFAULT_MAX_NEST_NODES`] or the
    /// gate's override).
    MaxNodes(usize),
    /// The tree exceeded [`MAX_NEST_DEPTH`].
    MaxDepth(usize),
    /// A per-row insert failed. Carries the underlying [`WriteError`] so a
    /// caller can surface field-level validation detail.
    Write(WriteError),
}

impl std::fmt::Display for NestedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NestedError::BadInput(m) => write!(f, "{m}"),
            NestedError::NotFound(m) => write!(f, "{m}"),
            NestedError::MaxNodes(n) => {
                write!(f, "nested write exceeds the maximum of {n} child rows")
            }
            NestedError::MaxDepth(n) => {
                write!(f, "nested write exceeds the maximum depth of {n}")
            }
            NestedError::Write(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NestedError {}

impl From<WriteError> for NestedError {
    fn from(e: WriteError) -> Self {
        NestedError::Write(e)
    }
}

impl From<DynError> for NestedError {
    /// Preserve the structured [`WriteError`] so a gate (REST) keeps its
    /// per-field validation map all the way to the response — a raw
    /// `sqlx::Error` lifts into `WriteError::Sqlx` (a 500 on the REST side),
    /// matching how `DynError` itself routes.
    fn from(e: DynError) -> Self {
        match e {
            DynError::Write(w) => NestedError::Write(w),
            DynError::Sqlx(s) => NestedError::Write(WriteError::from(s)),
        }
    }
}

/// Per-child security gating for the nested write, passed in by the caller
/// rather than baked into the walk. umbral-rest supplies a gate that strips
/// hidden fields, checks each child's own create permission, and enforces
/// object-scope; a direct (CLI/agent) caller uses [`NoGate`], which does none
/// of that.
///
/// The associated [`Error`](NestedWriteGate::Error) is the type the whole write
/// returns. It must absorb both the walk's own [`NestedError`] and per-row
/// [`WriteError`]s so `?` flows through the recursion into the caller's error.
#[async_trait::async_trait]
pub trait NestedWriteGate: Send + Sync {
    /// The error type this write surfaces. REST sets this to its `ApiError`
    /// (preserving structured validation errors); [`NoGate`] uses
    /// [`NestedError`].
    type Error: From<NestedError> + From<WriteError> + Send;

    /// Cap on total child rows across the tree. Defaults to
    /// [`DEFAULT_MAX_NEST_NODES`].
    fn max_nodes(&self) -> usize {
        DEFAULT_MAX_NEST_NODES
    }

    /// Veto a child table before it is resolved/written. REST uses this to
    /// enforce that a nested child clears the same exposure/block-list its own
    /// endpoint would. Default: allow every declared child.
    fn allow_table(&self, table: &str) -> bool {
        let _ = table;
        true
    }

    /// Strip fields the caller may not write on this child (hidden/denied
    /// columns) before the insert. Runs BEFORE the FK is injected so the FK
    /// survives. Default: no-op.
    fn strip_hidden(&self, table: &str, body: &mut Map<String, Value>) {
        let _ = (table, body);
    }

    /// Enforce this child's OWN create permission. Default: allow.
    fn check_create(&self, table: &str) -> Result<(), Self::Error> {
        let _ = table;
        Ok(())
    }

    /// Enforce this child's OWN update permission — the upsert path's UPDATE
    /// branch (an item that carries its primary key). Runs before the
    /// ownership read. Default: allow.
    fn check_update(&self, table: &str) -> Result<(), Self::Error> {
        let _ = table;
        Ok(())
    }

    /// Object-scope check for creating this child, run AFTER the FK is injected
    /// so the parent link participates. Default: allow.
    async fn scope_create(
        &self,
        table: &str,
        body: &Map<String, Value>,
    ) -> Result<(), Self::Error> {
        let _ = (table, body);
        Ok(())
    }

    /// Apply response-shaping overrides to a written row before it is returned.
    /// Default: no-op.
    fn apply_overrides(&self, table: &str, row: &mut Map<String, Value>) {
        let _ = (table, row);
    }
}

/// The no-op gate for non-REST callers (CLI seeders, AI-agent object-graph
/// seeders). No hidden-field stripping, no permission checks, no scope — the
/// caller is trusted. Its error type is [`NestedError`].
pub struct NoGate;

#[async_trait::async_trait]
impl NestedWriteGate for NoGate {
    type Error = NestedError;
}

/// Write a parent row plus every declared reverse-FK child subtree on the open
/// transaction `tx`, with NO security gating — the direct-caller entry point.
///
/// This is the public unlock behind gaps4 #77: a CLI seeder or agent hands ONE
/// nested JSON document to ONE call and gets the parent + children (+ any M2M
/// junctions carried in each node's body) written atomically. The caller owns
/// the transaction: on success `tx` is left open for the caller to
/// `commit()`; on any error the caller drops `tx` and the DB rolls the whole
/// tree back (no row becomes durable).
///
/// `spec` maps each table to its `(json_field, child_table)` children; `meta`
/// is the parent model; `body` is the nested document (mutated in place as
/// declared child arrays are split out). Returns the parent object with each
/// child array hydrated with the rows just written.
///
/// # Example
///
/// ```ignore
/// let mut tx = umbral::db::begin().await?;
/// let mut spec = NestedSpec::new();
/// spec.insert("author".into(), vec![("posts".into(), "post".into())]);
/// let mut body = /* { "name": "Ada", "posts": [ { "title": "…" } ] } */;
/// let author = write_nested_tree(&spec, &author_meta, &mut body, &mut tx).await?;
/// tx.commit().await?; // nothing is durable until here
/// ```
pub async fn write_nested_tree(
    spec: &NestedSpec,
    meta: &ModelMeta,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
) -> Result<Map<String, Value>, NestedError> {
    write_nested_tree_gated(&NoGate, spec, meta, body, tx).await
}

/// Gated variant of [`write_nested_tree`]: the same atomic tree walk, but every
/// child row passes through `gate` (hidden-strip, own create-permission,
/// object-scope, overrides) and the returned error type is the gate's. This is
/// the seam umbral-rest wraps to keep its REST security intact while the walk
/// itself lives here.
pub async fn write_nested_tree_gated<G: NestedWriteGate>(
    gate: &G,
    spec: &NestedSpec,
    meta: &ModelMeta,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
) -> Result<Map<String, Value>, G::Error> {
    let mut nodes: usize = 0;
    write_nested_subtree(gate, spec, meta, body, tx, 0, &mut nodes).await
}

/// Low-level entry that exposes the recursion's `depth` and a shared `nodes`
/// counter, so a caller weaving several subtrees into one larger write (e.g. an
/// UPDATE upserting a mix of existing and new children) can thread ONE
/// tree-wide node budget and the real depth through repeated calls. Most
/// callers want [`write_nested_tree`] / [`write_nested_tree_gated`] instead.
///
/// The subtree ROOT is not charged against `nodes` here (only its descendants
/// are), matching the convention where a caller that already counted the root
/// passes the running total in.
pub async fn write_nested_subtree<G: NestedWriteGate>(
    gate: &G,
    spec: &NestedSpec,
    meta: &ModelMeta,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
    depth: usize,
    nodes: &mut usize,
) -> Result<Map<String, Value>, G::Error> {
    insert_tree(gate, spec, meta, body, tx, depth, nodes).await
}

/// Recursively insert a row and every declared child subtree on the open `tx`.
async fn insert_tree<G: NestedWriteGate>(
    gate: &G,
    spec: &NestedSpec,
    meta: &ModelMeta,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
    depth: usize,
    nodes: &mut usize,
) -> Result<Map<String, Value>, G::Error> {
    if depth > MAX_NEST_DEPTH {
        return Err(NestedError::MaxDepth(MAX_NEST_DEPTH).into());
    }

    // Split THIS table's declared nested arrays out of the body BEFORE the
    // insert, so they're never handed to the row insert as unknown columns.
    let specs = spec.get(&meta.table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, child_table) in &specs {
        let items = match body.remove(field) {
            Some(Value::Array(a)) => a,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                return Err(NestedError::BadInput(format!(
                    "nested field `{field}` must be an array"
                ))
                .into());
            }
        };
        if items.is_empty() {
            continue;
        }
        let child = resolve_child_meta(gate, child_table)?;
        let fk = child_fk_to(&child, &meta.table)?.to_string();
        pending.push((field.clone(), child, fk, items));
    }

    // Anything array-shaped still in `body` is an undeclared nested relation.
    // Reject it loudly rather than letting the insert silently drop it (the
    // level-2+ silent-data-loss footgun). A scalar array *column* (ArrayField)
    // or an M2M write-through list stays allowed — it maps to a real relation.
    reject_undeclared_nested(meta, body)?;

    // Insert this row on the tx. `insert_json_in_tx` also mirrors any declared
    // M2M relation fields still in `body` into their junction tables on the
    // same tx, so M2M links ride along atomically.
    let mut row = DynQuerySet::for_meta(meta)
        .insert_json_in_tx(body, tx)
        .await?;
    let pk_name = pk_column(meta)?.name.clone();
    let pk_value = row.get(&pk_name).cloned().ok_or_else(|| {
        NestedError::BadInput("nested: row has no primary key after insert".into())
    })?;
    gate.apply_overrides(&meta.table, &mut row);

    // Recurse into each declared child array.
    for (field, child, fk, items) in pending {
        let mut created = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(mut child_body) = item else {
                return Err(NestedError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                ))
                .into());
            };
            // Count this child against the whole-tree budget.
            *nodes += 1;
            if *nodes > gate.max_nodes() {
                return Err(NestedError::MaxNodes(gate.max_nodes()).into());
            }
            // Per-child gating: strip denied fields (BEFORE the FK injection so
            // the FK survives), enforce the child's own create permission,
            // inject the FK from the parent's PK, then object-scope the create.
            gate.strip_hidden(&child.table, &mut child_body);
            gate.check_create(&child.table)?;
            child_body.insert(fk.clone(), pk_value.clone());
            gate.scope_create(&child.table, &child_body).await?;
            // `Box::pin` breaks the otherwise-infinitely-sized async recursion.
            let crow = Box::pin(insert_tree(
                gate,
                spec,
                &child,
                &mut child_body,
                tx,
                depth + 1,
                nodes,
            ))
            .await?;
            created.push(Value::Object(crow));
        }
        row.insert(field, Value::Array(created));
    }
    Ok(row)
}

/// Resolve a child model's [`ModelMeta`] by table name, scanning every
/// registered plugin's models. A gate may veto the table first (REST enforces
/// exposure/block-list parity here).
fn resolve_child_meta<G: NestedWriteGate>(gate: &G, table: &str) -> Result<ModelMeta, NestedError> {
    if !gate.allow_table(table) {
        return Err(NestedError::BadInput(format!(
            "nested: child table `{table}` is not writable here"
        )));
    }
    for plugin in crate::migrate::registered_plugins() {
        for m in crate::migrate::models_for_plugin(&plugin) {
            if m.table == table {
                return Ok(m);
            }
        }
    }
    Err(NestedError::BadInput(format!(
        "nested: unknown child table `{table}`"
    )))
}

/// The child column whose foreign key targets `parent_table`. Errors when there
/// are zero or multiple such columns (the latter is ambiguous).
pub fn child_fk_to<'a>(child: &'a ModelMeta, parent_table: &str) -> Result<&'a str, NestedError> {
    let mut found: Option<&str> = None;
    for c in &child.fields {
        if c.fk_target.as_deref() == Some(parent_table) {
            if found.is_some() {
                return Err(NestedError::BadInput(format!(
                    "nested: `{}` has multiple FKs to `{}` — ambiguous",
                    child.table, parent_table
                )));
            }
            found = Some(c.name.as_str());
        }
    }
    found.ok_or_else(|| {
        NestedError::BadInput(format!(
            "nested: `{}` has no foreign key to `{}`",
            child.table, parent_table
        ))
    })
}

/// Reject an array-of-values under a key that is neither a column, an M2M
/// relation, nor a declared writable nested relation on this table. Called
/// AFTER the declared nested arrays have been split out of `body`, so anything
/// array-shaped left over is an undeclared nesting attempt that the row insert
/// would otherwise silently drop.
fn reject_undeclared_nested(
    meta: &ModelMeta,
    body: &Map<String, Value>,
) -> Result<(), NestedError> {
    for (key, val) in body {
        if !matches!(val, Value::Array(_)) {
            continue;
        }
        let is_column = meta.fields.iter().any(|c| c.name == *key);
        let is_m2m = meta.m2m_relations.iter().any(|r| r.field_name == *key);
        if !is_column && !is_m2m {
            return Err(NestedError::BadInput(format!(
                "`{key}` on `{}` is not a column, an M2M relation, or a declared writable \
                 nested relation — declare it in the nested spec or remove it from the payload",
                meta.table
            )));
        }
    }
    Ok(())
}

/// The single primary-key column of `meta`, or a [`NestedError`] if none.
fn pk_column(meta: &ModelMeta) -> Result<&Column, NestedError> {
    meta.pk_column().ok_or_else(|| {
        NestedError::BadInput(format!(
            "nested: `{}` has no primary key column",
            meta.table
        ))
    })
}

// ── UPDATE (reconciliation) — gaps4 #91 ────────────────────────────────────
//
// The sibling of the CREATE walk above: update a parent row, then upsert every
// declared nested subtree on the SAME transaction. Lifted out of umbral-rest
// (the update half of #77) so a non-REST caller gets the same atomic
// reconciliation. REST re-supplies its per-child security via the same
// `NestedWriteGate` (now with `check_update`).
//
// **Reconciliation policy — upsert, no implicit deletes.** At every level, a
// nested item carrying the row's primary key UPDATES that row, scoped to its
// parent via the FK so one parent's payload can never mutate another parent's
// child (a cross-parent id is a [`NestedError::NotFound`]). An item WITHOUT the
// pk is CREATED (its whole subtree inserted, via [`insert_tree`]) with its FK
// set to the parent. Rows absent from the payload are left untouched.

/// The parent anchor threaded down one nesting level during an update.
struct NestAnchor<'a> {
    /// Column on the child that FKs back to this parent.
    fk_col: &'a str,
    /// Parent pk as a typed `Value` — set as the child's FK on a CREATE.
    pk_value: &'a Value,
    /// Parent pk as a `String` — scopes an UPDATE's ownership check.
    pk_str: &'a str,
}

/// Update a parent row plus every declared reverse-FK child subtree (upsert) on
/// the open transaction `tx`, with NO security gating — the direct-caller entry
/// point, symmetric with [`write_nested_tree`].
///
/// `pk_name`/`id` identify the parent row to update; `body` is the nested
/// document (mutated in place as declared child arrays are split out). The
/// caller owns the transaction (see [`write_nested_tree`] for the commit/rollback
/// contract). Returns the parent object with each child array hydrated with the
/// rows just upserted.
pub async fn update_nested_tree(
    spec: &NestedSpec,
    meta: &ModelMeta,
    pk_name: &str,
    id: &str,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
) -> Result<Map<String, Value>, NestedError> {
    update_nested_tree_gated(&NoGate, spec, meta, pk_name, id, body, tx).await
}

/// Gated variant of [`update_nested_tree`]: the same atomic upsert walk, but
/// every child passes through `gate` (create/update permission, hidden-strip,
/// object-scope on newly-created subtrees, overrides) and the returned error
/// type is the gate's. This is the seam umbral-rest wraps to keep its REST
/// security intact while the reconciliation lives here.
pub async fn update_nested_tree_gated<G: NestedWriteGate>(
    gate: &G,
    spec: &NestedSpec,
    meta: &ModelMeta,
    pk_name: &str,
    id: &str,
    body: &mut Map<String, Value>,
    tx: &mut Transaction,
) -> Result<Map<String, Value>, G::Error> {
    // Split the parent's declared nested arrays out of the body.
    let specs = spec.get(&meta.table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, child_table) in &specs {
        let items = match body.remove(field) {
            Some(Value::Array(a)) => a,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                return Err(NestedError::BadInput(format!(
                    "nested field `{field}` must be an array"
                ))
                .into());
            }
        };
        if items.is_empty() {
            continue;
        }
        let child = resolve_child_meta(gate, child_table)?;
        let fk = child_fk_to(&child, &meta.table)?.to_string();
        pending.push((field.clone(), child, fk, items));
    }

    // Anything array-shaped left in `body` is an undeclared nested relation.
    reject_undeclared_nested(meta, body)?;

    // Update the parent's own columns on the tx. A body with only nested arrays
    // (no scalar columns) is a safe no-op — `update_json_in_tx` returns 0
    // rather than emitting an UPDATE with no SET clause.
    DynQuerySet::for_meta(meta)
        .filter_eq_string(pk_name, id)
        .update_json_in_tx(body, tx)
        .await?;

    // The parent's typed pk value, read on the tx — used as the FK when
    // CREATING a child (so an i64 FK gets a number, not the stringified id).
    let pk_value = {
        let parent = fetch_one_in_tx(meta, pk_name, id, tx).await?;
        parent
            .get(pk_name)
            .cloned()
            .ok_or_else(|| NestedError::BadInput("nested: parent row has no primary key".into()))?
    };

    // Upsert each child subtree on the same tx, threading ONE tree-wide node
    // budget across every child (H3 spans the whole update, not one subtree).
    let mut nodes: usize = 0;
    let ctx = Upsert { gate, spec };
    let mut results: Vec<(String, Vec<Value>)> = Vec::new();
    for (field, child, fk, items) in pending {
        let mut upserted = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(child_body) = item else {
                return Err(NestedError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                ))
                .into());
            };
            let crow = upsert_child(
                &ctx,
                &child,
                child_body,
                &NestAnchor {
                    fk_col: &fk,
                    pk_value: &pk_value,
                    pk_str: id,
                },
                tx,
                1,
                &mut nodes,
            )
            .await?;
            upserted.push(Value::Object(crow));
        }
        results.push((field, upserted));
    }

    // Read the parent back and attach the upserted children (the same shape
    // `write_nested_tree` returns: only the children in the payload, hydrated).
    let mut parent = fetch_one_in_tx(meta, pk_name, id, tx).await?;
    gate.apply_overrides(&meta.table, &mut parent);
    for (field, children) in results {
        parent.insert(field, Value::Array(children));
    }
    Ok(parent)
}

/// Recursively upsert one nested item (and its own subtree) during an update.
///
/// The `anchor` carries the FK column plus the parent pk in both typed form
/// (set as the child's FK on a CREATE) and string form (scopes the ownership
/// check on an UPDATE). An item WITH its primary key UPDATES that row — but only
/// if it belongs to this parent (`FK == parent pk`), else [`NestedError::NotFound`]
/// — then recurses into its own declared nested arrays (upserting grandchildren).
/// An item WITHOUT a pk CREATEs the whole subtree via [`insert_tree`].
/// The invariants threaded unchanged through the upsert recursion (the gate and
/// the full nested spec don't vary level to level) — bundled so the recursive
/// [`upsert_child`] stays within the argument-count budget, mirroring how the
/// REST wrapper bundled the same into its `NestCtx`.
struct Upsert<'a, G: NestedWriteGate> {
    gate: &'a G,
    spec: &'a NestedSpec,
}

async fn upsert_child<G: NestedWriteGate>(
    ctx: &Upsert<'_, G>,
    meta: &ModelMeta,
    mut body: Map<String, Value>,
    anchor: &NestAnchor<'_>,
    tx: &mut Transaction,
    depth: usize,
    nodes: &mut usize,
) -> Result<Map<String, Value>, G::Error> {
    let gate = ctx.gate;
    let spec = ctx.spec;
    if depth > MAX_NEST_DEPTH {
        return Err(NestedError::MaxDepth(MAX_NEST_DEPTH).into());
    }
    // Count this child row against the whole-tree cap.
    *nodes += 1;
    if *nodes > gate.max_nodes() {
        return Err(NestedError::MaxNodes(gate.max_nodes()).into());
    }

    let pk_col = pk_column(meta)?.name.clone();
    let supplied_pk = body.get(&pk_col).filter(|v| !v.is_null()).cloned();

    let Some(pk_json) = supplied_pk else {
        // CREATE — strip denied fields + own create permission, set the FK,
        // then insert the whole subtree. `insert_tree` gates the descendants
        // (strip/create/scope) as it recurses; the root gating is here so the
        // create branch matches the top-level create walk.
        gate.strip_hidden(&meta.table, &mut body);
        gate.check_create(&meta.table)?;
        body.insert(anchor.fk_col.to_string(), anchor.pk_value.clone());
        return Box::pin(insert_tree(gate, spec, meta, &mut body, tx, depth, nodes)).await;
    };

    // UPDATE — enforce this child's own update permission before any read.
    let pk_str = json_pk_to_string(&pk_json).ok_or_else(|| {
        NestedError::BadInput(format!("nested `{}`: invalid primary key", meta.table))
    })?;
    gate.check_update(&meta.table)?;

    // Ownership gate: the row must exist AND belong to THIS parent. Checked as
    // an explicit read (not via the update's affected count) so the not-found
    // can name the cross-parent case, and so a body with no scalar columns
    // still verifies ownership before recursing into grandchildren.
    let owned = DynQuerySet::for_meta(meta)
        .filter_eq_string(&pk_col, &pk_str)
        .filter_eq_string(anchor.fk_col, anchor.pk_str)
        .fetch_one_json_in_tx(tx)
        .await
        .map_err(NestedError::from)?;
    if owned.is_none() {
        return Err(NestedError::NotFound(format!(
            "nested `{}`: no row with {} = {} belonging to this parent",
            meta.table, pk_col, pk_str
        ))
        .into());
    }

    // Split this row's own nested arrays out before the scalar update.
    let specs = spec.get(&meta.table).cloned().unwrap_or_default();
    let mut pending: Vec<(String, ModelMeta, String, Vec<Value>)> = Vec::new();
    for (field, gc_table) in &specs {
        let items = match body.remove(field) {
            Some(Value::Array(a)) => a,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => {
                return Err(NestedError::BadInput(format!(
                    "nested field `{field}` must be an array"
                ))
                .into());
            }
        };
        if items.is_empty() {
            continue;
        }
        let gc = resolve_child_meta(gate, gc_table)?;
        let gc_fk = child_fk_to(&gc, &meta.table)?.to_string();
        pending.push((field.clone(), gc, gc_fk, items));
    }

    // The pk is the WHERE key; the FK anchors ownership — never let the payload
    // rewrite either via the SET clause. Then strip the child's hidden/denied
    // fields so a nested UPDATE can't set them either.
    body.remove(&pk_col);
    body.remove(anchor.fk_col);
    gate.strip_hidden(&meta.table, &mut body);
    // Anything array-shaped still here is an undeclared nested relation.
    reject_undeclared_nested(meta, &body)?;
    DynQuerySet::for_meta(meta)
        .filter_eq_string(&pk_col, &pk_str)
        .update_json_in_tx(&body, tx)
        .await?;
    let mut row = fetch_one_in_tx(meta, &pk_col, &pk_str, tx).await?;
    let this_pk_value = row
        .get(&pk_col)
        .cloned()
        .ok_or_else(|| NestedError::BadInput("nested: row has no primary key".into()))?;
    gate.apply_overrides(&meta.table, &mut row);

    // Recurse into grandchildren (upsert), scoped to this row.
    for (field, gc, gc_fk, items) in pending {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let Value::Object(gc_body) = item else {
                return Err(NestedError::BadInput(format!(
                    "items in nested `{field}` must be objects"
                ))
                .into());
            };
            let grow = Box::pin(upsert_child(
                ctx,
                &gc,
                gc_body,
                &NestAnchor {
                    fk_col: &gc_fk,
                    pk_value: &this_pk_value,
                    pk_str: &pk_str,
                },
                tx,
                depth + 1,
                nodes,
            ))
            .await?;
            out.push(Value::Object(grow));
        }
        row.insert(field, Value::Array(out));
    }
    Ok(row)
}

/// Read one row back on the open transaction (so an upsert's response reflects
/// its own uncommitted writes), erroring if it vanished.
async fn fetch_one_in_tx(
    meta: &ModelMeta,
    pk: &str,
    pk_str: &str,
    tx: &mut Transaction,
) -> Result<Map<String, Value>, NestedError> {
    DynQuerySet::for_meta(meta)
        .filter_eq_string(pk, pk_str)
        .fetch_one_json_in_tx(tx)
        .await?
        .ok_or_else(|| {
            NestedError::BadInput("nested: row updated but disappeared on read-back".into())
        })
}

/// Stringify a JSON primary-key value (number or string) for a
/// `filter_eq_string` lookup. Any other JSON shape is not a valid pk.
fn json_pk_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
