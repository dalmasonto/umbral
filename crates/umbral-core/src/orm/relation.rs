//! Lazy relation-traversal handles.
//!
//! Django lets you traverse a relationship graph from any loaded object —
//! `post.author`, `user.developer.company.owner` — and umbral mirrors that
//! reach with a *method-based, awaited* equivalent (Rust has no lazy
//! attribute hook). The keystone is [`Relation<T>`]: a handle that is
//! **both awaitable and chainable**, so a deep to-one chain can be written
//! fluently without a forced `.await` between every hop.
//!
//! Phase 1, Task 1 (this module's initial cut) delivers the handle plus the
//! single forward-FK terminal (`post.author().await?`). The path is built
//! purely in memory; nothing touches the database until a terminal
//! (`get`/`get_opt`/`exists`, or awaiting the handle). Deep multi-hop
//! resolution, to-many widening to `QuerySet`, and the derive-emitted
//! accessors land in later tasks.
//!
//! See `docs/specs/orm-relation-traversal.md` for the full design.

use std::future::{Future, IntoFuture};
use std::marker::PhantomData;
use std::pin::Pin;

use sea_query::{Alias, Expr, Query, SimpleExpr};

use crate::db::DbPool;
use crate::orm::queryset::Manager;
use crate::orm::{HydrateRelated, Model, Predicate};

// =========================================================================
// Hop descriptors — the static metadata the derive (Task 5) will pass.
// =========================================================================

/// The kind of a single relation hop.
///
/// `Fk` / `O2OForward` / `O2OReverse` are **to-one** (resolve to a single
/// row, stay a [`Relation`]); `M2M` / `ReverseFk` are **to-many** (fan out,
/// widen to a `QuerySet` — wired in a later task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HopKind {
    /// A forward foreign key: the FK column lives on the *from* table and
    /// points at the *to* table's primary key.
    Fk,
    /// A forward one-to-one, child side: like [`HopKind::Fk`] but the FK
    /// carries a `UNIQUE` constraint.
    O2OForward,
    /// A reverse one-to-one, parent side: the FK column lives on the *to*
    /// table and points back at the *from* row.
    O2OReverse,
    /// A forward many-to-many through a junction table.
    M2M,
    /// A reverse foreign key: many child rows point back at the *from* row.
    ReverseFk,
}

impl HopKind {
    /// Whether this hop resolves to at most one row (stays a [`Relation`]).
    pub fn is_to_one(self) -> bool {
        matches!(
            self,
            HopKind::Fk | HopKind::O2OForward | HopKind::O2OReverse
        )
    }
}

/// The junction-table descriptor for a many-to-many hop (unused until the
/// M2M task, but part of the static shape the derive emits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JunctionSpec {
    /// The junction (through) table name.
    pub table: &'static str,
    /// The junction column referencing the *from* side.
    pub parent_column: &'static str,
    /// The junction column referencing the *to* side.
    pub target_column: &'static str,
}

/// A single hop in a relation path — the static descriptor the derive
/// passes to [`to_one_hop`] (and, later, the to-many builders).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HopSpec {
    /// The relation kind.
    pub kind: HopKind,
    /// SQL table the hop starts from.
    pub from_table: &'static str,
    /// SQL table the hop lands on.
    pub to_table: &'static str,
    /// The foreign-key column name driving the hop.
    pub fk_column: &'static str,
    /// `true` when `fk_column` lives on `from_table` (forward FK/O2O);
    /// `false` when it lives on `to_table` (reverse O2O parent-side).
    pub fk_on_from: bool,
    /// Junction descriptor for `M2M` hops; `None` for FK/O2O/reverse-FK.
    pub junction: Option<JunctionSpec>,
}

// =========================================================================
// Path model — built purely in memory as the chain is written.
// =========================================================================

