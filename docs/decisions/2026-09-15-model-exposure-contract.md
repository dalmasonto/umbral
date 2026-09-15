# Model exposure contract: one shared surface every serializer plugin reads

Status: proposed (design approved in brainstorming 2026-09-15; not yet implemented)
Scope: `umbral-core` (ORM), consumed by `umbral-rest`, `umbral-graphql`, `umbral-admin`, and any future serializer plugin (gRPC, …)

## Problem

Every plugin that turns a model into an external representation independently re-invents the same per-model configuration. When REST was built it needed "which fields are hidden", so `ResourceConfig` grew a `hidden` list and visibility logic. When GraphQL was built the same need reappeared and was solved again. A future gRPC or a second admin surface would do it a third time. The facts being re-derived — which fields are safe to serialize, which are client-writable, which is the display column, what the declared ordering/search intent is — are properties of the *model*, not of any one transport. Re-deriving them per plugin is duplication, invites drift (a field hidden in REST but visible in admin), and makes each new serializer plugin pay a config tax it shouldn't.

The idea: give the model one shared, plugin-agnostic record of these facts that every plugin reads and extends, so a plugin author configures a model once (on the model) and every serializer honours it.

## Why this belongs in core, not in the first plugin that needed it

The dependency arrow points inward: `umbral-core` defines the `Plugin` trait and names no concrete plugin; every plugin depends on the `umbral` facade, never the reverse. That is precisely why the shared home must be core. If the contract lived in `umbral-rest`, then `umbral-graphql` and `umbral-admin` reading it would introduce plugin→plugin dependencies, breaking the inversion Cargo's ban on circular deps exists to protect. Core is the only crate every serializer can depend on without coupling to a sibling. This is the framework's "thin core, plugin-heavy" principle applied to *data*: core owns the declarative facts, each plugin is a thin policy overlay.

## What already exists (this is an extension, not a new subsystem)

The seam is half-built. The audit that preceded this design found:

- Core already owns a runtime registry every plugin reads: `registered_models()` → `Vec<(String, ModelMeta)>`, populated once during `App::build`. This is the "shared place to learn from".
- `ModelMeta` (`crates/umbral-core/src/migrate.rs`) already carries the presentation block added by gaps4 #95 — `list_display`, `search_fields`, `list_filter`, `inline_edit_fields`, `readonly_fields` — explicitly so "any plugin reads the model's declared display+query intent from ONE place". `ModelMeta` also carries `display`, `str_template`, `ordering`, `m2m_relations`, `soft_delete`, and the field list as `Vec<Column>`.
- Field visibility is already a core fact. `Column` (the owned, serde mirror of `FieldSpec`) carries `private` (stripped from reads unless unlocked), `secret` (never serialized, no unlock; auto for `Masked<T>`), `noform` (dropped from write bodies and forms), `privileged` (mass-assignment guard), `noedit`, `is_string_repr`, plus `help`, `widget`, `example`, `choices`/`choice_labels`, `max_length`, the `auto_*` flags, and the relation fields (`fk_target`, `on_delete`, `on_update`). These flags are enforced today in the shared `DynQuerySet` read/write path, so `#[umbral(private)]` is already hidden across REST, GraphQL, and admin.
- No coupling smell today: `umbral-admin` does NOT depend on `umbral-rest`. Admin has its own `AdminModel` config; REST has `ResourceConfig`. The overlap is duplication, not coupling.

So the facts already live on `Column` and `ModelMeta`. What is missing is a single, documented set of *derived views* over them, so plugins stop hand-rolling "the set of serializable fields" etc. — and a written contract that says a serializer plugin reads these facts and layers only its own transport policy.

## Decision

Add the derived-view methods as an **inherent `impl ModelMeta`**, isolated in a new module `crates/umbral-core/src/orm/exposure.rs`. No new trait.

Rejected during brainstorming: a separate `ModelExposure` trait (Approach A). A trait only pays off when a *second* type implements it, and every serializer already funnels through `ModelMeta` (statically via `ModelMeta::for_::<T>()`, dynamically by-table from the registry). Inherent methods are simpler, need no `use` to bring into scope (killing an import-friction footgun), and are more discoverable (IDE autocomplete on `meta.` shows the whole contract). If a genuine second implementor ever appears — e.g. a GraphQL "effective exposed model" wrapper composing `ModelMeta` + its policy — a trait is extracted from these method signatures then, without breaking callers. That is the YAGNI-correct order.

