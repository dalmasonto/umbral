---
name: verify-typegen-output
description: Use when changing `umbral typegen` (crates/umbral-core/src/typegen.rs) or any SqlType→wire-format mapping, to prove the emitted TypeScript actually compiles and actually rejects the bugs it exists to catch.
---

# Verifying generated TypeScript

## Context

`typegen` emits a `.ts` file from the model registry. Rust tests can only assert
that the *string* contains a substring. That is not the same as the output being
valid TypeScript, and it is definitely not the same as the types being useful —
a generator that emits `author: any` passes every `assert_contains` you write and
catches nothing.

Two separate claims need evidence:

1. The output **compiles** under `tsc --strict`.
2. The output **rejects** the drift bugs it exists to prevent.

Claim 2 is the one that matters, and only a negative test can establish it.

## Approach

There is no `tsc` in this repo (no Node project at the root). Install one into
the scratchpad — never into the tree.

1. **Get a compiler.**

   ```bash
   mkdir -p "$SCRATCH/tscheck" && cd "$SCRATCH/tscheck"
   npm install --silent --no-audit --no-fund typescript
   ```

2. **Get the generated file out of Rust.** Append a temporary test that writes
   `typescript_for(...)` to a path from an env var, run it, then delete the test.
   Do not leave an env-var-driven test in the suite — it silently no-ops in CI
   and reads as coverage that isn't there.

   ```rust
   #[test]
   fn dump_for_external_typecheck() {
       if let Ok(path) = std::env::var("UMBRAL_TYPEGEN_DUMP") {
           std::fs::write(path, generated()).unwrap();
       }
   }
   ```

   ```bash
   UMBRAL_TYPEGEN_DUMP="$SCRATCH/tscheck/models.ts" \
     cargo test -p umbral-core --test typegen dump_for_external_typecheck
   ```

3. **Positive check.** Write a consumer that builds a legal object and compile
   both files. This catches syntax errors, unescaped string literals in a
   choices union, and a `Tag[]` referencing an interface that was never emitted.

   ```bash
   ./node_modules/.bin/tsc --strict --noEmit --skipLibCheck models.ts good.ts
   ```

4. **Negative check — the important one.** Write a consumer that commits the
   exact bugs typegen exists to catch, and assert `tsc` exits **non-zero**:

   - an FK to a `String`-PK model assigned a `number`
   - a typo'd value in a `choices` union
   - `null` assigned to a non-nullable column

   ```bash
   ./node_modules/.bin/tsc --strict --noEmit --skipLibCheck models.ts bad.ts
   echo "exit: $?"   # MUST be 1
   ```

   Expected output includes `TS2820: Type '"publshed"' is not assignable to type
   'PostStatus'. Did you mean '"published"'?` — if `tsc` exits 0, the generated
   types are decorative.

## Why

The Rust test suite and `tsc` check different things. `assert_contains(&ts, "
status: PostStatus;")` proves a substring was written. It cannot prove that
`PostStatus` was *declared*, that its literals were escaped, or that the union
is narrow enough to reject a typo. Those are properties of the TypeScript
program, so a TypeScript compiler has to judge them.

Keeping `tsc` out of the committed test suite is deliberate: adding Node to CI
for one generator is a real cost, and the mapping changes rarely. The Rust tests
guard the mapping; this procedure guards the *language*, and you run it whenever
you touch the mapping.

## Pitfalls

- **`--strict` matters.** Without `strictNullChecks`, `post.title = null`
  compiles and the negative test silently passes for the wrong reason.
- **The FK case needs a non-`i64` PK target.** With an `i64`-keyed target both
  the right answer and the wrong answer are `number`, so the test proves
  nothing. `typegen.rs` keys `TgAuthor` on a `String` slug for exactly this.
- **The doc comment can lie even when the type is right.** `TgAuthor` keys on
  `slug`, so `Foreign key: the \`id\` of a TgAuthor` was wrong while
  `author: string` was right. `tsc` will never catch a comment; read the output.
- **Keep `typegen` and `umbral-openapi` agreeing.** Both describe the same
  bytes. `openapi_type()` in `plugins/umbral-openapi/src/lib.rs` is the other
  half; a change to one (`Decimal` → string, `Bytes` → array) must land in both,
  or generated clients and the schema will contradict each other.
- `typescript_for` is pure and `typescript()` reads the registry. Test the
  mapping against the first and the wiring against the second
  (`tests/typegen_registry.rs`); `App::build()` publishes `OnceLock`s so only
  one successful build runs per test binary.

## See also

- `crates/umbral-core/src/typegen.rs` — the generator and its mapping table.
- `plugins/umbral-openapi/src/lib.rs` — `openapi_type`, the mapping that must agree.
- `documentation/docs/v0.0.1/orm/typegen.mdx` — the user-facing page.
- `planning/archive/gaps3-done.md` #38 — why types-not-a-client.
