# ORM relations, forms, and joins — hardening for Django-parity templating

**Date:** 2026-06-11
**Status:** Approved design, pending implementation plan
**Closes / advances:** gaps2 #39 (Form auto-skip ReverseSet + child-side count filters), #40 (FK in Form derive), #42 (FK save binds text not bigint), #45 (seamless reverse FK + concise generated SQL), parts of #35 (ModelMeta soft_delete). Carries the "deep nested left/right joins with random inner joins" headline ask.
**Explicitly out of scope (own specs):** #43 per-field error rendering (errors all-at-once, under each field); cross-relation `filter(rel__field=...)`; #37 FileField/ImageField + MediaPlugin; #44 admin table refresh; #46 session-plugin triple-insert; #38 `Model::FIELD` path-alias ergonomics.

## Goal

Django's ORM feels good in templates because relations traverse without ceremony: `{{ comment.plugin.author.name }}` just works, `post.comments.all` just works, and `annotate(Count("comments"))` collapses to one query. umbra already has most of the *read* machinery (`ReverseSet`, `prefetch_related`, nested `select_related`, `annotate_count`). The gaps that break the Django feel are concentrated in two places:

1. **The write/form half.** `#[derive(Form)]` rejects `ForeignKey<T>`, `ReverseSet<T>`, and choices enums outright, forcing the `#[umbra(noform)]` + hand-rolled `Default` boilerplate that litters `PluginComment` and keeps its public submission form commented out. The admin's dynamic save path binds an FK id as TEXT, producing `column "plugin" is of type bigint but expression is of type text`.
2. **Join depth and type.** `join_related` is one-hop and LEFT-only. Real apps need nested joins in one round-trip and the ability to pick INNER vs LEFT vs RIGHT.

This spec hardens both halves so a model with FKs and choices is form-submittable end to end, and so the JOIN builder spans relations with full join-type control.

## Non-goals

