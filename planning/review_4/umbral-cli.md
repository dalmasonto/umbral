# review_4 · umbral-cli

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | 8,412 LOC (`crates/umbral-cli`) |
| **Reviewer** | Claude (rev4 wave) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 0 high · 1 medium · 3 low · 0 info — 4 new, 10 known |

## Summary

- `crates/umbral-cli` is two artefacts: the app-embedded dispatcher (`umbral_cli::dispatch`) that hosts serve/migrate/makemigrations/worker/transferdata/inspectdb/doctor for a user binary, and the global `umbral` scaffolding binary (startproject/startplugin/startcommand). This is the first code every new user runs.
- Overall state is strong. Generated output is a real Cargo workspace with versions single-sourced via `env!("CARGO_PKG_VERSION")` + `.workspace = true`; scaffold error messages, path safety, idempotency and clobber-refusal all hold up; and the suite includes a CI-only test that `cargo check`s a generated project under `RUSTFLAGS=-D warnings` — the right bar for `startproject`.
- Biggest risk (CLI-1, MEDIUM): the generated workspace's `members = ["plugins/*"]` glob means any directory under `plugins/` without a `Cargo.toml` makes cargo refuse the *entire* project (exit 101, before anything compiles). A hand-made placeholder dir or an interrupted `startplugin` creates exactly that, and `startplugin`'s own AlreadyExists message ("move it aside") keeps the poison in place.
- Three smaller cleanups, all in generated output: the Cargo.toml hint comment names the auto-registered dep with the wrong crate name (CLI-2); two generated doc comments describe a layout the scaffolder no longer writes (CLI-3); the generated `.gitignore` discards the binary app's `Cargo.lock` (CLI-4).
- Fix order: CLI-1 first (decide explicit `members` vs glob + guard/comms), then CLI-2/3/4 as a single comment-and-template pass.
- The stray `crates/umbra-cli-demo.db` is a pre-rename artefact, not current CLI behaviour — provenance in Out-of-scope notes; do not delete.

## Findings

### CLI-1 · MEDIUM · ergonomics/reliability · self-verified
**The generated workspace's `members = ["plugins/*"]` glob means any directory under `plugins/` that lacks a `Cargo.toml` makes the whole project unloadable — and the tool's own recovery advice ("move it aside") leaves the offending directory in the glob.**

- **Where**: `crates/umbral-cli/src/scaffold.rs:502-503` (glob), `:1338-1343` (dirs created before any file), `:1412` (first write); `crates/umbral-core/src/codegen.rs:208-219` (the tolerant reader); Display at `scaffold.rs:142-146`.
- **Evidence**:
  ```toml
  [workspace]
  members = ["plugins/*"]
  ```
  ```rust
  let root = plugins_dir.join(name);
  if root.exists() {
      return Err(ScaffoldError::AlreadyExists(root));
  }

  fs::create_dir_all(&root)?;
  fs::create_dir_all(root.join("src"))?;
  ```
  ```rust
  Self::AlreadyExists(p) => write!(
      f,
      "an app already exists at `{}`; move it aside or pick a different name",
      p.display()
  ),
  ```
  and the same toolkit's plugin reader is *more* tolerant than cargo, so nothing else surface the problem:
  ```rust
  for entry in entries.flatten() {
      if !entry.path().join("Cargo.toml").is_file() {
          continue;
      }
  ```
- **Impact**: concrete scenario — user creates `plugins/notes/` by hand (or a repo carries a placeholder like `plugins/notes/.gitkeep`, which git preserves), or `umbral startplugin notes` is killed / hits ENOSPC (this machine runs at 97% disk) between `create_dir_all` (`:1342`) and the first `write_file` (`:1412`). From then on every cargo command in the project fails before compiling, with `error: failed to load manifest for workspace member .../plugins/notes referenced via plugins/* by workspace at .../Cargo.toml / Caused by: failed to read .../Cargo.toml: No such file or directory` and exit 101 — reproduced on this toolchain in a `/tmp` probe (`cargo metadata --no-deps`). The natural recovery makes it worse: rerunning `startplugin notes` hits the `AlreadyExists` arm above, whose advice is "move it aside" — renaming the directory keeps it in `plugins/*`, so the workspace stays broken; only deleting it or hand-writing a `Cargo.toml` clears the state. The failure is loud and the workaround is small once diagnosed, so this is the rubric's MEDIUM (dev-time tooling emitting non-compiling output; not silent, no data touched) — not HIGH.
- **Fix**: stop relying on the glob for membership — emit an explicit `members` list and have `startplugin` append `"plugins/<name>"` (idempotently, same string-surgery style as `ensure_dependency`), or keep the glob but (a) have `scaffold_plugin`, on `AlreadyExists` when `plugins/<name>/Cargo.toml` is missing, say that this directory currently blocks the whole workspace and how to unblock it, and (b) write `Cargo.toml` before `create_dir_all(root.join("src"))` so a failure never leaves a sourceless member.

