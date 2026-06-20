# Plugin/Core Hardening Wave — 2026-06-20

Autonomous loop closing concrete security/correctness items from `planning/gaps2.md` (#71/#73/#75/#77/#79/#80/#81/#83/#84/#86) and `planning/hardening/plugins-review/*.md`. Branch: `hardening/plugin-fixes` (off `main`). Every fix = one commit, TDD, verified with `cargo build`/`cargo test` (full `cargo test --workspace` for core/macro/shared-type changes). **Final state: full workspace builds clean, 0 test failures, 301 test binaries green.**

Status legend: **FIXED** = real bug fixed; **PINNED** = already fixed since the review snapshot, locked with a regression test; **DEFERRED** = needs a larger/cross-cutting change.

## Wave A — plugin security (gaps2 #81)
| Item | Status | Commit |
|---|---|---|
| security: `csrf_exempt_paths` segment-boundary match | PINNED (already correct) + test | `f75a974` |
| playground: single-pass shell substitution | **FIXED** (real template-injection: spec URL leaked into app-name slot) | `23c5ca0` |
| oauth: shared reqwest client + request/connect timeouts | **FIXED** (handler-stall DoS) | `2eb5936` |
| email: reject CRLF/control chars in subject | **FIXED** (real — lettre did NOT guard; Bcc-injection) | `e0a71cf` |
| static: ETag/304 + symlink-escape guard | **FIXED** (real — ServeDir followed symlinks → `/etc/passwd`) | `2762eaa` |
| cache: Host in key + session-cookie bypass | **FIXED** (cache poisoning) | `d311d77` |

## Wave B — plugin correctness (gaps2 #80/#83/#84)
| Item | Status | Commit |
|---|---|---|
| signals: isolate async subscriber panics (catch_unwind parity) | **FIXED** (real — panic killed the request) | `c186e71` |
| auth: propagate `is_superuser` into `Identity` across all 4 auth paths | **FIXED** | `710fe5b` |
| tasks: classify non-retriable via `TaskError::HandlerNotFound` (not string-match) | **FIXED** (typed error was never used) | `f9e19bd` |
| health: route DB probe through new `umbra::db::ping()` + per-check timeout | **FIXED** (ORM-bypass + `/ready` hang) | `14c30c4` |
| admin: honor `base_path` in inline-edit + fk-picker fragments | **FIXED** (404 under `.at()`) | `006033a` |
| permissions: `table_app_label` model-collision | **DEFERRED** — needs `const APP_LABEL` on the `Model` trait + macro capture of `#[umbra(plugin)]` + `ModelMeta.app_label` + ~25 test sites (own PR) | — |

## Wave C — core hardening (gaps2 #73/#75/#77)
| Item | Status | Commit |
|---|---|---|
| #77 dedup `to_snake_case`/`pascal_case` → new no-dep `umbra-casing` crate (5 sites) | **FIXED** (refactor; output unchanged) | `4b92067` |
| #75 empty `SECRET_KEY` → fail-closed in prod / warn in dev (CSRF HMAC) | **FIXED** | `71c75a0` |
| #73 media storage `.expect` per-request panic | PINNED (boot system-check already guards; accessor de-panicked) | `5ffc9ed` |

## Wave D — remaining sub-parts (gaps2 #73/#75/#79/#71/#86)
| Item | Status | Commit |
|---|---|---|
| #75b auth `password_hash` leak via `.expose()` | **FIXED** — REST-layer `HARD_DENIED_FIELDS`, un-overridable (1st attempt modified the derive macro + broke test-compile workspace-wide → **reverted** `e93d0b8`/`92be470`; narrow REST fix landed) | `e7e70ab` |
| #75c inactive superuser keeps perms | **FIXED** — `is_active` gate before superuser bypass | `e2dd1ae` |
| #73b float `min`/`max` validation | PINNED (already fixed via `validate_numeric_bounds`/`as_f64`) + 7 tests | `6053fe0` |
| #73c admin inline-edit writes `""` on parse failure | PINNED (already 400s via `DynQuerySet::update_one`) + 2 tests | `e029202` |
| #73d Masked malformed key → silent None keyring | **FIXED** — keyring now `Result`; present-but-bad key → `BadKey` (never silent plaintext) | `d979e9c` |
| #79a REST `?ordering=` reserved but never applied | **FIXED** — DRF-style, field-validated, multi-field | `ee9d5bf` |
| #71b `set_user_groups` non-transactional | PINNED (already uses `umbra::db::transaction` since c818cab) + rollback test | `a4cdbd8` |
| #86 doc drifts (signals.mdx, openapi `//!`, tasks.mdx) | **FIXED** | `5d5f745` |

## NOT closed — remaining concrete candidates for a future loop (not started)
- **#79** umbra-openapi hardcodes `/api/{table}/` ignoring `RestPlugin::base_path()` + always emits `page`/`page_size` under `NoPagination`/`LimitOffset`; `Action::permission` stored but not enforced (admin).
- **#73** non-i64 M2M child ids dropped from form junction writes (`forms_runtime.rs:226`); REST CSV writer errors dropped.
- **#71a** session `set_data` read-modify-write loses concurrent keys — **intentionally deferred**: Phase 2a (SessionStore, branch `perf/sessionstore-2a`) rewrites that exact code.
- **#85** test-coverage holes (oauth state-CSRF e2e, tasks double-claim, etc.).

## NOT closed — large features / need design (defer, not loop-closeable)
gaps2 #2 (posthog), #5/#6 (custom/dynamic widgets), #8 (`startproject` scaffold), #10/#60 (middleware contract), #50 (admin inline editing), #55 (collectstatic), #57 (media backends), #63 (large-data ORM bench), #70 (PostGIS/PG types), #78 (module splits >2800 LOC), and the larger #79 surfaces (umbra-rls per-request `app.user_id`, umbra-oauth refresh-token exchange, admin `InlineModel` rendering).
