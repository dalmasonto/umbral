# Contributing to umbral

Thanks for considering a contribution! umbral is a Django-inspired web framework in Rust — declare your data and get migrations, CRUD, an admin, and an optional REST API almost for free, with compile-time guarantees. This guide covers how to get set up, the conventions we hold to, and how changes land.

If anything here is unclear or out of date, open an issue — improving this doc is itself a welcome contribution.

## Code of conduct

Be kind, be constructive, assume good faith. Harassment or hostility isn't welcome. Maintainers may remove comments, commits, or contributors that violate this in spirit.

## Licensing of contributions

umbral is dual-licensed under [MIT](LICENSE-MIT) **OR** [Apache-2.0](LICENSE-APACHE). 

> Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in the work (as defined in the Apache-2.0 license) shall be dual-licensed as above, without any additional terms or conditions.

The **name** "umbral" and the project's branding are covered separately — see [TRADEMARK.md](TRADEMARK.md). Short version: the code is yours to fork and ship; the name stays pointing at the official project.

## Project layout

The Cargo workspace is rooted at the **repo-root `Cargo.toml`** and spans `crates/*` (the framework) plus `plugins/*` (the built-in plugins). `examples/`, `umbral_website/`, and `documentation/` are excluded and stay standalone projects, so the tree is still multi-purpose. `cargo` commands run from the repo root.

```
crates/        # the framework workspace
  umbral-core     # ORM, migrations, routing, DB backends, the Plugin trait
  umbral-macros   # #[derive(Model)], #[task], …
  umbral          # the facade: `use umbral::prelude::*;` — the only stable surface
  umbral-cli      # the `manage.py` equivalent
  umbral-casing / umbral-testing  # support crates
plugins/       # built-in plugins, each its own crate (auth, admin, rest, tasks, …)
examples/      # standalone apps that path-dep the local umbral (NOT workspace members)
documentation/ # user-facing docs site (SvelteKit + Specra, MDX)
umbral_website/ # the project's own marketing/community site (an umbral app)
```

**Architecture in one line:** dependencies point inward toward `umbral-core`; control flows outward through the `Plugin` trait. `umbral-core` never names a concrete plugin. The full design rationale is in [`arch.md`](arch.md) — read it before any substantial change.

## Getting set up

You need a recent stable Rust (the workspace pins `rust-version = "1.85"`, edition 2024). Postgres is the first-class backend; SQLite is used for tests.

```bash
cargo build                      # build all workspace crates
cargo test                       # run all tests
cargo test -p umbral-core         # test a single crate
cargo test <test_name>           # run one test by name
cargo run -p umbral-cli -- <cmd>  # migrate, makemigrations, worker, inspectdb, …
cargo clippy --all-targets       # lint
cargo fmt                        # format
```

For an example app (outside the framework workspace):

```bash
cd examples/<name> && cargo build
```

`sqlx::query!` compile-time checks need either a live `DATABASE_URL` or the prepared `.sqlx` offline cache.

## The rules that matter most

These come straight from how the framework is designed. PRs are checked against them.

- **Thin core, plugin-heavy.** If a built-in capability can't be expressed as a plugin, the plugin contract is wrong — fix the contract, don't special-case the core. `umbral-core` must never depend on a plugin crate (Cargo's ban on circular deps enforces this).
- **Plugins use the ORM, not raw SQL.** Every row-level read/write goes through the QuerySet API (`Model::objects().filter(...)…`), which emits the right SQL per backend. The only sanctioned raw SQL is schema DDL (owned by the migration engine) and backend-specific features the ORM can't model (gated per backend). A PR touching a plugin gets grepped for `sqlx::query` / `sqlx::query_as`; new hits need a justifying comment.
- **Migrations are sacred.** Never delete a migration file or wipe a database to "get a clean run." The `declare → migrate → change → migrate` loop is the product; existing rows are the test, not an obstacle. Use `makemigrations` then `migrate`.
- **Fix the root cause, don't patch the symptom.** No defensive guards, swallowed errors, `#[allow(unused)]`, or `_`-prefixed variables that hide a missing piece of the framework. If the proper fix is out of scope, log a tracker entry (see below) and leave a `// TODO(gaps2 #N)` breadcrumb pointing at where the real fix belongs.
- **Secure by default.** CSRF, security headers, template autoescaping, always-parameterized SQL — don't regress these.
- **Ship a feature, ship its doc page.** When you add something a user writes code against, add a minimal page under `documentation/docs/v0.0.1/<area>/` in the same PR (purpose + one example + a link to the spec). Don't translate specs into docs — link to them.

## Commit conventions

**One feature, one fix, one commit.** Squash WIP commits before opening a PR.

Commit message format:

- First line ≤ 72 chars, imperative, with an optional `<type>(<scope>):` prefix.
- Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `perf`.
- Scopes: a crate/area name (`orm`, `migrate`, `admin`, `website`) or `workspace` for cross-crate.
- The body explains *why*, not what.

```
feat(orm): add F-expression support to QuerySet
fix(migrate): handle nullable column rename safely
```

These messages feed the changelog automatically at release time (see below), so they're worth getting right.

## Before you push — verify the whole workspace

A change in `umbral-core` can silently break the facade's re-exports, so verify everything, not just the crate you touched:

```bash
cargo fmt
cargo clippy --all-targets
cargo build
cargo test
```

If any step fails, fix it or back the change out. Never use `--no-verify` to skip hooks.

## Opening a pull request

1. Fork and branch off `main`.
2. Make your change, following the rules above. Keep the PR focused — one logical change.
3. Add/update the user-facing doc page if you added a user-visible feature.
4. Run the full verify suite.
5. Open the PR with a clear description of *why*. If it closes a tracker entry or a spec open question, name it (e.g. `Closes gaps2 #98`).

Maintainers may ask for changes — that's normal and not personal. For large or cross-cutting work (cross-crate refactors, public type renames, anything that forces a downstream plugin to change), open an issue to discuss before writing a lot of code.

## Issue tracking and the gap backlog

Design gaps and deferred work are tracked under `planning/` (`gaps.md`, `gaps2.md`, `features.md`). Entry numbers are stable identifiers — commits and code comments cite them as `gaps2 #N`. If you find a gap you can't fix in your PR, append a new entry (number = current max + 1) rather than patching around it silently.

## How releases work (maintainers)

Releases are automated with [release-plz](https://release-plz.dev) and run **manually** from the Actions tab (`release-plz` workflow), not on every push. All crates share one **unified version** (`version_group = "umbral"` in `release-plz.toml`). The flow:

1. Dispatch `release-plz` with `command = release-pr` → it opens a single PR bumping every crate to the next version and updating changelogs from your commit messages.
2. Review and merge that PR.
3. Dispatch `release-plz` with `command = release` → it publishes every changed crate to crates.io in dependency order and tags the release.

Contributors don't need to touch versions or changelogs — write good commit messages and the release machinery does the rest.

---

Questions? Open an issue or email **dalmasogembo@gmail.com**. Happy hacking.
