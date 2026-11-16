# Code review: `umbral-casing`

Reviewed: `/home/dalmas/E/projects/umbra/crates/umbral-casing` @ `main` (workspace version 0.0.13).
Method: static reading only (no `cargo build`/`test`/`clippy` — disk constraints). Inputs quoted below as `hand-traced` are reasoned from the source, not executed.

## Verdict

This is a small, well-made crate: three pure total functions, zero dependencies, no `unsafe`, no panicking paths, docs with working doctests, and a test suite that covers the documented ASCII behaviour well. I found no defect that produces a wrong result for the documented ASCII contract. The findings below are about edges the docs and tests do not pin: non-ASCII input (where two sibling functions disagree), the fact that one of the three functions performs no sanitisation while another deliberately does, a doc example that is self-contradictory, and test gaps on precisely the inputs whose silent re-casing would rewrite existing `TABLE` constants.

I verified the crate's load-bearing claim before trusting anything else: the header says `to_snake_case` is "the **canonical** implementation — byte-identical to what `#[derive(Model)]` uses to compute a model's `TABLE` constant" (`src/lib.rs:28-30`). That is true — `crates/umbral-macros/src/lib.rs:89` imports `umbral_casing::to_snake_case` and `crates/umbral-macros/src/lib.rs:1619` calls it (`let bare_name = to_snake_case(&struct_name.to_string());`). No duplicated copy exists in the macro crate. The "intentionally dependency-free" claim also holds: `Cargo.toml` declares no `[dependencies]` table at all.

## Findings

### F1 — Low — Non-ASCII handling is inconsistent across the three functions and under-documented

Two independent behaviours, both hand-traced:

**(a) `to_snake_case`'s boundary rule is ASCII-only, but the doc states it in unqualified terms.** The doc says an underscore is inserted when "the previous character was a lowercase letter or digit" (`src/lib.rs:20`), yet the implementation tests:

```rust
48	        if c.is_ascii_uppercase() {
52	                matches!(prev, Some(p) if p.is_ascii_lowercase() || p.is_ascii_digit());
```

So `to_snake_case("CaféOrder")` returns `"caféorder"` — the boundary before `O` is dropped because `'é'` is not `is_ascii_lowercase()`. `MüllerName` → `müllername` likewise. Rust identifiers allow these characters, and this output is a `TABLE` constant, so the miss is invisible until a migration diff shows a surprise table name. The existing tests (`src/lib.rs:161-211`) never exercise non-ASCII input.

**(b) The two Pascal converters disagree on non-ASCII.** `pascal_case_from_table` uses Unicode case mapping:

```rust
101	            for u in ch.to_uppercase() {
106	            for l in ch.to_lowercase() {
```

while `pascal_case_from_ident` uses

```rust
145	            out.push(c.to_ascii_uppercase());
```

Hand-traced result: `pascal_case_from_table("über_flow")` → `"ÜberFlow"`, but `pascal_case_from_ident("über_flow")` → `"überFlow"`. The ident variant's own doc says "The **first** character of each segment is uppercased" (`src/lib.rs:117-118`) — false for a non-ASCII first character. Since `pascal_case_from_ident` is what produces generated type names (scaffold, inspectdb enum names), the result is a lowercase-initial Rust type (`non_camel_case_types`) rather than an error.

Because the value is migration-visible (`src/lib.rs:28-30`: "the output must never change without a coordinated migration"), the right resolution for (a) is to document the ASCII-only rule explicitly rather than "fix" it silently; for (b), either bring the ident variant in line with its sibling or state that the two deliberately differ.

### F2 — Low — `pascal_case_from_ident` performs no sanitisation, and a caller feeds DB-derived text through it into generated Rust source

The table variant skips anything that is not alphanumeric:

```rust
97	        if !ch.is_alphanumeric() {
98	            continue;
99	        }
```

The ident variant's only transformations are `-`/`_` → boundary and first-char uppercasing; every other character is pushed verbatim:

