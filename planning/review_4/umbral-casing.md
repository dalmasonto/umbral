# review_4 · umbral-casing

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 285 LOC (`crates/umbral-casing`) |
| **Reviewer** | Claude (rubric pilot) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 2 low · 1 info — 4 new, 0 known |

## Summary

- Three pure, dependency-free case-conversion functions shared across the workspace: `to_snake_case` (drives the `TABLE` constant in `#[derive(Model)]`), `pascal_case_from_table` (inspectdb), `pascal_case_from_ident` (CLI scaffold, REST generators, OpenAPI client-gen). Created by gaps2 #77 to end a five-copy duplication; that consolidation has held — every remaining local helper composes the canonical functions.
- Overall state is clean: no `unsafe`, no `unwrap`/`expect`, no reachable indexing, empty/single-char/punctuation inputs all handled, and the doc table at `lib.rs:9-13` maps each function to its canonical call site.
- Biggest risk (CASING-1): `pascal_case_from_table`'s output is not guaranteed to be a valid Rust identifier, and inspectdb interpolates it verbatim into `pub struct {name}` — a Postgres table like `2fa_tokens` yields generated models that do not compile.
- Doc/test polish: `pascal_case_from_ident`'s rustdoc is self-contradictory about its own example and overclaims for non-ASCII (CASING-2); the documented non-ASCII behaviors and the from_table/from_ident divergence are untested (CASING-3).
- Fix order: CASING-1 first (make the ident contract total on either side of the inspect path); CASING-2/CASING-3 are small same-day cleanups.

## Findings

Ordered by severity (CRITICAL → INFO). Every finding carries: exact location, quoted evidence, a concrete impact scenario, a fix sketch, and a status.

### CASING-1 · MEDIUM · correctness · self-verified
**`pascal_case_from_table` can return a string that is not a valid Rust identifier, and inspectdb emits it verbatim as a struct name — a legal table name produces uncompilable generated code.**

- **Where**: `crates/umbral-casing/src/lib.rs:89-112` (the function), reached via `crates/umbral-core/src/inspect.rs:405` (and `:449` for the PG path) and emitted at `crates/umbral-core/src/inspect.rs:1735`.
- **Evidence**:
  ```rust
  // lib.rs:100-104 — first char of every segment uppercased, no validity check
  if upper_next {
      for u in ch.to_uppercase() {
          out.push(u);
      }
      upper_next = false;
  ```
  ```rust
  // inspect.rs:405 — struct name = pascal_case_from_table(table-name)
  name: pascal_case_from_table(&table),
  // inspect.rs:1735 — emitted verbatim
  out.push_str(&format!("pub struct {this_struct} {{\n"));
  ```
- **Impact**: Concrete trace: a Postgres DB containing table `2fa_tokens` (legal, realistic; also `3d_assets`, `404_logs`). `umbral inspectdb postgres://…` → `introspect_pool_pg` lists every base table in `public` with no name filter (inspect.rs:435-440) → `pascal_case_from_table("2fa_tokens")` = `"2faTokens"` (the leading digit passes through `to_uppercase` unchanged) → generated `models.rs` contains `pub struct 2faTokens {`, which does not parse. Two more total-output holes: a table of only non-alphanumerics (`"___"`) yields `""` → `pub struct  {`; a table named `"self"` yields `"Self"` → reserved identifier. The tree already owns both a validator (`umbral-core/src/codegen.rs:184 validate_ident` rejects empty / leading-digit / keyword) and a keyword guard (`inspect.rs:1847 is_rust_keyword`) — but the guard is wired into `safe_field_ident` for FIELD names only (inspect.rs:1949); nothing validates struct names on the inspect path. The leading-digit case is even named in a test comment (inspect.rs:2372-2375, "…leading digits…") yet that test uses `"POSTS"` and never exercises it.
- **Fix**: Make the contract total where it is claimed. Either add an inspection-purpose entry point in `umbral-casing` (e.g. `struct_ident_from_table` returning `Option<String>`, or sanitizing via a `T`/`_` prefix for digit-leading/empty and disambiguating keywords) or validate-and-rename in `inspect.rs` before `struct_names` is built, reusing `validate_ident`'s rule. Do not change `pascal_case_from_table`'s existing output in place — gaps2 #77 pinned per-call-site output.
- **Status**: new. Self-verified: searched for guards (none on the struct path; field-only keyword guard confirmed), re-read the full render chain `inspectdb → render_models_with → struct_names → 1735`, and traced the `2fa_tokens` scenario end to end.

