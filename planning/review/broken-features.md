# Broken features

> **Sweep status — 2026-06-14**
> - **Fixed:** BROKEN-1 (conditional claim, `98ef6e9`), BROKEN-3 (mutex-poison recover + catch_unwind, `23ad8b0`), BROKEN-4 (no `process::exit`, `98ef6e9`), BROKEN-5 (logged decode failure, `23ad8b0`), BROKEN-6 (`get_opt`, `23ad8b0`), BROKEN-13 (stale comment, `23ad8b0`).
> - **Also fixed:** BROKEN-7 (`cache_page` logs + 502 instead of fake-200, `17939c4`), BROKEN-8 (`Form<T>` checks Content-Type → 415, surfaces parse errors → 400, `dbb57c8`), BROKEN-9 (`CachePlugin::new(cache)` wires the ambient cache in `on_ready`, `17939c4`).
> - **Deferred (larger change):** BROKEN-2 (worker-crash task reclaim — needs a lease/reclaim watcher). Low-severity BROKEN-10/11/12/14 not yet triaged.

Code that cannot work as written, swallows failures, or contradicts its own docs. Checked against `bugs/gaps.md`, `gaps2.md`, `REAL-GAPS.md`, `features.md` — only new findings here. The "already tracked" list is at the bottom.

---

## BROKEN-1 — Tasks: two Postgres workers can claim and run the same task
**Severity: high** · **Verified** (`claim_one` SELECTs `status='pending'` then UPDATEs `WHERE id = ?` only; no `FOR UPDATE SKIP LOCKED` anywhere in `crates/umbra-core/src`)

- **File:** `plugins/umbra-tasks/src/lib.rs:372-417` (`claim_one`)
- **Evidence:** `SELECT … WHERE status='pending' … LIMIT 1` then `UPDATE … WHERE id = ?` inside `umbra::transaction`, with the comment: *"Wrapped in a transaction so a concurrent worker can't double-claim — SQLite's single-writer model already guarantees this, but the explicit transaction keeps the contract correct for the Postgres backend."* There is **no `FOR UPDATE`/`SKIP LOCKED`** in the ORM, and the UPDATE filters on `ID` only, not `status='pending'`.
- **Why it matters:** Under Postgres READ COMMITTED, two concurrent transactions both SELECT the same pending row and both flip it to `running` — the task runs twice (double emails, double charges). The framework is "Postgres-first" (CLAUDE.md). The design spec `docs/specs/outlines/tasks.md:27` mandates `SELECT … FOR UPDATE SKIP LOCKED`; the implementation skipped it while the comment asserts correctness. `tasks.mdx:172` even says "Postgres-aware locking is a later follow-on", directly contradicting the comment.
- **Fix:** Add `select_for_update(skip_locked)` to the QuerySet (no-op on SQLite, `FOR UPDATE SKIP LOCKED` on Postgres) and use it in `claim_one` (see MISS-1). At minimum make the UPDATE conditional (`WHERE id = ? AND status = 'pending'`) and check `rows_affected` as an optimistic guard. Fix the comment either way.

## BROKEN-2 — Tasks: worker crash mid-task permanently loses the task
**Severity: high**

- **File:** `plugins/umbra-tasks/src/lib.rs:60-63, 420-496`
- **Evidence:** `STATUS_RUNNING` doc: *"a crashed worker leaves the row in this state until manual cleanup or a future timeout-watcher reclaims it."* Nothing implements that watcher; `claim_one` only ever selects `status='pending'`. Also, if `process_one`'s terminal-state `update_values` fails (DB hiccup at `:457`/`:489`), the executed task also stays `running` forever.
- **Why it matters:** This is at-most-once delivery, not the "with retries" durability the module header advertises. A SIGKILL/OOM/deploy during any task silently strands it; nothing retries or surfaces it. The `attempts` counter is incremented in `claim_one` precisely so a reclaim could be safe — the reclaim just doesn't exist.
- **Fix:** `started_at` is already stamped — have the worker loop reclaim rows where `status='running' AND started_at < now() - timeout` back to `pending`, or fold into the planned apalis migration. Log a gaps entry until then.

## BROKEN-3 — Core signals: a panicking sync handler poisons the registry mutex and bricks every ORM write
**Severity: high**