Rejected: a `ModelExposure` value type plugins build and overlay policy onto (Approach C). It risks pulling plugin policy back into a shared type, blurring the facts/policy line this design draws, and is more than a read contract needs.

### The isolation requirement (hard)

`ModelMeta` lives in `migrate.rs` and is fundamentally a *migration-snapshot* type (serde-serializable, diffable). The exposure methods must NOT be tangled into that code. Rust permits an `impl ModelMeta` block in a different module of the same crate, so the exposure methods live entirely in `orm/exposure.rs` behind a clear doc header ("the read contract serializer plugins consume"). Schema-snapshot concerns and runtime-exposure concerns stay in separate files even though they operate on the same struct.

## The guiding principle: facts on the model, policy in the plugin

The line that keeps the model lean and the contract correct:

- A **fact about the data** belongs on the model / `ModelMeta`: this field is secret; this field is client-writable; this is the display column; the default ordering is `-created`; these are the declared search fields.
- A **policy of a transport** stays in the plugin's own config, reading the facts: who may POST this; the rate limit; URL nesting; response `?expand=` shape; GraphQL resolver batching; admin column widths.

Concretely, REST's `.hide()` (hide this field in the *API* but not necessarily elsewhere) stays in `ResourceConfig` — it is transport policy. "This field is secret/private everywhere" stays the core fact. Same base, different overlay per plugin.

There is no runtime cost to centralizing facts: `FieldSpec` is `const`, and `ModelMeta` is built once into the registry at boot. Adding facts does not make a model "heavier" per request; the only thing to guard is attribute-surface bloat on the model, which the facts/policy line governs.

## The v1 method surface

Inherent methods on `ModelMeta`, in `orm/exposure.rs`. The contract adds ONLY methods that do real derivation. It deliberately does NOT wrap already-public data: `ModelMeta`'s fields are all `pub` (`name`, `table`, `display`, `str_template`, `ordering`, `search_fields`, `list_display`, `list_filter`, `readonly_fields`, `inline_edit_fields`, `m2m_relations`, `fields`), so callers read those directly (`meta.ordering`, `meta.search_fields`, …); raw per-field facts (`help`, `widget`, `choices`, `max_length`, `secret`, `private`, …) are read directly off each `Column`. Wrapping public fields in accessor methods would be redundant surface with two ways to get the same value, and `table_name()` already exists on `ModelMeta`.

```rust
// crates/umbral-core/src/orm/exposure.rs
impl ModelMeta {
    /// Look up a field by name (linear scan over `self.fields`).
    pub fn field(&self, name: &str) -> Option<&Column>;

    /// The display column — the first field flagged `is_string_repr`, if any.
    pub fn display_field(&self) -> Option<&Column>;

    /// Fields safe to serialize with no unlocks: not `secret` and not `private`.
    pub fn public_fields(&self) -> impl Iterator<Item = &Column>;

    /// Serializable set, optionally adding back `private` fields for a caller
    /// that has authorized it: `!secret && (!private || allow_private)`.
    /// `secret` is never included either way.
    pub fn serializable_fields(&self, allow_private: bool) -> impl Iterator<Item = &Column>;

    /// True when the server populates this field, never the client:
    /// the primary key, or any `auto_*` (`auto_now`, `auto_now_add`,
    /// `auto_user`, `auto_user_add`, `auto_uuid`).
    pub fn is_server_managed(&self, f: &Column) -> bool;

    /// The safe client-writable set on create/update:
    /// not server-managed, not `noform`, not `privileged`.
    pub fn writable_fields(&self) -> impl Iterator<Item = &Column>;

    /// Foreign-key columns (those with a `fk_target`).
    pub fn foreign_keys(&self) -> impl Iterator<Item = &Column>;
}
```

Semantics are grounded in the existing flags:

- `public_fields` = the set safe to serialize with no unlocks. `serializable_fields(true)` adds back `private` fields for a caller that has authorized it; `secret` is never in either.
- `writable_fields` = the set an untrusted client may set on create/update: excludes the primary key and every `auto_*` (server-populated), `noform` (declared off the write surface), and `privileged` (mass-assignment guarded). `is_server_managed` is exposed so a caller with more authority (e.g. one that has verified privilege) can compute a wider set itself; `privileged` is read directly off `Column`.
- Declared query/presentation intent is NOT wrapped in methods — it is read directly off the public fields (`meta.list_display`, `meta.search_fields`, `meta.ordering`, `meta.readonly_fields`, `meta.list_filter`, `meta.inline_edit_fields`). Fallback behaviour (e.g. "if `list_display` is empty, use the display field then all public scalars") is a plugin decision and stays in the plugin. If a fallback proves identical across plugins, a derived `effective_list_display()`-style helper can be added later — that WOULD be real derivation and belong here.

