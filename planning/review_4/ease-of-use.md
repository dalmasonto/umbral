# ease-of-use — review_4 companion (2026-10-10)

How easy is umbral to use, what can be automated away, and what's hidden that should be unhidden. Synthesized from the review_4 sweep (every claim below maps into `gaps7.md`), plus evidence reads of the two surfaces that define the real developer journey: the scaffold's generated `main.rs` (`crates/umbral-cli/src/scaffold.rs:685-760`) and the flagship app (`examples/shop/src/main.rs`, 328 lines). Point-in-time, commit `e0d5e18b`. This is a proposal document — the proposals need product decisions before they become work.

## The verdict

The **loop** is easy: declare a model → `makemigrations` → `migrate` → run — with sensible defaults (REST read-only by default, admin CRUD for every registered model, plugin migrations applied automatically). The **wiring** is manual and multiplicative: the same model is declared once but configured in up to four plugins by hand. The **hidden truths** are where the worst DX lives: two documented admin password flows are dead (`gaps7 #10`, `#11`), hidden columns are silently stripped from writes, dev pages break under a compression layer (`#87`), and a documented slim build doesn't compile (`#27`). Fixing what's *broken* and making what's *hidden* visible are worth more than any new feature.

## Evidence: what a developer types today

**Fresh scaffold** — a new project's `main.rs` is a ~65-line builder chain hand-wiring seven plugins, one `.model::<Post>()`, and one `ResourceConfig::new("post")`, plus CSRF/CORS/static configuration. Every *new* model means editing at least the models block and the REST resource block — and optionally admin/graphql/dashboard blocks.

**The flagship (`examples/shop`)** — `product` alone is configured in four places:

| Surface | What the developer types | Where |
|---|---|---|
| Registry | `.model::<Product>()` (via plugin autodiscovery for plugin models) | plugin crates |
| Admin | `AdminModel::new("product").search_fields(&[...]).list_filter(&[...])` | `main.rs:127-130` |
| REST | `ResourceConfig::new("product").hide("cost").permission(...)` | `main.rs:208-210` |
| GraphQL | `.expose("product").hide("product", "cost")` | `main.rs:233-242` |
| Dashboard | listed again in `dashboard_models_only(&umbral::models![Product, ...])` | `main.rs:152-159` |

The same pattern repeats for `order`, `brand`, `post`. The app also assembles its auth chain (`main.rs:90-94`), its CORS config, and its full route table by hand. The GraphQL block's own comment shows why this is a trap, not just tedium: *"REST hides don't carry across plugins… Miss this line and `{ products { cost } }` hands your margins to anyone who asks."* Nothing in the framework shows the developer that matrix.

## What is already easy (preserve; don't "fix")

- **Plugin migrations are automatic** — `migrate` walks every registered plugin's `plugin.migrations()`; adding auth adds its tables with zero setup.
- **Task handlers are inventory-discovered** (`gaps4 #40`) — a `#[umbral::task]` fn self-registers. This is the precedent for the proposals below.
- **Admin auto-mounts every registered model** — `register_for`/`register` are *refinements* (search/filter/labels), not registration.
- **`auto_migrate_on_serve()` + `seed_on_serve()`** — the dev loop is already seam-free on `serve`.
- **One app-wide `.authentication(...)` chain** inherited by REST and GraphQL (`gaps4 #42`); **env-driven settings**; **safe-by-default REST**; a real CLI surface (`typegen`, `inspectdb`, `transferdata`, `showmigrations`, `createsuperuser`); scaffolds and `startplugin` generate compiling, wired starting points.

## Three tiers of avoidable typing

### Tier 1 — wiring: the standard stack should be one intent
Today the scaffold emits the same seven-plugin block into every project, and real apps add six more by hand. **Proposal:** scaffold **presets** (`umbral startproject shop --preset web|api|saas`) that only shape the *generator* — the emitted code stays the same explicit builder chain, because the explicit chain is the architecture's contract (keep it). Add `umbral dev` that composes the pieces that already exist (serve + `auto_migrate_on_serve` + typegen watch + optional livereload) so the dev loop is one command with zero hand-wiring.

### Tier 2 — model registration: kill the N×`.model::<T>()`
Two steps, ordered by cost:

1. **Batch builder (small).** `.models(umbral::models![Post, Comment])` — the `models!` macro already exists (`crates/umbral/src/lib.rs:121`); a builder consuming it replaces N calls with one list. The list is also a natural home for the doctor matrix (below).
2. **Zero-config registration (medium, feature-gated).** `umbral-core` **already depends on `inventory`** and uses link-time discovery for task handlers — extend the pattern: the `Model` derive `submit!`s its metadata behind an opt-in feature (`auto-register`), with opt-out attributes for tests. This deletes an entire error class ("forgot `.model::<T>()` ⇒ empty admin / 404 REST"). Tradeoffs to respect: the model registry is process-wide (`OnceLock`), multi-app tests already strain it (see core findings on global-state limits) — so feature-gate it, document that it is link-time (one binary, no dylibs/WASM surprise), and pair it with the doctor so "which models exist" stays visible rather than implicit.

