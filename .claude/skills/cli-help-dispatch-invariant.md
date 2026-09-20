---
name: cli-help-dispatch-invariant
description: Use when adding or changing a CLI command family, the unified `umbral help` catalog, or the two `Cli` parsers — and whenever `cargo run -- <cmd>` answers `unknown command` for a command that DOES appear in `umbral help`. The trap: the help listing and the dispatcher are two enumerations that must stay in correspondence.
---

# The help catalog and the dispatcher must list the same commands

## Context

`umbral` has **two** clap `Cli` structs, and this is the thing that bites:

- `crates/umbral-cli/src/main.rs` — the **global `umbral` binary** (`cargo install umbral-cli`). Owns `plugin add`, forwards everything else to the project.
- `crates/umbral-cli/src/lib.rs` — the **app-embedded** parser that `cargo run -- <cmd>` reaches inside a project (`serve`, `migrate`, …), driven by `dispatch_with_argv`.

On top of those, the unified help screen (`umbral help` / `--help` / unknown-command) is rendered from a **catalog** that merges commands from several sources. The trap is that the *help catalog* (what the user is told they can run) and the *dispatcher* (what actually runs) are **two separate walks over the same sources**. Unify the listing without unifying the dispatch and the help lies: it advertises a command that answers `error: unknown command`.

That is exactly gap 66 → the bug fixed in `fix(cli): dispatch scaffolders from the app-embedded CLI too`: the scaffolders (`startproject` / `startplugin` / `startcommand`) were added to the help catalog but were only dispatchable by the global binary, so `cargo run -- startplugin --help` inside a project said `unknown command startplugin`.

## Approach

### The three command sources, each single-sourced

Every command belongs to exactly one group, and each group is now derived from **one** origin used by BOTH help and dispatch — so a divergence *within* a group is impossible:

| Group | Single source | Help reads | Dispatch reads |
|---|---|---|---|
| Built-in subcommands (`serve`, `migrate`, …) | the `Cli` type in `lib.rs` | `<Cli as CommandFactory>::command().get_subcommands()` | `Cli::try_parse_from(argv)` |
| Scaffolders (`start*`) | the `ScaffoldCli` type in `scaffold_cli.rs` | `scaffold_cli::command_catalog()` | `scaffold_cli::try_run_scaffold` → `is_scaffold_command` |
| Plugin / app commands | `CommandSet` / `collect_commands` (`umbral-core/src/cli.rs`) | `CommandSet::collect(..).catalog()` | `CommandSet::collect(..).dispatch(..)` |

The scaffolders and built-ins each read their names off a real clap type; the plugin group's `command_catalog_with_app_commands` is literally `CommandSet::collect(..).catalog()`, sharing `collect_commands` with the dispatcher.

### Dispatch order (in `dispatch_with_argv`, `lib.rs`)

0. top-level help (`wants_top_level_help`) → print the full catalog, exit.
0.25. **scaffolders** (`scaffold_cli::try_run_scaffold`) → runs the shared scaffold dispatch; `--help` renders usage and exits. This is BEFORE the `on_ready` decision, so scaffolding never boots the app.
1. **plugin / app commands** (`CommandSet::dispatch`).
2. **built-in subcommands** (`Cli::try_parse_from`); an unmatched token here is the `unknown command` path.

Both the global binary (`main.rs`) and the app-embedded dispatcher call the **same** `scaffold_cli::try_run_scaffold`, so the two entry points can't drift on scaffolders.

### The lock that keeps it honest

The remaining risk is a **whole new command group** added to the help catalog but not the dispatcher (or the reverse) — the gap-66 shape at the group level. That is locked by an invariant test:

`crates/umbral-cli/src/lib.rs` → `tests::help_and_unknown_list_builtins_and_plugin_commands` asserts `full_catalog(&app)` (help) as a set **equals** the union of the three dispatch sources' names. Add a group to one side only and it fails at `cargo test` — which is a mandatory pre-commit gate (there is no test CI, so the gate IS `cargo test` before commit).

### Adding a new command family (the checklist)

1. Give it a single source of truth (a clap type, or an entry in `CommandSet`).
2. Wire it into `dispatch_with_argv` (and `main.rs` if it must also run project-free).
3. Add its names to `full_catalog` so help lists it.
4. Run `cargo test -p umbral-cli` — the invariant test proves 2 and 3 agree.

## Why

A help screen that lists an unrunnable command is worse than omitting it: the user reads the catalog as a promise and trusts the tool less than they trust the resulting error. Deriving the listing from the same source the dispatcher routes through means the promise is structurally true, not maintained by discipline. Where a shared *type* isn't possible (a brand-new group), the set-equality test converts a silent, months-later divergence into a red `cargo test` on the commit that introduces it.

Why a test rather than a type-level guarantee: the three groups dispatch through genuinely different mechanisms (clap parse vs. `CommandSet` vs. the scaffold parser). Collapsing them into one data-driven dispatcher would be a large, risky rewrite for a seam that only a new *group* can breach; the invariant test guards that seam at proportionate cost.

## Pitfalls

- **One `App::build` per test binary.** `App::build` publishes settings into a process-wide `OnceLock` that panics on a second call ("settings::init called more than once"). The invariant assertions are folded INTO `help_and_unknown_list_builtins_and_plugin_commands` (which already built one app) rather than a separate `#[tokio::test]` that would build a second and blow up whichever test loses the race.
- **`cargo run -- <cmd>` inside a project is the APP binary, not the `umbral` CLI binary.** To exercise the `umbral` toolchain locally without installing it, run `cargo run -p umbral-cli -- <cmd>` from the repo root. Both routes share `try_run_scaffold`, so the global-binary run is a faithful proxy for the app-embedded one when you can't touch a running example (e.g. the shop dev server).
- **`unknown_token` picks the first non-flag token.** For `migrate --bogus` that's `migrate`, a *valid* catalogued command — so "is the bad token in the catalog?" does NOT by itself mean a wiring bug (it's a flag error on a real command). Only a clap `InvalidSubcommand` for a catalogued name means help/dispatch diverged. Don't build a runtime "listed but not dispatched" guard on catalog-membership alone.
- **`scaffold_command_catalog()` derives from `ScaffoldCli`.** Don't reintroduce a hand-written `(name, about)` list — that was the original "keep the two in sync" hazard the doc comment warned about, and it drifts.

## See also

- `crates/umbral-cli/src/scaffold_cli.rs` — the shared scaffold parser + `try_run_scaffold`.
- `crates/umbral-cli/src/lib.rs` — `dispatch_with_argv`, `full_catalog`, the invariant test.
- `crates/umbral-core/src/cli.rs` — `CommandSet::catalog()` / `dispatch()`, the shared `collect_commands`.
- `.claude/skills/management-command-registration.md` — where to register a command and the `on_ready`/reserved-name rules.
- `planning/archive/gaps3-done.md` #66 — the help-catalog-completeness write-up.
