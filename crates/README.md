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
cargo test                       # run all tests (~1600, across all crates + plugins)
cargo test -p umbra-core         # test one crate
cargo test -p umbra-core --test fulltext_field   # one test binary
cargo run -p umbra-cli -- <cmd>  # run the manage.py equivalent
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

Build artefacts land at `crates/target/` and `crates/Cargo.lock` (both inside this directory, gitignored except `Cargo.lock` which is tracked because the workspace contains a binary).

### Keeping `target/` small (the ~80 GB problem)

A full `cargo test` builds **one binary per `tests/*.rs` file** (dozens of them) plus every crate and plugin. With cargo's default `debug = 2`, each binary embeds full DWARF debug info — that, not your code, is what bloats `target/` toward **~80 GB**. Two settings cut it to **~20 GB**:

1. **`debug = "line-tables-only"`** — already set in this workspace's `Cargo.toml` (`[profile.dev]` + `[profile.test]`). It keeps panic/backtrace file:line locations (test failures still point at the right line) and drops the bulk of the debug info. This is the single biggest lever; you get it for free.
2. **`CARGO_INCREMENTAL=0`** — skips the incremental-compilation cache (~1.3 GB). Worth it for one-shot full runs (CI, a big test sweep); leave it on for day-to-day dev where incremental speeds up rebuilds.

So a disk-friendly full run is just:

```bash
CARGO_INCREMENTAL=0 cargo test
```

To override the debug level per-invocation without editing files (e.g. a throwaway clone, or to go even smaller with `0`):

```bash
CARGO_PROFILE_DEV_DEBUG=line-tables-only CARGO_PROFILE_TEST_DEBUG=line-tables-only CARGO_INCREMENTAL=0 cargo test
```

If `target/` has already ballooned from an earlier build, `cargo clean` (or `rm -rf target`) reclaims it; the next build uses the slim settings.

> Note: `--test-threads=1` serializes test *execution*, not the build, so it does **not** save disk — the build is what consumes it. Tests run in parallel by default; keep it that way.

### Running the Postgres-backed tests

Most tests run on in-memory SQLite and need nothing. A handful exercise Postgres-only behaviour (full-text search, native `uuid` relations, array/JSON/network types, PG backup) and are gated `#[ignore]` so they self-skip when no server is configured. To run them, point `UMBRA_TEST_POSTGRES_URL` at a database and pass `--include-ignored`:

```bash
export UMBRA_TEST_POSTGRES_URL="postgres://user:pass@localhost/umbra_test"

# all of a crate's tests, including the Postgres-gated ones
cargo test -p umbra-core -- --include-ignored

# just the full-text-search suite against Postgres
cargo test -p umbra-core --test fulltext_field -- --include-ignored
```

The tests create and drop their own tables (`DROP TABLE IF EXISTS …` first), so the target database only needs `CREATE`/`DROP` privileges; no migrations or fixtures to set up.

## Conventions

- **Workspace inheritance.** Each member crate's `Cargo.toml` uses `version.workspace = true`, `edition.workspace = true`, etc. Shared values live in `[workspace.package]` at the top of `Cargo.toml`. Per-crate values (`name`, `description`, `[dependencies]`) stay in the crate.
- **No wildcards in `[workspace.members]`.** Members are listed explicitly so a half-finished `wip-*` directory doesn't accidentally break `cargo build`. When you add a crate, add its name to the members list deliberately.
- **Path deps between members are relative.** Sibling crates reference each other via `umbra-core = { path = "../umbra-core", version = "0.0.1" }`. Moving the workspace under `crates/` preserved these (the crates are still siblings).
- **Edition 2024, resolver 3, MSRV 1.85.** Set once in `[workspace.package]`.

## Adding a new crate to the workspace

```bash
cargo new --lib --vcs none my-new-crate
# then edit the repo-root Cargo.toml:
#   [workspace] members   <- add "crates/my-new-crate" (or "plugins/<name>")
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
