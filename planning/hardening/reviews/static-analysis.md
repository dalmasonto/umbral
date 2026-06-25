done hardening

# Static Analysis Review — umbral workspace

Generated: 2026-06-16  
Scope: `crates/*/src` + `plugins/*/src` (non-test code)  
Tool: `cargo clippy --workspace --all-targets`

---

## Clippy

### Per-crate warning counts (non-duplicate targets only)

| Crate | Warnings |
|---|---|
| `umbral-core` | 53 |
| `umbral-admin` | 53 |
| `umbral-playground` | 6 |
| `umbral-permissions` | 6 |
| `umbral` (facade) | 6 |
| `umbral-macros` | 3 |
| `umbral-rest` | 3 |
| `umbral-health` | 3 |
| `umbral-security` | 1 |
| `umbral-openapi` | 1 |
| **Total** | **135** |

All crates with zero warnings: `umbral-auth`, `umbral-sessions`, `umbral-tasks`, `umbral-cache`,
`umbral-email`, `umbral-livereload`, `umbral-media`, `umbral-realtime`, `umbral-rls`, `umbral-signals`,
`umbral-static`, `umbral-oauth`, `umbral-cli`, `umbral-testing`.

Clippy exited 0 — no errors, only warnings.

### Top lints (by unique src location)