### CLI-2 · LOW · drift (generated output)
**The generated Cargo.toml's registration hint names the dependency `<project>-posts`, but every other surface — the tool's own registration, the plugin README, and `print_report` — uses the bare plugin name `posts`; uncommenting the hint yields a cargo name-mismatch error.**

- **Where**: `crates/umbral-cli/src/scaffold.rs:605-607` (hint) vs `:2262-2266` (actual registration); `crates/umbral-cli/src/scaffold_cli.rs:313` (report); `crates/umbral-cli/src/scaffold.rs:1435-1438` (plugin README).
- **Evidence**:
  ```toml
  # Once you `umbral startplugin <plugin>`, the crate is registered here as a path
  # dep (and picked up as a workspace member via `plugins/*` above):
  # {crate_name}-posts = {{ path = "plugins/posts" }}
  ```
  at `:489`, `crate_name` is the *project* name (`rust_ident(name)`), while the registration writes:
  ```rust
  umbral::codegen::ensure_dependency(
      cargo_toml_path,
      name,
      &format!("{{ path = \"plugins/{name}\" }}"),
  )
  ```
- **Impact**: project `blog` → the hint reads `# blog-posts = { path = "plugins/posts" }`. A user who uncomments it gets `error: no matching package named 'blog-posts' found` (the package at `plugins/posts` is `name = "posts"`), on the one file every new user edits first — and the tool's real line will later sit a few lines below, disagreeing with it. Cosmetic today; it teaches the wrong dep-name shape for the next `startplugin`.
- **Fix**: drop the `{crate_name}-` prefix from the hint (render `# posts = { path = "plugins/posts" }`), matching what `startplugin` and the plugin README already say.

### CLI-3 · LOW · docs (generated output drift)
**Two generated doc comments describe a project layout the same scaffolder no longer writes: the tree omits `templates/login.html`, `styles/` and `static/`, and `views/mod.rs` calls auth-gated `dashboard` a future `account` module while `main.rs` already wires it from `public.rs`.**

- **Where**: `crates/umbral-cli/src/scaffold.rs:399-404` (tree) vs `:1246-1279` (files actually written); `crates/umbral-cli/src/scaffold.rs:811-815` (views/mod.rs doc) vs `:771`, `:778` (route table).
- **Evidence**:
  ```text
    └── templates/
        ├── base.html
        ├── home.html
        ├── dashboard.html
        ├── 404.html
        └── 500.html
  ```
  ```rust
              "templates/login.html",
  ...
              "styles/input.css",
              "styles/tailwind.config.js",
              "styles/package.json",
  ...
              "static/css/app.css",
              "static/favicon.svg",
  ```
  and the views doc:
  ```rust
  //! Submodules:
  //!   - `public` — pages anyone can hit (home, JSON listings).
  //!
  //! Add `pub mod account;` here when auth-gated views land (dashboard,
  //! /me, staff-only pages)
  ```
  while `main.rs` already contains `.get(views::public::dashboard).layer(login_required_html("/login"))`.
- **Impact**: these comments ship inside every generated project as the authoritative map of its own layout. A user reading `views/mod.rs` is told dashboard views don't exist yet and adds a redundant `account` module; the tree omission makes the generated `static/` (which `StoragePlugin` serves, and whose presence the SecurityPlugin CSP story in Cargo.toml depends on) look accidental.
- **Fix**: generate the tree block from the same file list the writer loop already iterates (single source), and reword the `views/mod.rs` paragraph to describe what exists (`public.rs` holds public + auth-gated handlers; split into `account` when it grows).

### CLI-4 · LOW · hygiene (generated output)
**The generated `.gitignore` ignores `Cargo.lock` for a binary application, so a scaffolded project's dependency graph is not reproducible across clones — against Cargo's binary-vs-library guidance and this repo's own convention.**

- **Where**: `crates/umbral-cli/src/scaffold.rs:1122`.
- **Evidence**:
  ```rust
  let gitignore = format!("/target\n/{name}.db*\n.env\nCargo.lock\n");
  ```
- **Impact**: the scaffolded thing is an app (`[[bin]]` via `src/main.rs`), and for binaries the lockfile is the only record of the resolved transitive graph — `umbral` itself is pinned exactly by 0.0.x caret semantics, but axum/sqlx/minijinja and friends are not, so a teammate's fresh clone or CI run can resolve different patch versions with no diff visible. It also makes `umbral doctor`'s Cargo.lock scan (`scaffold.rs:552` advertises it) diagnose a different graph on each machine. The framework repo itself commits its lock (CLAUDE.md), so the generated convention contradicts the parent project's.
- **Fix**: drop the `Cargo.lock` line from the generated `.gitignore` (keep it only if a library-flavoured preset is ever added).

## Strengths

- **The scaffold's correctness bar is a compiler, not a grep.** `tests/scaffold_compiles.rs` scaffolds a project (and a member plugin) and hands it to `cargo check` with `RUSTFLAGS=-D warnings`, with a header documenting why (gaps3 #64, #57 and an unused import all shipped in generated output that string-assertion tests passed). That is exactly the right test for "the first thing every new user runs", and CI runs it on every push.
- **Error surface is better than most CLIs.** `ScaffoldError`'s `Display` impls name the fix and the alternatives (`ReservedName` lists every reserved name; `NoSuchPlugin` lists `root` plus the plugins actually present; `AlreadyExists` tells you to move it aside), and `startplugin help` / `-help` are normalized to real usage rather than scaffolding a crate literally named `help` (`scaffold_cli.rs:183-206`) — a footgun fixed at the right level.
- **Idempotency and no-clobber are layered.** Scaffolders refuse an existing root; `write_new_file` refuses to overwrite; `ensure_dependency` is idempotent and its comment (`scaffold.rs:2255-2271`) documents the two shipped bugs it replaced (dev-dependencies false-positive, `[dependencies.<name>]` table form).
- **Version single-sourcing is structural.** Every version in generated output comes from `env!("CARGO_PKG_VERSION")` (0.0.13 at this commit), and workspace members inherit via `.workspace = true` (gaps4 #90), so the sqlx-divergence class is dead by construction rather than by test.
- **Dispatch integrity.** `try_run_scaffold` is one implementation shared by both entry points, intercepted before `app.ready()` so a scaffold run never fires plugin `on_ready` hooks (`scaffold_cli.rs:1-22`); the destructive-migration gate is tested to refuse before any SQL and with nothing recorded in `umbral_migrations`.
- **Tests drive real paths.** `global_help_cli.rs` spawns the actual compiled binary; `migrate_destructive_gate.rs` asserts *no row* was applied; `version_pins.rs` keeps READMEs on `cargo add` and docs pinned to the current version — behavioral tests, not string archaeology (except where strings are the product).

## Clean areas checked

- **Path safety / traversal**: `validate_name` → `umbral::codegen::validate_ident` rejects empty, leading-digit, non-alphanumeric (so `/`, `\`, `.`, `..`) and Rust keywords; `resolve_target` validates *before* `join` with an explicit comment about absolute-path/`..` escape. `--path` is a user-chosen destination by design, not an injection surface.
- **Idempotency (run twice)**: second `startproject`/`startplugin` → `AlreadyExists`, never a partial overwrite; plugin dep registration returns "already lists it" without duplicating; `startcommand` markers make rewiring idempotent and `Wiring` reports only completed edits.
- **Bare-run semantics**: the generated README's "a bare `cargo run` auto-migrates AND seeds" claim verified true at this commit — `serve()` runs `auto_migrate` (`lib.rs:1058`) and `seed_on_serve` (`lib.rs:1086`) when the app was built with them; the old audit_2 #5 stale claim is fixed.
- **`builtin_needs_ready` invariant**: migrate/makemigrations/checkmigrations/squash/show/inspectdb/typegen/maskkeygen/doctor/dev/help (+ bare) are correctly marked as not requiring a built app; everything else readies it — matches the scaffolding intercept design.
- **Panics**: no `unsafe` anywhere in `src/`; no `unwrap`/`expect` outside `#[cfg(test)]` modules — the only non-test hits (`scaffold.rs:2187-2190`) are inside a `format!` raw string that *generates* command-skeleton code, where clap's guarantees hold.
- **Raw SQL**: no `sqlx::query` row-level hits in `src/` beyond the migration engine's own DDL path (cli is that engine's frontend); the Cargo.toml sqlx dependence is for the migration/seed paths, not row CRUD (CLAUDE.md's ORM rule constrains `plugins/`).
- **Secrets**: `random_dev_secret_key` generates a fresh 64-hex key per scaffold; the dev-admin seed is gated to `Environment::Dev` + opt-in `UMBRAL_DEV_ADMIN_PASSWORD` with no hardcoded password (audit_2 #1 fixed).
- **Templates**: generated HTML uses only minijinja-supported constructs (`!=`, loops, extends — engine confirmed at `umbral-core/src/templates.rs:1-59`); no `__PROJECT__`/`__DOCS__`/`__INITIAL__` placeholders leak (asserted by tests); base.html links the compiled bundle, not the Tailwind CDN (audit_2 #3's deferred fix shipped as removal per the storage/CSP comment).
- **Version pins**: `version_pins.rs` holds READMEs to `cargo add` (no stale literal versions) and docs manifests to the current version; generated manifests pin 0.0.13 everywhere via `CARGO_PKG_VERSION`.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps3 #63` — generated `main.rs` matched on `Environment` without `PartialEq`; shipped broken in 0.0.6 (fixed; guarded by the CI compile test).
- `gaps3 #65` — scaffold pinned whatever version was last released / sqlx drift (closed via gaps4 #90).
- `gaps3 #83` — `ScaffoldReport`/`ScaffoldError` vs `umbral::codegen` error types; `cargo_toml_registered: Option<bool>` overloads "not asked" with "not found" (open; the comment at `scaffold_cli.rs:306-309` concedes it).
- `gaps3 #84` — `RESERVED_PLUGIN_COMMAND_NAMES` hardcoded in the plugin-agnostic CLI crate; a third-party plugin's command is shadowed with only a `tracing::warn!` (open).
- `gaps5 #76` (tf#289) — same item as gaps3 #84, restated (open).
- `gaps4 #65` — sqlx-version divergence: doctor's Cargo.lock scan + scaffold pin (shipped).
- `gaps4 #90` — workspace scaffold, single-sourced `[workspace.dependencies]` (shipped; CLI-1 is the untracked residual of this design).
- `gaps4 #66` — scaffolders listed in the unified help but not dispatchable from the embedded surface (shipped; `try_run_scaffold`).
- `features.md #5` — scaffold Cargo.toml lists every built-in plugin up front (shipped).
- `audit_2 core-macros-cli #1/#3/#4/#5/#7` — seed credentials, Tailwind CDN, maskkeygen stdout warning, README `serve` claim, dev secret key (fixed/deferred as noted in that file).

## Out-of-scope notes

- `crates/umbra-cli-demo.db` — pre-rename artefact: introduced when the binary was `umbra` (commit `95f0709e` era, demo default `sqlite://umbra-cli-demo.db?mode=rwc`); no current source references `umbra-cli-demo`; ignored by root `.gitignore:17` (`*.db`); **not** produced by today's CLI. Safe to leave or delete manually — not a CLI behaviour to fix.
- `umbral::codegen` primitives (`validate_ident`, `resolve_target`, `write_new_file`, `ensure_dependency`, `discover_plugins`, `prompt`) live in `crates/umbral-core/src/codegen.rs` — contract issues there belong to the core reviewer; CLI-1 cites `discover_plugins` only as a tolerance contrast.
- `tests/scaffold_compiles.rs` runs only in CI (`--ignored`, `.github/workflows/scaffold.yml`) — a local `cargo test` never builds generated output; flows to the CI/test-infra reviewer.
- `plan_plugin_add` / static catalog seams used by the CLI live in `crates/umbral-core/src/cli.rs` — route to the core reviewer.
