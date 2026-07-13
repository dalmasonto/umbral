# umbral

A batteries-included web framework for Rust. Declare your data and you get migrations, an admin, CRUD, and an optional REST API, backed by Rust's compile-time guarantees.

The name "umbral" means "of the shadow" (from the Latin umbra, shadow).

## Install

```bash
cargo add umbral
```

```rust
use umbral::prelude::*;
```

`umbral` is the facade crate. One dependency brings in the ORM, the derive macros, routing, and the plugin system. Add the built-in plugins you want alongside it, such as `umbral-auth`, `umbral-rest`, `umbral-admin`, or `umbral-tasks`.

## What you get

- A typed ORM with managed migrations: declare a model, generate the migration, apply it.
- Routing and request handling built on axum.
- A plugin system where auth, sessions, admin, tasks, and REST are all plugins.
- Postgres first, with SQLite for tests.

## Documentation

- Guide and reference: https://dalmasonto.github.io/umbral/
- Repository: https://github.com/dalmasonto/umbral
- API reference: https://docs.rs/umbral

## License

Licensed under either of MIT or Apache-2.0, at your option.
