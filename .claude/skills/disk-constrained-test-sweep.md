---
name: disk-constrained-test-sweep
description: Use when `cargo test` dies with "No space left on device" (os error 28) in this workspace, or when full-workspace verification must run on a nearly-full disk.
---

# Testing the workspace when the disk can't hold the test build

## Context

Every integration-test binary in this workspace statically links the whole stack (umbral + sqlx + axum + sea-query…) and weighs **~95 MB even with `debug=0`**. The workspace has 200+ test targets across `crates/*` and `plugins/*`, so a plain `cargo test` builds >20 G of executables before running anything — on a disk with a few GB free it ENOSPCs mid-build, and once `/` hits 0 bytes the Claude Code harness itself starts failing (its output capture lives under `/tmp/claude-1000/...` on the same filesystem, so every Bash call errors with "Command output was lost").

## Approach

1. **Reclaim the known space first** (see the `feedback_clean_all_target_dirs` memory): `examples/*/target` and `umbral_website/target` are separate Cargo projects and fully rebuildable; `target/debug/incremental` is pure cache. Export `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=0` for every test invocation.
2. **Run ONE test target at a time, deleting its linked executable right after it runs.** The rlib/rmeta cache for the whole dep tree is only ~600 MB — it's the final linked test executables that are huge, and they are disposable the moment the target has run. Purge between targets with:
   ```bash
   find target/debug/deps -maxdepth 1 -type f \
     ! -name "*.rlib" ! -name "*.rmeta" ! -name "*.d" ! -name "*.so" \
     -size +20M -delete
   ```
3. Enumerate targets from `cargo metadata --no-deps` (kind `test` → `cargo test -p <crate> --test <name>`; kind `lib`/`proc-macro` → `--lib`; kind `bin` → `--bin <name>`). A ready-made script lives at the session scratchpad as `test-sweep.sh` / `crate-tests.sh` from the 2026-07-31 session; recreate from this recipe if gone.
4. Capture exit codes as `rc=${PIPESTATUS[0]}` **immediately after the pipeline** — reading `PIPESTATUS` after an `if` wrapper returns the wrapper's status, which silently reports failures as `exit=0`.
5. Run the sweep as a background Bash task writing a log; grep the log for `FAIL` / `test result:` totals.

## Why

Crate-by-crate (`cargo test -p`) is NOT fine-grained enough: umbral-core alone has ~100 integration targets ≈ 10 G+ of executables built *before* the first test runs, so even one crate can't fit. Per-target is the granularity that works; each step is mostly a link + run since the rlib cache persists.

## Pitfalls

- Don't wipe `target/` between targets — that forces a full dep rebuild each time. Only purge the >20 MB non-rlib files.
- Doc tests aren't covered by the per-target enumeration; run `cargo test -p <crate> --doc` separately if needed.
- When `/` hits 0 free, *every* harness command fails with "output was lost" — you must free space with a command whose output is tiny (an `rm` with `2>/dev/null`) before you can even run `df`.
- Editing source mid-sweep contaminates the verification: later targets compile the edited code. Either finish the sweep first or restrict edits to crates outside the sweep's dependency closure.

## See also

- Memory: `feedback_clean_all_target_dirs.md`, `project_v002_release.md` (the ~78 G full-build note).