- **File:** `crates/umbra-core/src/signals.rs:189-220` (also `:155, :169, :443`)
- **Evidence:** `emit` runs sync handlers **while holding the registry lock**: `let reg = registry().lock().expect("signals registry poisoned"); … for h in handlers { h(&payload); }`. Every `subscribe`/`emit`/`clear_for_tests` uses `.expect("signals registry poisoned")`.
- **Why it matters:** Two request-reachable failure modes. (a) One panicking sync handler poisons the `std::sync::Mutex`; thereafter **every** `Manager::save`, `DynQuerySet::insert_json`, REST POST, and admin form submit (all call `signals::emit_*`, e.g. `orm/queryset/mod.rs:3528`, `orm/dynamic.rs:1170`) panics at the `.expect` → permanent 500s on all writes until restart. (b) A sync handler that calls `emit`/`subscribe` deadlocks instantly (non-reentrant mutex). The tasks worker survives handler panics via `spawn`; the signals registry has no equivalent protection.
- **Fix:** Clone the sync-handler invocation out from under the lock (handlers are `Arc`-able), wrap each call in `catch_unwind`, and replace `.expect` with `lock().unwrap_or_else(|p| p.into_inner())` to shrug off poisoning.

## BROKEN-4 — `run_worker` "graceful shutdown" calls `std::process::exit(0)` from library code
**Severity: medium** (doc says it panics — neither is the documented behavior)

- **File:** `plugins/umbra-tasks/src/lib.rs:321-334`
- **Evidence:** doc: *"A graceful shutdown is modelled as a `panic!` rather than a normal return"*; code: `if *opts.shutdown.borrow() { std::process::exit(0); }`.
- **Why it matters:** Running the worker as a `tokio::spawn` inside the web-server process (the single-binary deployment the plugin header advertises: "background work for the cost of one `.plugin(TasksPlugin)` line") means the **entire HTTP server is killed** when the shutdown watch flips. `exit(0)` also skips destructors/flushes.
- **Fix:** Change `run_worker` to return `Result<(), TaskError>` (the doc already says "M10+ can lift this to a Result" — lift it now); let the caller decide process exit.

## BROKEN-5 — umbra-signals typed handlers silently never fire on deserialize failure
**Severity: medium**

- **File:** `plugins/umbra-signals/src/lib.rs:145-149` (same at `:172, :201, :229`); core mirror at `signals.rs:233-235`
- **Evidence:** `let instance: Option<M> = payload["instance"].as_object().and_then(|_| serde_json::from_value(...).ok()); … if let Some(f) = fut { f.await; }` — deserialize failure → handler skipped, zero log.
- **Why it matters:** Dynamic write paths (REST/admin via `DynQuerySet`) emit `instance` JSON from DB rows, which can legitimately fail to deserialize into the user's `M` (field added after a migration, type drift, NULL in a non-`Option` field). The subscriber — e.g. "enqueue welcome email on user create" — simply never runs, no diagnostic. This is the `.ok()`-swallow pattern CLAUDE.md's "Fix, don't patch" forbids.
- **Fix:** `tracing::warn!` (table + serde error) on the deserialize-failure branch in all four methods and core's serialize-failure branches.

## BROKEN-6 — umbra-email `send()` panics on the console backend when settings aren't initialised
**Severity: medium**

- **File:** `plugins/umbra-email/src/lib.rs:440` vs `:357-362`
- **Evidence:** `load_config` deliberately re-parses `Settings::from_env()` because the ambient `settings::get()` *"would panic before `App::build()` runs"*. But `send()`'s non-Dev warning calls `umbra::settings::get().environment` directly — which is `.expect("umbra: settings not initialised — did you call App::build()?")` (`settings.rs:28`).
- **Why it matters:** Any context sending mail without `App::build()` — unit tests, standalone scripts, a task-worker binary — panics instead of printing, despite the console backend being designed exactly for those contexts. Introduced by the cd656d5 security warning; the panic side-effect isn't tracked anywhere.
- **Fix:** Use `umbra::settings::get_opt()` and treat `None` as Dev (or warn that environment is unknown).

## BROKEN-7 — `cache_page` body-collection failure fabricates an empty 200 with stale headers
**Severity: medium**

