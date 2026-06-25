# umbral

A Django-shape web framework for Rust.

Declare your data and you get migrations, CRUD, an admin, and an optional REST API almost for free, with Rust's compile-time guarantees instead of runtime hopes.

The name is the adjective 'of the shadow' (from Latin *umbra*, shadow). umbral is a separate Rust framework inspired by Django's shape and ergonomics; it isn't a port of Django and shares no code with the Django project.

> **Status: early/alpha, published on crates.io.** The framework and its built-in plugins ship under the `umbral-*` namespace; start with the [`umbral`](https://crates.io/crates/umbral) facade. APIs will still move before 1.0. See `docs/specs/` for the design and `arch.md` for the architecture.

## What's in this repo

| Path | Purpose |
|---|---|
| `crates/` | The framework's Cargo workspace. Four crates: `umbral-core` (internals), `umbral-macros` (proc macros), `umbral` (the public facade), `umbral-cli` (the `manage.py` equivalent binary). All cargo commands run from inside here. |
| `plugins/` | Built-in plugins, each its own crate that depends only on the `umbral` facade. Lands at M9 onward. |
| `examples/` | Standalone test apps that path-dep the local umbral. Not workspace members; each builds independently and sees the framework only through what the facade re-exports. See `examples/README.md`. |
| `docs/specs/` | Per-subsystem deep specs (`00`–`08`), half-page outlines for M7–M13 in `outlines/`, and the post-M13 backlog in `deferred.md`. |
| `docs/decisions/` | ADR-style design notes. |
| `documentation/` | User-facing docs site (SvelteKit + Specra). |
| `arch.md` | Authoritative architecture spec. |
| `umbral-PRD.md` | Product requirements. |
| `CLAUDE.md` | Working-in-the-codebase guide for AI agents and human contributors. |

## Quick start

```bash
cd crates
cargo build
cargo run -p umbral-cli
```

Today this prints a scaffold-only message; subcommands (`migrate`, `makemigrations`, `inspectdb`, `worker`, ...) land as their milestones do. See `arch.md §8` for the build order.

## The shape umbral is aiming at

- **Thin core, plugin-heavy.** Auth, sessions, admin, tasks, and REST are all plugins. Structurally they're identical to a third-party one. A REST-free app compiles with zero serializer code. See `docs/specs/02-plugin-contract.md` and the authoring guide at `docs/specs/08-authoring-plugins.md`.
- **Managed migrations from day one.** Declare or change a model, an autodetected migration is generated, `migrate` applies it. The declare → migrate → change → migrate cycle *is* the product. See `docs/specs/06-migration-engine.md`.
- **Porting on-ramp via `inspectdb`.** Point umbral at an existing Postgres database; it generates models plus an initial migration that drops straight into the same migration loop. See `docs/specs/07-inspectdb.md`.
- **The easy path is the safe path.** Nullable columns are `Option<T>`. Errors are `Result`. Backend mismatches fail at boot. SQL is always parameterized.
- **Stand on shoulders.** axum, sqlx, sea-query, and tower do the heavy lifting. umbral reimplements conventions and integration, not HTTP, async, SQL, or JSON.

## Documentation

The user-facing docs site is at **https://dalmasonto.github.io/umbral/** (source in `documentation/`, a SvelteKit + Specra site; run locally with `cd documentation && yarn dev`).

For the design specs, start at `docs/specs/00-overview.md` for the reading order and the Django-to-umbral glossary.

## License

Dual-licensed under MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`. The name "umbral" is a trademark of the project (see `TRADEMARK.md`).