/// The starting node of a relation path.
///
/// An enum so future base shapes (e.g. a multi-row base for chains rooted
/// at a `QuerySet`) can be added without breaking the single-object case.
#[derive(Debug, Clone)]
pub enum PathBase {
    /// A path rooted at a single loaded object, identified by its table and
    /// primary key.
    SinglePk {
        /// The root object's SQL table.
        table: &'static str,
        /// The root object's primary-key column name.
        pk_column: &'static str,
        /// The root object's primary-key value.
        pk_value: sea_query::Value,
    },
}

/// An ordered relation path: a base node plus the hops taken from it.
///
/// Built entirely in memory; consumed at a terminal to emit one query.
#[derive(Debug, Clone)]
pub struct RelPath {
    /// The starting node.
    pub base: PathBase,
    /// The hops taken from [`RelPath::base`], in order.
    pub hops: Vec<HopSpec>,
}

impl RelPath {
    /// Extend this path by one hop, returning the new path.
    fn push(mut self, hop: HopSpec) -> Self {
        self.hops.push(hop);
        self
    }
}

/// Anything a relation chain can start from: a loaded object (`&From`) or an
/// existing relation handle (`Relation<From>` / `&Relation<From>`), so the
/// next accessor extends the path rather than forcing a query.
pub trait RelationSource<From: Model> {
    /// The path accumulated so far — a bare base for an object, or the
    /// carried path for an existing handle.
    fn into_rel_path(self) -> RelPath;
}

impl<From: Model> RelationSource<From> for &From {
    fn into_rel_path(self) -> RelPath {
        let pk_column = pk_column_name::<From>();
        RelPath {
            base: PathBase::SinglePk {
                table: From::TABLE,
                pk_column,
                pk_value: self.primary_key().into(),
            },
            hops: Vec::new(),
        }
    }
}

impl<From: Model> RelationSource<From> for Relation<From> {
    fn into_rel_path(self) -> RelPath {
        self.path
    }
}

impl<From: Model> RelationSource<From> for &Relation<From> {
    fn into_rel_path(self) -> RelPath {
        self.path.clone()
    }
}

/// The primary-key column name of a model, from its `FIELDS` metadata.
/// Falls back to `"id"` for the (derive-impossible) no-PK case.
fn pk_column_name<M: Model>() -> &'static str {
    M::FIELDS
        .iter()
        .find(|f| f.primary_key)
        .map(|f| f.name)
        .unwrap_or("id")
}

// =========================================================================
// The handle.
// =========================================================================

/// A lazy, single-target traversal handle for a to-one relation.
///
/// Returned by a to-one accessor (forward FK / O2O). It is simultaneously
/// **awaitable** (`let a = post.author().await?`) and **chainable** (the
/// next to-one accessor extends the in-memory path instead of querying), so
/// a deep to-one chain resolves at one terminal.
///
/// Nothing touches the database until a terminal (`get`/`get_opt`/`exists`,
/// or awaiting via [`IntoFuture`]).
pub struct Relation<T: Model> {
    path: RelPath,
    explicit_pool: Option<DbPool>,
    _t: PhantomData<T>,
}

impl<T: Model> Relation<T> {
    /// Pin this relation's terminal query to an explicit SQLite pool.
    ///
    /// Wins over the ambient default — used by tests that drive the ORM
    /// without `App::build()`. The Postgres counterpart is [`Self::on_pg`].
    pub fn on(mut self, pool: &sqlx::SqlitePool) -> Self {
        self.explicit_pool = Some(DbPool::Sqlite(pool.clone()));
        self
    }

    /// Pin this relation's terminal query to an explicit Postgres pool.
    pub fn on_pg(mut self, pool: &sqlx::PgPool) -> Self {
        self.explicit_pool = Some(DbPool::Postgres(pool.clone()));
        self
    }
}

