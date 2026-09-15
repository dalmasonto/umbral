---
name: worktree-shared-target-contamination
description: Use when building or testing umbral in a git worktree while the main checkout (or another worktree) sits on a different branch — sharing one CARGO_TARGET_DIR corrupts proc-macro artifacts and produces phantom `__resolved` / `from_resolved` errors.
---

# Worktree builds need an isolated CARGO_TARGET_DIR

## Context
Multi-agent work on umbral often runs several `git worktree`s at once (one branch per task) plus the main checkout on its own in-flight branch. It is tempting to point every worktree at the one big repo-root `target/` via `CARGO_TARGET_DIR=/…/umbra/target` to save disk (the root target can be 100G+, and per-worktree targets add up).

**Don't.** When two working trees on *divergent* branches build into the same target dir, cargo overwrites the shared `umbral-macros` proc-macro dylib and `umbral-core` rlib with whichever branch built last. A test binary then links `umbral-core` compiled from branch A against a `#[derive(Model)]` expansion from branch B. The symptom is a compile error that has nothing to do with your change:

```
no method named `__resolved` found …
no associated function `from_resolved` …
```

(`__resolved` / `from_resolved` exist on the `feat/orm-heavy-relations` refactor branch but not on `main`, so a `main`-based worktree "borrowing" heavy-relations' macro dylib fails exactly there.) It flip-flops: a suite is green, then a concurrent sibling build (another agent, or **rust-analyzer** indexing the main checkout) flips the shared dylib and the same suite fails to compile. FK/`Model`-deriving tests are the ones that break; the lib itself compiles clean.

## Approach
1. Give each worktree its **own** target dir. Cleanest: a worktree-local one that stays out of git (the repo ignores `target/`, so name it under the worktree or use `target-iso` and don't commit it):
   ```bash
   export CARGO_TARGET_DIR=/…/umbra/.worktrees/<task>/target-iso
   ```
2. Run the worktree's fmt/clippy/build/test with that env set. First build is a full cold compile — that is the price of a trustworthy signal; accept it.
3. **Never** `cargo clean` the shared root `target/` to "fix" contamination — that clobbers the main checkout's in-flight build (and the user's dev work). Isolate instead.
4. Delete the worktree's `target-iso` after the task is reviewed/merged to reclaim disk. Target specific `--test <name>` binaries rather than `--all-targets` to keep a single isolated target small (a focused 2–4-crate run is a few GB; `--all-targets` across core builds ~76 test binaries and balloons).
5. If you *see* `__resolved`/`from_resolved` in a worktree you believe is isolated, something is still sharing the dir — recheck the env var; it is never a bug in your change.

## Why
Cargo fingerprints by source, but the *output paths* (`target/debug/deps/libumbral_macros-*.so`, `libumbral_core-*.rlib`) collide across trees that share `CARGO_TARGET_DIR`. Proc-macro dylibs are loaded by rustc at compile time, so a stale/other-branch macro silently changes what `#[derive(Model)]` expands to. Per-worktree targets cost disk but the disk is cheap next to a phantom-error debugging spiral and, worse, an agent "fixing" code that was never broken.

## Pitfalls
- rust-analyzer on the main checkout is a silent background builder — it alone can flip the shared dylib mid-run. Isolation is the only reliable defense.
- The failure masquerades as a real regression. Before believing a `__resolved` error, check whether the target dir is shared with a divergent branch.
- Watch disk: the root `target/` here has hit 120G+. Check `df -h` before spinning up multiple isolated targets; clean each up promptly.

## See also
- CLAUDE.md "Commit cadence" (verify the whole workspace before commit) and "Never stash the user's working tree" (same family: don't disrupt the user's in-flight tree/build to unblock yourself).
- `superpowers:using-git-worktrees` for creating the isolated worktree in the first place.