```rust
142	        if c == '-' || c == '_' {
143	            next_upper = true;
144	        } else if next_upper {
145	            out.push(c.to_ascii_uppercase());
146	            next_upper = false;
147	        } else {
148	            out.push(c);
149	        }
```

One call site relies on this unsanitised path for text that comes out of a database: `crates/umbral-core/src/inspect.rs:1291-1293`

```rust
fn choices_enum_name(enum_type: &str) -> String {
    umbral_casing::pascal_case_from_ident(enum_type)
}
```

is called with the Postgres enum type name recovered from `pg_type.typname` (query at `crates/umbral-core/src/inspect.rs:781`), and the result is emitted as a Rust type name (`crates/umbral-core/src/inspect.rs:1262` and `:1796`). A quoted type name such as `CREATE TYPE "Payment Method" AS ENUM (...)` therefore produces `pub enum Payment Method {` in the generated file, and nothing between this call and the emitted source re-validates the identifier (I searched `inspect.rs` for any ident validation — there is none). Practically: `inspectdb` on a slightly unusual schema dies with a syntax error in generated code instead of a clear "unsupported name" message; the injection angle is real but requires a hostile/odd schema in an inspected DB, which is why this is Low rather than higher.

The fix is a contract decision: either validate where the identifier is produced (a validator already exists in the family — `crates/umbral-core/src/codegen.rs:184 validate_ident`), or state in this function's doc that it performs no sanitisation and the caller owns validity. Today the crate documents neither, and the two functions' different safety properties are invisible at the call site.

### F3 — Low — The doc example that distinguishes the two Pascal functions is self-contradictory

```rust
119	/// This preserves digits and already-correct casing in subsequent positions
120	/// (`api2` → `Api2`, not `Api2` which lowercasing would give).
```

"not `Api2`" is the same string as the shown result, so the sentence conveys nothing — and the contrast it was reaching for is the entire reason two functions exist. The real difference is inner-case preservation: `pascal_case_from_ident("API2")` → `"API2"` while `pascal_case_from_table("API2")` → `"Api2"`. As written, a caller who grabs the wrong variant (e.g. ident on a DB name, where lowercasing is wanted) gets no signal that a choice was involved.

### F4 — Low — The "cross-impl consistency check" tests a direction that cannot hold for the cases the algorithm exists for, and the risky shapes are untested

```rust
275	    #[test]
276	    fn snake_then_table_roundtrip() {
277	        // A PascalCase name → snake → table should recover the original.
278	        let cases = ["BlogPost", "UserProfile", "AuthGroup", "Comment"];
```

The test asserts Pascal → snake → Pascal, but that direction is unprovable for any name containing a consecutive-capital run: `to_snake_case("HTTPRequest") == "http_request"` and `pascal_case_from_table("http_request") == "HttpRequest" != "HTTPRequest"`. So the test can never be extended to cover the acronym rule — the one thing `to_snake_case` is documented to do better than a naive splitter. The direction the tree actually depends on is the other one: `crates/umbral-core/src/inspect.rs:1724` checks `if to_snake_case(&this_struct) != table.table`, i.e. `to_snake_case(pascal_case_from_table(t)) == t` for lowercase `t`. That holds for `"api_key"`, `"auth_user_groups"`, `"s3_upload"` (hand-traced) and is the assertion worth having; the current one gives false confidence while skipping the interesting inputs.

Two other gaps in the same file, both tied to the migration-safety warning at `src/lib.rs:28-30` (output "must never change"):

- **No case pins the shapes a future refactor is most likely to alter silently**: single leading capital + acronym, e.g. `to_snake_case("OAuthToken") == "o_auth_token"` and `to_snake_case("IPAddress") == "ip_address"` (hand-traced). I grepped `crates/` and `plugins/` for `"o_auth_token"` / `"ip_address"` and found no pin anywhere in the tree, so nothing fails if the boundary rule is "improved". These are exactly the names where a bug-fix would silently rewrite an existing app's table name.
- **The documented non-ASCII passthrough has no test** (`src/lib.rs:26`), which is also what would have surfaced F1.

