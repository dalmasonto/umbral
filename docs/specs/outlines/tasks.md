# Outline — Tasks (DB-backed task queue)

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at M9 entry. |
| **Maps to milestone** | M9 |
| **Companions** | `02-plugin-contract.md`, `04-orm-model-and-fields.md`, `06-migration-engine.md`, outline `email.md`, outline `signals.md`, outline `testing.md` |

## Purpose

`umbral-tasks` enqueues work that runs outside the request/response cycle, with retries, periodic schedules, and a worker process you run alongside the web server. The default broker is the application's own Postgres database - the same pool the ORM already uses. No Redis, external message broker, or other service is required to get from `cargo new` to "this email sends in the background." That choice is the whole point: a fresh umbral project gets background work for the cost of one `.plugin(TasksPlugin::default())` line. Structurally `umbral-tasks` is an ordinary plugin — it implements the same `Plugin` trait as `umbral-auth`, owns its `tasks` table via its own migration that `migrate` picks up like any other, and contributes `worker` and `beat` subcommands through `Plugin::commands()`. If the contract from `02-plugin-contract.md` couldn't express a task queue, the contract would be wrong; this plugin is one of the dogfooding tests that proves it.

## Key concepts

**`#[task]` macro and `Task` trait.** A task is a function annotated with `#[task]` whose arguments are serde-serializable. The macro generates a `Task` impl that knows how to serialize the args into the broker row and deserialize them in the worker. The call site looks like a normal function but returns a handle the caller can `.enqueue()`, `.delay()`, or `.schedule_at()`:

```rust
#[task]
async fn send_welcome_email(user_id: i64) -> Result<()> {
    Email::welcome(user_id).send().await
}

// caller (e.g. inside a handler or a signal):
send_welcome_email::enqueue(user.id).await?;
```

**DB-backed broker.** The broker owns a `tasks` table (queue name, payload, status, attempt count, scheduled-for timestamp, result) provisioned by a plugin migration. Enqueue is an insert; the worker claims rows with `SELECT … FOR UPDATE SKIP LOCKED` so multiple workers don't double-execute. The engine choice — `underway` (Postgres-native, already shaped this way) vs `apalis` (multi-backend, would need its Postgres adapter) — is an open question; the surface above doesn't change either way.

**Worker process.** `cargo run -p umbral-cli -- worker` boots the framework (settings, pool, plugins, `on_ready`) without binding the HTTP listener, then loops: claim a batch, dispatch each row to the registered `Task` impl, mark success/failure. Concurrency is a thread pool sized from settings.

**Retries.** A failed task is rescheduled with exponential backoff up to a per-task `max_retries`. The retry policy is declared on `#[task]` (`#[task(retries = 5, backoff = "exp")]`) and stored in the broker row so a worker restart preserves it.

**Periodic scheduling ("beat").** `cargo run -p umbral-cli -- beat` is the scheduler process: it reads periodic-task declarations (`#[task(cron = "0 * * * *")]` or registered in `on_ready`) and enqueues them at their next-fire time. Beat is single-process by design; running two beats is a misconfiguration the system check warns about.

**Ambient handle.** `umbral::tasks::enqueue(...)` reads the `TaskQueue` from the `OnceLock` set in `01-app-and-settings.md`. Code outside a task definition (handlers, signals, other plugins) goes through this accessor; it returns an error if `umbral-tasks` wasn't registered, mirroring the cache and DB accessors.

## Promote-to-deep trigger

Promote to a deep spec at M9 entry, once the migration engine (M5) and the plugin trait (M7) are stable enough for a real plugin to consume them end-to-end.

## Open questions

- **Engine choice: `underway` vs `apalis`.** `underway` is Postgres-native and already implements the `FOR UPDATE SKIP LOCKED` shape we want; `apalis` offers a backend-agnostic abstraction that maps onto a future pluggable-broker design. Settle by benchmarking the worker loop on a non-trivial workload at M9.
- **Pluggable broker for future Redis support.** The plugin should expose a `Broker` trait so a third party can ship `umbral-tasks-redis` later without changing user code. The shape of that trait — and whether it's worth defining before a second backend exists — is open.
- **Result storage strategy.** Three options: drop results, store them in the `tasks` row (cheap, bounded), or write them to a separate `task_results` table with TTL. Decision lives with the deep spec.
- **`#[task]` and serde.** Arg types must be `Serialize + Deserialize`, but compile-time enforcement (so a non-serde arg fails at macro expansion, not at first enqueue) needs proc-macro work; how strict to be is open.
- **Per-task vs per-queue settings.** Retries and backoff are per-task today; rate limits, concurrency caps, and priorities probably belong at the queue level. Where the boundary sits is open.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (Plugin trait, `commands()`, `on_ready`), `04-orm-model-and-fields.md` (the `tasks` table is declared as a model), `06-migration-engine.md` (plugin-owned migration registration).
- Ambient `umbral::tasks::enqueue(...)` handle: `01-app-and-settings.md` §Ambient state via `OnceLock`s.
- Sibling outlines: `email.md` (the canonical first task category — password reset, welcome mail), `signals.md` (a `post_save` handler is a common task-enqueue site), `testing.md` (test client runs tasks inline or against a transactional broker).