### Tier 3 — per-model refinements: one declaration, per-surface defaults
Split the configuration by *ownership*, not by convenience:

- **Model-owned descriptors** (search fields, filters, ordering, labels, icons, password column) belong on the model: `#[umbral(search_fields = "name, sku")]` should be the single place admin **and** REST read — the plumbing half-exists (admin falls back to `model.search_fields`, `rows.rs:62-70`; REST reads model metadata for queryable columns once `#9` lands the visibility-policy fix). **Extend the fallback coverage**: `list_filter`, ordering, and admin `label`/`icon` should default from the model (display name / table) so `.label("Users")` stops being required typing; `AuthPlugin` should supply `password_field("password_hash")` for `auth_user` as a plugin-contributed default instead of every app re-declaring it (`shop main.rs:141-146`).
- **Endpoint-owned exposure** stays explicit: which surface exposes which model, with which permission and which hidden columns, is a *security* decision per surface (the GraphQL comment argues this well). Do **not** unify disclosure; unify defaults for *descriptors*, keep widening explicit.

## Unhide the hidden

1. **`umbral doctor` (or `umbral routes --matrix`)** — print, per model × per surface: registered? admin (label, search/filter)? REST (permission posture, hidden/denied columns)? GraphQL (exposed / `expose_if` / hidden)? typegen current? Plus pending migrations, drift, and the system-check warnings. Today the only way to know is reading `main.rs` across four plugins — the shop's own comment proves a missed cell is a data leak. The registry and plugin configs are already introspectable; this is mostly presentation.
2. **Make the silent loud in Dev** — follow the framework's existing boot-warn pattern (`plugin.security_missing`, "gated model without auth"): log when a hidden/denied column is stripped from a REST/admin write (today a silent 200 — see `gaps6 #20`'s class), and have the doctor *inform* (not warn) about registered-but-unexposed models. Warnings that teach at boot beat discovering at 2am.
3. **Errors that teach** — one workstream over `gaps7 #46` (garbled field name in REST errors), `#59` (typo'd plugin flag reads as "unknown command"), `#93` ("database error" reported as "no matching record"), `#135` (DB outage as a wave of 401s), `#17` (wrong status class). Theme: every error names the field, the limit, and what to do.
4. **Docs that are true** — the sweep's stale-doc cluster (`#108-111`, `#119-124`, `#167-175`): the fix pattern is to make claims checkable (un-`ignore` doctests, mirror generated trees from the generator's own list, correct comments in the same commit as the code).
5. **Dead documented flows first** — `#10`/`#11` (both admin password flows; `examples/shop` and `derive-demo` are live consumers), `#27` (slim build), `#87`/`#166` (dev pages break under compression / blank on read failure). Nothing erodes trust like a documented feature that cannot work.
6. **Feature discovery** — the framework hides genuinely good tools (typegen, inspectdb, transferdata, `showmigrations`). Surface them in `umbral --help`'s descriptions and one "what's available" page; the feature matrix becomes discoverable once `#27` is fixed.

## Ranked shortlist

| # | Move | Size | Why first |
|---|---|---|---|
| 1 | Fix `#10`/`#11` (dead password flows) + `#29` (CI) | hours | Trust + a gate so fixes stay fixed |
| 2 | `umbral doctor` matrix | small | Un-hides the highest-stakes hidden thing (exposure) — and every proposal below gains its visibility surface |
| 3 | `.models(...)` batch builder; then feature-gated inventory auto-registration | small → medium | Kills the N× registration typing; stands on machinery that already ships |
| 4 | Model-level descriptor defaults (`search_fields` first, then `list_filter`/label/icon; AuthPlugin's `password_field`) | medium | Deletes the 3–4× per-model typing; coordinate with `#9`'s visibility-policy refactor |
| 5 | Scaffold presets + `umbral dev` | medium | One-command start; codegen-only, no runtime magic |
| 6 | Dev loudness pass (stripped-write log, doctor intel) | small | Pairs with 2 |

## What NOT to do

- Don't make exposure magical — safe defaults plus explicit widening is the framework's security shape; automate *descriptors*, never *disclosure*.
- Don't hide the builder chain behind runtime magic — keep generated code explicit; put the smarts in the generator.
- Don't automate against the registry globals carelessly — `OnceLock` semantics are already load-bearing (multi-app tests); every registry change needs that lens.
