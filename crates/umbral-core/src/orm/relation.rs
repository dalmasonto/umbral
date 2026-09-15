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
use crate::orm::queryset::{Manager, QuerySet};
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
    /// Whether the hop's target is guaranteed present: a `NOT NULL` forward FK
    /// is `required`; a nullable forward FK or a reverse-O2O (whose parent-side
    /// row may simply not exist) is not. Descriptive metadata only — reserved
    /// for a future codegen phase — and not read at runtime today: the
    /// accessor's return type is uniformly `Relation<T>` regardless of this
    /// flag, and it is the CALLER's choice of terminal (`get()` for `T`,
    /// erroring on an absent row, vs `get_opt()` for `Option<T>`) that picks
    /// the shape, not this field. The resolver uses INNER joins either way.
    pub required: bool,
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
    /// A path rooted at a table itself (no specific row) — the base for
    /// `select_related` / aggregate JOINs, which hang off the outer query's
    /// own FROM rather than a `WHERE pk = ?`. Produced by [`RelPath::from_path`].
    TableRoot {
        /// The root table.
        table: &'static str,
    },
}

/// Whether a nullable hop LEFT-joins (keep the parent row) or INNER-joins
/// (a null link drops the row). Traversal uses `Inner`; hydration /
/// `select_related` uses `LeftForNullable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullJoinPolicy {
    Inner,
    LeftForNullable,
    /// Unconditional `RIGHT JOIN`, ignoring `HopSpec::required` — the
    /// explicit `.right_join_related(...)` override `apply_join_related`
    /// (`queryset/mod.rs`) applies to a single caller-chosen hop.
    Right,
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

    /// Resolve a `__`-separated relation path off a typed root, into a
    /// [`RelPath`] rooted at [`PathBase::TableRoot`] — the string-resolver
    /// counterpart to composing `to_one_hop`/`to_many_hop` calls by hand.
    ///
    /// `RelPath::from_path::<Post>("author__company")` builds the same
    /// two-hop path a chain of `to_one_hop` calls would, but from a bare
    /// path string — the shape `select_related` / `join_related` / `values`
    /// traversal, and the later aggregate/hydration engine (heavy-relations
    /// epic, sub-projects B/C), resolve a relation name string through.
    ///
    /// Each `__`-separated segment resolves, in order:
    /// 1. A forward FK/O2O field on the current table (`T::FIELDS` for the
    ///    typed root; the migrate registry's `Column::fk_target` for a
    ///    deeper hop, where there is no `T: Model` to hang a const off).
    ///    `O2OForward` when the field/column is `unique`, else `Fk`.
    /// 2. A forward M2M field (`T::M2M_RELATIONS`, root hop only — see the
    ///    "Deferred" note below).
    /// 3. A reverse-FK: a declared `#[umbral(reverse_fk = "...")]` relation
    ///    (`T::REVERSE_FK_RELATIONS`, root hop only) or an auto-discovered
    ///    child table whose FK targets the current table, matched against
    ///    `relation` by the same conventional name forms the `annotate_related`
    ///    / `prefetch_related` accessors use (bare table name, struct name in
    ///    `snake_case` / lowercase, or any of those with a `_set` suffix) —
    ///    see [`crate::orm::queryset::discover_reverse_relation_by_table`].
    ///
    /// Errors loudly — never silently drops a segment — on an unresolved
    /// segment (names the bad segment and the table it failed to resolve
    /// against) or an ambiguous auto-discovered reverse-FK (names every
    /// candidate and points at the disambiguation escape hatches).
    ///
    /// Deferred (not in Plan A Task 2's scope — no test exercises it, and
    /// none of B/C's Task-1-dependent work needs it yet): a deeper-than-root
    /// M2M segment. The migrate registry's `ModelMeta` carries plain
    /// columns, not `M2MRelationSpec` metadata, so resolving `a__b__c` where
    /// `b` is an M2M field on the (registry-only) intermediate table `a`
    /// would need the registry to carry M2M relation metadata too — tracked
    /// as a follow-up, not a silent gap: this method errors loudly (`` `b`
    /// on `<table>` is not a foreign key ``) rather than resolving wrongly.
    pub fn from_path<T: Model>(path: &str) -> Result<RelPath, sqlx::Error> {
        let segs: Vec<&str> = path.split("__").filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return Err(protocol_error("empty relation path"));
        }

        // A deeper hop (segs[1..]) resolves a forward-FK column against an
        // intermediate table via the migrate registry (no `T::FIELDS` const
        // once we're off the typed root); fetch it once, up front, only when
        // the path actually needs it — a single-segment path stays
        // registry-free, matching the rest of the Task-1 object-rooted
        // machinery's "no App needed for one hop" property.
        let registered = if segs.len() > 1 {
            Some(crate::migrate::registered_models_opt().ok_or_else(|| {
                protocol_error(
                    "no model registry available to resolve a multi-segment relation \
                     path — build an App (which registers models) before resolving a \
                     deep `from_path` chain",
                )
            })?)
        } else {
            None
        };

        let mut hops: Vec<HopSpec> = Vec::with_capacity(segs.len());
        let mut current_table: &'static str;

        // Hop 0, off the typed root.
        if let Some(f) = T::FIELDS.iter().find(|f| f.name == segs[0]) {
            let tgt = f.fk_target.ok_or_else(|| {
                protocol_error(&format!(
                    "`{}` on `{}` is not a relation (no fk_target)",
                    segs[0],
                    T::NAME
                ))
            })?;
            hops.push(HopSpec {
                kind: if f.unique {
                    HopKind::O2OForward
                } else {
                    HopKind::Fk
                },
                from_table: T::TABLE,
                to_table: tgt,
                fk_column: f.name,
                fk_on_from: true,
                required: !f.nullable,
                junction: None,
            });
            current_table = tgt;
        } else if let Some(rel) = T::M2M_RELATIONS.iter().find(|r| r.field_name == segs[0]) {
            // Junction convention: `<table>_<field>` / `parent_id` /
            // `child_id` — the same shape the derive emits (see
            // `umbral-macros`' `FieldKind::Many2Many` arm) and `orm::m2m`
            // documents. `M2MRelationSpec` doesn't carry the junction name
            // (only `field_name`/`target_table`/`target_name`), so it's
            // rebuilt here by the same convention, not read off a const.
            let junction_table = intern(&format!("{}_{}", T::TABLE, rel.field_name));
            hops.push(HopSpec {
                kind: HopKind::M2M,
                from_table: T::TABLE,
                to_table: rel.target_table,
                fk_column: "",
                fk_on_from: false,
                required: false,
                junction: Some(JunctionSpec {
                    table: junction_table,
                    parent_column: "parent_id",
                    target_column: "child_id",
                }),
            });
            current_table = rel.target_table;
        } else if let Some(rev) = reverse_fk_lookup::<T>(segs[0])? {
            hops.push(HopSpec {
                kind: HopKind::ReverseFk,
                from_table: T::TABLE,
                to_table: rev.child_table,
                fk_column: rev.fk_column,
                fk_on_from: false,
                required: false,
                junction: None,
            });
            current_table = rev.child_table;
        } else {
            return Err(protocol_error(&format!(
                "unknown relation `{}` on `{}`",
                segs[0],
                T::NAME
            )));
        }

        // Deeper hops read the migrate registry for `current_table` —
        // forward FK (a column with an `fk_target`) OR reverse FK (a child
        // table whose column points back at `current_table`).
        if let Some(registered) = &registered {
            for seg in &segs[1..] {
                let meta = registered
                    .iter()
                    .find(|m| m.table == current_table)
                    .ok_or_else(|| {
                        protocol_error(&format!("table `{current_table}` not registered"))
                    })?;
                if let Some(col) = meta.fields.iter().find(|c| c.name == *seg) {
                    let tgt = col.fk_target.as_deref().ok_or_else(|| {
                        protocol_error(&format!(
                            "`{seg}` on `{current_table}` is not a foreign key"
                        ))
                    })?;
                    let to_table = intern(tgt);
                    hops.push(HopSpec {
                        kind: if col.unique {
                            HopKind::O2OForward
                        } else {
                            HopKind::Fk
                        },
                        from_table: current_table,
                        to_table,
                        fk_column: intern(seg),
                        fk_on_from: true,
                        required: !col.nullable,
                        junction: None,
                    });
                    current_table = to_table;
                } else if let Some(rev) = reverse_fk_lookup_by_table(current_table, seg)? {
                    hops.push(HopSpec {
                        kind: HopKind::ReverseFk,
                        from_table: current_table,
                        to_table: rev.child_table,
                        fk_column: rev.fk_column,
                        fk_on_from: false,
                        required: false,
                        junction: None,
                    });
                    current_table = rev.child_table;
                } else {
                    return Err(protocol_error(&format!(
                        "unknown relation `{seg}` on `{current_table}`"
                    )));
                }
            }
        }

        Ok(RelPath {
            base: PathBase::TableRoot { table: T::TABLE },
            hops,
        })
    }
}

