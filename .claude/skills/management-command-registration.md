---
name: management-command-registration
description: Use when adding, scaffolding, or debugging a CLI subcommand in umbral — a project's own command, a plugin's command, or a new built-in — and when a command "isn't found" despite being registered.
---

# How a management command reaches argv

## Context

There are now **three** layers that can own a `cargo run -- <cmd>` subcommand, and they are tried in a specific order. Getting the order wrong is how a command silently shadows `migrate`. This is the map.

## Approach

### The three layers, in dispatch order

`umbral_cli::dispatch_with_argv` (crates/umbral-cli/src/lib.rs) does:

1. **App commands + plugin commands** — `umbral_core::cli::dispatch_with_app_commands(app.commands(), app.plugins(), argv)`. App commands (registered via `AppBuilder::command` / `.commands(vec)`) are collected **first**, then each plugin's `Plugin::commands()`. First-registered wins a name clash; the loser is dropped with a `tracing::warn!`.
2. **Built-in subcommands** — only if step 1 returns `Unmatched`. clap parses argv against the `Command` enum in `umbral-cli/src/lib.rs` (`serve`, `migrate`, `makemigrations`, …).

**The consequence that bites:** step 1 runs BEFORE step 2. A plugin or app command named `migrate` does not collide loudly — it *takes over*, and migrations quietly stop applying. That's why `scaffold::reserved_command_names()` rejects those names at `startcommand` time. The framework half of that set is read off the derived clap parser (`<Cli as CommandFactory>::command()`), so a new built-in reserves its own name; the built-in *plugins'* commands can't be read that way (they only exist on a built App) and are listed in `RESERVED_PLUGIN_COMMAND_NAMES`.

### Where to register a new command

| The command belongs to… | Register it via | Scaffold with |
|---|---|---|
| The user's project (a backfill, an import) | `App::builder().command(Cmd)` or `.commands(commands::all())` | `umbral startcommand <name> --in root` |
| A plugin (ships with it) | `Plugin::commands()` → `Vec<Box<dyn PluginCommand>>` | `umbral startcommand <name> --in <plugin>` |
| The framework itself | a variant on `Command` in `umbral-cli/src/lib.rs` + an arm in `dispatch_with_argv` | by hand |

### Implementing `PluginCommand`

```rust
use umbral::cli::{CliError, PluginCommand, clap};   // clap from the FACADE

#[umbral::async_trait]
impl PluginCommand for MyCommand {
    fn command(&self) -> clap::Command { clap::Command::new("my_cmd").about("...") }
    async fn run(&self, m: &clap::ArgMatches) -> Result<(), CliError> { Ok(()) }
}
```

`umbral::cli::clap` is a re-export of the framework's own clap. Declaring `clap = "4"` in your own Cargo.toml and implementing against *that* risks resolving a different major version than the dispatcher parses with — which surfaces as a page-long type mismatch on `fn command`, not a friendly error.

### `on_ready` fires for a command — but not for schema commands

`command_needs_ready()` (umbral-cli/src/lib.rs) returns `false` for `migrate` / `makemigrations` / `inspectdb` / `serve` / … and `true` for **everything else**, which includes every app and plugin command. So by the time your `run()` fires, the app is fully ready: pool open, models registered, every plugin's `on_ready` hook fired. The ORM is ambient — no pool to thread through. (gaps3 #41 is why schema commands are excluded: their hooks seed tables that `migrate` hasn't created yet.)

## Why

**Why `AppBuilder::command` exists at all (gaps3 #81).** Before it, `Plugin::commands()` was the only doorway to argv, so a command owned by the *binary* could only be added by inventing a dummy plugin to carry it. That's the plugin trait being used as a doorway rather than as a packaging unit. The App now owns its own commands, mirroring `App::plugins()` with `App::commands()`.

**Why the help catalog and the dispatcher share `collect_commands`.** They dedup identically, so the listing can never advertise a command that a name clash would prevent from running.

**Why `commands/mod.rs::all()` and not real auto-detection.** Rust has no runtime module reflection — nothing can scan `commands/` at startup and find the structs. The alternatives (build script, `inventory`-style linker section) hide the list where you can't read it. `all()` is a plain function the scaffolder maintains via `// umbral:startcommand` marker comments, and that you can still edit by hand.

### Writing a command that GENERATES code

Use `umbral::codegen` (in `umbral-core`, re-exported from the facade). Never hand-roll `mod` insertion — that's how a generator eats somebody's `main.rs`.

| Need | Use |
|---|---|
| "root or which plugin?" | `Target` / `resolve_target` → `ResolvedTarget { crate_root, owner_file, is_root }` |
| Write a file | `write_new_file` — refuses to overwrite, always |
| Declare `mod foo;` in main.rs / lib.rs | `declare_module` (`ResolvedTarget::module_decl` picks `mod` vs `pub mod`) |
| Append to a generated registry / re-export list | `insert_before_marker` with a `// umbral:<tool>` marker comment |
| The generated code needs a crate the target doesn't have | `ensure_dependency` |
| Ask the user something | `codegen::prompt` — `is_interactive()` FIRST; never prompt a pipe |

Every editing primitive **declines** (returns `None`) when the file isn't the shape it expected. Report the lines to add by hand; don't guess at a file you don't recognise.

`umbral-rest` is the worked example: `plugins/umbral-rest/src/commands.rs` ships `startpermission` / `startauthentication` / `startpagination` / `startthrottle` on exactly these primitives, and `umbral-cli`'s `startcommand` uses the same ones (`CommandTarget` is an alias of `codegen::Target`).

## Pitfalls

- **`if !app.plugins().is_empty()`** used to guard step 1. A plugin-free project's own command was therefore unreachable — `cargo run -- my_cmd` said "unknown command" for a command in its own `main.rs`. The guard now also checks `app.commands()`. Any future short-circuit there needs the same care.
- **One `App::build` per test binary.** `App::build` publishes settings into a process-wide `OnceLock`; a second build in the same test binary panics with "settings::init called more than once". Integration tests that build an App live one-per-file (see `crates/umbral-cli/tests/app_command_*.rs`).
- **A command with no `.about(...)`** lists as a dash in `umbral help` and nobody finds it. `command_catalog` emits a `debug!` about it.
- **String templates have no typechecker.** `crates/umbral-cli/tests/app_command_dispatch.rs` is deliberately written in the exact shape `scaffold::render_command_file` emits, so a generated file that stops compiling takes a test with it. For the REST class templates there's no such mirror — they were verified by scaffolding into a real `startproject` demo and running `cargo check`, which caught two bugs (`PaginationScalar::Integer` doesn't exist; `RateLimiter::new` takes a `Rate`, not a `&str`). **Always compile a generated template before shipping it.** String assertions prove nothing about whether the code builds.
- **A trait that names a foreign type in its signature must re-export that crate.** `PluginCommand::command` → `umbral::cli::clap`. `Pagination::paginate` → `umbral_rest::serde_json`. Otherwise every implementor declares its own dependency and bets on resolving the same major version, and a scaffolded project can't implement the trait at all without editing `Cargo.toml`.

## See also

- `crates/umbral-core/src/cli.rs` — the trait, dispatch, catalog.
- `crates/umbral-cli/src/scaffold.rs` — `scaffold_command`, the registry surgery, the reserved-name set.
- `documentation/docs/v0.0.1/cli/startcommand.mdx` — the user-facing page.
- `planning/archive/gaps3-done.md` #81 — the full design write-up.
