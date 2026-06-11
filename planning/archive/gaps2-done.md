# Archived done entries from `bugs/gaps2.md`

Moved verbatim from `bugs/gaps2.md` to keep the active tracker small. Entry numbers are unchanged — a reference like `gaps2 #N` resolves here once the entry ships.

1. [x] **Save-feedback toast in the admin sheet — SHIPPED in commit `d2916d5` as gaps2 #13.** Same symptom; `d2916d5` wired `showToast` alongside the existing `closeSheet` + `refreshTable` HX-Trigger events on every CRUD success path (`sheet::sheet_create`, `crud::update`, `crud::htmx_delete`). 3 regression tests in `plugins/umbra-admin/tests/phase2_sheet.rs` pin the trigger payload. The failure-path toast already worked via the inline error fragment (commit `5b163ab`); this commit completed the success-side symmetry.
3. [x] **Change-password dialog extracted to an HTML `<template>` — SHIPPED in commit `5b22cc5`.** Dialog markup lives at `plugins/umbra-admin/templates/wrapper.html` as `<template id="umbra-change-password-dialog-template">`; the JS opener (`umbra._openChangePasswordDialog`) clones the template content and patches the form's `hx-post` URL to `{{ admin_base }}/<table>/<id>/change-password`. Form gained a `data-change-pw-form` attribute hook (single call-site-varying piece — the URL). Designers can edit the dialog markup without touching `<script>` tags; Tailwind's content scanner finds the classes natively; no JS string concat. Pinned by `change_password_dialog_uses_html_template_not_js_concat` in `plugins/umbra-admin/tests/phase4_dashboard.rs` — three asserts (template present, hook present, old shape absent).
15. [x] **REST `?include=fk1,fk2` query-param plumbing → DynQuerySet.select_related().** — Shipped.

    `DynQuerySet::select_related_dyn(&[String])` mirrors the typed `QuerySet::select_related` (validates each name is an FK on the meta, dedups, no-ops on bad names). `fetch_as_json` runs `hydrate_select_related_into` after the main fetch — one batched `IN (...)` per FK regardless of parent row count, splicing the resolved row's JSON in place of the integer id. Reuses the existing `queryset::hydration::fetch_related_as_json` helper that powers the typed path (now `pub(crate)`) so SQLite + Postgres dispatch stays in one place.

    REST list + retrieve handlers parse `?include=fk1,fk2` via `parse_include` — rejects unknown / non-FK names with a 400 (loud failure, unlike `?fields=` which silently drops) because an unknown include is almost always a typo or stale-client expectation.

    OpenAPI: `include_parameter` emits a per-resource `?include=` entry only when the model has FK columns. `x-umbra-include-fks` extension carries the candidate FK names so the playground builds a multi-select. Mirrors `x-umbra-fields-columns` shape.

    One-hop only on day one (matching the typed `select_related`'s current scope; nested `?include=author.manager` lands when typed `select_related("author__manager")` does — already supported on the typed side but not yet wired through the dynamic helper).

    Demo: `GET /api/customer/?include=user` returns customers with `user` as the full AuthUser object instead of `7`. `GET /api/customer/?include=banana` returns 400 with "?include=: unknown field `banana` on `customer`".

~~15. ~~ (the originally-open description below kept for archive trail)

    The ORM ships `select_related` (features.md #18 marked `[x]`) and FK fields serialize as the full object after the call. But the REST plugin's standard list / retrieve handlers use `DynQuerySet::fetch_json` / `first_json`, which DON'T accept a select_related hint — there's no query-param hook that turns `GET /api/customer/?include=user` into `Customer::objects().select_related("user").fetch()`.

    Today's workarounds:
      - **Custom action** (`ResourceConfig::action("with_user", ...)`) that calls the typed `Customer::objects().select_related("user")` and serializes the row by hand. Works, but every "I want the user expanded" call site needs its own action — doesn't scale to "expose the standard endpoint with optional expansion."
      - **`.computed("user_obj", |row| ...)`** does a second lookup synchronously inside the response transformer. Wrong shape — turns one query into N+1, defeats the whole point of select_related.

    **Fix shape**:
      - Add a `?include=` (or `?expand=`) reader to the REST list / retrieve handlers (alongside the existing `?fields=` sparse-fieldset, `?search=`, `?filter_*=` readers).
      - Parse comma-separated FK column names; reject unknown / non-FK fields with 400 (consistent with the rest of the validator surface).
      - Forward to `DynQuerySet` via a new `.select_related_dyn(&[...])` method — mirror the typed `.select_related()` shape but driven by `ModelMeta` lookups instead of compile-time field constants.
      - Emit the param into the OpenAPI spec as an `enum` of valid FK field names so the playground builds a checkbox/multi-select for it.

    **Scope reality check**: this is the ORM gap that flowed through to the REST surface — the typed path was completed in feature #18 but DynQuerySet was left behind (because it predates select_related landing). It's a focused ~150-line PR on the REST + ORM dynamic surface, mostly mechanical wiring.

    **One-hop only on day one** (matching the typed `select_related`'s current scope; nested `?include=author.manager` lands when typed `select_related("author__manager")` does).

20. [x] **Shop example ships render-blocking CDN Tailwind + Google Fonts — replace with compiled CSS + self-hosted Inter.** SHIPPED in commit `726fba6` (2026-06-09). Mirrored `plugins/umbra-admin`'s setup at the shop: `examples/shop/styles/{input.css,tailwind.config.js,package.json}` source dir + `examples/shop/build.rs` runs `npx tailwindcss --minify` when `styles/node_modules` exists. `@fontsource/inter` (Latin subset) gets inlined into the compiled `static/css/shop.css` (21 KiB minified, 5× smaller than CDN). 5 weights × {woff,woff2} = 10 Inter binaries committed at `static/css/files/`. `StaticPlugin::new("/static", "./static")` mounted in `src/main.rs`. `wrapper.html` lost the 4 CDN/Google-Fonts lines + the inline `<style>` block; gained a single `<link rel="stylesheet" href="/static/css/shop.css">`. Live verification: `shop.css → HTTP 200 / 21368 B`, `inter-latin-400-normal.woff2 → HTTP 200 / 23664 B`, zero `cdn.tailwindcss.com` / `googleapis.com` references in served HTML. Framework-level follow-up (`umbra startproject` should scaffold this shape by default) tracked under gap #8. Original diagnosis preserved below:

      - `cdn.tailwindcss.com/3.4.17` — **124 KiB**, ~310ms. JIT-in-browser bundle that Tailwind's own docs explicitly say is "for development only." Ships the full compiler to every visitor, runs JS before paint, causes FOUC.
      - `fonts.googleapis.com/css2?family=Inter` — **1.2 KiB CSS, ~430ms blocking**. Even with `preconnect`, the CSS itself blocks render; woff2 files are extra round-trips against an external origin.

    For an example app meant to teach production-shaped patterns, this is hostile — newer devs reading the shop will absorb "umbra apps use a Tailwind CDN" as the canonical shape and ship the same setup in their own apps.

    **Fix shape** (~30 minute job):

      1. **Compile Tailwind to a static CSS file at build time.** Same pattern `umbra-admin` already uses (see `plugins/umbra-admin/build.rs`). Add `examples/shop/styles/input.css` + `tailwind.config.js`, run `npx tailwindcss -i styles/input.css -o static/css/shop.css --minify --content "templates/**/*.html"` (either manually documented in the example's README, or wired via `build.rs`). Output: ~25 KiB minified — 5× smaller than the CDN bundle, zero runtime JS.

      2. **Self-host Inter as woff2.** Drop 5 woff2 files (Regular / Medium / SemiBold / Bold / ExtraBold) into `examples/shop/static/fonts/`. Add `@font-face` blocks to `input.css` (gets compiled into `shop.css`) with `font-display: swap` so text paints in the fallback immediately and re-renders when Inter loads.

      3. **Drop three lines from wrapper.html**: the cdn.tailwindcss.com script, the two `<link rel="preconnect">` lines, the `<link href="googleapis...">`. Add one: `<link rel="stylesheet" href="/static/css/shop.css">`.

    **Expected gains** (table values from the diagnosis):

    | Metric | Before | After |
    |---|---|---|
    | Render-blocking resources | 2 (310ms + 430ms) | 1 (~20ms local) |
    | CSS transfer | 125 KiB | ~25 KiB |
    | JS execution before paint | ~100 KiB Tailwind runtime | 0 |
    | External domains hit | 3 (cdn, googleapis, gstatic) | 0 |
    | LCP estimate | 700ms+ | ~150ms |

    **Framework-level fix lands via gap #8 (bootstrap project layout)**: `umbra startproject <name>` should scaffold the compiled-Tailwind setup by default — `styles/input.css`, `tailwind.config.js`, the build step in `build.rs` (or a documented npm script), `static/fonts/Inter-*.woff2`, and the wrapper template with `<link rel="stylesheet" href="/static/css/<name>.css">`. The CDN shape becomes the deliberate opt-out, not the default. Until startproject grows that scaffold, the shop should manually demonstrate the right shape so it doesn't actively teach the wrong one.

    **Triggering case to fix**: load `http://localhost:8000/` in Lighthouse → 700ms+ LCP from the two blocking resources. Post-fix the same page should hit ~150ms LCP locally.

19. [x] **`Form<T>` extractor + `#[derive(Form)]` validation — Shipped.**

    New surface in `crates/umbra-core/src/forms.rs`:

    - `Form<T>` axum extractor (struct) — wraps `Result<T, FormErrors>` so handlers ALWAYS receive a `Form<T>` and branch via `form.into_result()`. The HTTP layer never rejects on validation failure; the handler decides whether to re-render with errors or return 4xx.
    - `FormErrors` — thin wrapper around `WriteError` exposing `field_errors()` / `non_field_errors()` (DRF shape) AND `as_template_ctx()` (template-friendly flat shape: each field key maps to its FIRST error string, plus a `form` key for the non-field message).
    - `From<ValidationErrors> for WriteError` lifter — every per-field message becomes a `WriteError::Validator { field, message }`; non-field messages become an anonymous-field validator. Bundles under `WriteError::Multiple` for >1 error. **This is the architectural unifier**: ORM-validator errors, REST 400 bodies, admin form spans, and HTML form errors now all flow from the same `WriteError` source — no per-surface translator drift.
    - **Trait rename**: `forms::Form` (old) → `forms::FormValidate`. The name with generics went to the extractor (matches `axum::extract::Form<T>` shape); the trait got the more descriptive `FormValidate`. The derive macro name stays `Form` (it lives in a different namespace).

    Macro additions in `crates/umbra-macros/src/lib.rs`:

    - `#[form(required, ...)]` field-level — explicit Django-style declaration (no-op since Required is the default; accepted so users can mirror the spec's verbose shape).
    - `#[form(length(min = N, max = M))]` field-level — validator-crate-style combined syntax. Lowers to the same MinLength/MaxLength validators the legacy `min_length` / `max_length` keys produce.
    - `#[form(normalize_strings)]` container-level — auto-trims every `String` field before validation runs. Eliminates the per-field `form.name = form.name.trim().to_string()` boilerplate.

    Shop's `contact_submit` ported (`examples/shop/src/views/public.rs`) — the bespoke `ContactErrors` struct + `has_any()` + `validate_contact_form()` + `normalize_contact_form()` + `looks_like_email()` are GONE. Form now declares its rules inline:

    ```rust
    #[derive(Debug, Deserialize, Default, umbra::forms::Form)]
    #[form(normalize_strings)]
    pub struct ContactForm {
        #[form(required, length(min = 1, max = 100))]    name: String,
        #[form(required, email, max_length = 254)]        email: String,
        #[form(optional, max_length = 30)]                phone: String,
        #[form(required, length(min = 1, max = 200))]     subject: String,
        #[form(required, length(min = 10, max = 5000))]   message: String,
    }
    ```

    Handler is 25 lines instead of ~80.

    Live verification on the shop:
      - `POST /contact` with empty body → HTTP 422, every required field carries a rose-bordered input + "<field> is required" message, form-level banner "Please fix the highlighted fields and send again."
      - `POST /contact` with `email=not-an-email` → HTTP 422, only the email field highlights, message "email must contain `@`"
      - `POST /contact` with valid body → HTTP 303 redirect to `/contact?sent=1` (matches pre-port behaviour)

    Tests: 5 new in `crates/umbra-core/tests/form_extractor.rs` (happy path, missing-required, bad-email, normalize_strings, flat-template-ctx). 12 existing in `tests/form_derive.rs` still pass after the trait rename. Full workspace `cargo test`: 1219 passed, 0 failed.

    **Architectural rule shipped with this work** (per the spec): validation errors originate at the ORM's `WriteError`. Every surface MAPS them, none REDEFINES them. The `From<ValidationErrors> for WriteError` lifter is the proof — if a new surface (Form<T>, REST, admin, custom) needs to render validation errors, it consumes `WriteError`'s accessors. New custom field-type validators declare a `Validator` variant once and flow through every surface for free.

    **Deferred** (not gating the PrimaryKey swap):
      - Validator-crate integration (`#[validate(email)]` / `#[validate(url)]` / `#[validate(range)]` attrs from the `validator` crate). The macro accepts the simple shapes today; the rich rule set lands behind a cargo feature when a real consumer surfaces a need.
      - Multipart / file-upload bodies. Current extractor is x-www-form-urlencoded only.
      - Input-preservation on re-render: today the failure branch re-renders with a default form (not the user's typed input). Pairing `axum::Form` + `Form<T>` in the same handler would give both shapes; v1 keeps the surface simple at the cost of one extra retyping on rare validation failures.

19. ~~ (the originally-open description below kept for archive trail)

    Reference: `examples/shop/src/views/public.rs:21-279` is the canonical "what every handler ends up writing today" — a bespoke `ContactForm` struct, a parallel `ContactErrors` struct with one `Option<String>` per field, a `has_any()` accessor that ORs every Option, a `validate_contact_form()` that walks the form imperatively setting error strings one by one, and a `normalize_contact_form()` that trims + lowercases before validation. ~80 lines of boilerplate for what should be 5 lines of declarative validators on the form struct.

    Three things wrong with the hand-rolled pattern:

    1. **Single-error-per-field**. `errors.name: Option<String>` can't represent "name is too short AND contains numbers" — the second error overwrites the first or the validator short-circuits. The REST plugin's `WriteError::field_errors() -> BTreeMap<String, Vec<String>>` (lives in `crates/umbra-core/src/orm/write.rs:120`) is the right shape; HTML form handling should reuse it.

    2. **Diverges from REST's shape**. The REST plugin returns `{"email": ["This field is required."], "phone": ["Bad format."]}` on validation failure (DRF-style). HTML form handlers return their own ad-hoc envelope (`ContactErrors`) that templates have to wire to per-field placeholders by hand. Same validation problem, two different solutions, two different error shapes — the framework should pick one.

    3. **No reusable rules**. `looks_like_email`, `trim()`, "is required" — every form re-implements the same checks. The validator crate (already in the Rust ecosystem) provides `#[validate(email)]`, `#[validate(length(min = 5))]`, etc. as attribute macros; integrating it is the front-runner.

    **Proposed shape — `Form<T>` extractor + structured errors**:

    ```rust
    use umbra::forms::{Form, FormErrors};

    #[derive(Form, Deserialize)]
    #[form(normalize_strings)]                  // auto-trim every String field
    struct ContactForm {
        #[form(required, length(min = 1, max = 100))]
        name: String,

        #[form(required, email)]                // validator crate's email rule
        email: String,

        #[form(length(max = 30))]
        phone: String,

        #[form(required, length(min = 1, max = 200))]
        subject: String,

        #[form(required, length(min = 10, max = 5000))]
        message: String,
    }

    pub async fn contact_submit(form: Form<ContactForm>) -> Result<...> {
        // form is Result<ContactForm, FormErrors> — either valid input
        // or the structured error map. No manual checks at the call site.
        match form.into_result() {
            Ok(valid) => { /* persist, render success */ }
            Err(errors) => render_with_errors(errors),    // ← FormErrors
        }
    }
    ```

    `FormErrors` shape:

    ```rust
    pub struct FormErrors {
        pub fields:     HashMap<String, Vec<String>>,   // per-field
        pub non_field:  Vec<String>,                    // form-level
    }
    ```

    Renderable directly in templates with `{% for msg in errors.fields.email %}<p class="err">{{ msg }}</p>{% endfor %}` — no per-field `Option<String>` plumbing.

    **Architectural rule: validation errors originate at the ORM. Every surface MAPS them, none REDEFINES them.**

    The ORM is the only layer that knows the truth about a column — its type, its NOT-NULL-ness, its UNIQUE constraints, its `#[umbra(...)]` validators, its `#[validate(...)]` rules, its FK target's existence. So the ORM's `WriteError` (`crates/umbra-core/src/orm/write.rs`) is the **canonical, structured error type** that every higher surface must use as input. Not as inspiration, not as "shape we mirror" — as the actual `serde_json`-friendly value they parse and render.

    Today the REST plugin already does this correctly: its 400 body is `WriteError::field_errors()` serialised verbatim (DRF-style `{"field": ["msg"]}`). The work this gap names is **bringing the other two surfaces in line**:

    ```text
                       ┌──────────────────────────────────────────────┐
                       │  ORM: WriteError (the single source of truth)│
                       │  - RequiredFieldMissing { field }            │
                       │  - BlankNotAllowed     { field }             │
                       │  - ForeignKeyNotFound  { field, target, val }│
                       │  - UniqueViolation     { field, value }      │
                       │  - Validator           { field, message }    │
                       │  - TypeMismatch        { field, ... }        │
                       │  - .field_errors() -> HashMap<String, Vec>   │
                       └──────────────────────────────────────────────┘
                                          │
                  ┌───────────────────────┼───────────────────────┐
                  ▼                       ▼                       ▼
            ┌──────────┐           ┌──────────────┐         ┌───────────┐
            │ REST 400 │           │ Admin form   │         │ HTML Form<T>│
            │ body     │           │ template     │         │ extractor   │
            │  (today  │           │ (gap #12)    │         │ (gap #19)   │
            │  works)  │           │              │         │             │
            └──────────┘           └──────────────┘         └───────────┘
        DRF-shaped JSON       per-field error spans      template ctx for {% for %}
    ```

      - **Re-use `WriteError::field_errors()` directly.** `FormErrors` is NOT a new type — it's a type alias / thin wrapper around `WriteError::field_errors()`'s `HashMap<String, Vec<String>>` + `non_field_errors()`'s `Vec<String>`. The validator crate's `#[validate(email)]` failure produces a `WriteError::Validator { field, message }` that flows through the same accessor. There is no second error map.

      - **HTML `Form<T>` failure path** = constructs `WriteError::Multiple { errors: vec![...] }` from validator rules, returns it via the existing accessor. Same type, same shape, just a different population source than the ORM's own.

      - **Admin form-submit handlers** = already receive `WriteError` from `insert_json` / `update_json` (today they stringify it via `sqlx::Error::Protocol` — see commit 5b163ab and gap #12). Stop stringifying; thread the `WriteError` through to the template context as `field_errors` + `non_field_errors`.

      - **REST handlers** = already do this correctly. No change.

    **Rule for plugin authors**: a new custom field type (`#[derive(Model)]` field with custom validators) declares a `Validator` variant on `WriteError` once. The error appears in REST 400s, admin form spans, and HTML form extractors with zero per-surface plumbing. That's the test of whether the unification worked.

    **Anti-pattern to forbid**: a surface inventing its own error type (`AdminFormErrors`, `ContactErrors`, `MyEndpointErrors`) and translating from `WriteError` at the boundary. The translation IS the bug; every surface's "translation" drifts independently and the framework grows three subtly-different error shapes nobody can map between. Every gap entry that mentions form errors should reference `WriteError` as the source.

    **Triggering case for v1**: `examples/shop/src/views/public.rs::contact_submit` — porting that handler to `Form<ContactForm>` would drop the file by ~80 lines (the bespoke validators and the `ContactErrors` struct disappear). Use that as the proof-of-shape consumer.

    **Estimate**: ~400 lines across:
      - `umbra-core::forms` — the trait + `FormErrors` + `Form<T>` extractor + `normalize_strings` impl.
      - `umbra-macros::Form` — derive that walks `#[form(...)]` attributes, emits a `validate(&self) -> Result<(), FormErrors>` method.
      - Validator-crate integration (feature-gated so REST-only apps don't pull it).
      - One worked example (shop's contact form) + one passing integration test.

    **Related to**: gap #12 (admin per-field rendering), features.md #51 (form validation framework — currently a stub).

18. [x] **Nested `?include=` (dotted / `__` chain) — ORM half shipped.**

    ORM-side fix landed in `crates/umbra-core/src/orm/dynamic.rs`:

    - `normalize_sr_token` accepts both `.` and `__` separators (mixed in one token is fine too) and normalises to the canonical dotted form.
    - `validate_sr_chain` walks the FK graph hop-by-hop against `registered_models()`; rejects on missing meta / unknown column / non-FK column.
    - `select_related_dyn` calls `validate_sr_chain` and silently drops invalid chains (preserves the pre-existing single-hop drop contract for power-user/internal callers).
    - `hydrate_select_related_into` rewritten to mirror `queryset::hydration::hydrate_select_related_nested`: per-hop batched IN through `fetch_related_as_json`, bottom-up embed of each level into the previous one's hop slot, then splice level-0 into the root rows. Query budget = `1 + len(hops)` per chain regardless of parent count.

    REST surface (`plugins/umbra-rest/src/lib.rs::parse_include`) now validates the same way upstream — loud 400 with the resolved chain on the failing hop (not a silent drop), depth-capped at 3 hops per the spec.

    Demo: `GET /api/post/?include=author.profile` issues 3 queries total (posts → authors IN → profiles IN); `GET /api/post/?include=author__profile` is identical; `GET /api/post/?include=author.banana` returns 400 with "?include=: unknown field `banana` on `author` (resolving chain `author.banana`)".

    **Deferred to a follow-up**: `?fields=user.profile.email` recursive sparse-fieldset walk (gap's part 4) — REST-side concern in `apply_sparse_fields`, untouched by this turn. The ORM contract is now nested-capable; the sparse-fieldset reader just needs to recurse instead of `split_once('.')`.

18. ~~ (the originally-open description below kept for archive trail)

    Today (commits f6f204a + 182703e) `?include=` and `?fields=` are ONE HOP only on the dynamic / REST path:

      - `?include=author` works; `?include=author.profile` is silently dropped.
      - `?fields=user.id` works; `?fields=user.profile.email` no-ops (the inner `profile.email` is treated as a literal key on the user object).

    The typed ORM IS nested-capable — `crates/umbra-core/src/orm/queryset/hydration.rs::hydrate_select_related_nested` handles `__`-containing names like `Post::objects().select_related("author__manager")`. One batched query per hop, no recursion, query budget = `1 + len(hops)` regardless of parent count. The dynamic side (`select_related_dyn` + `hydrate_select_related_into`) was implemented against the SINGLE-hop helper only.

    **Worked target**: `GET /api/post/?include=author.profile&fields=id,author.profile.github_url`
      - Issues 3 queries total: posts → authors (IN) → profiles (IN).
      - Returns each post with `author.profile.github_url` accessible, everything else dropped.

    **Circular cases stay safe**: `?include=user.customer.user` doesn't loop infinitely. Each hop is one query against a known target table; the framework never traverses anything the caller didn't explicitly name. The depth in the URL IS the budget.

    **Fix shape**:

      1. **URL convention — accept both `.` and `__`.** Either separator works on `?include=` and on the dotted path of `?fields=`. Both of these resolve to the same chain:

         ```
         ?include=author.profile&fields=author.profile.github_url
         ?include=author__profile&fields=author__profile__github_url
         ?include=author.profile&fields=author__profile__github_url   ← mixed is fine too
         ```

         Two reasons to accept both:
           - **`.`** is what most REST APIs use (URL-natural, no underscore-encoding considerations) and matches the dotted-fields semantic already shipped in commit 182703e.
           - **`__`** is what Django / DRF users reach for muscle-memory-wise (mirrors `Post::objects().select_related("author__profile")` from the typed side).

         Cost: trivial — a one-line normalization (`name.replace("__", ".")` or vice versa) at the top of `parse_include` + `apply_sparse_fields` before any other processing. The internal representation stays canonical; the URL-facing surface accepts both. No semantic difference, no precedence rules to remember.

         **Edge case to pin in tests**: a column NAMED with a double underscore (e.g. `meta__source`) would alias to `meta.source`. Real models don't do this, but the test should assert that an unknown-after-normalisation path returns 400 with a message that surfaces the resolved chain — gives the caller enough info to spot the collision if it ever happens.

         Internally normalise to the Django-style `__` only when calling into the existing typed `hydrate_select_related_nested` helper, since that's what it expects.

      2. **`select_related_dyn`** — accept dotted names. For each token containing a dot, validate the first hop is an FK on the current meta (existing check); look up the FK target's `ModelMeta` from `registered_models()`; validate the next hop on that meta; repeat. Reject the whole token with 400 if any hop fails.

      3. **`hydrate_select_related_into`** — for dotted names, delegate to a `hydrate_dyn_nested(target_chain, ids, rows)` helper that mirrors `hydrate_select_related_nested`'s pattern: per-hop IN query, splice each level's JSON into the prior level's object.

      4. **`apply_sparse_fields`** — recursive dotted matching. Replace the current `split_once('.')` with a real path walk: for `user.profile.email`, descend into row["user"], then ["profile"], then keep ["email"]. The "most-specific wins" rule extends naturally (any dotted token under a parent filters that parent's nested object at the named depth).

    **Day-one constraints**: depth limit (cap at e.g. 3 hops in the REST handler — pathologically deep `?include=a.b.c.d.e.f.g` is almost always a typo). Integer PK targets only on every hop (matches existing select_related constraint).

    **Estimate**: ~250 lines across `dynamic.rs` (recursive resolver + chained hydration) + `lib.rs::parse_include` (multi-hop validation) + `apply_sparse_fields` (recursive walk). Pre-existing typed helper covers the SQL pattern; the dynamic side is wiring + meta-graph traversal.

17. [x] **Playground multi-select pickers for `?include=` and `?fields=` — SHIPPED in commit `3ff8d22`.** New `plugins/umbra-playground/frontend/src/components/IncludeFieldsPicker.tsx` component renders a popover-anchored checkbox list driven by the OpenAPI spec data the SPA already has in memory (`fieldInfosFromSchema(listItem, spec)` for the resource columns + `f.fkTarget` for the FK badges + `components.schemas[fkTarget]` for nested sub-trees). Two variants:

    - `?include=` — one checkbox per FK column with a `→ <target>` badge. Empty resource → "No FK columns on this resource" message instead of a broken-feeling empty popover.
    - `?fields=` — top-level checkboxes for every column, plus a nested sub-tree per FK column with dotted notation (`user.email`, `user.username`). Cross-coupling: ticking `user.email` auto-enables `?include=user` so the response actually carries the nested keys.

    Filter box, alphabetical-sort serialization for stable URLs, lossless round-trip with the legacy free-text input shape (a user with `user,billing_address` typed in pre-fix sees both pre-checked on first open). Wired into `RequestBuilder.tsx` via a 60-line dispatch: when `p.name === "include"` or `p.name === "fields"` AND a `listItem` schema is available, render the picker instead of the plain `<Input>`. Empty-schema fallback to the free-text input so an in-flight spec fetch doesn't yield a useless greyed-out button. Vite + tsc build clean; full umbra-playground tests (8) + full workspace sweep (1243) green. Live-verified: `/api/playground/assets/index-Bus4dlCi.js` HTTP 200, 912 KiB. Original symptom preserved below:

    **The spec already publishes the data the SPA needs:**

      - `?include=` → `x-umbra-include-fks` vendor extension (commit f6f204a) is an array of FK column names the resource exposes.
      - `?fields=` → `x-umbra-fields-columns` is an array of every column name on the resource.

    Both are sitting in the OpenAPI document waiting to be read. The SPA's existing string-input renderer ignores them.

    **Fix shape**:

      1. **`?include=` picker** — checkbox group. Read `x-umbra-include-fks`, render one checkbox per FK name. Checking emits comma-joined names into the query-string state. Single click, no typing.

      2. **`?fields=` picker** — two-level checkbox tree. Top level lists `x-umbra-fields-columns`. Each FK column expands (when included via the `?include=` picker above) into a sub-tree of the FK target's columns, accessed via dotted notation (`user.id`, `user.email`, ...). Checking nested boxes emits `user.id, user.email` etc. The dotted-vs-plain semantic from commit 182703e: plain box = full nested object, any nested box = filtered to those keys.

      3. **Cross-coupling**: enabling a checkbox in the `?include=` picker should unlock the corresponding sub-tree in the `?fields=` picker (otherwise the SPA would let users tick `user.email` without realising they also need `?include=user`). Or — be helpful and auto-enable the include when a dotted field is ticked.

    **What the spec is missing for full nested rendering**: the FK target's column list. Today `x-umbra-include-fks` is just FK names; the SPA can render the include picker but not the sub-tree for fields until it also knows what columns each FK target has. Two paths:

      - **Server-side**: extend `x-umbra-include-fks` from `["user", "billing_address"]` to an object `{"user": ["id", "username", "email", ...], "billing_address": [...]}`. One extra walk through `models_for_plugin` at spec-build time; no runtime cost.

      - **Client-side**: the SPA already loads the full OpenAPI spec; it has every `components.schemas.<X>` entry. When rendering Customer's fields tree, look up the `User` schema from the spec's components for the `user` FK's nested columns. Zero server change; just SPA lookup work.

    Client-side is cleaner — the spec already has the data and we don't need a new extension. The component-resolution pattern is already in `components/EndpointDetail.tsx` for the body-schema panel.

    **Related**: gap #15 (FK include — shipped) and the dotted-fields work (commit 182703e) built the backend surface. This gap is the SPA UI making it usable without manual typing. Until shipped, users have to know columns by name; once shipped, building a request is point-and-click.

16. [x] **M2M echo on `DynQuerySet::fetch_as_json` is N+1.** — Shipped.

    `hydrate_m2m_batched(meta, pk_name, &mut rows)` in `crates/umbra-core/src/orm/dynamic.rs` runs ONE `SELECT parent_id, child_id FROM <junction> WHERE parent_id IN (...)` per registered M2M relation, then groups by parent and splices each row's `<relation>: [child_id, ...]` array. Per-row, per-relation `hydrate_m2m_into` calls were removed from `fetch_as_json`'s row loop and replaced with a single post-loop batched call. Query budget drops from `1 + N*M` to `1 + count(M2M relations)` regardless of N.

    Preserves the existing contract: parents with no junction rows still surface the field as an empty array (initialised up front before the SELECT). Mixed integer + string PK shapes both work (`pk_json_key` namespaces the group key as `n:` vs `s:` so a numeric PK and a stringified-equal PK can't collide).

    Single-row insert / update paths still use the per-row `hydrate_m2m_into` (they only have one row — batching there would be ceremony, not savings).

    Demo: `GET /api/post/?fields=id,title,tags` on a 20-post page now issues 2 queries (1 + 1) instead of 21.

16. ~~ (the originally-open description below kept for archive trail)

    `crates/umbra-core/src/orm/dynamic.rs` lines ~744 + ~766: for every row in `fetch_as_json`, if `meta.m2m_relations` is non-empty, the loop calls `hydrate_m2m_into(meta, pk, &mut entry).await?` — which runs ONE `SELECT child_id FROM <junction> WHERE parent_id = ?` per parent per M2M relation. So `GET /api/post/` against N posts with M m2m relations issues `1 + N*M` queries total.

    Pre-existing — predates the `?include=` work in commit f6f204a — but the same batched-IN pattern that fixed FK expansion fixes this too:
      - Collect all parent PKs across the N rows (already in scope).
      - For each M2M relation, run ONE `SELECT parent_id, child_id FROM <junction> WHERE parent_id IN (...)` query.
      - Group results by parent_id, splice each row's children in via the existing `hydrate_m2m_into` shape.

    Same query budget guarantee `?include=` gets: `1 + count(M2M relations)` regardless of N. The typed-ORM path already does this via `prefetch_related` (features.md #19 marked `[~]` partial); the dynamic path was left behind, same way `select_related` was before this turn.

    **Recommendation**: extract `batched_m2m_for(meta, parent_pks, &mut rows)` from the per-row helper, fan out per-relation IN-queries, splice results. ~80-line PR mirroring the FK expansion shape from commit f6f204a. Worth doing in the same revisit window — the two helpers (`hydrate_select_related_into` for FKs, the new `hydrate_m2m_batched` for M2Ms) sit next to each other in dynamic.rs and share the "collect ids → batched IN → splice" pattern.

    **Triggering case to fix**: `GET /api/post/?fields=id,title,tags` on a 20-post page currently issues 21 queries (1 + 20). Should be 2 (1 + 1).

14. [x] **Template-side reverse-O2O / forward-FK traversal on `user` — Shipped.**

    `user_context_layer` in `plugins/umbra-auth/src/session_user.rs` now expands relations on the serialized user up to depth 2, recursively, with `(table, pk)` cycle detection. Templates can write `{{ user.customer.loyalty_points }}` directly and get the resolved value — no handler-side prefetch declaration, no `tokio::Handle::block_on` ceremony, no template-level `.await`.

    What's expanded at each hop:
      - **Forward FKs**: every FK column on the current row is replaced with the full target row (mirrors the dynamic `select_related_dyn` semantics from gap2 #15).
      - **Reverse-O2O**: every other registered model with a UNIQUE FK pointing at the current table gets injected under the child's table name as the key (`Customer { user: FK<AuthUser> (unique) }` → `user.customer`). Naming follows Django's lowercase-model convention.
      - **Skipped**: M2M arrays (different shape; pre-resolving every parent's tag set on every request was the wrong trade-off), reverse-FK arrays without UNIQUE (one-to-many; same reason).

    Query budget per authenticated request: `1 (user) + count(relations within depth 2)`. For the shop's `AuthUser`, that's `1 + 1 (customer)` = 2 queries — the second hop walks back into `auth_user` from `customer.user` and hits the cycle guard, so no extra query.

    Always-on once `.with_user_in_templates()` is set; the depth is fixed at 2 via `USER_RELATION_DEPTH` constant. Anonymous requests stay on the cheap path (no expansion, just the `{ is_authenticated: false }` sentinel).

    Verified live (shop, shopadmin session):
    ```
    user.username = shopadmin
    user.customer is defined = true
    user.customer.id = 1
    user.customer.loyalty_points = 0
    user.customer.phone = +15555550100
    user.customer.user.username = (stopped — cycle detected)
    ```

    **What's still not shipped** (intentional out-of-scope for this turn):
      - `request` namespace (Django's `request.user.X`) — umbra exposes `user` directly; adding `request` would mean materializing a per-request context object in templates. Worth a separate gap entry if anyone needs Django-shape compatibility.
      - Reverse-FK arrays (`user.orders` returning all of a user's orders). Different cardinality, different cost model — the M2M-style fan-out cost would be wrong for the "every authenticated request" budget. Keep this opt-in via handler-side prefetch.
      - Custom `UserModel` impls (non-AuthUser): the expansion currently hard-binds to the `auth_user` table lookup in `serialize_authenticated_with_relations`. A custom user would need its own middleware variant or a generalised hook. Backlog if a custom-user app surfaces.

14. ~~ (the originally-open description below kept for archive trail)

    A user wrote `{{ user.customer.id }}` in a template, expecting the Django shape `{{ request.user.profile.email }}` — where you walk the related object directly in the template. It failed because:

    1. **`user` in templates is the JSON-serialized AuthUser** (commit `bd48bf8` shipped this — see [auth/user-in-templates.mdx](/v0.0.1/auth/user-in-templates)). Reverse-OneToOne accessors like `user.customer().await` are Rust async methods; they don't survive serialization to `serde_json::Value`.
    2. **Templates can't `.await`** — minijinja is sync. So even if we exposed a method, the template couldn't drive the DB read.
    3. **Pre-resolving every possible relation** on every request would be wasteful (a typical AuthUser has 10+ reverse-FK candidates, none of which most pages need).

    Today the canonical workaround is what the shop's `/me` handler does — resolve the relation in Rust, stuff the resolved value into the template context explicitly:

    ```rust
    let customer = user.0.customer().await?;
    let customer_id = customer.as_ref().map(|c| c.id);
    render("me.html", &context!(username, customer_id))
    ```

    Template: `{{ customer_id }}` — works, but the handler had to know in advance what the template needs.

    **Possible shapes for a real fix** (none ship today; this is the design space):

    - **A) Synchronous resolver registration**. Templates expose a callable `user.related("customer")` that the framework satisfies by enqueueing the lookup and blocking on it via a `tokio::runtime::Handle::block_on`. Works but couples templates to the runtime; very easy to misuse (one big N+1 risk).
    - **B) Eager prefetch declaration in the handler**. `render_with_prefetch("me.html", user.with_prefetch(&["customer", "orders"]))` declares which relations the template will walk; the framework pre-resolves before rendering. Honest about cost (the handler picks what to load), Rust-typed, no runtime surprises. Closest fit to Django's `select_related` + `prefetch_related` pattern.
    - **C) Custom minijinja function**. `{{ resolve(user, "customer").id }}` — explicit "I'm hitting the DB here," same block_on machinery as A but no surprise (the call is visible in the template).

    **Recommendation**: ship B first. It's honest about cost, integrates with the existing `select_related` / `prefetch_related` ORM surface, and the handler-declared prefetch list is greppable. Defer A / C until a real consumer surfaces — most templates need the same 3-4 relations across most pages anyway, and the handler is the right place to declare them.

    **Why not just throw away the request**: the user's expectation ("walk relations like Django does") is right — it's the framework's job to figure out HOW to deliver that within Rust's constraints. The async-sync gap is the obstacle; the prefetch declaration is the answer.

    **For now**: documented as a manual handler-side resolution in [auth/user-in-templates.mdx](/v0.0.1/auth/user-in-templates) and [orm/relationships.mdx](/v0.0.1/orm/relationships). Users hitting the wall here should reach for option (B)'s ergonomics via the existing `OneToOne<C>` parent-side field + `select_related`.

13. [x] **Admin form success: no toast + no table refresh after sheet-create / sheet-edit.** Reported 2026-06-09 after a clean Customer create — the request succeeded, the row landed in the DB, but the user saw nothing happen on the page:

    - **No success toast.** A failed submit fires a clear inline error (commit `5b163ab`), but a successful submit is silent. The user can't tell the sheet's submit handler from the sheet's "I closed without saving" handler.
    - **No row in the list.** After the create, the sheet closes (or doesn't — see below), but the changelist behind it still shows the pre-create page. The user has to hit refresh to see the row they just created.
    - **Sheet stays open / closes inconsistently.** Depending on whether the user clicked "Save" vs "Save & continue editing," the sheet behaviour differs in opaque ways.

    **Root cause**: the sheet's create handler (`plugins/umbra-admin/src/handlers/sheet.rs` + `handlers/crud.rs::create_post`) returns either a `Redirect::to(...)` on success or the re-rendered form on error. The Redirect is the wrong shape for an HTMX sheet — HTMX follows the 303, swaps the redirect target's body into `#umbra-sheet-slot`, and the user's browser sits on a janky half-state. There's no first-class "success → close sheet + toast + refresh table" path.

    **Fix shape**:
      - Success response sets `HX-Trigger` header with two events: `umbra:rowCreated` (carries the new row id) + `umbra:showToast` (carries the message). The wrapper.html JS already has `showToast` listener wiring (line ~1212); add a `rowCreated` listener that:
        - Closes the sheet (`umbra.closeSheet()`).
        - Triggers an `htmx.trigger('#changelist-table', 'umbra:reload')` so the table fetches the new page.
      - Save vs Save&Continue: Save closes; Save&Continue keeps the sheet open BUT swaps the body to the edit form for the just-created row (HX-Location to the edit-sheet URL with `hx-target=#umbra-sheet-slot`).
      - The same shape applies to edit/update: success → toast "Updated" + reload the row's `<tr>` in place (the existing row-update HTMX pattern in `rows_fragment.html` already supports this).

    **Why HX-Trigger over JS**: keeps the contract server-side. The handler decides "this was a success → fire these events"; the JS is generic listener wiring. A future custom action (mark order as shipped, etc.) reuses the same `umbra:showToast` listener without touching the handler-side ergonomics.

    **Related**: gap #12 (per-field error rendering) and this success path land naturally in the same refactor — both touch the form-submit handlers and both depend on a structured `WriteError` flowing through the response, not a stringified one. Both could ship in one focused commit.

11. [x] **Persist all admin UI state into `AdminUserPref` — filters, sort orders, page sizes, search, per-table preferences.** SHIPPED in two commits: `89fba96` (round 1: schema + helpers + filter/search/sort/per_page round-trip) and `d569fcb` (round 2: last_path, column visibility, widget period overrides). `AdminUserPref` grew `preferences: Option<String>` — one nullable TEXT column, one auto-generated migration (`0002_add_admin_user_pref_preferences`), zero backfill. JSON blob shape (`tables.<table>.{filters,search,sort,per_page,hidden_cols}` + `last_path` + `dashboard.widget_periods.<key>`) sits behind seven typed helpers in `plugins/umbra-admin/src/models.rs`: `get_table_pref` / `set_table_pref` / `toggle_table_col` / `get_last_path` / `set_last_path` / `get_widget_period` / `set_widget_period`. Read-modify-write merges throughout so sibling keys coexist (pinned by `last_path_coexists_with_table_pref_writes`). Three handler wirings:

    1. **Changelist (`/admin/{table}/`)** — paramless visit + saved prefs → 303 to the saved URL; every render writes `(filters, search, sort, per_page)` and `last_path`; render-time `display_cols` filter against `hidden_cols`. `POST /admin/{table}/columns/{column}/toggle` flips visibility and returns `HX-Trigger: refreshTable + showToast`.
    2. **Admin index (`/admin/`)** — first visit AFTER a changelist visit 303-redirects to the user's `last_path`. Opt-outs: `?dashboard=1` (explicit dashboard intent) and HTMX requests (the dashboard's own widget hx-gets pass through this handler).
    3. **Dashboard widget data** — period resolution priority: URL `?period=` → saved `preferences.dashboard.widget_periods.<key>` → widget's registration-time `default_period`. URL `?period=` writes the user's override as a side-effect so chip clicks become sticky cross-tab / cross-device.

    `plugins/umbra-admin/tests/phase4_user_prefs.rs` carries 12 tests (was 3); 9 new pins cover every helper + their interactions (per-table namespacing, malformed-JSON-graceful, idempotent toggle, sibling-coexistence). Full umbra-admin suite: 128 passed.

    What's deliberately deferred as separate items: per-table density override (low value vs. the global density toggle); `favorites` sidebar pinning (no consumer yet); the changelist front-end UI affordance for column visibility (a chip strip / dropdown in the header would invoke the toggle endpoint — storage + endpoint are in place, just the template surface left). Original symptom + design preserved below: `plugins/umbra-admin/src/models.rs:43` already has `AdminUserPref { theme, density, sidebar_collapsed, dashboard_layout, updated_at }` and the upsert plumbing (`fetch_or_default`, `upsert`). Only two pieces of UI state currently round-trip through it: theme + sidebar-collapsed (typed columns) and the dashboard layout (a JSON blob in `dashboard_layout: String`). Everything else lives in the URL and dies on refresh / new tab / restart:

    - Active list filters (`?filter_status=active&filter_brand=acme`).
    - Search input (`?search=widget`).
    - Sort column + direction (`?sort=-created_at`).
    - Page size (`?per_page=50`).
    - Page number (`?page=3`).
    - Per-table column visibility (the "hide cost column" affordance from feature #57).
    - Density override per-table (a designer might want the products table compact even when the rest is comfortable).
    - Last-viewed model (so `/admin/` can land them back in the table they were working on instead of the dashboard).

    Symptom: an admin filters Products to `status=active, brand=acme, sort=-price, per_page=50`, opens a product to edit it, hits "Save and continue editing," and lands back on a Products list that's lost every filter — the URL the form-action POST'd to doesn't carry the changelist's query string, and the redirect-after-save goes to the canonical `/admin/product/` with no params. Today the user re-applies the four filters by hand every time.

    **Proposed shape**: extend `AdminUserPref` with a `preferences: serde_json::Value` field — a free-form JSON map keyed first by feature/table, then by setting:

    ```jsonc
    {
      "tables": {
        "product": {
          "filters":      { "status": "active", "brand": "acme" },
          "search":       "widget",
          "sort":         "-price",
          "per_page":     50,
          "hidden_cols":  ["cost", "external_id"],
          "density":      "compact"          // overrides global
        },
        "order": {
          "filters":      { "status": "shipped" },
          "per_page":     20
        }
      },
      "dashboard": {
        "widget_periods": {
          "shop_daily_sales_chart": "7d",    // overrides Widget::default_period
          "shop_activity_chart":    "30d"
        }
      },
      "last_path": "/admin/product/?filter_status=active",
      "favorites": ["/admin/product/", "/admin/order/"]
    }
    ```

    Read path: the changelist handler reads `preferences.tables.<table>` on first hit and rewrites the URL with the persisted query params (HTTP redirect, so the URL is the source of truth from there on). The dashboard's widget chip strip reads `preferences.dashboard.widget_periods.<key>` instead of `Widget::default_period` when present (the existing `default_period` becomes a "first-ever-visit" default).

    Write path: every URL-state-changing interaction (filter chip click, sort header click, page-size dropdown, search submit, widget period chip) fires an HTMX `hx-trigger="every change"` to `POST /admin/api/prefs/tables/<table>` (or `.../dashboard/widget_periods/<key>`) with the new value. The handler debounces (~500ms client-side via `hx-trigger="change delay:500ms"`) and merges into `preferences` via `JSON_SET` (Postgres) / `json_replace` (SQLite) so two tabs filtering different tables don't clobber each other.

    **Why JSON blob over typed columns**: the surface grows. Every new admin feature adds at least one piece of per-user state (column visibility, density per-table, favorite filters, saved searches with a name, dashboard widget overrides per period, etc.). Each typed column requires a migration + a typed accessor; the JSON path is one migration + a `serde_json::Value` to read into. The framework still gets to type-check what it cares about — the existing `theme` / `density` / `sidebar_collapsed` columns stay typed for the global cross-page settings; `preferences` is for the per-table / per-widget state where churn is highest.

    **What this unblocks**:
      - Feature #8 (per-user widget reordering) — `preferences.dashboard.widget_order: ["shop_total_sales", "shop_orders", ...]` is the natural place.
      - "Save and continue editing" landing back on the filtered changelist instead of the canonical URL.
      - Cross-device continuity — a user filtering on their laptop sees the same filters on their phone, because the URL gets rehydrated server-side.
      - Per-table "Show me what I last saw" — `last_path` redirect at `/admin/`.

    Migration: add `preferences: serde_json::Value` (Postgres `jsonb`, SQLite `TEXT` storing JSON) to `AdminUserPref`. Default `{}`. The existing `dashboard_layout: String` field could fold into `preferences.dashboard.layout` long-term but stays typed for one release as a compatibility shim — gives the framework's own dashboard-layout code one cycle to migrate without breaking on-disk rows.

9. [x] **`render_500` swallows secondary template errors silently** — Fixed. `render_500` in `crates/umbra-core/src/errors.rs` now `match`es on the recovery template's render result instead of `.ok()`-ing it. Secondary failures get `tracing::error!`'d with the template name + the failure cause + a hint about the likely `{% extends "wrapper.html" %}` chain bug. In dev mode the body of the plain-text fallback includes both errors so the developer sees the recovery failure inline instead of having to grep logs.

    **Sibling issue (also fixed)**: the 500-rendering path runs OUTSIDE the user-context middleware's task-local scope, so `user` was undefined in the recovery template even when AuthPlugin's middleware was mounted. Fixed in `crates/umbra-core/src/templates.rs::merge_ambient_user` by injecting an anonymous-user sentinel (`{ is_authenticated: false, is_staff: false, is_superuser: false }`) when no task-local is set — so the 500 template gets a defined `user` to evaluate against regardless of where in the layer stack it renders.

    Originally found while debugging the `request.user.is_staff` template error in `examples/shop/templates/wrapper.html:37`. Re-hit while debugging `request.user.customer.id` in `me.html:19` — same chain (handler 500s → 500.html extends wrapper.html → secondary failure → silent plain-text fallback). Both halves of the chain now behave correctly: the wrapper renders cleanly with the anonymous-user fallback, and any further secondary failure surfaces with full diagnostics.

7. [x] **Wire `AuthPlugin::with_user_in_templates()`** — Shipped. The builder method lives on `AuthPlugin` and flips a `user_in_templates: bool` field; the impl uses `Plugin::wrap_router` (which was already there — no new framework hook needed) to mount `user_context_layer` on the full merged router. Templates now write `{% if user.is_staff %}` cleanly; anonymous requests see `{ is_authenticated: false }` and the link hides; staff requests see the populated context and the link surfaces. Shop wired with `.with_user_in_templates()` on its AuthPlugin call. The doc-comment-references-a-nonexistent-method case is what triggered the new "Fix, don't patch" CLAUDE.md rule.

28. [x] **`allowed_hosts` request-time enforcement — SHIPPED.** `crates/umbra-core/src/hosts.rs` adds a `host_guard` middleware wired automatically by `App::build()` (outermost layer, after CORS) that rejects a request whose `Host` header isn't in `settings.allowed_hosts` with a **400** — the request-time half of Django's `ALLOWED_HOSTS` that defends against Host-header injection (cache poisoning, poisoned password-reset / absolute-URL links). Enforced **only in `Environment::Prod`** (dev passes through so localhost / LAN-IP / tunnel hosts don't 400 mid-iteration). Pattern matching mirrors Django: exact (`example.com`), subdomain wildcard (`.example.com` → domain + any subdomain), and `*` (allow-any escape hatch / "disable"); port and IPv6 brackets are stripped from the Host value. Driven by the existing `settings.allowed_hosts` (default `["localhost","127.0.0.1"]`, `UMBRA_ALLOWED_HOSTS` / `umbra.toml`) — pairs with the pre-existing `check.rs::settings_allowed_hosts` boot warning. 7 tests in `hosts.rs` (exact / case-insensitive / subdomain / star / port+IPv6 stripping / middleware 400-vs-200 in prod / dev pass-through). NOT CORS — that's browser cross-origin response policy; this is server-side Host validation.

29. [x] **CORS path scoping — SHIPPED.** `AppBuilder::cors_for(prefix, CorsConfig)` (`crates/umbra-core/src/app.rs`) layers CORS only onto requests whose path starts with the prefix (e.g. `"/api"`), leaving HTML pages same-origin — the django-cors-headers + DRF shape. Implemented as a `ScopedCorsLayer`/`ScopedCors` tower layer in `cors.rs` that holds both the CORS-wrapped and bare inner service and dispatches per `req.uri().path()`. Batch `allow_origins(vec![...])` also shipped. The shop now uses `.cors_for("/api", CorsConfig::strict().allow_origins(vec![...]).allow_credentials(true))`. Pinned by `cors::tests::scoped_cors_only_affects_matching_prefix` (a `/api` request gets `access-control-allow-origin`, a non-`/api` request doesn't). Global `.cors(CorsConfig)` retained for whole-app policies. Remaining (deferred, low priority): nested/regex path patterns and folding CORS into a dedicated `umbra-cors` plugin for config-struct symmetry with `umbra-security`.
26. [x] **Signed/session-bound CSRF (`SecurityConfig::signed_csrf`) is now the default — SHIPPED in commit `f145daf` (2026-06-10), with the admin's mint unification in `38ff747`.** The original blocker ("the admin mints raw tokens via `generate_token`, a signature-requiring middleware would 403 admin login") was removed by making the middleware the ONLY mint: `csrf_middleware` now mints **before** the handler runs on safe methods, scopes the token into the new `umbra::templates::CURRENT_CSRF` task-local (commit `08f6ce2`), and the admin's `ensure_csrf_token` prefers that ambient token over self-minting (self-mint survives only for SecurityPlugin-less deployments, where the admin's own `login_post` comparison — now constant-time via the newly-`pub` `umbra_security::tokens_match` — is the validator). The deploy-safety mechanism for the flip is **rotation**: `CsrfState::token_acceptable` rejects any cookie token that can't pass signed-mode validation (typically a pre-upgrade unsigned cookie) and the middleware re-mints + re-sets it on the next safe request, so existing browsers converge instead of 403ing. Also fixed in the same commit: the middleware's `Set-Cookie` attach switched from `insert` (which clobbered handler-set cookies, e.g. the session cookie) to `append`; `ensure_csrf_cookie` + the `response_sets_csrf_cookie` deference logic were deleted outright. Pinned by 6 integration tests in `plugins/umbra-security/tests/csrf_flow.rs` (first-visit pre-handler mint, append-not-replace, POST re-render scoping, 403 mismatch, unsigned→signed rotation, no-rotation for valid signed cookies) and the rewritten `middleware_token_wins_over_handler_minted_cookie` in `tests/integration.rs`. Design: `docs/decisions/2026-06-10-automatic-csrf.md`. Original directive preserved below:

    Make signed/session-bound CSRF (`SecurityConfig::signed_csrf`) the default once every token-minting path uses the signed mint. SHIPPED as opt-in (HMAC-SHA256 over the random token keyed by `secret_key`, optional `session_bind_cookie`; closes the AUTH-7 sibling-subdomain cookie-injection gap and finally gives `secret_key` a job). It defaults off because the admin mints raw tokens via `umbra_security::generate_token()` (`plugins/umbra-admin/src/auth.rs` `ensure_csrf_token`) and a signature-requiring middleware would 403 admin login. Route the admin's mint through a shared signed mint (e.g. expose `umbra_security::mint_csrf(&headers)`), then flip the default to on.

42. [x] **FK save binds text not bigint — SHIPPED (Plan B, 2026-06-11).** Saving a foreign-key value through the dynamic JSON write path hit `column "plugin" is of type bigint but expression is of type text` (Postgres) because `json_to_sea_value`'s `ForeignKey` arm (`crates/umbra-core/src/orm/write.rs`) bound *every* string-valued FK id as `SeaValue::String` (TEXT). The function's signature was `(SqlType, &JsonValue, bool, &str)` — it had **no access to `fk_target`**, so it couldn't tell an i64-PK FK from a String-PK one and defaulted any `JsonValue::String` FK id to text. The earlier WIP's `form_str_to_sea_value` FK arm (`orm/dynamic.rs:1954`) was already correct because it short-circuits FK before calling `json_to_sea_value` and resolves via `fk_target_pk_sql_type`; the bug lived on the JSON path (`DynQuerySet::insert_json`/`update_json`) and latently on the typed `create` path (`build_insert_one_for`).

    **Fix (root-cause, not a patch):** threaded the resolved target-PK `SqlType` into `json_to_sea_value` as a new `fk_target_pk: Option<SqlType>` parameter. The FK arm now coerces against it — `Some(Text)` binds the id as text, `Some(Uuid)` parses+binds a UUID, and a numeric-PK target (or unresolved `None`, the common i64 case) coerces the string/number → `BigInt` via the existing `coerce_i64` (which already accepts `JsonValue::String("1")`). Every caller across the workspace was updated to pass the hint: the dynamic paths (`insert_json`/`update_json` in `orm/dynamic.rs`) pass `fk_target_pk_sql_type(col)`; the typed paths (`build_insert_one_for`/`build_insert_many_for` in `orm/queryset/write_helpers.rs`, plus the `update_values` / bulk-update / save paths in `orm/queryset/mod.rs`) pass a new `pub(crate) fk_pk_hint(field)` helper resolving `FieldSpec.fk_target` → `pk_meta_for_table`; PK-bind call sites and the umbra-admin junction binds (which pass the PK's own `SqlType` directly, never `ForeignKey`) pass `None`. Non-FK behavior is byte-for-byte identical (the new arm only triggers on `SqlType::ForeignKey`).

    **Tests (`crates/umbra-core/tests/fk_save_coercion.rs`):** four pins. The deterministic SQLite proof is `json_fk_arm_coerces_numeric_string_to_bigint` — pre-fix it returned `SeaValue::String(Some("1"))`, post-fix `SeaValue::BigInt(Some(1))` (and a `Some(Text)` target still binds text). Three behavioral round-trips drive the REAL public paths — `insert_json` with `{"parent": "1"}`, typed `Manager::create` with `ForeignKey::new(id)`, and `insert_form` with a string FK id — each seeds an i64-PK parent, writes the child, reads it back, and asserts `child.parent.id()` + `resolve().name` link the real parent row. NB: on SQLite a TEXT bind into an INTEGER column is silently corrected by column affinity, so the round-trips can't go red on SQLite (only Postgres rejects outright); the unit-level `SeaValue` assertion is what makes the bug deterministic on SQLite. The typed + dynamic parallel pins keep the two write paths from diverging.

    Verified: `cargo build`, `cargo test -p umbra-core` (each binary green in isolation; the only aggregate-run failures are the pre-existing gaps2 #30 cross-binary contention — `json_form_parse`/`annotate_count`, which pass alone and don't touch the FK path), `cargo test -p umbra-admin` (32+ tests, the original failing consumer), `cargo test -p umbra-rest`.

39. [x] **`annotate_count` follow-ups: child-side filters + child soft-delete; Form derive auto-skips `ReverseSet` — SHIPPED (Plans A + D, 2026-06-11).**

    **(a) Child-side filters + soft-delete awareness (Plan D).** `soft_delete: bool` was lifted onto `ModelMeta` (`crates/umbra-core/src/migrate.rs`, `#[serde(default, skip_serializing_if = "is_false")]` so old migration snapshots still deserialize) and onto `ReverseFkRelationSpec` (`orm/model.rs`), both filled by the Model derive / `ModelMeta::for_` from the child model's `Model::SOFT_DELETE` (commits `bd21c79`, `bd99808`). `annotate_count` now auto-folds `AND <child>.deleted_at IS NULL` into the correlated subquery when the resolved child relation is soft-delete (`1fc971b`). A new `annotate_count_where::<C: Model>(alias, relation, Predicate<C>)` renders a typed child predicate into the same subquery WHERE alongside the FK correlation — Django's `Count("comments", filter=Q(moderation="visible"))` (`1dd3961`). M2M relations also annotate (count junction rows). The umbra.dev homepage moved to `annotate_count_where::<PluginComment>("comment_set_count", "comment_set", plugin_comment::MODERATION.eq("visible"))` so it counts visible-only, soft-delete excluded automatically (`a518803`). Behavioral tests in `crates/umbra-core/tests/annotate_count.rs`: soft-deleted child excluded (3→2 via the real `delete()` path), filtered visible-only count, M2M junction count, zero-child parent still returned as 0. The generic predicate's bare child columns resolve to the subquery's innermost FROM (the child table) — verified sound in review.

    **(b) Form derive auto-skips `ReverseSet` (Plan A).** `#[derive(Form)]` previously rejected a `ReverseSet<C>` field with the unsupported-type error, forcing a manual `#[umbra(noform)]`. `expand_form`'s `form_is_reverse_relation` now skips both `ReverseSet<C>` AND reverse `OneToOne<T>` (the `#[sqlx(skip)]` variant) before type classification — they are never user-submittable by construction, mirroring what the Model derive already does for `FIELDS` (commit `27ce2ec`). Test `reverse_relations_absent_from_fields` asserts both a `ReverseSet` and a reverse-O2O field compile without `#[umbra(noform)]` and are absent from the derived `fields()`. This is what let `PluginComment` re-derive `Form` cleanly.

    Both halves verified green; the holistic branch review (1363 tests, 0 real failures) cleared it.

40. [x] **Foreign keys work with `#[derive(Form)]` (`ModelChoice`) — SHIPPED (Plan A, 2026-06-11).** `umbra_website/plugins/plugin_directory/src/models.rs` `PluginComment` (the ln-343 case) had its `Form` derive commented out as an admin workaround because the derive couldn't handle FK fields. Now a `ForeignKey<T>` (or `Option<ForeignKey<T>>`), and equally a forward `OneToOne<T>` (a unique FK), become a `ModelChoice` form field — Django's `ModelChoiceField`. The submitted id is parsed to the FK target's PK kind (`PkKind` resolved from the registry via `pk_kind_for_table`); `FormValidate` went async (`#[async_trait]`) so `validate()` verifies the referenced row exists through the ORM (`DynQuerySet::for_meta(meta).filter_eq_string(pk, id).count()`) before insert, and `render_html` fetches `(id, label)` rows to emit a populated `<select>` (label = first non-PK text column). Commits: `2ede012` (async `FormValidate`), `4aa060e` (FK/forward-O2O → `ModelChoice` + a gated `ForeignKey<T>: Default`), `41b0106` (async existence check), `382ba8f` (async render fetches options), `844eabc` (re-enable `derive(Form)` on `PluginComment`, delete the hand-rolled `Default`). A latent footgun from `ForeignKey<T>: Default` (a non-nullable FK left at the id-0 placeholder being silently inserted off the form path) was closed at the contract level by a zero-FK insert guard in `build_insert_one_for`/`build_insert_many_for` that returns a clear `WriteError` instead of persisting a dangling `FK = 0` (`b9a80ca`) — without breaking the REST `FK=0`→"not found" client contract (the guard lives in the INSERT builders, not the validation layer). Behavioral tests in `crates/umbra-core/tests/form_fk.rs` (FK round-trip + `resolve()` returns the real parent; nonexistent id → field error + no row; forward-O2O UNIQUE violation) and the `PluginComment` acceptance test. The parallel M2M `ModelMultiChoice` (junction write) + choices `Select` shipped in the same plan.
