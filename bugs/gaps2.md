# Seen/Known gaps - Continued from @gaps.md

1. [ ] When you save an item in the create/edit sheet in the admin panel, you don't see any feedback toast ie was it saved successfully or did it fail?
2. [ ] Can we have a posthog wiring maybe as a plugin, or a way of linking such logging systems into umbra
3. [ ] The change password widget is in js - Very wrong, write this in html, give it an id, query, get the inner html and replace the replaceable parts. Don't try this game for js inline html. That is the work for HTML templates. This is in wrapper.html around line `// ---- Change password dialog ----`
4. [ ] Wrapper.html is growing larger and larger. Replace most widget specific js with `/static/<WIDGET>.js` files. Static assets should be auto served from the admin plugin using the static plugin, we don't have to manually write them anywhere. Just reference the static assets into wrappet.html. The same goes with the inline style elements. Move them to `/static/index.css` files and reference them in wrapper.html.
5. [ ] Ability to register custom widgets, ie with full html, js, and css. Its like self contained widgets or widgets that extend on top of the current setup ie tailwind widgets with apex charts.
6. [ ] Ability to create more dynamic widgets right from the admin. This is inline with the ability to create dynamic admin pages ie /admin/page/<reports> which holds specific data like different report widgets etc. This is captured in `../features.md #4, #56, #76` and

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