/// A resolved reverse-FK hop: the child table a segment named, and the FK
/// column on it that points back at the parent (current) table.
struct ReverseHop {
    child_table: &'static str,
    fk_column: &'static str,
}

/// Hop-0 reverse-FK resolution off the typed root `T`: a declared
/// `#[umbral(reverse_fk = "...")] ReverseSet<Child>` field takes precedence
/// (matched by field name, exactly like [`Model::REVERSE_FK_RELATIONS`]'s
/// other consumers); otherwise fall back to the same auto-discovery scan
/// [`crate::orm::queryset::annotate_related`] uses.
fn reverse_fk_lookup<T: Model>(seg: &str) -> Result<Option<ReverseHop>, sqlx::Error> {
    if let Some(spec) = T::REVERSE_FK_RELATIONS.iter().find(|r| r.field_name == seg) {
        return Ok(Some(ReverseHop {
            child_table: spec.target_table,
            fk_column: spec.fk_column,
        }));
    }
    reverse_fk_lookup_by_table(T::TABLE, seg)
}

/// Reverse-FK resolution for a hop whose parent is an intermediate table
/// reached mid-path (no `T: Model` type parameter to check a declared
/// `REVERSE_FK_RELATIONS` const against — only the migrate registry knows
/// this table). `Ok(None)` when nothing matches (the caller reports the
/// unresolved-segment error, naming the table); `Err` only for a genuine
/// ambiguity (two-or-more candidates), naming every candidate.
fn reverse_fk_lookup_by_table(
    parent_table: &str,
    seg: &str,
) -> Result<Option<ReverseHop>, sqlx::Error> {
    use crate::orm::queryset::{AutoDiscovery, discover_reverse_relation_by_table};
    match discover_reverse_relation_by_table(parent_table, seg) {
        AutoDiscovery::Resolved {
            child_table,
            fk_column,
            ..
        } => Ok(Some(ReverseHop {
            child_table: intern(&child_table),
            fk_column: intern(&fk_column),
        })),
        AutoDiscovery::Ambiguous(candidates) => Err(protocol_error(&format!(
            "umbral::orm::relation::from_path: ambiguous reverse relation `{seg}` on \
             `{parent_table}` — candidates: [{}]; declare a \
             `#[umbral(reverse_fk = \"<fk>\")] ReverseSet<Child>` field (or use the \
             `<child>_via_<field>_set` accessor) to disambiguate",
            candidates.join(", "),
        ))),
        AutoDiscovery::NotFound(_) => Ok(None),
    }
}

