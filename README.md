# umbral

A batteries-included web framework for Rust.

Declare your data models and umbral gives you managed migrations, a typed ORM, an admin UI, an auto-generated REST API, auth, sessions, background tasks, and email - wired together, with Rust's compile-time guarantees instead of runtime hopes.

The name is the adjective "of the shadow" (from Latin *umbra*).

> **Status: early/alpha, published on crates.io.** The framework and its built-in plugins ship under the `umbral-*` namespace; start with the [`umbral`](https://crates.io/crates/umbral) facade. APIs will still move before 1.0.

## Quick start

Install the CLI (the `umbral` binary), scaffold a project, and run it:

```bash
# 1. Install the CLI
cargo install umbral-cli

# 2. Scaffold a new project (a working app: ORM, admin, REST, auth, OpenAPI, security)
umbral startproject myapp
cd myapp

# 3. Apply migrations and start the dev server (the first run does both)
cargo run -- serve

# 4. In another shell, create an admin user, then open http://127.0.0.1:8000/admin/
cargo run -- createsuperuser
```

The generated app already wires up, out of the box:

| Path | What it is |
|---|---|
| `/` | A server-rendered page |
| `/api/post/` | JSON CRUD via the REST plugin, with query-string filtering (`?published=true`) |
| `/admin/` | Auto CRUD admin UI |
| `/openapi/` | Swagger UI |

Add a new plugin (an "app") to your project at any time:

```bash
umbral startapp blog
```

Project commands run through the project binary with `cargo run -- <command>`: `serve`, `migrate`, `makemigrations`, `showmigrations`, `createsuperuser`, `inspectdb`, the task `worker`, and more. Run `cargo run -- --help` to list them all.

## Adding umbral to an existing project

Most apps depend on the `umbral` facade plus the plugins they want:

```toml
[dependencies]
umbral = "0.0.1"          # the facade: ORM, migrations, routing, the plugin system
umbral-auth = "0.0.1"     # add the built-in plugins you need
umbral-rest = "0.0.1"
umbral-admin = "0.0.1"
```

```rust
use umbral::prelude::*;
```

Install the CLI separately with `cargo install umbral-cli`.

## What you get

- **A typed ORM with managed migrations.** Declare or change a model, an autodetected migration is generated, `migrate` applies it. The declare -> migrate -> change -> migrate cycle is the everyday loop.
- **One model, many surfaces.** A single model declaration drives the database schema, the JSON REST API, the admin UI, and the OpenAPI document.
- **Thin core, plugin-heavy.** Auth, sessions, admin, tasks, and REST are all plugins, structurally identical to a third-party one. An app that doesn't use REST compiles with zero serializer code.
- **Porting on-ramp via `inspectdb`.** Point umbral at an existing Postgres database and it generates models plus an initial migration that drops straight into the managed-migration loop.
- **The easy path is the safe path.** Nullable columns are `Option<T>`, errors are `Result`, backend mismatches fail at boot, and SQL is always parameterized. CSRF, secure cookies, and HTML autoescaping are on by default.
- **Stand on shoulders.** axum, sqlx, sea-query, and tower do the heavy lifting; umbral provides the conventions and the integration, not a reimplementation of HTTP, async, SQL, or JSON.

## Documentation

The docs site is at **https://dalmasonto.github.io/umbral/** (source in `documentation/`, a SvelteKit + Specra site; run locally with `cd documentation && yarn dev`).

## Working in this repository

This is a multi-purpose tree, not a single cargo project:

| Path | Purpose |
|---|---|
| `crates/` | The framework crates: `umbral` (the public facade), `umbral-core` (internals), `umbral-macros` (proc macros), `umbral-cli` (the `umbral` binary), plus support crates. |
| `plugins/` | The built-in plugins, each its own `umbral-*` crate that depends only on the facade. |
| `examples/` | Standalone apps that path-dep the local umbral (not workspace members), so each one exercises the framework exactly as a downstream consumer would. |
| `documentation/` | The user-facing docs site (SvelteKit + Specra). |
| `docs/specs/`, `docs/decisions/` | Per-subsystem design specs and ADR-style decision notes. |
| `arch.md` | The authoritative architecture spec. |
| `CLAUDE.md` | The working-in-the-codebase guide for contributors. |

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for setup, the architecture rules, and the release flow.

## License

Dual-licensed under MIT OR Apache-2.0. See [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE).
