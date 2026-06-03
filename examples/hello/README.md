# umbra-hello

The smallest umbra app: two routes, default settings, default sqlite pool, served on `127.0.0.1:3000`. This is the floor — everything an M0 user needs and nothing more.

## Run it

```bash
cd examples/hello
cargo run
```

In another shell:

```bash
curl http://127.0.0.1:3000/
curl http://127.0.0.1:3000/settings
```

The first call returns `hello from umbra-hello`. The second returns a JSON blob with the loaded `database_url` and `environment`. `secret_key` is deliberately omitted.

## What it demonstrates

Every umbra symbol in `src/main.rs` comes through the facade. There is no `umbra_core::` or `umbra_macros::` anywhere in the file.

- `Settings::from_env()` to load settings with the documented defaults → toml → env precedence.
- `umbra::db::connect(url)` to open a sqlite pool.
- `App::builder().settings(...).database("default", pool).routes(...).build()` to wire everything up.
- The `Routes::new().get("/", root).get("/settings", settings_view)` builder from `umbra::prelude::*` — each per-method call records the path AND registers the handler in one shot.
- `app.serve(addr)` to bind a listener and run the server.

The point of the example is structural, not behavioural: it proves the facade is complete enough that a downstream user gets a working app without ever reaching past `umbra::*`.

## What it doesn't yet demonstrate

This is M0. So no models, no `#[derive(Model)]`, no QuerySet, no migrations, no plugins, no admin, no tasks, no REST. Each of those lands as its own milestone (see `arch.md §7`). When they do, this example stays as the baseline and new examples will sit alongside it showing one feature at a time.

## Workspace note

`examples/hello/` is its own Cargo project with its own `Cargo.lock`. It is intentionally not a member of the umbra workspace under `crates/Cargo.toml`. That's what makes it a real downstream-consumer smoke test: a missing facade re-export breaks here, not silently inside the workspace.
