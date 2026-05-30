# crates/

The umbra framework's Cargo workspace.

## What's in here

| Crate | Purpose |
|---|---|
| `umbra-core/` | Internals: ORM, migrations, routing, DB backends, the `Plugin` trait. Don't depend on this crate directly. |
| `umbra-macros/` | Proc macros: `#[derive(Model)]`, `#[task]`, etc. Don't depend on this crate directly. |
| `umbra/` | The **facade**. The single stable surface for user code and plugin authors. `use umbra::prelude::*;` brings in everything a typical handler / model / plugin needs. |
| `umbra-cli/` | The `manage.py` equivalent binary (`migrate`, `makemigrations`, `inspectdb`, `worker`, ...). |

## Dependency direction

```
umbra-cli  →  umbra (facade)  →  { umbra-core, umbra-macros }
                  ↑
            plugins/* (from M9 onward, each a separate crate)
```

Arrows point inward. Plugins depend on the facade; the facade depends on the internals. Nothing in `umbra-core` ever depends on a plugin or the facade. Cargo's ban on circular crate deps is what enforces this. See `arch.md §1` for the architectural rationale.

## Build, test, run

All cargo commands run from inside this directory:

```bash
cargo build                      # build the whole workspace
cargo test                       # run all tests
cargo test -p umbra-core         # test one crate
cargo run -p umbra-cli -- <cmd>  # run the manage.py equivalent
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

Build artefacts land at `crates/target/` and `crates/Cargo.lock` (both inside this directory, gitignored except `Cargo.lock` which is tracked because the workspace contains a binary).

## Conventions

- **Workspace inheritance.** Each member crate's `Cargo.toml` uses `version.workspace = true`, `edition.workspace = true`, etc. Shared values live in `[workspace.package]` at the top of `Cargo.toml`. Per-crate values (`name`, `description`, `[dependencies]`) stay in the crate.
- **No wildcards in `[workspace.members]`.** Members are listed explicitly so a half-finished `wip-*` directory doesn't accidentally break `cargo build`. When you add a crate, add its name to the members list deliberately.
- **Path deps between members are relative.** Sibling crates reference each other via `umbra-core = { path = "../umbra-core", version = "0.0.1" }`. Moving the workspace under `crates/` preserved these (the crates are still siblings).
- **Edition 2024, resolver 3, MSRV 1.85.** Set once in `[workspace.package]`.

## Adding a new crate to the workspace

```bash
cargo new --lib --vcs none my-new-crate
# then edit Cargo.toml:
#   [workspace.members]   <- add "my-new-crate"
# and edit my-new-crate/Cargo.toml to use workspace inheritance:
#   version.workspace = true
#   edition.workspace = true
#   license.workspace = true
#   ...
```

## See also

- `../CLAUDE.md`. The "Working in the workspace" section with the where-new-code-goes table, the facade rule, and the commit cadence rules.
- `../docs/specs/02-plugin-contract.md`. What the `Plugin` trait demands.
- `../docs/specs/08-authoring-plugins.md`. The walked guide for writing a third-party plugin crate (which depends on the facade only).