| Lint | Count | Representative file:line |
|---|---|---|
| `needless_borrow` (ref immediately deref'd) | 15 | `umbral-core/src/orm/dynamic.rs:589`, `:983`, `:995` |
| `doc_list_item` (malformed rustdoc list) | 16 | `plugins/umbral-playground/src/lib.rs:29-31`, `plugins/umbral-admin/src/util.rs:63-65` |
| `useless_conversion` (`sea_query::SimpleExpr` → same type) | 10 | `umbral-core/src/orm/m2m.rs:241`, `:242`, `:370` |
| `clone_on_ref_ptr` (clone → `slice::from_ref`) | 7 | `umbral-core/src/orm/forms_runtime.rs:196`, `plugins/umbral-admin/src/handlers/list.rs:57` |
| `multiple_bound_locations` (duplicate trait bound) | 2 | `umbral-core/src/orm/queryset/mod.rs:4674`, `:4767` |
| `result_large_err` (`FormErrors` ≥128 B in Err position) | 1 | `umbral-core/src/forms.rs:1123` |
| `useless_format` | 1 | `umbral-macros/src/lib.rs:4089` |
| `dead_code` (variant `Json` never constructed) | 1 | `plugins/umbral-rest/src/lib.rs:1436` |
| `match_or_default` (match → `.unwrap_or_default()`) | 2 | `plugins/umbral-admin/src/view.rs:511`, `:548` |
| `item_name_repetitions` / `module_inception` | — | suppressed via `#[allow]` (see §allow) |

**Real-bug risk among these:** `result_large_err` on `forms.rs:1123` is a genuine performance/size
warning — `FormErrors` is boxed on the heap only when there are errors, but if this type is cloned
or returned frequently in a hot path it will cause heap churn. Low severity; box the err-variant
when the surface stabilises.

The `umbral-permissions` / `umbral` / `umbral-playground` 6-warning clusters all trace back to
generated code from `#[derive(Model)]` emitting `type Foo is more private than item foo::COLUMN`
warnings for test-only structs. Style noise, not bugs.

---

## Panic/unwrap risks

### (a) Clearly safe — config-time or infallible

| Location | Pattern | Why safe |
|---|---|---|
| `umbral-core/src/backend.rs:274,283,287,298` | `panic!` | Postgres-only types reaching SQLite `map_type`; blocked at boot by field.backend system check. Boot-time sentinel, not request-path. |
| `umbral-core/src/backup.rs:649,658,667` | `panic!` via `unreachable_*` helpers | Same gating: only reachable if boot system check was bypassed. |
| `umbral-core/src/cors.rs:237-255` | `panic!` in `into_layer()` | Called once at `App::build()`, not per-request. Config error → crash at startup. Acceptable; documented. |
| `umbral-core/src/settings.rs:17,28` | `.expect("not initialised")` | `OnceLock` init/get guards — crash at boot if `App::build()` skipped. |
| `umbral-core/src/backend.rs:318,329` | `.expect("not initialised")` | Same `OnceLock` pattern. |
| `umbral-core/src/orm/masked.rs:145` | `.expect("XSalsa20-Poly1305 encryption is infallible…")` | In-memory AEAD encrypt; truly infallible per API contract. |
| `umbral-core/src/templates.rs:1028` | `.expect("walked path is rooted…")` | `strip_prefix` after `WalkDir` — prefix always present by construction. |
| `plugins/umbral-static/src/lib.rs:470,480,489` | `.expect("static response is always valid")` | `Response::builder()` with all-literal headers/status; cannot fail. |
| `umbral-core/src/static_files.rs:302,403` | `.expect("static NNN response…")` | Same as above. |

### (b) Risky — potential request-path or untrusted-input panics

| Location | Pattern | Severity | Notes |
|---|---|---|---|
| `umbral-core/src/storage.rs:186` | `.expect("no Storage backend registered")` | **Medium** | Called from handler code via `umbral::storage::storage()`. If `MediaPlugin` wasn't registered but a handler calls `storage()`, every request to that handler panics. Should return `Result`. |
| `umbral-core/src/middleware.rs:143` | `.expect("request present for each before hook")` | **Low** | The `Option` is guaranteed `Some` by the immediately-preceding loop structure; this is a logic invariant, not an external input. But a future middleware refactor could violate it silently. Convert to `unreachable!` with a comment, or use the `Option`. |
| `umbral-core/src/middleware.rs:162` | `.expect("request present when not short-circuited")` | **Low** | Same invariant — `short_circuit.is_none()` implies `req_opt.is_some()`. Safe today but fragile. |
| `plugins/umbral-admin/src/handlers/inline_edit.rs:163` | `serde_urlencoded::from_str(&body).unwrap_or_default()` | **Medium** | Body comes from an HTTP request. Parse failure silently yields empty map → `new_value` = `""` → writes empty string to DB column. See Silent Failures §. |
| `plugins/umbral-rest/src/lib.rs:1813,1816` | `let _ = wtr.write_record(...)` | **Low** | CSV writer writing to `Vec<u8>` — cannot fail in practice (in-memory sink), but the error is silently dropped. If the underlying `csv` crate changes this, rows disappear from exports with no warning. |
| `plugins/umbral-sessions/src/lib.rs:389,447` | `serde_json::from_str(...).unwrap_or_default()` | **Medium** | If `session.data` is corrupt (truncated write, schema migration, manual edit), the map silently becomes empty. Reads return `Ok(None)` (session data loss) and writes overwrite the corrupt data with only the new key. A logged warning would expose DB corruption early. |

**Critical:** None found. No reachable panic on arbitrary unauthenticated HTTP input detected in
the above audit.

---

## Silent failures

Patterns that discard errors — graded by whether they conceal a real failure.

### Genuinely safe discards (idiomatic)

| Pattern | Location examples | Why OK |
|---|---|---|
| `let _ = OnceLock::set(...)` | `routes.rs:375,405`, `errors.rs:90`, `db.rs:146`, `static_files.rs:133`, `rest/lib.rs:1215`, `realtime/lib.rs:754`, `livereload/lib.rs:138,141` | `OnceLock::set` fails only if already initialised. All callers in `on_ready()` / `Plugin::build()` hooks where double-init is a no-op. |
| `let _ = tx.rollback().await` | `db.rs:566,591,615`, `orm/queryset/mod.rs:2837,2867,3030,3066,3145,3175,3679,3718,3808,3844` | Best-effort cleanup; the original error is already being propagated. Rollback failure would be shadowed by the outer `Err` return. Comment in `db.rs:565` documents this explicitly. |
| `.to_str().ok()` on headers | `hosts.rs:86,266`, `errors.rs:463,530,549`, `forms.rs:1157`, etc. | Header value → `&str` conversion; non-UTF-8 headers are correctly rejected/ignored. |
| `let _ = session_destroy / logout / touch_last_used` | `auth/token.rs:187`, `sessions/lib.rs:501,516`, `admin/auth.rs:252`, `auth/auth_routes.rs:337` | Fire-and-forget maintenance ops; the user-facing response is already determined. |
| `let _ = redis PUBLISH` | `realtime/lib.rs:354` | Commented: publish failure surfaces as stream error on next command; `ConnectionManager` reconnects. |
| `let _ = self.tx.send(env)` | `realtime/lib.rs:390` | Documented: silently dropped if process is shutting down. |
| `.parse::<i64>().ok()` / filter_map | `admin/handlers/actions.rs:52,170`, `admin/handlers/crud.rs:563,670,726`, `admin/handlers/inline_edit.rs:172`, `admin/handlers/sheet.rs:431` | Parsing an object-id from URL/form params: non-numeric → `None` → correctly returns 404. Not a silent failure. |
| `serde_urlencoded::to_string(...).unwrap_or_default()` | `admin/handlers/list.rs:668,775` | Serialising a known-good `HashMap<String,String>` to querystring; cannot fail. |

### Potentially problematic discards

| Location | Pattern | Risk |
|---|---|---|
| `plugins/umbral-admin/src/handlers/inline_edit.rs:163` | `serde_urlencoded::from_str(&body).unwrap_or_default()` | Parse failure → empty map → `new_value = ""` → silently writes empty string to DB. Should early-return 400 on parse error (as `actions.rs:44-46` already does correctly). **Inconsistency with `actions.rs`.** |
| `plugins/umbral-sessions/src/lib.rs:389` (`get_data`) and `:447` (`set_data`) | `serde_json::from_str(&session.data).unwrap_or_default()` | Corrupt session row → silently returns empty map → reads return `None`, writes silently lose unrelated keys. Should log a warning and optionally surface as `SessionError`. |
| `plugins/umbral-rest/src/lib.rs:1813,1816` | `let _ = wtr.write_record(...)` | In-memory CSV writer; failure is impossible today but the error is unchecked. Rows can disappear from exports with no signal. |
| `umbral-core/src/errors.rs:194` | `crate::templates::render(name, &ctx).ok()` | Template render failure for custom error pages is silently swallowed; falls back to plain "Not Found". This is intentional design (no double-fault), but the error is not logged — a broken error template leaves no trace. |
| `plugins/umbral-admin/src/handlers/list.rs:510` | `serialize_table_pref(&pref).unwrap_or_default()` | Serialisation of user pref fails silently → querystring becomes empty → sort/filter state is lost for this request. Low severity but invisible. |
| `umbral-core/src/orm/masked.rs:204` | `MaskKeyring::from_env().ok()` in `get_or_init` | If `UMBRAL_MASK_PRIVATE_KEY` is set but malformed, `from_env()` errors and the keyring initialises to `None`. All `Masked<T>` reads will then fail at reveal-time with a confusing error rather than a clear startup message. |

---

## allow() inventory

| File:line | Attribute | Assessment |
|---|---|---|
| `crates/umbral-core/src/settings.rs:348` | `#[allow(clippy::result_large_err)]` | Intentional: `Settings::from_env` is a cold-path parse; size does not matter. OK. |
| `crates/umbral-core/src/orm/post.rs:201` | `#[allow(clippy::module_inception)]` | Module named `post` contains struct `Post`. Cosmetic; acceptable. |
| `crates/umbral-core/src/orm/expr.rs:151,159,167,175` | `#[allow(clippy::should_implement_trait)]` x4 | Methods named `add`, `sub`, `mul`, `div` that clippy wants as `Add`/`Sub` trait impls. Deliberate builder API choice. OK. |
| `crates/umbral-macros/src/lib.rs:2027` | `#[allow(clippy::module_inception)]` | Same cosmetic as above. OK. |
| `crates/umbral-macros/src/lib.rs:2041` | `#[allow(dead_code)]` | Suppresses dead-code on generated associated consts for private models. Rationale in comment: parity with `pub mod` form. OK — but should this be `pub(crate)` instead? |
| `crates/umbral-macros/src/lib.rs:2083` | `#[allow(dead_code)]` | `FieldKind` enum — `Cidr`/`NullableCidr` variants matched but not emitted yet. Comment documents the follow-on. Acceptable placeholder. |
| `plugins/umbral-rest/src/filtering.rs:94` | `#[allow(dead_code)]` | `into_condition()` on `FilterClause` — `pub(crate)` method, only compiled when the crate is a dep. The comment says "used by call sites taking &FilterClause". If it's truly unused, it should be removed; if it's part of the planned API surface, `#[allow]` is fine. **Flag for cleanup.** |
| `plugins/umbral-admin/src/rows.rs:93` | `#[allow(clippy::too_many_arguments)]` | `build_row_ctx` takes many args from the list handler. This is a sign the function should accept a struct; medium-term cleanup candidate but not a correctness issue. |
| `plugins/umbral-admin/src/view.rs:803` | `#[allow(dead_code, private_interfaces)]` | On a test-local `Repro` model used in `view.rs` tests (inside `#[cfg(test)]`). The `#[allow]` itself is not inside `#[cfg(test)]` but annotates a struct that is. This is acceptable. |

**Flagged for cleanup:**
- `plugins/umbral-rest/src/filtering.rs:94` — `dead_code` allow on `into_condition()`. Verify it is actually used by at least one call site; if not, delete the method.
- `plugins/umbral-admin/src/rows.rs:93` — `too_many_arguments`. Candidate for a context-struct refactor when the admin API stabilises.

---

## TODO/FIXME

**Zero** `TODO`, `FIXME`, or `HACK` markers found in any `src/` file across the workspace.

The only `XXX` hits in the corpus are inside comments explaining `\uXXXX` JSON escape sequences in
`plugins/umbral-admin/src/engine.rs` — not task markers.

---

## Summary

| Metric | Value |
|---|---|
| Total clippy warnings (non-duplicate) | **135** |
| Crates with most warnings | `umbral-core` (53), `umbral-admin` (53) |
| Top lint | `needless_borrow` x15, `rustdoc list indent` x16 |
| Prod-path panics (Critical) | **0** |
| Prod-path panics (Medium / config-or-invariant) | **2** (`storage()` expect, `middleware.rs` expects) |
| Silent-failure discards worth fixing | **4** |
| `#[allow]` attributes in src | **12** — 2 flagged for cleanup |
| TODO/FIXME/HACK markers | **0** |

### Top 3 to fix (in priority order)

1. **`plugins/umbral-admin/src/handlers/inline_edit.rs:163`** — `serde_urlencoded::from_str(&body).unwrap_or_default()` silently writes `""` to the DB on a malformed POST body. `actions.rs` already has the correct early-return pattern; apply it here for consistency.

2. **`plugins/umbral-sessions/src/lib.rs:389,447`** — `serde_json::from_str(&session.data).unwrap_or_default()` silently swallows corrupt session data. Add a log-warn on the `Err` branch before defaulting; this surfaces DB corruption that would otherwise be completely invisible.

3. **`umbral-core/src/storage.rs:186`** — `storage().expect(...)` panics on every request to any media handler if `MediaPlugin` was not registered. Convert to `Result<Arc<dyn Storage>, StorageError>` so call sites can return a proper 500 instead of crashing the worker thread.
