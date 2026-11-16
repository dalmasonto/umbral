# review_4 · umbral

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 1,162 LOC (`crates/umbral`) |
| **Reviewer** | umbral-facade reviewer (one of ~20) |
| **Verdict** | healthy |
| **Findings** | 0 critical · 0 high · 0 medium · 3 low · 1 info — 4 new, 1 known |

## Summary

- The facade is what CLAUDE.md says it is: one stable surface over `umbral-core` + `umbral-macros`, with module-scoped power-user exports and a deliberately tight prelude. Its dependency list (verified in `Cargo.toml`) contains no plugin, no serializer, no REST crate — the "a REST-free app compiles with zero serializer code" claim holds statically at this boundary.
- No correctness, security, or soundness problems found. There is no `unsafe`, no raw SQL, no logic — the crate is re-exports plus documentation, and the documentation is the surface that gets reviewed.
- All four findings are documentation drift: a missing re-export the casing crate's README promises (UMBRAL-1), two escape hatches `arch.md §2.1` promises that never existed (UMBRAL-2), a stale `Status: M0` crate header (UMBRAL-3), and one stale narration on the `Post` fixture re-export (UMBRAL-4).
- The routed lead from the casing pilot is real: `pascal_case_from_table` is used internally by `inspectdb` but is unreachable through the facade, while `crates/umbral-casing/README.md:5` claims the facade re-exports what that crate provides. The pilot's own out-of-scope note called the claim accurate; it is not (see UMBRAL-1).
- Fix first: UMBRAL-1 (one line in `umbral_core::codegen` + facade, or narrow the README claim). UMBRAL-2/3 are spec/header edits; UMBRAL-4 wants a decision (keep the fixture or retire it), not a doc tweak.

## Findings

### UMBRAL-1 · LOW · docs/api · self-verified
**The facade's `codegen` module re-exports two of the three casing helpers, but `umbral-casing`'s README promises the facade surfaces what that crate provides.**

- **Where**: `crates/umbral/src/lib.rs:668-672` (re-export list), `crates/umbral-core/src/codegen.rs:30` (pass-through), `crates/umbral-casing/src/lib.rs:89` (`pub fn pascal_case_from_table`), `crates/umbral-casing/README.md:5` (the claim)
- **Evidence**:
  ```rust
  // crates/umbral-core/src/codegen.rs:30
  pub use umbral_casing::{pascal_case_from_ident, to_snake_case};

  // crates/umbral/src/lib.rs:671
  insert_line_before, pascal_case_from_ident, prompt, resolve_target, to_snake_case,
  ```
  and the promise being broken — `crates/umbral-casing/README.md:5`: "You almost certainly want the [umbral](https://crates.io/crates/umbral) facade crate instead, which re-exports the stable public surface **including what this crate provides**." `pascal_case_from_table` (casing `lib.rs:89`) is real and load-bearing inside the framework: `crates/umbral-core/src/inspect.rs:56` imports it (`use umbral_casing::{pascal_case_from_table, to_snake_case};`) and uses it at `inspect.rs:405,449,1221` to derive struct names from SQL table names.
- **Impact**: a plugin author writing a generator that maps a DB table name to a struct name — exactly the inspectdb shape — following the plugins-import-only-the-facade rule cannot reach the helper through `umbral::codegen`; the alternatives are a direct dependency on an "internal crate" (the README tells them not to) or re-implementing casing (the drift class this tree suffers from). No runtime breakage; the contract gap is in the doc claim plus the incomplete pass-through.
- **Fix**: add `pascal_case_from_table` to the `umbral_core::codegen` re-export (one line each in `crates/umbral-core/src/codegen.rs:30` and the facade list), and sweep the other public casing items for the same gap; or narrow the README sentence to name exactly what is re-exported. The pilot (`planning/review_4/umbral-casing.md`) should be amended — its out-of-scope notes record this claim as accurate.

### UMBRAL-2 · LOW · docs
**`arch.md §2.1` promises two escape hatches — `umbral::axum::*` and `umbral::db::query!` — that do not exist in the facade.**