### F5 — Info — README overstates the facade re-export

`README.md:5` says the `umbral` facade "re-exports the stable public surface including what this crate provides", but the facade exposes only two of the three functions: `crates/umbral/src/lib.rs:671` re-exports `pascal_case_from_ident, to_snake_case` (via `crates/umbral-core/src/codegen.rs:30`), and `pascal_case_from_table` appears nowhere in `crates/umbral/src/`. Cosmetic, but a reader following the README to `umbral::` for the table converter will not find it.

### F6 — Info — CHANGELOG stops at 0.0.12 while the crate is tagged 0.0.13

The workspace version is `0.0.13` (`Cargo.toml:58`) and tag `umbral-casing-v0.0.13` exists, but `crates/umbral-casing/CHANGELOG.md`'s newest section is `## [0.0.12] ... 2026-08-16`. Of the six crates under `crates/` that ship a `CHANGELOG.md`, four have a `[0.0.13]` section (`umbral-core` does); only `umbral-casing` and `umbral-testing` do not. This may be release-plz deliberately skipping a no-change crate, but the file is what crates.io/docs.rs renders for the current release — worth confirming rather than assuming.

### F7 — Info — Nits

- `src/lib.rs:55` `if i != 0 && (prev_lower_or_digit || run_break) {` — the `i != 0` guard is dead: at `i == 0`, `prev` is `None`, so both disjuncts are already false. Dropping it, and spelling `prev.map(|p| p.is_ascii_uppercase()).unwrap_or(false)` (`src/lib.rs:53`) as `prev.is_some_and(...)` (MSRV is 1.85 per `Cargo.toml:63`), lets the predicates carry the invariant instead of a redundant guard.
- `src/lib.rs:45` `let chars: Vec<char> = camel.chars().collect();` — a heap copy exists only so neighbours can be read by index; a lookahead iterator (or a byte scan on the ASCII fast path) avoids it. None of the call sites I checked is hot (`queryset/mod.rs:508` model discovery, `transfer.rs:153` `to_lower_camel`, macro expansion), so this is a note, not a request.
- None of the three functions is `#[must_use]`; `to_snake_case(x);` as a statement is a silent no-op, and these are pure functions whose only effect is the return value.

## Verified sound (checked, not assumed)

- Canonicality/dependency claims: true (see Verdict), including `src/lib.rs:11-13`'s three "canonical call site" rows, which all check out in the tree (`umbral-macros/src/lib.rs:1619`; `inspect.rs:405,449,1221` for table → struct; `scaffold.rs:1346` and `plugins/umbral-openapi/src/lib.rs:325` for ident).
- Panic-freedom: no `unsafe`, no `unwrap`/`expect`/`panic!`; the only indexing is `chars[i - 1]` at `src/lib.rs:49`, guarded by the `i == 0` check on the preceding line; `camel.len() + 4` at `src/lib.rs:46` cannot overflow for any realistic input.
- All doctest examples (`src/lib.rs:34-43`, `:79-88`, `:128-137`) hand-trace correctly, including the fiddly ones: `HTTPRequest` → `http_request`, `ParseHTTP` → `parse_http`, `S3Upload` → `s3_upload`, `post2Tag` → `post2_tag`, `_Hidden` → `_hidden`.
- `pascal_case_from_table`'s separator/skip/lowercase contract (`src/lib.rs:66-75`) matches its tests, including the deliberate `foo!bar` → `Foobar` skip behaviour.
- Security: `pascal_case_from_table`'s output is alphanumeric-only, so even a hostile table name cannot inject source through it — the caveat is F2 for the ident variant.

## Limitations

Static reading only, per the constraints: the non-ASCII traces in F1 and the roundtrip counter-example in F4 are hand-derived, not executed. I read call sites to validate the crate's claims, not to audit the callers; e.g. I did not trace whether anything downstream re-checks inspectdb's generated identifiers before the file is compiled.