/// Intern an owned string into a leaked `&'static str`, deduplicating on
/// content so a repeated name (e.g. every path through the same table)
/// leaks at most once per unique string for the life of the process.
///
/// `HopSpec`'s fields are `&'static str` (the derive's macro-emitted hops
/// are all string literals or `T::TABLE`/`T::NAME` consts — genuinely
/// `'static`), but [`RelPath::from_path`]'s deeper hops read table/column
/// names out of the migrate registry's `ModelMeta`/`Column`, which are
/// owned `String`s reconstructed from a `OnceLock` snapshot — never
/// `'static` themselves. Chosen over widening `HopSpec` to
/// `Cow<'static, str>` (the spec's other option) because that would touch
/// every existing `HopSpec` literal across the derive (5 call sites) AND
/// lose `HopSpec`'s `Copy` impl everywhere it's already copied by value
/// (`relation.rs`, `relation_resolve.rs`, and every hand-built `HopSpec` in
/// the existing traversal test suite) — churn disproportionate to what
/// Task 2 needs. A process-wide intern pool is the standard trick for
/// "occasionally need a `String` to act `'static`" and only touches the
/// NEW resolver path.
fn intern(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = guard.get(s) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    guard.insert(leaked);
    leaked
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

/// A `QuerySet` produced by a to-many relation accessor ([`to_many_hop`])
/// carries the [`RelPath`] it was built from, so a SECOND hop can extend the
/// chain — this is how a deep to-many chain (`dev.software_groups().software()`)
/// is composed by nesting `to_many_hop`/`to_one_hop` before the Task-5 derive
/// generates the accessors. An ordinary `T::objects()` QuerySet carries no
/// path; hopping off it is a programming error (like passing a to-one
/// [`HopKind`] to `to_many_hop`) and panics with a clear message.
impl<From: Model> RelationSource<From> for QuerySet<From> {
    fn into_rel_path(self) -> RelPath {
        self.rel_path.clone().expect(
            "to_many_hop / to_one_hop off a QuerySet requires a QuerySet produced by a \
             relation accessor (it carries the RelPath); a bare `T::objects()` QuerySet \
             has no relation path to extend",
        )
    }
}

impl<From: Model> RelationSource<From> for &QuerySet<From> {
    fn into_rel_path(self) -> RelPath {
        self.rel_path.clone().expect(
            "to_many_hop / to_one_hop off a QuerySet requires a QuerySet produced by a \
             relation accessor (it carries the RelPath); a bare `T::objects()` QuerySet \
             has no relation path to extend",
        )
    }
}

/// The foreign-key column on `Child` that points back at `Parent::TABLE` — the
/// driving column of a reverse-O2O (`user.developer()`) or reverse-FK hop.
///
/// The parent model's derive can't name this column at macro-expansion time:
/// the FK lives on the *child*, whose fields are expanded by a different (and
/// possibly cross-crate) `#[derive(Model)]`. So the Task-5 codegen emits a call
/// to this helper, which resolves the column at runtime from `Child::FIELDS`
/// (the first field whose `fk_target` is `Parent::TABLE`). A well-formed
/// reverse relation always has exactly one such anchoring FK; a model that
/// declares a `OneToOne<Child>` back-link with no forward `ForeignKey<Parent>`
/// on `Child` is malformed and panics here with a message naming both ends.
///
/// Retained as a public helper for type-only reverse resolution (a hand-written
/// or future accessor that has only the two model types). The derive's own
/// reverse-O2O accessor does NOT use it: it knows the child's FK field directly,
/// which is exact even when a child has multiple FKs to the same parent.
// TODO(orm-traversal): multi-FK-to-same-parent picks the first match — add
// disambiguation (or take an explicit column) when a type-only caller needs it.
pub fn back_fk_column<Parent: Model, Child: Model>() -> &'static str {
    Child::FIELDS
        .iter()
        .find(|f| f.fk_target == Some(Parent::TABLE))
        .map(|f| f.name)
        .unwrap_or_else(|| {
            panic!(
                "reverse relation from `{}` to `{}` has no anchoring foreign key: \
                 `{}` declares no `ForeignKey<{}>` for the back-link to resolve through",
                Parent::NAME,
                Child::NAME,
                Child::NAME,
                Parent::NAME,
            )
        })
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
    /// A pre-hydrated value — populated when this handle was built from a
    /// `select_related`-loaded FK/O2O cache slot via [`Self::from_resolved`].
    /// `get()`/`get_opt()` serve this before touching the database, which is
    /// what makes the derive-generated accessor zero-query after
    /// `select_related`. `None` for every path built by [`to_one_hop`]
    /// (traversal from a bare PK — the pre-Task-1 behaviour).
    resolved: Option<T>,
    _t: PhantomData<T>,
}

