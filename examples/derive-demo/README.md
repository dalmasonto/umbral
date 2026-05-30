# umbra-derive-demo

Demonstrates `#[derive(Model)]` against a user-defined struct, served over HTTP.

## Run it

```bash
cd examples/derive-demo
cargo run
```

In another shell:

```bash
curl http://127.0.0.1:3001/
curl http://127.0.0.1:3001/articles
```

The first call returns `umbra-derive-demo`. The second returns a JSON array of two seeded `Article` rows ordered by `id`:

```json
[
  {"id":1,"title":"Deriving Model","body":"this row came back through Article::objects().fetch()","published_at":"2026-05-30T12:00:00Z"},
  {"id":2,"title":"User-defined struct","body":"no hand-written impl Model anywhere in this file","published_at":null}
]
```

The handler is the Django ergonomic the framework promises: `Article::objects().order_by(article::ID.asc()).fetch().await` with no pool parameter, no `.on(&pool)`, no `State<DbPool>` extractor. The Manager picks up the ambient pool installed by `App::build()`.

## What it demonstrates

Every umbra symbol in `src/main.rs` comes through the facade — no `umbra_core::` or `umbra_macros::` anywhere. The whole point is that a downstream user gets the full M3 surface from a single `use umbra::prelude::*;`:

- `#[derive(Model)]` — re-exported as `umbra::orm::Model` (the macro), sharing its name with the `Model` trait through Rust's separate type and macro namespaces. The prelude pulls in both at once.
- The auto-generated `TABLE` constant. The struct is `Article`, so the table defaults to `"article"` (snake_case of the struct name).
- The auto-generated `Article::objects() -> Manager<Article>` entry point. Same shape as the M1 hand-written `Post::objects()`, only the derive writes it.
- The auto-generated sibling column module `article`, populated with `article::ID`, `article::TITLE`, `article::BODY`, `article::PUBLISHED_AT` in SCREAMING_SNAKE_CASE. The handler uses `article::ID.asc()` to order results, which is the typed predicate surface from M1's column types.
- `Option<chrono::DateTime<chrono::Utc>>` mapping to a `NullableDateTimeCol`. The other M3-supported field types (`i64` → `IntCol`, `String` → `StrCol`, `chrono::DateTime<chrono::Utc>` → `DateTimeCol`) are all exercised by the struct's other fields.

The schema is no longer hand-written. The example registers `Article` with `App::builder().model::<Article>()` and runs the M5 migration engine in-process on startup (`umbra::migrate::make()` writes `migrations/app/0001_create_article.json` on first run; `umbra::migrate::run()` applies it). Re-runs are no-ops. The seed `INSERT` doesn't supply an `id` value — SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT` (the shape the migration engine renders for `i64` PKs) hands out monotonically increasing ids.

The auto-migrate-on-startup pattern is demo-only. Production deployments run `cargo run -p umbra-cli -- migrate` as a separate step so schema changes can be reviewed before they touch the request-serving path.

`Manager::create` (which would retire the raw `INSERT`) is still deferred to a later milestone; the seed uses bound `sqlx::query` for now.

## Templates

The example also exercises `umbra::templates` (the M11-promoted, minijinja-backed substrate). The `templates/` directory holds:

- `base.html` — layout, navigation, footer, all `{% block %}` slots.
- `home.html` — child template at `/` showing the article count.
- `articles_list.html` — child template at `/articles` iterating with `{% for %}` and a `{% if article.published_at %}` conditional.
- `article_detail.html` — child template at `/articles/:id`.
- `not_found.html` — rendered with `404` when an id doesn't match.

The builder method `App::builder().templates_dir(...)` points at the directory. The example uses `concat!(env!("CARGO_MANIFEST_DIR"), "/templates")` so the path is correct no matter where `cargo run` is invoked from. Autoescape is on by default for `.html` templates: a `<script>` value rendered into the page comes out as `&lt;script&gt;`, which is the XSS guarantee from `arch.md §4.5`.

The JSON view at `/api/articles` is still there — same QuerySet, just a different response shape. The point is to show templates and JSON coexisting cleanly.

## Compare with examples/hello/

`examples/hello/` is the M0 floor: settings, default pool, two hand-written routes, no models. It doesn't use the ORM at all.

`examples/derive-demo/` adds the M3 derive on top of an M0 base: same `App::builder()` shape, same facade-only imports, plus a user-defined `Article` struct with `#[derive(Model)]`. Hello uses the built-in `Post` fixture model from `umbra::orm` only by way of comparison; this example proves the derive works on a struct the user defines, which is the realistic downstream case.

## Workspace note

`examples/derive-demo/` is its own Cargo project with its own `Cargo.lock`. Like `examples/hello/`, it is intentionally not a member of the umbra workspace under `crates/Cargo.toml`. That's what makes it a real downstream-consumer smoke test: a missing facade re-export or a regressed derive breaks here, not silently inside the workspace.