impl<T> Relation<T>
where
    T: Model
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + HydrateRelated,
{
    /// Resolve a required to-one relation, erroring if the target row is
    /// absent.
    ///
    /// A missing target on a required FK means referential integrity is
    /// already broken; that surfaces as [`sqlx::Error::RowNotFound`], never
    /// a silent `None` (per the spec's return-shape rules). For a
    /// legitimately-nullable relation use [`Self::get_opt`].
    pub async fn get(self) -> Result<T, sqlx::Error> {
        self.get_opt().await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// Resolve a to-one relation, returning `None` when the target row is
    /// absent (the nullable-FK / reverse-O2O shape).
    pub async fn get_opt(self) -> Result<Option<T>, sqlx::Error> {
        self.terminal_queryset()?.first().await
    }

    /// Whether the to-one target row exists, without materialising it.
    pub async fn exists(self) -> Result<bool, sqlx::Error> {
        self.terminal_queryset()?.exists().await
    }

    /// Build the leaf `QuerySet<T>` that selects the traversal target.
    ///
    /// Task 1 resolves a **single** to-one hop. The path→SQL emission for
    /// deep and to-many chains extends this in later tasks; an unsupported
    /// shape errors loudly here rather than silently returning wrong rows.
    fn terminal_queryset(self) -> Result<crate::orm::QuerySet<T>, sqlx::Error> {
        let Relation {
            path,
            explicit_pool,
            ..
        } = self;

        if path.hops.len() != 1 {
            return Err(protocol_error(
                "multi-hop relation resolution is not implemented in Phase 1 Task 1 \
                 (single forward-FK only); deep chains land in a later task",
            ));
        }
        let hop = path.hops[0];
        if !hop.kind.is_to_one() {
            return Err(protocol_error(
                "to-many relations resolve to a QuerySet, not a single row \
                 (wired in a later task)",
            ));
        }

        let PathBase::SinglePk {
            table: base_table,
            pk_column: base_pk_column,
            pk_value: base_pk_value,
        } = path.base;

        let to_pk_col = pk_column_name::<T>();

        // Two directions, both a single query:
        //
        // - `fk_on_from` (forward FK/O2O): the FK column lives on the *from*
        //   table. Select the target whose PK is the FK value stored on the
        //   single root row:
        //     WHERE <to.pk> IN (SELECT <fk_col> FROM <from> WHERE <from.pk> = ?)
        //
        // - reverse O2O parent-side: the FK column lives on the *to* table
        //   and points back at the root row's PK:
        //     WHERE <fk_col> = <root pk>
        let predicate: Predicate<T> = if hop.fk_on_from {
            let mut sub = Query::select();
            sub.column(Alias::new(hop.fk_column))
                .from(Alias::new(base_table))
                .and_where(
                    Expr::col(Alias::new(base_pk_column)).eq(SimpleExpr::Value(base_pk_value)),
                );
            Predicate::new(Expr::col(Alias::new(to_pk_col)).in_subquery(sub))
        } else {
            Predicate::new(
                Expr::col(Alias::new(hop.fk_column)).eq(SimpleExpr::Value(base_pk_value)),
            )
        };

        let qs = Manager::<T>::new().filter(predicate);
        Ok(match explicit_pool {
            Some(DbPool::Sqlite(p)) => qs.on(&p),
            Some(DbPool::Postgres(p)) => qs.on_pg(&p),
            None => qs,
        })
    }
}

impl<T> IntoFuture for Relation<T>
where
    T: Model
        + for<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow>
        + for<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow>
        + HydrateRelated,
{
    type Output = Result<T, sqlx::Error>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    /// Awaiting a `Relation<T>` is an alias of [`Relation::get`].
    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.get())
    }
}

// =========================================================================
// Constructor.
// =========================================================================

/// Build a to-one [`Relation<To>`] by extending `src`'s path with one hop.
///
/// The single door a to-one accessor (the Task 5 derive, or a hand-written
/// call) uses to produce a handle. Nothing touches the database — the path
/// is assembled in memory and resolved at a terminal.
pub fn to_one_hop<From: Model, To: Model>(
    src: impl RelationSource<From>,
    hop: HopSpec,
) -> Relation<To> {
    Relation {
        path: src.into_rel_path().push(hop),
        explicit_pool: None,
        _t: PhantomData,
    }
}

/// A loud protocol error for an unsupported path shape.
fn protocol_error(msg: &str) -> sqlx::Error {
    sqlx::Error::Protocol(msg.to_string())
}
