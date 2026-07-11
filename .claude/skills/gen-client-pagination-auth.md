---
name: gen-client-pagination-auth
description: Use when changing how `umbral gen-client` (client_gen.rs) emits the list envelope, pagination builder methods, or the auth surface — or when adding a custom paginator that should generate typed client code.
---

# gen-client: pagination + auth adaptation

## Context
The generated TS client (`plugins/umbral-openapi/src/client_gen.rs`) must reflect *this* app's REST config, not assume defaults. Two things used to be hardcoded and now adapt: the list envelope/pagination and the auth headers.

## Approach

**Testability seam.** `generate_for(all)` reads umbral-rest's process-global `OnceLock`s and delegates to `generate_with(all, base_path, style, schema, security_schemes)`. Always test through `generate_with` — it's pure, so every paginator/auth combination runs in one test binary with **no `App::build`** (which can only run once per process). `is_exposed`/`is_hidden`/`filters_enabled_for` still read OnceLocks but default gracefully (exposed, not hidden, filters on).

**Pagination.** Driven by `(PaginationStyle, Option<&PaginationSchema>)`:
- Built-in `None`/`PageNumber`/`LimitOffset` → known envelope + builder methods, hardcoded in `envelope_type` / `runtime`.
- `Custom` + `Some(schema)` → typed envelope from `schema.envelope` (each `PaginationField` → `name: ts_scalar` + `| null` if nullable) and one builder method per `schema.params` (name camelCased via `camel_case`).
- `Custom` + `None` → permissive envelope (`results?`, `count?`, `[key: string]: unknown`) + rely on the generic `.param(key, value)` escape hatch (emitted for *every* style).

A custom paginator opts into typed codegen by overriding `Pagination::schema()` (defaulted `None`) in `plugins/umbral-rest/src/pagination.rs`. `registered_pagination_schema()` in umbral-rest lib.rs exposes it (mirrors `registered_pagination_style()`).

**Auth.** `AuthModel::from_schemes(&[(name, Value)])` parses OpenAPI security scheme objects (from `registered_security_schemes()`):
- `{"type":"http","scheme":S}` (not `basic`) → `bearer_prefix = title_case(S)`. So `bearer`→`Bearer`, `token`→`Token`. **The prefix is the `scheme` field, never hardcoded.**
- `{"type":"apiKey","in":"header","name":N}` → `api_key_header = N` (`x-umbral-api-key`, whatever declared).
- `{"type":"apiKey","in":"cookie"}` → `cookie = true` → `credentials: "include"`.
These become the *baked defaults* (`bearer_prefix()`→"Bearer", `api_key_header()`→"X-API-Key" as ultimate fallbacks) interpolated into the emitted `UmbralOptions` + `_request`. All stay overridable (`tokenPrefix`/`apiKeyHeader`), plus a `getAuthHeaders()` async hook merged **last** (wins) for JWT refresh.

## Why
The client is generated once and shipped to a frontend with no Rust toolchain — it must be correct for the app's actual scheme (a `Token` prefix, an `x-umbral-api-key` header, a cursor paginator) without the developer patching generated code. Reading the same registry + config that renders the OpenAPI doc keeps it honest.

## Pitfalls
- `gen` is a reserved keyword (2024 edition) — don't name a test helper `gen`.
- The `runtime()` format string uses `{{`/`}}` for literal TS braces; new interpolations (`{bearer_prefix}`, `{credentials_default}`) must be `let`-bound before the `format!`.
- `credentials_default` is the literal string `"undefined"` or `"\"include\""` — it's spliced raw into `?? {credentials_default}`.
- Verify with real tsc (positive + negative), not just `.contains` asserts — see `.claude/skills/verify-typegen-output.md`.

## See also
- `.claude/skills/verify-typegen-output.md` — the tsc positive/negative harness.
- `planning/building/kikosi.md` #1 — the type-safety roadmap this serves.
- `documentation/docs/v0.0.1/rest/typescript-client.mdx` — user-facing surface.
