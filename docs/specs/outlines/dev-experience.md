# Outline — Dev experience

| | |
|---|---|
| **Status** | Outline. Promotes at M13 (polish). |
| **Maps to milestone** | M13 |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `06-migration-engine.md`, outlines `web-layer.md`, `testing.md`, `arch.md §4.6` |

## Purpose

`umbral-cli` ships the scaffolding, dev server, and rich error pages that make "the day in the life" of an umbral developer feel like Django's — `umbral startproject myblog && cd myblog && umbral runserver` should produce a running app in under a minute, and once that app is running, an unhandled error in a handler should land in the browser as a useful HTML traceback rather than a bare 500. None of this is hard once the rest of the framework is solid; all of it is high-leverage for adoption. It belongs at M13 because the generators emit a workspace whose shape isn't stable until the Plugin contract (M7), the migration engine (M5), and the built-ins re-expressed as plugins (M8) have settled — generating code against a moving target is wasted work, and a rich error page can't render span context until the routes, ORM, and middleware chain it inspects are real.

Scope, as fixed in the spec-set design §6: "`startproject` / `startapp` generators, dev server + autoreload (cargo-watch / listenfd), rich error pages, debug-mode tracebacks."

## Key concepts

### `startproject` and `startapp` generators

`umbral startproject <name>` writes a fresh Cargo workspace: a binary crate (`<name>-server`) with a `main.rs` that calls `App::builder().settings(Settings::from_env()?).plugin(...).build()?`, an example plugin crate (`<name>-blog`) registered in the binary, an `umbral.toml` matching `Settings`'s file layout from `01-app-and-settings.md`, a `migrations/` folder per plugin, and a `README.md` with the first commands to run. It mirrors `django-admin startproject` — the user gets a working app, not a tutorial. `umbral startapp <name>` adds a new plugin crate inside an existing workspace and wires it into the binary's `App::builder()` chain by patching `main.rs` (an open question: AST edit vs marker comment). Both generators are subcommands of `umbral-cli`, not a separate `cargo-umbral` binary; the user installs one tool.

### Dev server with autoreload

`umbral runserver` is `App::serve` with two wrappers: it runs only when `Settings.environment == Environment::Dev` (or with `--force`), and it integrates with `cargo-watch` to recompile on source edits. `listenfd` keeps the TCP socket alive across rebuilds so a request in flight when the rebuild trips doesn't get its connection dropped — the new binary inherits the listener fd from the watcher. The flag set mirrors Django's: `--port`, `--bind`, `--no-reload`, plus an umbral-specific `--migrate` that runs pending migrations before binding.

```rust
pub fn runserver(opts: RunserverOpts) -> Result<(), CliError> {
    let listener = listenfd_or_bind(&opts.bind)?;   // inherited across rebuilds
    let app = App::builder().settings(Settings::from_env()?).build()?;
    tokio::runtime::Runtime::new()?.block_on(app.serve_on(listener))
}
```

### Rich error pages and debug-mode tracebacks

In `Environment::Dev`, the framework installs a fallback error handler that renders an HTML page when a route panics or returns an unhandled `Err`. The page shows the route that matched, the request method and headers (with `Authorization`, `Cookie`, and any header listed in `Settings.sensitive_headers` redacted), the deserialized path/query params, the active `tracing` span stack at the error site, and — if the error is an `umbral::Error` variant carrying ORM state — the offending query and bound parameters. In `Environment::Prod` the same handler returns a plain 500 with a request id and logs the full context through `tracing` instead. The page never renders settings values; secret leakage is the failure mode that matters most.

```rust
async fn dev_error_page(req: Request, err: Error) -> Response {
    let ctx = ErrorContext::capture(&req, &err, current_span());
    Html(render_traceback(&ctx)).with_status(500)
}
```

### Migration drift banner

In Dev, the same middleware that owns the error page also runs a cheap check on every request: hash the in-memory model state and compare it to the snapshot the migration engine wrote on the last `makemigrations` (see `06-migration-engine.md`). On a mismatch, inject a thin HTML banner ("models changed since last `makemigrations` — run `umbral makemigrations`") into HTML responses. The check is gated to Dev and skipped on non-HTML content types.

## Promote-to-deep trigger

Promote at M13 entry, once M8's plugin extraction has stabilized the shape of what the generators emit and the migration engine's snapshot format is no longer churning.

## Open questions

- **`cargo-watch` integration vs a standalone reloader.** Shelling out to `cargo-watch` is the cheap path; an in-process file watcher driving `cargo build` gives better control over rebuild diagnostics and avoids a second tool in the user's PATH. Open because the cost only shows up at scale.
- **Capturing state for the error page without leaking secrets.** A redact-list (`Settings.sensitive_headers`, `Settings.sensitive_params`) is the obvious shape, but cookies, multipart bodies, and ORM bound parameters that happen to contain credentials all need a consistent rule. Open because the right default is conservative-by-policy, and we want it written down.
- **Debug toolbar.** Django's `debug_toolbar` is a beloved add-on (SQL panel, template panel, signal panel). Whether umbral ships one in-box, leaves it to a community plugin, or starts with just the rich error page is open; the panel infrastructure overlaps with the admin's introspection.
- **`startproject` packaging: `umbral-cli` subcommand vs `cargo-umbral` binary.** `cargo new` is the ecosystem norm for project bootstrap; `umbral-cli` is the norm for everything afterwards. Picking one means duplicating a small surface or splitting the user's mental model. Open because the answer depends on whether `cargo install umbral-cli` is the install path or whether we ship a single `umbral` binary.
- **What the example plugin in `startproject` should contain.** A blog model is the canonical Django tutorial choice; an empty plugin teaches less but ships less code the user has to delete. Open because the answer drives how aggressively the generator hand-holds.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (`Settings.environment` gates Dev-only behaviour; the builder phases the runserver wraps), `02-plugin-contract.md` (`startapp` emits a `Plugin` impl; `umbral-cli` extension is `Plugin::commands()`), `06-migration-engine.md` (the snapshot the drift banner compares against; `--migrate` runs the engine).
- Sibling outlines: `web-layer.md` (the rich error page is a middleware on the umbral middleware chain; the fallback handler shape lives there), `testing.md` (the test client and `runserver` share `App::serve_on(listener)` so test fixtures and dev requests exercise the same boot path), `security-defaults.md` (the redact list overlaps with what security middleware already names sensitive).
- `arch.md §4.6` (CLI / Tooling row — the source for this outline's scope).