- **Where**: `arch.md:65-66`; absence verified against `crates/umbral/src/lib.rs` (`grep -n "pub mod axum\|pub use axum"` = 0 hits) and `crates/umbral/src/lib.rs:406-408`.
- **Evidence** (arch.md, the authoritative spec per CLAUDE.md):
  ```
  | **axum** | **Hidden** by default. `umbral::web::{Router, Request, Response, Json, Path, Query, Form}`. Escape hatch: `umbral::axum::*`. |
  | **sqlx** | **Hidden** behind `QuerySet` / `Manager`. Escape hatch: `umbral::db::query!` is `sqlx::query!`. |
  ```
  The facade has no `axum` module, and `umbral::db::query` is only a value type for `raw_with`, not the sqlx macro namespace:
  ```rust
  // crates/umbral/src/lib.rs:406-408
  pub mod query {
      pub use umbral_core::_sea_query::Value;
  }
  ```
  The sqlx `macros` feature is deliberately excluded from the facade (`crates/umbral/Cargo.toml:33-41`: "deliberately not the broader `macros` feature — that also enables the `query!`/`query_as!` compile-time-checked-query macros"), which is the right call, but it means no `query!` re-export can exist as described.
- **Impact**: a contributor following the spec reaches for `umbral::axum::*` or `umbral::db::query!` and finds nothing; worse, the spec's stated escape hatch hides a real decision that was made differently (`QuerySet::raw`/`raw_with` + the un-re-exported `sqlx` derive path). Spec-vs-code drift compounds: the next spec update copies the table forward.
- **Fix**: rewrite the two table rows to name the shipped surfaces (`umbral::web::*` type re-exports; `QuerySet::raw`/`raw_with` per gaps4 #25; the `umbral::sqlx` re-export for `FromRow`) or record the missing escape hatches as a deliberate decision in `docs/decisions/`.

### UMBRAL-3 · LOW · docs
**The crate-level doc header still says `Status: M0`, twelve minor releases after M0.**

- **Where**: `crates/umbral/src/lib.rs:12-13`
- **Evidence**:
  ```rust
  //! Status: M0. Surfaces `Settings`, the db pool accessors, the web
  //! re-exports, and the `App` builder.
  ```
  The same file re-exports, in addition to those four things, `migrate`, `plugin`, `middleware`, `static_files`, `storage`, `cli`, `codegen`, `forms`, `validate`, `backup`, `templates`, `pagination`, `ratelimit`, `signals`, `inspect`, plus the whole `orm` (spanning `lib.rs:15-1001`).
- **Impact**: the first paragraph a docs.rs reader sees misstates the surface's maturity and extent; a reader cannot tell whether the paragraph is a plan or a fact.
- **Fix**: delete the line or replace it with the current version (`0.0.13`). Same class as the open `gaps6 #90` batch (stale code doc-comments), but this site is not one of its five enumerated items.

### UMBRAL-4 · INFO · docs
**Where**: `crates/umbral/src/lib.rs:947-954` — the `umbral::orm::post::post` doc says "The model is a development fixture: M3's `#[derive(Model)]` retires it". M3 shipped long ago and the fixture was not retired, so the sentence is stale and gives no reason for `umbral::orm::Post` still existing on the public surface; either state why it stays (doc examples / core tests) or retire it.

## Strengths

- **The architecture claim is enforced by the manifest, not prose.** `crates/umbral/Cargo.toml` lists only `umbral-core`, `umbral-macros` and five leaf libraries (`minijinja`, `serde_json`, `serde`, `sqlx`, `async-trait`); no plugin, no `umbral-rest`/`umbral-openapi`. The "serializers are a plugin" proof is visible in one file.
- **Macro-hygiene re-exports are individually justified.** `_serde_json` (`lib.rs:160`), `_serde` (172), `_sea_query` (181), `_sqlx` (189) and `__rt` (267) each carry a comment naming the generated code that needs them, so "why is this here" never has to be re-derived. The public `sqlx` re-export (`lib.rs:233`, `Cargo.toml:29-41`) documents the exact `derive`-without-`macros` feature choice and why the compile-time `query!` macros are excluded — a footgun turned into a stated decision.
- **Prelude discipline is deliberate and documented.** The prelude (`lib.rs:15-71`) re-exports no bare ambiguous names; `Result` is explicitly kept out ("Not in the prelude on purpose", `lib.rs:254`) and storage/pool accessors are reached by module path, with the reason stated inline (`lib.rs:22-23`). This is the anti-footgun prelude done right.
- **The facade's own macros have behavioral tests.** `tests/models_macro.rs` and `tests/umbral_main_macro.rs` compile and run the `models!` / `#[umbral::main]` public surface (both `Ok`/`Err` shapes, trailing-comma and rename forms), rather than relying on `ignore`d doctests — the right level of proof for a re-export crate.
- **Doc-claims were spot-checked against their sources and held**, including the non-obvious ones: `#[task]`'s "generated code references `::umbral_tasks::register_handler` directly" (`lib.rs:980-1001`, matches umbral-macros), the `Result` exclusion, and the `raw_with` doc on `db::query::Value`.

## Clean areas checked

- **`unsafe`**: none in the crate (`grep -n "unsafe" crates/umbral/src/` = 0 hits).
- **ORM-not-raw-SQL rule**: N/A by construction — no query code; `grep "sqlx::query"` = 0 hits.
- **Dependency direction**: `Cargo.toml` has no dependency on any `plugins/*` or plugin-side crate; the inward arrow holds. (Plugins referencing `umbral_core::*` appear only inside comments, not `use` statements.)
- **Feature gating**: exactly one feature (`postgis`, a pass-through to `umbral-core/postgis`); the sqlx feature set is `default-features = false` with `derive` but not `macros`, documented at `Cargo.toml:29-41`. A serializer-free app links no serializer code from this layer.
- **Prelude ambiguity audit**: every name in `umbral::prelude` checked for collision with std/other re-exports — none; the only bare traits are the framework's own (`Model`, `Plugin`, `Middleware`, …).
- **`STABILITY.md` tier mapping**: module-scoped power surfaces (`umbral::codegen`, backend/query builders) sit in the Evolving tier as the policy states; `pub use umbral_core::orm::forms_runtime` is correctly `#[doc(hidden)]` (`lib.rs:962-964`) as an Internal item.
- **tests/**: both test files assert behavior (the generated slice API is called; the generated `main` runs on both outcome shapes), not mere compilation.
- **Hygiene**: no stray artifacts, no committed db/lock files, no dead modules in the facade's own src.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #90` — open: batch of stale code doc-comments surfaced by the docs sweep (five enumerated sites in core/plugins). UMBRAL-3 is a further instance of the same class but not one of the enumerated sites, so it is kept numbered above for triage to fold in.

## Out-of-scope notes

- `crates/umbral-casing/README.md:5` — the "facade re-exports ... including what this crate provides" overclaim UMBRAL-1 cites; `planning/review_4/umbral-casing.md`'s out-of-scope notes call this claim accurate, which contradicts this file's UMBRAL-1 — route to the casing reviewer for amendment.
- `arch.md:101` — promises a `test_with_pool(pool, async { ... })` helper that scopes the ambient-pool override for a test future; `grep -rn "test_with_pool" crates/ plugins/` = 0 hits. The shipped substitute is `umbral-testing::boot`, whose one-shot semantics are reviewed in `umbral-testing.md` (TESTING-1/2); route to core/spec owners.
- `documentation/docs/v0.0.1/testing/setup.mdx:26-28` — user-facing copy of boot's "the rest are no-ops ... each test can just say what it needs" claim; must move together with the `umbral-testing.md` fix.
- `planning/REAL-GAPS.md:35` — row 18 marks "DB rollback per test" as shipped under Testing Utilities; `umbral-testing` has no rollback-per-test (boot's own doc: rows persist for the life of the test binary). Tracking-doc accuracy note for the repo/planner file, not a crate finding.