### CASING-2 · LOW · docs
**`pascal_case_from_ident`'s rustdoc contradicts itself in the example that demonstrates its defining difference, and overclaims uppercasing for non-ASCII input.**
- **Where**: `crates/umbral-casing/src/lib.rs:118-120` and `crates/umbral-casing/src/lib.rs:145`.
- **Evidence**:
  ```rust
  /// This preserves digits and already-correct casing in subsequent positions
  /// (`api2` → `Api2`, not `Api2` which lowercasing would give).
  ```
  ```rust
  out.push(c.to_ascii_uppercase());
  ```
- **Impact**: The parenthetical says "`Api2`, not `Api2`" — both sides are the same string, and for `"api2"` `pascal_case_from_table` also returns `"Api2"`, so the sentence fails to demonstrate why the two Pascal functions exist. The real distinguishing input is mixed case after the first position: `pascal_case_from_ident("aPI2") == "API2"` vs `pascal_case_from_table("aPI2") == "Api2"`. Second, the doc's "the **first** character of each segment is uppercased" is ASCII-only (`to_ascii_uppercase`), so `"élan"` → `"élan"`, while the sibling functions explicitly document their non-ASCII behavior. Reachability is low (CLI names are validated ASCII and model names are effectively ASCII in this tree), hence LOW.
- **Fix**: Replace the parenthetical with the real contrasting pair (`aPI2`), and say "ASCII-uppercased" (or switch to `to_uppercase`, though ASCII-only is probably the correct choice for ident production — then just document it).
- **Status**: new.

### CASING-3 · LOW · tests
**The documented non-ASCII behaviors and the deliberate divergence between the two Pascal functions have no test coverage.**
- **Where**: `crates/umbral-casing/src/lib.rs:154-284` (test module).
- **Evidence**: The module has 18 tests covering acronyms, digits, empty strings, and one cross-impl round-trip (`snake_then_table_roundtrip`, `lib.rs:276-284`), but no non-ASCII input anywhere and no input on which `pascal_case_from_ident` and `pascal_case_from_table` disagree — every shared case tested (`"blog_engine"`, `"blog-engine"`, `"posts"`) is one where both functions return the same string by construction.
- **Impact**: The two claims most likely to regress silently — `to_snake_case`'s non-ASCII pass-through (lib.rs:26) and the preserve-vs-lowercase split between the Pascal variants (lib.rs:114-124 vs 66-75) — are unpinned. `to_snake_case` output is migration-load-bearing (it computes `TABLE`); a change there without its own test would be caught only by a downstream `cargo test --workspace`. The non-ident outputs from CASING-1 (`"2fa_tokens"`, `"___"`, `"self"`) are also untested, so the suite reads greener than the reachable behavior.
- **Fix**: Add four asserts: `to_snake_case("ÜberPost")` (documented pass-through), the contrasting pair (`pascal_case_from_ident("aPI2") == "API2"` vs `pascal_case_from_table("aPI2") == "Api2"`), and `pascal_case_from_table` on a leading-digit / all-separator input asserting whatever CASING-1's fix decides.
- **Status**: new.

### CASING-4 · INFO · perf/alloc
**`to_snake_case` allocates a `Vec<char>` and a `String` sized `len + 4`, which can under-reserve for alternating-case ASCII.**
- **Where**: `crates/umbral-casing/src/lib.rs:45-46`.
- **Evidence**:
  ```rust
  let chars: Vec<char> = camel.chars().collect();
  let mut out = String::with_capacity(camel.len() + 4);
  ```
- **Impact**: The `chars` Vec exists only for a one-character look-back; output can reach about 1.5× input for alternating-case ASCII (`"aAaAaAaAaA"` → 15 bytes) which exceeds `len + 4` and costs one reallocation. Every call site is cold — macro expansion (compile time), startup relation discovery, inspectdb, scaffold — so no action required; noted because this is the crate's only allocation nuance.
- **Fix**: None required. If ever touched, iterate `chars().peekable()` (drops the Vec) or reserve `len * 3 / 2`.
- **Status**: new — INFO, no action.

## Strengths