impl<T: Model> Relation<T> {
    /// Build a [`Relation<T>`] that already carries its resolved value — the
    /// derive-generated to-one accessor's cache-hit path (`post.author()`
    /// after `select_related("author")` populated the FK's cache).
    ///
    /// The path still gets a proper [`PathBase::SinglePk`] built from the
    /// object's own primary key, so a FURTHER hop off this handle (e.g.
    /// `post.author().company()`, once that accessor also composes off a
    /// `Relation<T>`) still has a real base to chain from — it just never
    /// gets used because [`Self::get_opt`] short-circuits on `resolved`
    /// first.
    pub fn from_resolved(obj: T) -> Self {
        let path = RelPath {
            base: PathBase::SinglePk {
                table: T::TABLE,
                pk_column: pk_column_name::<T>(),
                pk_value: obj.primary_key().into(),
            },
            hops: Vec::new(),
        };
        Relation {
            path,
            explicit_pool: None,
            resolved: Some(obj),
            _t: PhantomData,
        }
    }

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

    /// The SQL this relation's terminal would run, for the SQLite builder —
    /// the `to_sql()` probe surface tests use to assert the shape of the emitted
    /// query (one flat `SELECT … JOIN … JOIN …`, no nested subqueries). The
    /// Postgres string is available via [`Self::to_sql_pg`].
    ///
    /// This builds the same statement the terminal executes but binds no pool,
    /// so it works with or without a booted app.
    pub fn to_sql(&self) -> Result<String, sqlx::Error> {
        crate::orm::queryset::relation_resolve::to_one_sql::<T>(&self.path, false)
    }