21. [ ] **Template-side image optimization — auto lazy-loading + responsive srcset + on-the-fly resize.** Today templates write raw `<img src="...">` tags: no `loading="lazy"`, no `decoding="async"`, no `srcset`, no modern format (`webp` / `avif`), no resize. Every visitor downloads the original asset at full size. For a product-image-heavy app like the shop this is the biggest LCP / bandwidth lever after the Tailwind/font fix from gap #20.

    Scope: HTML templates only. REST responses ship raw URLs as before — the API consumer picks how to handle images.

    **Three options on the table:**

      - **A. Custom MiniJinja `img` filter** — `{{ product.thumbnail | img }}` expands to a fully-formed `<img>` with `loading="lazy"`, `decoding="async"`, `srcset` for 1x/2x/3x, format negotiation. Registered globally on the env once (see `crates/umbra-core/src/templates.rs`).
        - **Pro**: zero ambiguity at the call site; the template author can't accidentally ship a heavy image. Composable with any URL — model field, hardcoded path, computed value.
        - **Con**: opt-in per call site. Old templates with raw `<img>` tags don't benefit until rewritten.

      - **B. `lol_html` post-render middleware** — parses the rendered HTML in a streaming rewriter, injects `loading="lazy"` + `decoding="async"` on every `<img>` that's missing them.
        - **Pro**: covers EVERY template + every plugin's template + every error page automatically. No call-site changes.
        - **Con**: streaming-but-still-parses every byte of HTML on every response (CPU overhead for high-traffic apps). Can't easily inject `srcset` because that requires knowing the asset's real dimensions on disk.

      - **C. On-the-fly resize handler** — `/static/images/...?w=400&format=webp` routes through a Rust handler that reads the original, resizes via `image` / `fast_image_resize`, transcodes to webp/avif, caches the result on disk (or in `umbra-cache`). Original asset never reaches a browser.
        - **Pro**: solves the "raw 5MB JPEG" problem at its source, not just the markup. Works regardless of which markup option above is used.
        - **Con**: meaningful infra surface — needs a cache layer + cache invalidation + storage growth strategy.

    **Recommendation: ship A + C as a pair.**

    - A is the ergonomic surface developers actually touch — one filter, one mental model. The filter emits URLs pointing at C's handler with the right `?w=...&format=...` params, so the markup AND the bytes get optimized together.
    - C does the actual heavy lifting (resize + transcode + cache); the filter is just curated URL generation.
    - B becomes the safety net only IF a real call site stays raw — defer until that surfaces. Skipping B keeps the per-response CPU budget free for actual work.

    Skip B unless a real consumer needs it; the filter-then-handler pattern is enough for the 90% case and is cheaper at runtime.

    **Dependencies + crates**:
      - `image` (or `fast_image_resize` for hot-path scaling) — resize + transcode.
      - `umbra-cache` (feature #44) — result caching keyed by `(asset, width, format)`. Fine to start with on-disk caching in `target/image-cache/` and switch to Redis later.
      - A tiny `umbra-media` adjacent (or just folded into core templates) plugin that registers the `img` filter + mounts the resize route.

    **Triggering case**: same Lighthouse run as gap #20 — the shop's product images hit hundreds of KB each on the home + product-list pages. Post-fix should drop to ~20-40 KB per image (webp at the right pixel size) AND defer offscreen images entirely via `loading="lazy"`.

20. [ ] **Shop example ships render-blocking CDN Tailwind + Google Fonts — replace with compiled CSS + self-hosted Inter.** Reported 2026-06-09. Lighthouse on `examples/shop/templates/wrapper.html` flags two render-blocking resources:

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

19. [ ] **Form validation primitive — `Form<T>` extractor returning `{field_name: ["msg1", "msg2"]}` instead of hand-rolled per-app validators.** Reference: `examples/shop/src/views/public.rs:21-279` is the canonical "what every handler ends up writing today" — a bespoke `ContactForm` struct, a parallel `ContactErrors` struct with one `Option<String>` per field, a `has_any()` accessor that ORs every Option, a `validate_contact_form()` that walks the form imperatively setting error strings one by one, and a `normalize_contact_form()` that trims + lowercases before validation. ~80 lines of boilerplate for what should be 5 lines of declarative validators on the form struct.

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

17. [ ] **Playground: render `?include=` and `?fields=` as multi-select pickers, not free-text inputs.** Reported 2026-06-09. Screenshot: `/home/dalmas/Pictures/Screenshots/Screenshot from 2026-06-09 05-47-52.png` — the playground currently surfaces both params as plain `string` inputs where the user types `user, billing_address` (for include) or `user.email, user.username, id, loyalty_points` (for fields). Error-prone, no discoverability, no autocomplete, easy to typo a column name and get a 400.

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

13. [ ] **Admin form success: no toast + no table refresh after sheet-create / sheet-edit.** Reported 2026-06-09 after a clean Customer create — the request succeeded, the row landed in the DB, but the user saw nothing happen on the page:

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

12. [~] **Admin form errors — DynError enum landed; per-field template rendering still to do.**

    **Part 1 (shipped):** `DynError` in `crates/umbra-core/src/orm/dynamic.rs` lifted from `pub type DynError = sqlx::Error;` alias to a real enum `pub enum DynError { Write(WriteError), Sqlx(sqlx::Error) }` with `From<sqlx::Error>` + `From<WriteError>` + `Display` + `Error` impls. Form-coercion failures in `insert_form` / `update_form` / `update_one` now emit `DynError::Write(WriteError::Validator { field, message })` carrying the offending column name, replacing the pre-fix `sqlx::Error::Protocol("umbra::orm::write: <message>")` string-flatten that lost the per-field hint.

    `AdminError` (in `plugins/umbra-admin/src/error.rs`) gained a `Write(WriteError)` variant + `From<WriteError>` + `From<umbra::orm::DynError>` impls so admin handlers' `?` chains route Write → Write and Sqlx → Sqlx. `sanitise_form_error` (`util.rs`) gained the matching `Write` arm — renders the validator message directly with capitalisation matching the legacy sqlx::Error::Protocol path. The REST plugin's `ApiError` gained the parallel `From<DynError>` impl so its `?` chains stay clean.

    Every admin call site that constructed `AdminError::Sqlx(e)` against a now-`DynError` value was lifted to `AdminError::from(e)` so the enum dispatch is preserved (3 sites: `crud.rs::delete`, `crud.rs::bulk_delete`, `sheet.rs::change_password`, plus `inline_edit.rs` which already used `AdminError::Sqlx(e)` for a sanitise call now goes through the From impl too).

    Verification: 3 new tests in `crates/umbra-core/tests/dyn_error_enum.rs` pin the contract (`form_coercion_failure_surfaces_as_dyn_error_write_with_field_name`, `update_form_coercion_failure_also_surfaces_as_dyn_error_write`, `dyn_error_lifts_via_from_for_both_arms`). Full workspace `cargo test`: 1214 passed, 0 failed.

    **Part 2 (deferred to its own PR):** the form template (`form.html` + `_macros/sheet.html` + `_macros/field_editor.html`) doesn't yet consume the `WriteError::field_errors()` map — `sanitise_form_error` still flattens to a single string at the top of the form. The plumbing is now in place; threading `field_errors: HashMap<String, Vec<String>>` into the template context is a focused admin-template change without further ORM work. This is what unblocks gaps2 #19 (`Form<T>` extractor) too — same template surface, same context key.

12. ~~ (the originally-open description below kept for archive trail)

    Today's commit `5b163ab` made the admin surface the message text from `WriteError` (e.g. "A record with this `user` already exists.") instead of the blanket "database error" — but the message is delivered as a single string at the top of the form. The REST plugin returns the same write-error as a structured per-field map:

    ```jsonc
    // POST /api/customer/ with a duplicate FK → 400
    {
      "user": ["A row with this value already exists."],
      "phone": ["This field is required."]
    }
    ```

    The admin form template should render the same shape — each `<input>` knows its column name; render any messages for that key directly beneath the field as a small red span, the way DRF + Django admin both do. A "Customer" form with three FK fields and a UNIQUE violation is currently ambiguous ("which FK collided?"); per-field rendering removes the guess.

    **Root cause**: `DynQuerySet::insert_form` / `update_form` return `Result<_, DynError = sqlx::Error>`. The validator's structured `WriteError` (already a real enum with `field_errors()` + `non_field_errors()` accessors — see `crates/umbra-core/src/orm/write.rs:113-241`) gets flattened to `sqlx::Error::Protocol("umbra::orm::write: <message>")` at the boundary, losing the per-field map. The admin form handler then has nothing to render per-input — only the joined string.

    **Fix shape**:
      - Promote `DynError` from a `pub type DynError = sqlx::Error;` alias (`dynamic.rs:45`) to a real enum: `pub enum DynError { Write(WriteError), Sqlx(sqlx::Error) }` with `From<sqlx::Error>` for backwards compat. `insert_form` / `update_form` / `insert_json` / `update_json` return the structured error directly.
      - Extend `AdminError` with a `Write(WriteError)` variant. The admin form-submit handlers branch on it: render `field_errors()` per-input + `non_field_errors()` at the top.
      - `form.html` + `_macros/sheet.html` + `_macros/field_editor.html` accept a `field_errors: HashMap<String, Vec<String>>` context map and render messages under each input that has a key.

    Same architectural footprint as REST's existing 400 response shape — REST already builds this map (`umbra-rest/src/handlers.rs::write_error_to_drf_body`); the admin should reuse the same `WriteError::field_errors()` accessor.

    **Demo case to fix**: a Customer form with three FK fields (`user`, `default_shipping`, `default_billing`) and a UNIQUE-on-user violation. Today: "A record with this `user` already exists." at the top — fine. But if the user ALSO submitted an unrelated FK that points at a deleted row, the second error is invisible until they fix the first. Structured per-field rendering shows both at once.

    **Related**: ties into gaps2.md #10 (middleware contract) — a unified error-rendering middleware could format both REST + admin from the same `WriteError` source. Also ties into the "two paths for same operation" footnote on commit `5b163ab` (form path vs. JSON path divergence) — collapsing them simplifies the error-rendering refactor.

11. [ ] **Persist all admin UI state into `AdminUserPref` — filters, sort orders, page sizes, search, per-table preferences.** `plugins/umbra-admin/src/models.rs:43` already has `AdminUserPref { theme, density, sidebar_collapsed, dashboard_layout, updated_at }` and the upsert plumbing (`fetch_or_default`, `upsert`). Only two pieces of UI state currently round-trip through it: theme + sidebar-collapsed (typed columns) and the dashboard layout (a JSON blob in `dashboard_layout: String`). Everything else lives in the URL and dies on refresh / new tab / restart:

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

10. [ ] **Middleware contract — proper plugin + app-level middleware system, not ad-hoc `wrap_router` closures.** Today (commit `bd48bf8`) `AuthPlugin::with_user_in_templates()` mounts `user_context_layer` via `Plugin::wrap_router(router) -> Router`. That works for one middleware but the shape doesn't scale:

    - **Order is invisible.** Each plugin's `wrap_router` runs in topological plugin order and the order they wrap matters (auth must be outside-of CSRF outside-of rate-limit outside-of session, etc.). Today nobody can see the resulting stack without reading every plugin's closure.
    - **No user-side middleware surface.** The user has no `App::builder().middleware(MyMiddleware)` to add their own rate-limit / request-id / cors / auth-shield. The escape hatch is `Routes::layered(method, path, handler.layer(L))` per-route — fine for one route, untenable for "rate-limit every API endpoint."
    - **No conflict / duplication detection.** Two plugins independently calling `router.layer(CorsLayer::permissive())` silently stack two CORS layers; the request goes through both and the response gets two `Access-Control-Allow-Origin` headers. The framework should detect "this kind of middleware is already mounted" and warn.
    - **No semantic ordering.** A real middleware system has slots: `PreAuth` (request-id, tracing), `Auth` (the user-context layer), `PostAuth` (rate-limit by user, CSRF), `Outer` (CORS, compression). Plugins declare which slot their middleware belongs in; the framework assembles the stack in a documented order. `wrap_router` collapses all of that into one un-orderable closure.
    - **No introspection.** The dev-mode 404 page lists routes via `Plugin::route_paths()`; there's no equivalent for "what middleware is mounted on this request path?" That's the kind of thing you grep CI logs for when an unexpected 401 lands.

    **Proposed shape**:

    ```rust
    pub enum MiddlewareSlot {
        // Outermost — runs first on request, last on response
        Outer,         // CORS, compression
        Logging,       // tracing, request-id stamping
        Auth,          // session lookup, identity hydration
        PostAuth,      // CSRF check, rate-limit by user
        // Innermost — closest to the handler
    }

    pub trait Middleware: Send + Sync + 'static {
        fn name(&self) -> &'static str;            // for introspection
        fn slot(&self) -> MiddlewareSlot;
        fn layer(&self) -> tower::util::BoxLayer<Body, Body>;
        fn route_filter(&self) -> RouteFilter { RouteFilter::All }  // restrict to a subset
    }

    // On Plugin:
    fn middleware(&self) -> Vec<Box<dyn Middleware>> { Vec::new() }

    // On App::builder():
    pub fn middleware(self, m: impl Middleware) -> Self { ... }
    ```

    Build phase: collect every plugin's `middleware()` + every app-level `.middleware(...)` call, group by slot, sort within slot by registration order, wrap the router slot-by-slot in `Outer → ... → PostAuth` order. Duplicate detection by `name()` (warns or errors based on a builder flag). Introspection at `/admin/_debug/middleware` (dev-only) shows the resolved stack for any path.

    **What this unblocks**:
      - `umbra-ratelimit` plugin (gap from features.md #46) plugs in cleanly: `RateLimitMiddleware::per_user(60_per_min)` with slot=`PostAuth`.
      - User adds their own: `App::builder().middleware(RequestIdMiddleware::default())` — no Plugin trait needed.
      - The current `Plugin::wrap_router` becomes deprecated in favour of `Plugin::middleware()` — auth's `user_context_layer` migrates to `UserContextMiddleware { slot: Auth }`. `wrap_router` stays as the escape hatch for the rare "I need to wrap the whole router non-linearly" case.

    **Reference**: tower's existing `Layer` trait + `ServiceBuilder` already do the composition; the gap is the umbra-side trait + Plugin + App::builder surface around it. Maybe ~300 lines across umbra-core (trait + builder + slot enum + introspection endpoint) + per-plugin migrations of the existing `wrap_router` users (just AuthPlugin today).

9. [x] **`render_500` swallows secondary template errors silently** — Fixed. `render_500` in `crates/umbra-core/src/errors.rs` now `match`es on the recovery template's render result instead of `.ok()`-ing it. Secondary failures get `tracing::error!`'d with the template name + the failure cause + a hint about the likely `{% extends "wrapper.html" %}` chain bug. In dev mode the body of the plain-text fallback includes both errors so the developer sees the recovery failure inline instead of having to grep logs.

    **Sibling issue (also fixed)**: the 500-rendering path runs OUTSIDE the user-context middleware's task-local scope, so `user` was undefined in the recovery template even when AuthPlugin's middleware was mounted. Fixed in `crates/umbra-core/src/templates.rs::merge_ambient_user` by injecting an anonymous-user sentinel (`{ is_authenticated: false, is_staff: false, is_superuser: false }`) when no task-local is set — so the 500 template gets a defined `user` to evaluate against regardless of where in the layer stack it renders.

    Originally found while debugging the `request.user.is_staff` template error in `examples/shop/templates/wrapper.html:37`. Re-hit while debugging `request.user.customer.id` in `me.html:19` — same chain (handler 500s → 500.html extends wrapper.html → secondary failure → silent plain-text fallback). Both halves of the chain now behave correctly: the wrapper renders cleanly with the anonymous-user fallback, and any further secondary failure surfaces with full diagnostics.

7. [x] **Wire `AuthPlugin::with_user_in_templates()`** — Shipped. The builder method lives on `AuthPlugin` and flips a `user_in_templates: bool` field; the impl uses `Plugin::wrap_router` (which was already there — no new framework hook needed) to mount `user_context_layer` on the full merged router. Templates now write `{% if user.is_staff %}` cleanly; anonymous requests see `{ is_authenticated: false }` and the link hides; staff requests see the populated context and the link surfaces. Shop wired with `.with_user_in_templates()` on its AuthPlugin call. The doc-comment-references-a-nonexistent-method case is what triggered the new "Fix, don't patch" CLAUDE.md rule.

8. [ ] **Bootstrap project layout convention** — `umbra startproject <name>` should scaffold the app with the per-concern layout we landed on in `examples/shop` (commits `32cd1c1` extracted seed + widgets; `2d3693b` extracted views). The shop went from a 1320-line `main.rs` to 243 by following one repeated shape across three sibling modules:

    ```
    src/
      main.rs         # App builder + route table + boot helpers (~200-300 lines)
      auth.rs         # Custom authenticators (if any)
      views/
        mod.rs        # re-exports + shared `internal_error` helper
        public.rs     # public/unauth handlers
        account.rs    # auth-gated handlers
        # ...one file per resource grouping
      seed/
        mod.rs        # `all()` orchestrator that pins dependency order
        credentials.rs
        products.rs
        demo_data.rs
        # ...one file per concern; each step idempotent
      widgets/
        mod.rs        # per-kind re-exports
        aggregates.rs # helpers used by widgets (counts/sums per window)
        cards.rs
        charts.rs
        tables.rs
        # ...one file per widget kind
      plugins/
        <name>/       # local app plugins (existing convention from shop)
    ```

    Three properties this gives for free:
      - **`main.rs` reads like a table of contents.** Every call site is `<bucket>::<name>`; "which file holds it" is a lookup, not memorisation.
      - **`mod.rs` files are the discoverability layer.** Open one and you see the whole subsystem's surface in 20-30 lines. The `seed::all()` orchestrator pattern doubles as documentation of dependency order (e.g. "demo_data must run before blogs because the comments reference user ids 2 + 3 that demo_data creates").
      - **Per-concern submodules stay small.** No file we extracted exceeds ~320 lines (`demo_data.rs` is the heaviest). The Django/Rails convention of "one app, one file per concern" carried over cleanly.

    **What the scaffolder should ship**:
      - Empty `views/{mod.rs, public.rs}` (no `account.rs` by default — added when auth lands).
      - Empty `seed/{mod.rs, credentials.rs}` with `mod.rs::all()` calling just `credentials::test_credentials()`.
      - Empty `widgets/{mod.rs, cards.rs}` with one builtin re-export so a fresh dashboard isn't empty.
      - `mod.rs` files commented to explain "this is the orchestrator / re-export layer" so the developer knows where to slot new submodules.

    **What it should NOT do**: forbid other layouts. The framework reads `main.rs` directly — nothing in the runtime cares whether handlers live in `views/`, `handlers/`, `endpoints/`, or just inline in `main.rs`. The scaffold is a recommended convention so the user opens day-one to a project that already scales past the 1000-line mark; they remain free to flatten or restructure if they have different preferences.

    See `examples/shop/src/` for the canonical reference.
