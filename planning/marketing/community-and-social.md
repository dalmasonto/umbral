# umbral launch kit: paste-ready community + social drafts

All drafts avoid em-dashes and en-dashes. Links: repo https://github.com/dalmasonto/umbral , docs https://dalmasonto.github.io/umbral/ , site https://umbralrs.dev

---

## 1. Hacker News (Show HN)  [you post: https://news.ycombinator.com/submit]

**Title:** Show HN: umbral, a declarative web framework for Rust

**URL field:** https://github.com/dalmasonto/umbral

**Text field:**
I have been building umbral, a declarative web framework for Rust. The idea is the Django and Rails deal in a compile-checked language: you declare a data model once and get managed migrations, a typed ORM, an admin UI, a JSON REST API, and an OpenAPI document from that single declaration.

Every capability is a plugin, including auth. The core is thin, and auth, sessions, admin, tasks, and REST are all plugins structurally identical to a third-party one, so an app that does not use REST compiles with zero serializer code. Cargo's ban on circular dependencies is what enforces that, since the core crate is not allowed to depend on the REST crate.

The part I focused on most is the migration loop - declare or change a model, it autodetects a reversible migration, and migrate applies it. inspectdb introspects an existing Postgres schema into models so you can port an existing database into the same loop.

It stands on tokio, axum, sqlx, sea-query, and tower rather than reimplementing HTTP, async, SQL, or JSON. It is early and alpha, published on crates.io. Feedback, especially where it breaks, is very welcome.

*Best time to post: a weekday around 8 to 10am US Eastern. Then stay near the keyboard for the first two hours to answer comments.*

---

## 2. r/rust  [you post: https://www.reddit.com/r/rust/submit]

**Title:** umbral - a declarative web framework where you declare a model and get migrations, an admin, REST, and OpenAPI

**Body:**
I have been building umbral, a declarative web framework for Rust. The goal is the Django and Rails experience with Rust's compile-time guarantees: one model declaration is the single source of truth for the schema, the ORM, the REST API, the admin, and the OpenAPI document.

A model looks like this:

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
}
```

From that you get an autodetected reversible migration, a typed ORM (`Brand::objects().filter(...).first().await?`), a JSON REST endpoint if you add the REST plugin, an admin page, and Swagger. Relations are types, for example `pub category: ForeignKey<Category>`.

The design bet I would most like feedback on: every capability is a plugin, including auth. The core is thin, and auth, sessions, admin, tasks, and REST are all plugins structurally identical to a third-party one. If a built-in cannot be expressed as a plugin, the plugin contract is wrong. The test is that a REST-free app compiles with zero serializer code.

Honest status - early and alpha, Postgres-first with SQLite for tests, published on crates.io under the `umbral-*` namespace (Please don't mix this with umbral_rs - an existing rust crate for proxy re-enryption crate. I wanted the name shadow, only to land on this, maybe the name might change - I will let you know). It builds on tokio, axum, sqlx, sea-query, and tower.

Prior art I have learned from and am happy to compare against in the comments: Loco and Cot. I would love to hear where this approach helps and where it does not.

Repo: https://github.com/dalmasonto/umbral
Docs: https://dalmasonto.github.io/umbral/

*Note for r/rust: engage genuinely in the comments, treat critique as the point. Avoid reposting the same text elsewhere the same day.*

---

## 3. Lobsters  [you post, needs an account; tag: rust]

**Title:** umbral - a declarative Rust web framework (declare a model, get migrations, admin, REST, OpenAPI)
**URL:** https://github.com/dalmasonto/umbral
**Tags:** rust, web
(If you post a text intro, reuse the first two paragraphs of the r/rust body.)

---

## 4. This Week in Rust  [submit via PR or the suggestion form]

TWiR takes community suggestions. Submit this as a project update line (PR to https://github.com/rust-lang/this-week-in-rust or via their suggestion issue):

> umbral, a declarative web framework (managed migrations, typed ORM, auto admin, REST, and OpenAPI from one model declaration; thin core with everything as a plugin), published an early alpha on crates.io. https://github.com/dalmasonto/umbral

---

## 5. X / Mastodon thread

1/ Rust has tokio, axum, sqlx, sea-query, and tower. What it has been missing is the Django deal: declare your data once and get migrations, an admin, and an API for free. I am building that. It is called umbral.

2/ One struct is the single source of truth. From a model with `#[derive(Model)]` you get an autodetected reversible migration, a typed ORM, a JSON REST endpoint, an admin page, and an OpenAPI document.

3/ The everyday loop is the product: declare, migrate, change, migrate. Change a model and it diffs against the last snapshot and writes the right migration. inspectdb ports an existing Postgres schema into the same loop.

4/ The bet that keeps it honest: every capability is a plugin, including auth. Sessions, the admin, tasks, and REST are plugins too, on the same trait your code uses. A REST-free app compiles with zero serializer code. Cargo's ban on circular deps enforces it.

5/ Early and alpha, Postgres-first, published on crates.io. If this is the Rust web framework you have been wanting, a star and a "this broke for me" issue both help a lot.
Repo: https://github.com/dalmasonto/umbral
Docs: https://dalmasonto.github.io/umbral/

*Hashtags for X: #rustlang #webdev . For Mastodon (fosstodon): #rustlang #rust #webdev*

---

## 6. LinkedIn

I have been building umbral, a declarative web framework for Rust.

The productivity of Django, Rails, and Laravel never came from the language. It came from one deal: declare your data once, and the framework gives you migrations, an admin, and an API without hand-writing them. umbral brings that deal to Rust, with compile-time guarantees instead of runtime hope.

One model declaration becomes the database schema, a typed ORM, a JSON REST API, an admin UI, and an OpenAPI document. Every capability is a plugin, including auth: the core is thin, and auth, sessions, admin, tasks, and REST are all plugins on the same trait your code uses. It stands on tokio, axum, sqlx, sea-query, and tower.

It is early and alpha and published on crates.io. If you work with Rust on the backend, I would value your feedback.

Repo: https://github.com/dalmasonto/umbral