- **File:** `plugins/umbra-cache/src/cache_page.rs:177-186`
- **Evidence:** `Err(_) => { /* Body collection failed … Can't reconstruct the body here so return an empty 200. */ let fallback = Response::from_parts(parts, Body::empty()); return Ok(fallback); }`
- **Why it matters:** A streaming-body failure becomes a successful-looking 200 with an empty body and the original headers — including a now-wrong `Content-Length`, which can desync keep-alive connections or make clients hang/truncate. The failure is invisible (no log) and indistinguishable from a real empty page. Wrapping any handler in `cache_page` converts its body errors into silent blank pages.
- **Fix:** Log the error and return 500 (or strip `Content-Length` and propagate a 502-style error); never re-use the success `parts` with a fabricated body.

## BROKEN-8 — `Form<T>` extractor ignores `Content-Type` and swallows body-parse failures into "field required" errors
**Severity: medium**

- **File:** `crates/umbra-core/src/forms.rs:862-876`
- **Evidence:** `let pairs: HashMap<String, String> = serde_urlencoded::from_bytes(&bytes).unwrap_or_default();` — no `Content-Type` check (axum's own `Form` rejects non-urlencoded with 415), and a malformed body silently becomes an empty map.
- **Why it matters:** A client POSTing JSON (or multipart) to a `Form<T>` handler gets a wall of "name is required / email is required…" 422s instead of a 415, hiding the real bug (wrong content type) from both client and developer. The `unwrap_or_default()` is the masking pattern CLAUDE.md calls out. (Multipart being unsupported is documented — gaps2 #19 — but silently mis-diagnosing it is not.)
- **Fix:** Check `Content-Type`; on mismatch or parse error return 415/400 (or a distinct non-field `FormErrors` naming the parse failure).

## BROKEN-9 — `CachePlugin` registered via `.plugin(...)` is inert
**Severity: medium** (doc-contract)

- **File:** `plugins/umbra-cache/src/lib.rs:74-75, 466-484`
- **Evidence:** `/// Process-wide ambient cache, set once during App::build() (or manually by calling CachePlugin::init).` — `App::build()` never sets `AMBIENT_CACHE` (core can't depend on the plugin), and `impl Plugin for CachePlugin` contributes nothing; only the **static** `CachePlugin::init(cache)` wires it. A user who writes the idiomatic `App::builder().plugin(CachePlugin)` gets a silently dead cache — `cache_page` "silently skips" when ambient is unset, so every page is uncached with zero feedback.
- **Why it matters:** "Registering the plugin" is the framework's one wiring idiom; a plugin whose registration is inert while a separate static init does the real work is a contract violation, and the doc actively misleads.
- **Fix:** Give `CachePlugin` a constructor carrying the `Cache` and set the OnceLock in `Plugin::on_ready` — or correct the doc and emit a boot-time warning when `cache_page` layers exist with no ambient cache.

## Lower-severity
- **BROKEN-10 (low–med)** — `MemoryBackend` never evicts expired entries except on read (`umbra-cache/src/lib.rs:241-276`); `SqliteBackend` has `sweep()`, memory has none. Write-once-read-never keys (e.g. `cache_page` keys for long-tail query strings) grow unbounded — a crawler inflates RSS without limit. Add opportunistic purge on `set`, a `sweep()` parity method, and/or a max-entry cap. (Expiry-on-read itself is correctly enforced.)
- **BROKEN-11 (low)** — Embedded static service answers every HTTP method with the body (`umbra-static/src/lib.rs:288-313`): `EmbeddedDirService::call` never inspects `req.method()`, so `POST /widget/assets/app.js` returns 200+body (Fs mode correctly returns 405). Embedded mode also lacks the ETag/`If-Modified-Since` the module doc attributes to the plugin. Early-return 405 for non-GET/HEAD; consider a content-hash ETag.
- **BROKEN-12 (low)** — `CacheBackend` trait doc promises "backends swallow errors internally **and log them**" (`umbra-cache/src/lib.rs:129-132`) but `SqliteBackend::set/delete/clear` and all `RedisBackend` methods are `let _ = …` with no `tracing` (`:362-384, :433-450`). A dead Redis / locked SQLite means every cache write silently no-ops forever. Add `tracing::warn!` on each swallowed `Err`.
- **BROKEN-13 (low)** — Stale comment in `crates/umbra-core/src/orm/write.rs:26-34` claims umbra-rest still has a SQLite-only `bind_json_value` path; `bind_json_value`/`sqlx::Sqlite` no longer exist in `plugins/umbra-rest/src/`. Delete/update the paragraph — it sends the next dev hunting a Postgres-breaking path that was already fixed.
- **BROKEN-14 (low)** — `#[derive(Form)]` rejection message (`crates/umbra-macros/src/lib.rs:3019-3025`) lists only `required, optional, email, password, min_length, max_length, length(...)` but the parser also accepts `phone`, `url`, `regex = "..."`, `message = "..."` (`:2963-2976`). Extend the message.

## Plugin-contract review (raw `sqlx::query` in plugins)
| Hit | Verdict |
|---|---|
| `umbra-cache/src/lib.rs:311-384` (SqliteBackend CRUD) | **Borderline-allowed** — long justification comment naming the missing ORM escape hatch (`Manager::upsert_with(&pool)` for non-ambient pools); CREATE TABLE is DDL, row ops are raw because the backend takes an explicit pool. The `?` placeholders make a future `PgBackend` copy-paste-broken — the deferred `*_with(&pool)` ORM gap should be logged in gaps2.md, not just a code comment. |
| `umbra-health/src/lib.rs:243-248` (`SELECT 1`) | **Allowed** — dialect-neutral connectivity probe, dispatched per `DbPool`. |
| `umbra-rls/src/lib.rs:317-335` | **Allowed** — Postgres RLS policy DDL (exception 2), backend-gated. |
| `umbra-admin/src/models.rs:464-479` | **Allowed** — `ensure_tables_for_tests`, the blessed test pattern. |

`umbra-tasks`, `umbra-email`, `umbra-signals`, `umbra-static` contain **zero** raw sqlx. Tasks goes fully through the ORM — exemplary.

## Directed-question answers
- **Tasks semantics:** effectively **at-most-once**. Crash after claim → task lost (BROKEN-2). Two Postgres workers → duplicate execution (BROKEN-1). Retries exist only for handler-returned `Err` within the same healthy worker, with **no backoff** (immediate re-eligibility — backoff tracked in features.md #43).
- **Email:** **really sends** — `lettre::AsyncSmtpTransport::starttls_relay` on 587 with default cert verification; **no TLS bypass** (no `danger_accept_invalid_certs`). Console fallback loud-by-design (but see BROKEN-6's panic).
- **Cache:** expiry **is enforced** (read-time for memory/SQLite, server-side for Redis). Gaps are the never-logged write failures (BROKEN-12), memory eviction (BROKEN-10), empty-200 masking (BROKEN-7). No data race — memory backend is mutex-guarded; `cache_page` has no stampede protection (documented deferral).
- **CLI:** no stubs — `serve/makemigrations/migrate(--fake/--fake-initial/--allow-drift)/showmigrations/inspectdb/dumpdata/loaddata/dev` all implemented with real error paths/help. No SQLite-works/Postgres-`todo!` arms; Postgres-only field gating happens at boot via `check.rs` by design.

## Already tracked (skipped, with references)
- Task retry backoff + apalis/TaskRunner rework — features.md #43; REAL-GAPS A8.
- No periodic scheduling ("beat") — tasks module doc deferral + `docs/specs/outlines/tasks.md`.
- Email API backends (SendGrid/SES), CC/BCC, inline images — features.md #39; email.mdx scope.
- Multipart/file-upload extractor (`Form<T>` urlencoded-only) — gaps2 #19; features.md #40.
- cache `get_or_set`, ETag/304, Vary-awareness, memcached — gaps.md #70 deferral list + module doc.
- Static `STATIC_URL`/`collect_static`/per-plugin static exposure — gaps.md #67 (open).
- Testing factories / per-test DB rollback — REAL-GAPS Part A #18; features.md #52.
- Signals: typed `m2m_changed`, `disconnect`, cross-process broadcast — umbra-signals module doc deferrals.
- Input-preservation on form re-render — shipped (commit 7235ed0, `FormErrors::with_raw`).
