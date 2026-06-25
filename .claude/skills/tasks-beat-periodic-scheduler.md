---
name: tasks-beat-periodic-scheduler
description: Use when working on umbral-tasks periodic/cron "beat" scheduling — the PeriodicTask model, Schedule type, run_beat loop, the optimistic-claim double-enqueue guard, or the tasks-beat CLI command.
---

# umbral-tasks periodic "beat" scheduler

## Context
umbral-tasks ships Celery-beat-style recurring tasks: a schedule (cron or fixed interval) fires a task on a cadence. Beat is the *scheduler* (a separate process from the worker); it enqueues normal `TaskRow`s that the worker then drains. All of it lives in the single file `plugins/umbral-tasks/src/lib.rs`.

## Approach

**The pieces (all in `lib.rs`):**
- `enum Schedule { Cron(String), Every(Duration) }` — `next_after(after) -> Option<DateTime<Utc>>`, `to_storage()`/`from_storage()` round-trip to a single string column (`"cron:0 0 * * *"` / `"every:3600"`).
- `PeriodicTask` model — added to `Plugin::models()` so `makemigrations`/`migrate` autodetects a new `periodic_task` table. NO migration file (built-in plugins ship none). `name` is `#[umbral(unique)]` (the stable key). All identity columns are written only by code; nullable/defaulted columns keep it additive.
- `PeriodicSpec { name, schedule, task, payload }` + `TasksPlugin::periodic(...)` builder — collects specs on the (now non-unit) `TasksPlugin` struct.
- `static REGISTERED_PERIODIC: OnceLock<Vec<PeriodicSpec>>` — installed in `Plugin::on_ready` (sync); the async DB sync runs in the beat loop.
- `run_beat(BeatOptions)` / `run_beat_once()` — mirror `run_worker`/`run_worker_once`. Startup: `sync_registered_periodic()`. Each tick: `fire_due_periodic()`.
- `BeatCommand` (`tasks-beat`, `--once`) in `Plugin::commands()` alongside `WorkerCommand`.

**The atomic claim (the load-bearing bit):** `fire_due_periodic()` loads `enabled = true AND next_run <= now`, then for each row does an *optimistic conditional UPDATE*:
```rust
let affected = PeriodicTask::objects()
    .filter(periodic_task::ID.eq(row.id) & periodic_task::NEXT_RUN.eq(row.next_run))
    .update_values(patch)  // patch advances next_run + stamps last_run
    .await?;
if affected == 1 { enqueue_periodic(&row).await?; }
```
`update_values` returns `Result<u64, WriteError>` — the **affected-row count**. Gating the enqueue on `affected == 1` is what makes multiple beat instances safe: the second instance reads the same `next_run`, but its UPDATE's `NEXT_RUN.eq(row.next_run)` guard no longer matches (the winner already advanced it), so it affects 0 rows and enqueues nothing. Same pattern as `claim_one()` on the worker side.

**Cron format adaptation:** the `cron` crate wants a 6-field expr (`sec min hour dom mon dow`). `normalize_cron()` prepends `"0 "` to a standard 5-field expr; 6-field passes through. `Schedule::next_after` does `cron::Schedule::from_str(&normalized).ok()?.after(&after).next()`.

## Why
- Beat is its own process (Celery-style) → a separate `tasks-beat` command, not a worker flag.
- `on_ready` is sync, so it can only *publish* the in-memory specs to a OnceLock; the async upsert (`sync_periodic_specs`) happens when beat starts. Sync recomputes `next_run` ONLY when the schedule string changed — an unchanged redeploy that recomputed would shove `next_run` forward forever and starve the task.
- The conditional-UPDATE guard reuses the affected-count return the ORM already gives, so no new ORM surface was needed (no raw SQL).

## Pitfalls
- `TasksPlugin` is no longer a unit struct (it holds `Vec<PeriodicSpec>`). Every `.plugin(TasksPlugin)` / `Box::new(TasksPlugin)` call site must become `TasksPlugin::default()`. The existing tests in `tests/{integration,reliability,macro_integration}.rs` were updated for this.
- `enqueue_periodic` writes `row.payload` verbatim (it's already a JSON string) — re-serializing a `&str` would double-encode it. Don't call `enqueue::<P: Serialize>` with the stored string.
- Beat integration tests need BOTH `task_row` AND `periodic_task` tables created in the boot harness (`tests/beat.rs`).
- `cron = "0.12"` in Cargo.toml; `cron::Schedule::after()` returns an iterator — use `.next()`.

## See also
- `plugins/umbral-tasks/src/lib.rs` (the whole plugin, single file)
- `plugins/umbral-tasks/tests/beat.rs` (next_after, fire-once, double-enqueue guard, registration upsert)
- `documentation/docs/v0.0.1/plugins/tasks.mdx` § "Periodic tasks (beat)"
- `planning/features.md` #82 (remaining Celery gaps: result backend, status API, priority queues)