- Cross-relation `filter`/`order_by` (`filter(plugin__moderation="approved")`). That reworks the typed `Predicate<T>` surface and is a separate, larger spec. Joins here are for eager-loading (hydration), not predicate spanning.
- Per-field error rendering in templates (#43). The plumbing this spec produces (structured `WriteError`, FK existence failures landing under their field) feeds that spec, but the template churn lives there.
- FileField/ImageField widgets and the MediaPlugin (#37).

## Relation coverage matrix

All four relation kinds are in scope. The taxonomy (verified in code): `ForeignKey<T>` (forward FK, a real column), `OneToOne<T>` without `#[sqlx(skip)]` (a **unique FK** — a real column, so it *is* a forward FK for every purpose here), `OneToOne<T>` with `#[sqlx(skip)]` (reverse O2O back-pointer, no column), `ReverseSet<C>` (reverse FK collection, no column), and `M2M<T, P=i64>` (no parent column — junction rows).

| Relation | Form field (Part 1/2) | Join (Part 4) | annotate (Part 5) |
|---|---|---|---|
| Forward `ForeignKey<T>` | `ModelChoice` (single select) | `*_join_related` FK join | count via reverse — n/a here |
| Forward `OneToOne<T>` (unique FK) | `ModelChoice` (single select) — same path as FK | FK join — same path | n/a (0/1) |
| Reverse `OneToOne<T>` (`#[sqlx(skip)]`) | **auto-skip** (like ReverseSet) | load via `prefetch_related` | n/a (0/1) |
| `ReverseSet<C>` (reverse FK) | **auto-skip** | load via `prefetch_related` | `annotate_count` / `annotate_count_where` |
| `M2M<T>` | **`ModelMultiChoice`** (multi-select) + post-insert junction write | double-join (junction→child), join-type on child hop | `annotate_count` over junction |

The "nearly free" kinds (forward O2O, reverse O2O) get explicit test pins so the free-ness can't silently regress. M2M-in-forms (the one genuinely new chunk) mirrors the admin's existing dynamic junction machinery rather than inventing a second path.

---

## Part 1 — `#[derive(Form)]` learns FK / O2O, M2M, choices, and ignores reverse relations

All three changes land in `expand_form()` / `classify_form_field_type()` in `crates/umbra-macros/src/lib.rs` (today's reject site is ~line 3447). The Model derive already distinguishes these field kinds (`FieldKind::ReverseSet`, the `#[umbra(choices)]` flag, and the `ForeignKey<T>` / `Option<ForeignKey<T>>` path shapes); the Form derive reuses the same detection so the two derives agree on what a field *is*.

### 1a. Reverse relations → auto-skip (#39b)

A `ReverseSet<C>` field and a reverse `OneToOne<T>` (the `#[sqlx(skip)]` variant) are back-pointers, never user-submittable by construction. The Model derive already drops them from `FIELDS`; the Form derive must do the same — skip them before type classification, exactly as it already skips `#[umbra(noform)]` / `primary_key` / `auto_now*` fields. Detection reuses the Model derive's `FieldKind::ReverseSet` and `FieldKind::OneToOne` + `has_sqlx_skip` checks so the two derives agree. Removes the manual `#[umbra(noform)]` requirement on every reverse-relation field.

### 1b. Choices enum → a `Select` field (#39)

When a field carries `#[umbra(choices)]` (the same signal the Model derive keys on), emit a `Select` form field whose options come from `<T as ChoiceField>::VALUES` / `LABELS` — both compile-time consts, so **no DB access is needed** for choices. `validate()` checks the submitted string is a member of `VALUES`; a non-member produces a per-field validation error. A nullable choices field (`Option<T>`) renders a leading empty `<option>`.

New `InputKind` variant:

```rust
// crates/umbra-core/src/forms.rs
pub enum InputKind {
    // ...existing...
    /// Closed-set enum (#[umbra(choices)]). Options are compile-time.
    Select { options: Vec<(String, String)> },     // (value, label)
}
```

### 1c. `ForeignKey<T>` / forward `OneToOne<T>` → a `ModelChoice` field (#40)

A `ForeignKey<T>` (or `Option<ForeignKey<T>>`), and equally a forward `OneToOne<T>` (the non-`#[sqlx(skip)]` variant — a unique FK with a real column), becomes a `ModelChoice` — Django's `ModelChoiceField`. It carries the target table and the value's PK type so `validate()` can parse the submitted id, and it knows how to fetch `(id, label)` rows so the render path can emit a populated `<select>`. The two field types resolve identically here because a forward O2O is a unique FK; the only difference is the DB's UNIQUE constraint, which the existence/uniqueness errors surface through the same `WriteError` path.

```rust
pub enum InputKind {
    // ...
    /// FK to another model. Options fetched at render time (async).
    ModelChoice {
        target_table: &'static str,   // <T as Model>::TABLE
        label_field: Option<&'static str>, // #[form(label_field="name")] override
        pk_kind: PkKind,              // BigInt | Uuid | Text — how to parse the id
    },
}
```

- **Label column convention:** first non-PK text column of the target model (matches the admin's `fk_picker.rs` today), overridable with `#[form(label_field = "name")]`.
- **PK kind:** resolved from `<T as Model>::TABLE`'s PK at render/validate time via the existing `pk_meta_for_table` registry cache, so a uuid- or text-PK target parses correctly (forward-compatible with the PrimaryKey refactor in memory).

This unblocks deriving `Form` directly on `PluginComment`, deleting its hand-rolled `Default` impl and the commented-out field attributes.

### 1d. `M2M<T>` → a `ModelMultiChoice` field

`M2M<T, P=i64>` has **no column on the parent** — it's junction rows written *after* the parent insert. In a form it's Django's `ModelMultipleChoiceField`: a multi-select of related rows. The Form derive emits a `ModelMultiChoice` field; `validate()` parses the submitted id *list* (HTML multi-value, the `m2m_<field>` convention the admin already uses) and verifies each id exists; rendering fetches `(id, label)` candidates and emits a multi-select / chip-picker.

```rust
pub enum InputKind {
    // ...
    /// M2M relation. Submits a list of child ids; written as junction rows.
    ModelMultiChoice { target_table: &'static str, label_field: Option<&'static str>, pk_kind: PkKind },
}
```

The write is the one structurally new piece: because M2M isn't a parent column, the validated struct can't carry it through a plain `INSERT`. The typed `create` path must, after inserting the parent and learning its PK, write the junction rows — reusing the **existing dynamic junction machinery** (`set_junction_dynamic` / the `<parent>_<field>` junction-table convention the admin POST handler and migration emitter already share), not a second code path. M2M fields are skipped by the column-level `INSERT` and handled in this post-insert step. At v1 this follows the existing M2M constraint (i64 child PKs); non-i64 surfaces the same clean error the rest of the M2M plumbing gives.

---

## Part 2 — `FormValidate` becomes async (validate + render)

`ModelChoice` must (a) verify the submitted FK id points at a live row before insert and (b) fetch options to render the `<select>`. Both need the DB. Per the approved decision, the whole validation+render surface goes async via `#[async_trait]` (already a dependency).

```rust
#[async_trait]
pub trait FormValidate: Sized {
    /// Parse + validate. FK fields verify existence; choices check membership.
    async fn validate(data: &HashMap<String, String>) -> Result<Self, ValidationErrors>;

    /// Field descriptors. Sync — kinds/validators only, no live options.
    fn fields() -> Vec<Field>;

    /// Render every field. Async because ModelChoice fetches its options.
    async fn render_html(data: &HashMap<String, String>) -> String;
}
```

- **Ambient pool.** `validate` / `render_html` resolve the pool ambiently through the ORM's existing `pool_dispatched()` (the "one intentional global"), so signatures stay `(data)` — no pool threaded through call sites. Tests set the ambient pool as they already do.
- **FK existence check.** `validate` runs `<target>::objects-equivalent` existence probe through the ORM (`DynQuerySet::for_meta(target).filter(pk.eq(id)).exists()` — never raw SQL). A miss lands as a `ValidationErrors` entry keyed to that field, ready for the #43 per-field renderer.
- **Choices stay pure.** Membership is checked against the compile-time `VALUES`; no query.
- **Ripple (contained).** `Form<T>` extractor (`forms.rs:912`) is already an async axum `FromRequest` — it awaits `validate`. The in-crate `forms.rs` unit tests and any `render_html` caller gain `.await`. No new public async surface beyond the trait.

`async fn` in traits is provided by `#[async_trait]` to keep object-safety and dyn-compat unchanged.

---

## Part 3 — Fix the dynamic FK-save coercion bug (#42)

`form_str_to_sea_value` (`dynamic.rs:1922`) *looks* correct — an FK with an i64 target yields `SeaValue::BigInt`, and a unit test (`dynamic.rs:2610`) passes for it. So the `bigint is of type text` error fires on a path the helper-level test doesn't exercise. The fix is diagnosis-first:

1. **Reproduction test first** (systematic-debugging). Write a failing test that drives the *admin insert path* (`DynQuerySet::for_meta(meta).insert_form(...)`) for a model whose FK targets an i64-PK parent, asserting the bound value is `BigInt`, not `String`. The live-registry shape — not the isolated helper — is where the bug lives. Suspect set: (a) the FK column arriving with `col.ty != SqlType::ForeignKey` in the meta the admin builds (so the FK arm is skipped and it falls through to `json_to_sea_value`, which binds `String` → TEXT), or (b) `fk_target_pk_sql_type` misresolving the target PK type.
2. **Fix the contract, not the symptom.** Whichever suspect proves out, the guaranteed contract is: *every dynamic write path binds an FK to its target PK's `SqlType`, never raw text.* If the fallthrough is the culprit, harden the `json_to_sea_value` FK arm to coerce a numeric string → i64 for a numeric-PK target instead of binding `String`; if the meta is the culprit, fix the meta construction so FK columns keep `SqlType::ForeignKey` + `fk_target`.
3. **Regression pin.** Keep the reproduction test green; add the parallel typed-path assertion so the two paths can't diverge again.

---

## Part 4 — Joins go deep: nested + INNER / LEFT / RIGHT

Today `apply_join_related` (`queryset/mod.rs:858`) is one-hop and hard-codes `LeftJoin`. Three additions:

### 4a. Typed join-type methods

```rust
impl<T: Model> QuerySet<T> {
    pub fn left_join_related(self, path: impl Into<String>) -> Self;
    pub fn inner_join_related(self, path: impl Into<String>) -> Self;
    pub fn right_join_related(self, path: impl Into<String>) -> Self;
    // existing join_related stays — now auto-infers (see 4c)
}
```

Each records `(path, JoinType)` instead of just a path. `apply_join_related` reads the recorded type instead of the hard-coded `LeftJoin`.

### 4b. Nested chains (`"plugin__author"`)

Split the path on `__`, resolve each hop's `(table, fk_column, target_table, target_pk)` from `FIELDS` / the registry (the resolution `select_related`'s nested path at `hydration.rs:162` already performs), and emit one JOIN per hop with a per-hop alias. Child columns alias as the full dotted path (`plugin__author__name`) so hydration can rebuild the nested JSON bottom-up and `comment.plugin.author` resolves in templates. One round-trip for the whole tree.

### 4c. Auto-inference default

A plain `join_related` (and a hop with no explicit type) picks `INNER` for a `NOT NULL` FK and `LEFT` for a nullable FK — Django's exact rule, read from `FieldSpec.nullable`. The common case needs no annotation; the explicit methods override.

### 4d. Backend note

RIGHT/FULL JOIN requires SQLite ≥ 3.39 (Postgres is unconditional). The boot-time system check (`check.rs`) warns if a `right_join_related` is reachable on an older SQLite — consistent with "backend mismatches caught at boot."

### 4e. Relation kinds in joins

- **Forward `OneToOne<T>`** is a unique FK — it joins through the exact FK path above, no special case.
- **`M2M<T>`** already routes through a double LEFT JOIN (junction → child). The join-type method applies to the **child** hop (the junction hop stays INNER — a parent only reaches a child *through* an existing junction row), and a nested chain can pass *through* an M2M hop (`tags__category`). The per-hop alias scheme extends to the junction alias.
- **Reverse relations** (`ReverseSet`, reverse `OneToOne`) are *not* joined — their multiplicity (or back-pointer nature) makes `prefetch_related`'s batched-IN the right tool, exactly as Django uses `prefetch_related` rather than `select_related` for reverse FKs.

---

## Part 5 — `annotate_count` child-side filter + soft-delete awareness (#39a)

`annotate_count("comment_set")` counts **all** children — blind to the child's `SOFT_DELETE` and to any predicate — so umbra.dev counts hidden/trashed notes.

### 5a. Carry `soft_delete` onto the relation/registry (shared with #35)

`Model::SOFT_DELETE` lives only on the typed trait. Surface it where the dynamic/annotate path can read it: add `soft_delete: bool` to `ReverseFkRelationSpec` (and `ModelMeta`, the #35 enabler — done once, reused). The Model derive fills it from the child model's flag.

### 5b. Fold `deleted_at IS NULL` into the count

When the resolved relation's child meta says `soft_delete`, `annotate_count` adds `AND <child>.deleted_at IS NULL` to the correlated subquery automatically.

### 5c. `annotate_count_where`

```rust
pub fn annotate_count_where<C: Model>(
    self, alias: &str, relation: &str, pred: Predicate<C>,
) -> Self;
```

Renders the child `Predicate<C>` into the subquery WHERE alongside the correlation. Mirrors Django's `Count("comments", filter=Q(moderation="visible"))`. Generic over the child model `C` so the predicate is typed against the child's columns.

The umbra.dev homepage moves from `annotate_count("comment_set")` to `annotate_count_where::<PluginComment>("notes", "comment_set", comment::MODERATION.eq("visible"))`, counting visible-only.

### 5d. `annotate_count` over M2M

`annotate_count("tags")` resolves an `M2M<Tag>` relation too (Django's `Count("tags")`). Resolution falls back to `M2M_RELATIONS` when the name isn't a reverse-FK relation, and the correlated subquery counts **junction rows** (`SELECT COUNT(*) FROM <parent>_<field> WHERE parent_id = parent.id`). Reverse `OneToOne` and forward O2O annotate to 0/1 and aren't worth a count helper — they're omitted (a clean "use `prefetch_related`/the FK directly" error if asked).

---

## Testing strategy (TDD throughout)

**These are behavioral, round-trip tests against a real SQLite DB — not tautological asserts.** The bar: every test sets up real rows, drives the *actual* public path (form submit / queryset terminal), reads the result back out of the database, and asserts on the **observed data or object graph** — never on a substring of generated SQL as a proxy for behavior, never an assertion that restates the line above it. Where the emitted SQL shape *is* the contract (join type, subquery presence), assert the SQL **in addition to** a behavioral round-trip that proves the rows come back right — the SQL assertion catches *how*, the round-trip catches *whether*. Each relation kind gets both **surface-level** (one hop / single relation) and **deep-nested** (multi-hop graph) coverage. No test passes by construction.

### Per-relation behavioral tests

**Foreign key**
- *Surface:* seed a parent + child; submit a form whose FK field is the parent's id; read the child back and assert `child.plugin.resolve()` hydrates the *actual parent row* (id + a real field), and that the stored column is the bigint id (Part 3's bug). Submit a nonexistent id → assert a field-keyed validation error, and assert **no row was inserted**.
- *Deep-nested:* seed `Comment → Plugin → Author` (3 levels); `inner_join_related("plugin__author")`; assert the returned `comment.plugin.author.name` equals the seeded author's name, and that exactly **one** query ran (no N+1) — counted, not assumed.

**One-to-one**
- *Forward:* round-trip a `OneToOne` field exactly like an FK, **plus** assert the UNIQUE constraint fires — insert a second row pointing at the same target and assert a `WriteError` (uniqueness), not a silent second row.
- *Reverse:* seed a parent with and without its O2O child; `prefetch_related` the reverse field; assert the with-child parent hydrates `Some(child)` and the without-child parent hydrates `None` (distinguishing loaded-empty from not-loaded), and assert the reverse field is **absent from the derived form's `fields()`**.

**Many-to-many**
- *Surface:* seed three candidate children; submit a form selecting two of their ids; after insert, query the junction table directly and assert **exactly those two junction rows exist** (and the third does not); then `prefetch_related` the M2M field and assert it returns the two child rows. Submit a list containing one bad id → assert a field error and **zero junction rows written** (atomicity).
- *Deep-nested:* `inner_join_related("tags__category")` through an M2M hop; assert the child + its onward FK both hydrate, and the junction hop didn't drop or duplicate parents (assert parent count is stable).

**Choices**
- Store every variant via a form submit and read each back **decoded as the enum** (not the raw string); submit a value outside `VALUES` → field error + no row. Assert a nullable choices field accepts empty → `None`.

**Joins (SQL-shape + behavior together)**
- For each of `inner_/left_/right_join_related`: assert the emitted SQL contains the right `JOIN` keyword **and** that a parent with no related row is dropped (INNER) vs kept with a null relation (LEFT) — proven by the returned row set, not just the SQL.
- *Auto-inference:* a `NOT NULL` FK via plain `join_related` produces INNER (assert via the drop-behavior round-trip); a nullable FK produces LEFT (assert the orphan parent survives).

**annotate**
- Seed a parent with 3 children, soft-delete 1; assert `annotate_count` returns **2**, not 3 (real trashed row, real exclusion). `annotate_count_where(... moderation="visible")` with a mix of visible/hidden children → assert the visible count. M2M `annotate_count("tags")` → assert it equals the junction-row count for that parent. A parent with zero children → assert **0** (the LEFT-ish "still returned" property), not a dropped row.

### File map

| Part | Test file | Anchored by the behavioral cases above |
|---|---|---|
| 1a | `form_derive.rs` | reverse `ReverseSet`/`OneToOne` skip + absent from `fields()` |
| 1b | `form_derive.rs` | choices round-trip + reject |
| 1c | new `form_fk.rs` | FK + forward-O2O round-trip, existence reject, uniqueness |
| 1d | new `form_m2m.rs` | junction-row round-trip, atomicity on bad id |
| 2 | `forms.rs` (async) + extractor test | async validate existence; multi/single `<select>` render |
| 3 | `dynamic.rs` reproduction + `cross_crate` round-trip | FK bound bigint, row links, read-back |
| 4 | new `joins_nested.rs` | per-join-type drop/keep behavior + SQL shape; nested graph; M2M chain |
| 5 | `annotate_count.rs` (extend) | soft-delete exclusion, filtered count, M2M count, zero-child kept |

`PluginComment` on umbra.dev is the primary end-to-end acceptance case: derive `Form`, delete the hand-rolled `Default`, submit a comment with FK + `Option<FK>` + choices through the public form, and confirm it saves with the FK bound as bigint. It exercises FK / forward-O2O / choices / reverse-skip but **not M2M** (no M2M field on `PluginComment`) — so a second acceptance uses an existing `M2M<T>` model (e.g. the shop's tag-style relation) to submit a multi-select and confirm junction rows land.

## Docs (ship a feature, ship its doc page)

- `documentation/docs/v0.0.1/orm/forms-relations.mdx` — FK, forward O2O, M2M (multi-select), and choices in `#[derive(Form)]`; the async render path; reverse relations auto-skipped.
- `documentation/docs/v0.0.1/orm/joins.mdx` — `inner/left/right_join_related`, nesting, auto-inference, backend caveat.
- Extend the existing `orm/aggregates.mdx` with `annotate_count_where` + soft-delete behavior.

## Sequencing

Part 5's `soft_delete`-on-`ModelMeta` is the shared enabler (#35 + #39a) — land it first. Then Part 3 (isolated bug, unblocks the admin immediately), then Parts 1–2 (forms, the macro + trait change), then Part 4 (joins, the largest read-side surface). Each part is independently shippable and independently testable.