## The composition contract (how a plugin overlays policy)

A plugin never re-derives visibility. It starts from the contract's facts and layers its policy:

```rust
// REST effective read set = core facts − plugin denylist + per-caller unlocks
let base = meta.public_fields();                              // core fact
let visible = base
    .filter(|f| !cfg.hidden.contains(&f.name))               // REST policy: .hide()
    .chain(cfg.unlocked_privates(caller));                   // REST policy: private unlocks

// admin readonly = core noedit fact ∪ admin-declared readonly ∪ sensitive-column heuristic
let readonly = meta.fields.iter()
    .filter(|f| f.noedit)                                    // core fact (Column.noedit)
    .map(|f| &f.name)
    .chain(admin_cfg.readonly_fields.iter())                 // admin policy
    .chain(meta.fields.iter().filter(|f| is_sensitive(&f.name)).map(|f| &f.name));
```

The rule for reviewers and plugin authors: read the fact from `ModelMeta`; put only transport policy in the plugin config. A new fact that two plugins would compute identically is a signal it belongs on the model, not in each plugin.

## Migration path (incremental, non-breaking)

Adding the methods breaks nothing — it is additive. Existing plugins adopt them opportunistically:

1. Land `orm/exposure.rs` with the methods and behavioural tests. No plugin change required to compile.
2. `umbral-rest`: replace its hand-rolled "serializable fields" / "writable fields" derivations with `meta.serializable_fields(..)` / `meta.writable_fields()`, keeping `ResourceConfig.hidden`, permissions, throttles, expand, nesting as the policy overlay. Verify existing REST tests unchanged.
3. `umbral-admin`: back its field-visibility and default `list_display`/`readonly` on the contract; keep `AdminModel` widths/inlines/actions as policy. Admin's `discovery.rs` already falls back to `ModelMeta.list_display`, so this is a small step.
4. `umbral-graphql`: read the contract for type/field generation; drop its parallel visibility logic.

Each step is its own commit and can land independently. No plugin is forced to change in the commit that adds the methods.

## Facade exposure

`ModelMeta` is already re-exported from the facade. Because the contract is inherent methods (not a trait), no extra import is needed — the methods are available wherever `ModelMeta` is. No prelude change.

## Non-goals / deferred

- **Computed / derived fields as a model-level fact** (the originally-motivating feature; slice #2). Today they exist only in REST as response-shaping closures. They cannot be a plain `ModelMeta` field because their value is *code*, and `ModelMeta` is serde-snapshot data; they need a separate non-serialized registry keyed by table, holding type-erased evaluators every serializer calls. This gets its own design once the contract lands, and slots in as an additional method (`computed_fields()`) with no breaking change.
- **Extracting a `ModelExposure` trait.** Only if a genuine second implementor appears.
- **Reverse-relation discovery** (which models FK *to* this one). Needs the whole registry, not just `self`, so it is a free function over the registry, not a method — deferred until a consumer needs it.
- **Moving policy to core.** Permissions, throttles, object-scoping, expand/nesting stay in the plugins. This contract is read-only facts.

## Testing strategy

Behavioural, against real models (per the repo's testing convention — real rows / real derivations, not asserting internals):

- Define a model with a `private`, a `secret`, a `noform`, a `privileged`, an `auto_now_add`, and an `is_string_repr` field. Assert `public_fields`, `serializable_fields(true/false)`, `writable_fields`, and `display_field` return exactly the right sets.
- Assert the same `ModelMeta` (from the registry, by table) yields identical derivations as the static `ModelMeta::for_::<T>()` — proving one impl serves both paths.
- After the REST/admin migration steps, assert existing REST and admin behaviour is unchanged (the contract is a refactor of derivation, not a behaviour change).

## Resolved decisions

1. Keep the `serializable_fields(allow_private: bool)` convenience alongside `public_fields()`; it is what nearly every caller wants, so callers should not have to chain `public_fields()` with a private set by hand. (Resolved 2026-09-15.)
2. `writable_fields()` does NOT exclude `readonly_fields`. `readonly_fields` is a form/UI concern (admin); server-side write-blocking is expressed by `noform`/`privileged`, which `writable_fields()` already honours. A plugin that wants to also drop `readonly_fields` from a form does so in its own overlay. (Resolved 2026-09-15.)