- **The #77 consolidation has held.** Every remaining local case helper composes the canonical functions rather than reimplementing them: `umbral-macros` `to_screaming_snake_case` (lib.rs:5167) and `to_pascal_case` (lib.rs:5176) route through `to_snake_case`; `umbral-openapi` `camel_case` (client_gen.rs:984) composes `pascal_case_from_ident`; `umbral-core` `to_lower_camel` (transfer.rs:152) composes `to_snake_case`. No fifth copy has regrown — which is exactly the failure mode this crate exists to prevent.
- **The doc table at `lib.rs:9-13`** names each function's canonical call site and the one semantic difference that matters (lowercasing vs not). For a shared-helper crate whose variants are otherwise easy to confuse, that is the right documentation shape.
- **Panic-free and allocation-disciplined:** no `unsafe`, no `unwrap`/`expect`, the only indexing (`chars[i - 1]`, `chars.get(i + 1)` at lib.rs:49-50) is bounds-guarded, and every function pre-sizes its output with `with_capacity`.
- **`to_snake_case`'s doc pins the load-bearing contract** (lib.rs:28-30): byte-identical to the `#[derive(Model)]` TABLE derivation, must not change without a coordinated migration. That constraint is otherwise rediscovered by breaking production.
- **The dependency-free claim is true and checkable:** `Cargo.toml` has no `[dependencies]` table at all, matching the docstring's "intentionally dependency-free so it can be used from `umbral-macros`" rationale.
- **Tests read as a grammar of the intended algorithm** (acronym runs, trailing acronyms, digit boundaries, leading underscore, empty) rather than a couple of happy paths, plus a cross-impl round-trip.

## Clean areas checked

- Panics/soundness: no `unsafe`, no `unwrap`/`expect`, no reachable indexing; empty strings, single chars, and all-punctuation inputs return without panicking (full read of all 285 lines).
- Security: pure string transforms — no SQL, HTML, path, or serialization output; the only security-adjacent flow (introspected name → generated source) has no injection surface in this crate's own code (the unescaped interpolation concern lives in `inspect.rs`, see out-of-scope).
- Architecture/dependencies: zero deps, proc-macro-safe as documented; dependency arrows point inward (macros/core/cli/openapi → casing, never the reverse), confirmed against every `Cargo.toml`.
- Drift: `grep -rn "fn to_snake_case\|fn pascal_case\|fn camel_case\|fn snake_case\|fn to_lower_camel"` over `crates/` + `plugins/` finds only the four documented thin wrappers listed in Strengths — no un-consolidated copy.
- Off-by-one/acronym logic in `to_snake_case`: the `prev`/`next` windows and the `run_break` rule were traced by hand against `HTTPRequest`, `ParseHTTP`, `Post2Tag`, `_Hidden`, `2FA` — all match the documented algorithm and the tests.
- rustdoc examples: every `assert_eq!` in the three doc blocks was traced against the implementation (`S3Upload` → `s3_upload`, `Post2` → `post2`, `BLOG_POST` → `BlogPost`, `api2` → `Api2`, …) — they hold and would compile.
- Facade role: re-exported via `umbral-core/src/codegen.rs:30` and the `umbral` facade; README's "you almost certainly want the facade" claim is accurate for an internal crate.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps2 #77` — dedup `to_snake_case` (×3) / `pascal_case` (×2) into this crate; shipped 2026-06-20 (`4b92067`), archived (`planning/archive/gaps2-done.md:563`, `planning/hardening/HARDENING-PROGRESS-2026-06-20.md:30`). Its per-site output-preservation constraint is what CASING-1's fix must respect.
- `audit_2/findings/core-macros-cli.md` — prior audit covering this crate ("The casing crate is correct and well-tested"); its still-open items (#2 Form-derive allowlist, #3 Tailwind SRI, #6 `default=` DDL → gaps3 #28; #7 dev `secret_key` → gaps3 #27) are macros/CLI, none against `umbral-casing`.
- `review_3/claude/02_triage.md:177-183` — LOW duplication findings against `umbral-macros` PascalCase copies (fixed) — the sibling-drift context that #77 and this crate closed.

## Out-of-scope notes

- `umbral-core/src/inspect.rs:1216-1223` — the collision fallback re-inserts `pascal_case_from_table(&t.table)` as the "full pascal" name, which for non-Django tables is the same value that collided: tables `"blog_post"` and `"BLOG_POST"` (both legal in Postgres) yield two `pub struct BlogPost` in one generated file.
- `umbral-core/src/inspect.rs:1725` — `#[umbral(table = "{}")]` interpolates the raw introspected table name with no escaping or attribute-string safety check.
- `umbral-core/src/inspect.rs:1847-1949` — `is_rust_keyword`/`safe_field_ident` guard field idents only (keyword list even includes `"Self"`); struct names route unguarded (CASING-1's reachability, but the guard's home is umbral-core).
- `umbral-core/src/orm/queryset/mod.rs:508` — `to_snake_case(&meta.name)` runs per registered model inside `discover_reverse_relation_by_table`; small and cold, but caching the snake form in `ModelMeta` would remove the per-call allocation if that path ever shows in profiles.