    /// The Postgres rendering of [`Self::to_sql`].
    pub fn to_sql_pg(&self) -> Result<String, sqlx::Error> {
        crate::orm::queryset::relation_resolve::to_one_sql::<T>(&self.path, true)
    }

    /// Resolve the pool this terminal runs against: the explicit `.on(...)` /
    /// `.on_pg(...)` override, else the ambient default set at `App::build()`.
    fn resolve_pool(&self) -> Result<DbPool, sqlx::Error> {
        match &self.explicit_pool {
            Some(p) => Ok(p.clone()),
            None => crate::db::try_pool_dispatched().cloned().ok_or_else(|| {
                protocol_error(
                    "no database pool available to resolve a relation — either \
                     build an App (which sets the ambient pool) or pin one with \
                     `.on(&pool)` / `.on_pg(&pool)`",
                )
            }),
        }
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
    ///
    /// A deep chain (`hops.len() > 1`) resolves in one flat `SELECT … JOIN …
    /// JOIN … WHERE root.pk = ?` (see [`crate::orm::queryset::relation_resolve`]);
    /// any NULL / dangling link along the way drops the row and surfaces here as
    /// `None`. A single forward-FK / reverse-O2O hop keeps the Task-1 subquery
    /// path, which needs no model registry and so works in a bare `.on(&pool)`
    /// test without a booted `App`.
    ///
    /// When this handle was built via [`Self::from_resolved`] (the
    /// `select_related`-cache hit path), the resolved value is returned
    /// directly — BEFORE any of the above, and with zero database round
    /// trips.
    pub async fn get_opt(mut self) -> Result<Option<T>, sqlx::Error> {
        if let Some(obj) = self.resolved.take() {
            return Ok(Some(obj));
        }
        if self.path.hops.len() > 1 {
            let pool = self.resolve_pool()?;
            return crate::orm::queryset::relation_resolve::resolve_to_one_path::<T>(
                &self.path, &pool,
            )
            .await;
        }
        self.terminal_queryset()?.first().await
    }

    /// Whether the to-one target row exists.
    pub async fn exists(self) -> Result<bool, sqlx::Error> {
        if self.path.hops.len() > 1 {
            let pool = self.resolve_pool()?;
            return Ok(
                crate::orm::queryset::relation_resolve::resolve_to_one_path::<T>(&self.path, &pool)
                    .await?
                    .is_some(),
            );
        }
        self.terminal_queryset()?.exists().await
    }

    /// Build the leaf `QuerySet<T>` for a SINGLE to-one hop (Task-1 path).
    ///
    /// Deep chains go through the JOIN resolver instead (see [`Self::get_opt`]);
    /// this stays for the single-hop case because it needs no model registry
    /// and so resolves in a bare `.on(&pool)` test without a booted `App`.
    fn terminal_queryset(self) -> Result<crate::orm::QuerySet<T>, sqlx::Error> {
        let Relation {
            path,
            explicit_pool,
            ..
        } = self;

        if path.hops.len() != 1 {
            return Err(protocol_error(
                "terminal_queryset resolves a single to-one hop only; deep chains \
                 route through the JOIN resolver",
            ));
        }
        let hop = path.hops[0];
        if !hop.kind.is_to_one() {
            return Err(protocol_error(
                "to-many relations resolve to a QuerySet, not a single row \
                 (wired in a later task)",
            ));
        }

        let (base_table, base_pk_column, base_pk_value) = match path.base {
            PathBase::SinglePk {
                table,
                pk_column,
                pk_value,
            } => (table, pk_column, pk_value),
            PathBase::TableRoot { .. } => {
                return Err(protocol_error(
                    "TableRoot base has no pk to anchor a single-object traversal",
                ));
            }
        };

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
        resolved: None,
        _t: PhantomData,
    }
}

/// A loud protocol error for an unsupported path shape.
fn protocol_error(msg: &str) -> sqlx::Error {
    sqlx::Error::Protocol(msg.to_string())
}

/// Build a chainable, ambient-pooled `QuerySet<To>` for a to-many hop
/// (`M2M` or `ReverseFk`) off a `RelationSource` — the general-path sibling
/// of [`M2M::query`](super::m2m::M2M::query) for callers that only have a
/// `HopSpec` (the Task-5 derive's M2M/reverse-FK accessors when the source
/// isn't a hydrated `M2M<T>` slot).
///
/// - **Single hop off a bare source** (a `&From` object, or a fresh handle
///   nothing has hopped off yet): the lighter junction-subquery (`M2M`) /
///   reverse-FK-predicate (`ReverseFk`) form, which needs no model registry
///   and so resolves in a bare `.on(&pool)` test without a booted App.
/// - **Deep chain** (this hop follows one or more prior hops, reached by
///   composing public calls — `Relation<T>` and `QuerySet<T>` both implement
///   [`RelationSource`]): the whole traversal resolves to the leaf via the
///   crossing-to-many resolver ([`crate::orm::queryset::relation_resolve::resolve_leaf_queryset`],
///   Task 4) — an all-to-one prefix + one-or-more to-many hops, `SELECT
///   DISTINCT` on the leaf PK by default, `.with_duplicates()` to opt out.
///
/// Either way the returned `QuerySet` carries the [`RelPath`] so a further
/// hop can extend it. A shape Phase 1 does NOT resolve (a to-one hop after a
/// to-many, or an inner reverse-FK) comes back **poisoned** — the builder
/// succeeds but every fallible terminal reports a clear
/// `Err(sqlx::Error::Protocol(_))` naming the deferred shape, never a wrong
/// query.
///
/// # Panics
///
/// - `hop.kind` is not [`HopKind::M2M`] or [`HopKind::ReverseFk`] (a to-one
///   kind belongs on [`to_one_hop`], not here) — a directly-malformed
///   `HopSpec` argument, not a shape reachable by composing public calls.
/// - `hop.kind` is [`HopKind::M2M`] and `hop.junction` is `None` (likewise
///   a malformed `HopSpec` — every M2M hop must carry its [`JunctionSpec`]).
pub fn to_many_hop<From: Model, To: Model>(
    src: impl RelationSource<From>,
    hop: HopSpec,
) -> QuerySet<To> {
    assert!(
        matches!(hop.kind, HopKind::M2M | HopKind::ReverseFk),
        "to_many_hop requires a to-many HopKind (M2M or ReverseFk); \
         to-one kinds resolve through `to_one_hop` instead"
    );
    let path = src.into_rel_path().push(hop);

    // A deep chain (this hop follows one or more prior hops) resolves the
    // whole traversal to the leaf via the crossing-to-many resolver (Task 4);
    // a single hop off a bare source keeps the lighter junction-subquery /
    // reverse-FK-predicate form (which needs no model registry, so it works
    // in a bare `.on(&pool)` test without a booted App). Either way the
    // returned QuerySet carries the `RelPath` so a further hop can extend it.
    let mut qs = if path.hops.len() > 1 {
        crate::orm::queryset::relation_resolve::resolve_leaf_queryset::<To>(&path)
    } else {
        single_to_many_queryset::<To>(&path.base, &hop)
    };
    qs.rel_path = Some(path);
    qs
}

/// Build the `QuerySet<To>` for a SINGLE to-many hop off a bare source — the
/// junction subquery (`M2M`) or reverse-FK predicate (`ReverseFk`) form. Kept
/// registry-free so it resolves in a bare `.on(&pool)` test.
fn single_to_many_queryset<To: Model>(base: &PathBase, hop: &HopSpec) -> QuerySet<To> {
    let pk_value = match base {
        PathBase::SinglePk { pk_value, .. } => pk_value,
        PathBase::TableRoot { .. } => {
            return Manager::<To>::new()
                .filter(Predicate::new(Expr::cust("1 = 1")))
                .poisoned(
                    "single_to_many_queryset requires a SinglePk base (an object-rooted \
                 relation source); a TableRoot base (from RelPath::from_path) has no \
                 row to anchor a single-hop to-many query — it resolves through the \
                 unified walk_joins path instead",
                );
        }
    };
    let predicate: Predicate<To> = match hop.kind {
        HopKind::ReverseFk => Predicate::new(
            Expr::col(Alias::new(hop.fk_column)).eq(SimpleExpr::Value(pk_value.clone())),
        ),
        HopKind::M2M => {
            let junction = hop.junction.expect("M2M HopSpec must carry a JunctionSpec");
            let to_pk_col = pk_column_name::<To>();
            let mut sub = Query::select();
            sub.column(Alias::new(junction.target_column))
                .from(crate::db::router::schema_qualified_table(junction.table))
                .and_where(
                    Expr::col(Alias::new(junction.parent_column))
                        .eq(SimpleExpr::Value(pk_value.clone())),
                );
            Predicate::new(Expr::col(Alias::new(to_pk_col)).in_subquery(sub))
        }
        // Guarded by the leading `assert!` in `to_many_hop`.
        _ => unreachable!("to-one HopKind rejected by the leading assert"),
    };
    Manager::<To>::new().filter(predicate)
}

/// Render the SQLite SQL a to-one [`RelPath`] resolves to — a probe surface
/// for tests that build a path directly (via [`RelPath::from_path`] or by
/// hand) without going through a `Relation<T>` handle. Delegates to the same
/// builder [`Relation::to_sql`] uses, so it exercises the real `walk_joins`
/// engine, never a parallel code path.
pub fn to_sql_for_path<Leaf: Model>(path: &RelPath) -> Result<String, sqlx::Error> {
    crate::orm::queryset::relation_resolve::to_one_sql::<Leaf>(path, false)
}
