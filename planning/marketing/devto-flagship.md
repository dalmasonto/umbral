The reason Django, Rails, and Laravel made people productive was never the language. It was the deal they offered: declare your data once, and the framework hands you migrations, CRUD, an admin UI, and an API, without you writing any of that by hand.

Rust has all the raw materials for that deal. tokio for the async runtime, axum for routing, sqlx for the database, sea-query for SQL, tower for middleware. What it has been missing is the wiring that turns those into "declare a model and get everything." That wiring is what I am building, and it is called umbral.

## The pitch in one struct

Here is a real model from the example shop app:

```rust
use umbral::prelude::*;

#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize, Model)]
pub struct Brand {
    pub id: i64,
    #[umbral(unique, string)]
    pub name: String,
    #[umbral(unique)]
    pub slug: String,
    pub logo: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
}
```

That one declaration is the single source of truth. From it, umbral gives you:

- A database table and an autodetected, reversible migration. You run `cargo run -- makemigrations`, then `cargo run -- migrate`.
- A typed ORM. `Brand::objects().filter(...).first().await?` is checked at compile time, and `Option<String>` is exactly the nullable column, with no nulls sneaking past the type system.
- A JSON REST endpoint, if you install the REST plugin. Query-string filtering comes with it.
- A row in the admin UI at `/admin/`, with create, edit, and delete.
- An OpenAPI document and Swagger UI at `/openapi/`.

Relations are types, not string joins. On the richer `Product` model those look like `pub category: ForeignKey<Category>` and `pub brand: Option<ForeignKey<Brand>>`. Field behavior is attributes you can read at a glance: `#[umbral(unique)]`, `#[umbral(choices)]` on an enum, `#[umbral(default = "0")]`, `#[umbral(auto_now_add)]`.

## The everyday loop is the product

The thing I care about most is the change loop, because that is where a lot of ORMs quietly fall apart:

```
declare -> migrate -> change -> migrate
```

You declare or change a model. umbral diffs it against the last migration snapshot and writes an ordered, reversible migration: create table, add column, alter column, drop. `migrate` applies what is pending. If you already have a Postgres database, `inspectdb` introspects it into models plus an initial migration, so an existing schema drops straight into the same loop.

Existing rows are treated as the test, not an obstacle. A UNIQUE you add trips on real duplicates. A new NOT NULL asks for a default. That friction is the point: it shows up in development instead of in production.

## Every capability is a plugin. Including auth.

This is the architectural bet that keeps the framework honest. Auth, sessions, admin, tasks, and REST are all plugins. Structurally they are identical to a third-party plugin. A plugin can contribute models (which become migrations), routes, middleware, admin registrations, settings, and lifecycle hooks. The core defines the `Plugin` trait and never names a concrete plugin.

The proof is a rule the codebase enforces: an app that does not use REST compiles with zero serializer code. If a built-in cannot be expressed as a plugin, the plugin contract is wrong. Cargo's ban on circular dependencies is what enforces "serializers are a plugin," because `umbral-core` is not allowed to depend on the REST crate.

## Secure and typed by default

- SQL is always parameterized.
- CSRF, secure cookies, and HTML autoescaping are on by default.
- Backend mismatches fail at boot, not in production. A Postgres-only field on SQLite is a clear startup error.
- Errors are `Result` values, with `?` flowing through a framework error enum.

## Try it

```bash
cargo install umbral-cli
umbral startproject myapp
cd myapp
cargo run -- serve
```

The generated app already serves a page at `/`, JSON CRUD at `/api/post/`, the admin at `/admin/`, and Swagger at `/openapi/`.

## Examples and real-world use

- A full runnable example lives in the repo: [examples/shop](https://github.com/dalmasonto/umbral/tree/main/examples/shop), an e-commerce app that exercises models, relations, the admin, and REST.
- The project site itself, [umbralrs.dev](https://umbralrs.dev), is built with Umbral.
- [pipeline.supercodehive.com](https://pipeline.supercodehive.com/) is a live site built on Umbral. It captures streamed price values from Chainlink for Polymarket, so you can follow the opening and closing prices of different crypto market types.

## Honest status

umbral is early and alpha. It is published on crates.io under the `umbral-*` namespace, so start with the `umbral` facade. APIs will still move before 1.0. Postgres is the first-class production backend, and SQLite is for tests. It stands on tokio, axum, sqlx, sea-query, and tower rather than reimplementing the async runtime, HTTP, SQL, or JSON.

If "declare your data and get everything" in a compile-checked language sounds like something you have wanted in Rust, I would love your eyes on it.

- Repo: https://github.com/dalmasonto/umbral
- Docs: https://dalmasonto.github.io/umbral/
- Site: https://umbralrs.dev

A star on the repo genuinely helps, and a "this broke for me" issue helps even more.

---

*Disclosure: this post was co-authored with an AI assistant (Claude) and reviewed by Dalmasonto for correctness and usability. The framework, the examples, and the direction are the author's.*
