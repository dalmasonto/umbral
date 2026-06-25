# Outline — Testing

| | |
|---|---|
| **Status** | Outline. Promotes at M9 entry. |
| **Maps to milestone** | M9 |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `03-orm-querysets.md`, `06-migration-engine.md`, outlines `web-layer.md`, `auth-and-sessions.md`, `tasks.md` |

## Purpose

`umbral::test` is the surface tests reach for so they exercise the same framework the app does. It ships a test client that mounts the App's real router, a task-local pool override that scopes a test future against an isolated database, a transactional `TestCase` analog that rolls back the world between tests, and a small factories layer that produces saved models without each test re-deriving its fixtures. The thing that matters most is the *symmetry*: tests build an `App` through `App::builder()`, drive routes through `Client`, and read through `Post::objects()` — the same surfaces a handler uses in production. The framework's invariants (the system check at boot, the plugin contract, the ambient-pool resolution rule) are *exercised by tests too*, so a green test suite is also evidence that the framework's contracts hold. The client is intentionally thin: a wrapper over `axum-test`, not a reimplementation. The value is the framework-aware shortcuts on top — auth helpers, factory integration, transactional isolation — that bare `axum-test` can't know about because they live in umbral plugins.

## Key concepts

### `umbral::test::Client` — the test client

`Client` wraps `axum-test::TestServer` against the router produced by `App::builder().build()?`. It exposes the verb methods directly (`.get(path)`, `.post(path).json(...)`, `.form(...)`, `.multipart(...)`) and returns a typed `Response` with helpers (`.status()`, `.json::<T>()`, `.header(name)`, `.cookies()`). Framework-aware shortcuts are what justifies the wrapper: `client.login(&user)` sets up a session through `umbral-auth` and `umbral-sessions` rather than fabricating a cookie by hand; `client.as_user(&user, async { ... })` scopes a logged-in identity for a block; `client.assert_no_n_plus_one()` instruments the ambient pool to count queries through a span.

```rust
let app = App::builder().settings(test_settings).plugin(AuthPlugin::default()).build()?;
let client = umbral::test::Client::new(app);
let resp = client.post("/posts").json(&NewPost { title: "x".into() }).send().await?;
assert_eq!(resp.status(), 201);
```

### `umbral::test::with_pool` — task-local pool scoping

The task-local override designed in `01-app-and-settings.md` §Test override is re-exported here as the surface tests reach for. This outline owns *how it appears in test code*; the deep spec owns the mechanism (the `tokio::task_local!` plus the accessor's fall-through to the `OnceLock`). A test that exercises a handler reading the ambient pool wraps the call:

```rust
umbral::test::with_pool(test_pool.clone(), async {
    let resp = client.get("/posts").send().await?;
    assert_eq!(resp.json::<Vec<Post>>().await?.len(), 3);
}).await
```

For ORM-only tests, `Post::objects().on(&test_pool)` (from `03-orm-querysets.md`) is the more direct route; `with_pool` is for code that itself reaches for the ambient pool (handlers, plugin internals, signals).

### Transactional `TestCase`

A `TestCase` opens a savepoint at setup and rolls back at teardown. On Postgres — the default — that means every test starts in a known state without truncating tables or re-running migrations. SQLite, used for CI parity and quick local runs, falls back to a delete-all-rows path because nested transactions are weaker; the system-check from `05-backends-and-system-check.md` is what tells `TestCase` which path to take. Tests that themselves open transactions get nested savepoints; the deep spec pins down "savepoints all the way down" and what happens when application code commits (the outer savepoint still rolls back, so production-style commits look like no-ops from outside the test).

### Factories

A `Factory<T>` trait standardises "produce a `T` for a test." `build()` returns an in-memory instance with sensible defaults; `create()` inserts it through the ambient ORM and returns the saved row with its PK populated. Realistic data comes from `fake-rs`; overrides are passed as a struct-update literal.

```rust
impl Factory<Post> for PostFactory {
    fn build(&self) -> Post { Post { id: 0, title: Fake.fake(), body: Fake.fake(), .. } }
    async fn create(&self) -> Result<Post> { Post::objects().create(self.build()).await }
}

let post = PostFactory::default().with(|p| p.title = "fixed".into()).create().await?;
```

### Fixtures

JSON or RON files of pre-baked rows live under `fixtures/` and load through `cargo run -p umbral-cli -- loaddata <name>`. Useful for integration tests that need stable seed data (a known admin user, a set of categories) without per-test factory ceremony. Fixtures share the migration tracking table's view of the schema, so a fixture for an old model shape fails loudly rather than corrupting the database.

### `RequestFactory` — handler-level unit tests

The lower-level companion to `Client`: build a `Request` directly and pass it to a handler without routing. Used for testing extractors and small handlers in isolation, where the routing layer and middleware chain are noise. Reuses the same `umbral::web::Request` type the runtime sees.

## Promote-to-deep trigger

Promotes at M9 entry, when `umbral-tasks` and the re-expressed auth/sessions plugins need real integration tests to prove the contract from `02-plugin-contract.md` holds end-to-end.

## Open questions

- **Factory derive shape.** A `#[derive(Factory)]` macro that picks defaults from field types vs. hand-written `Factory<T>` impls. Derive is ergonomic but commits the framework to a defaults catalogue (random strings? sequential integers? `fake-rs` providers per type?); hand-written is verbose but transparent. Resolve once enough built-in plugin tests have been written to see the duplication.
- **Transactional `TestCase` vs application-opened transactions.** Savepoints-all-the-way-down is the direction, but the precise semantics when application code calls `Db::tx` inside a test (does the inner savepoint commit, or just merge into the outer rollback?) need a concrete worked example to lock down.
- **Test runner integration.** Whether `umbral` ships a `#[umbral::test]` attribute that wires the runtime, the pool override, and the `TestCase` setup in one annotation, or whether the canonical pattern is `#[tokio::test]` plus explicit `umbral::test::*` helpers. The attribute is more concise; the explicit form is more transparent and composes with existing test infrastructure.
- **Fixture format choice.** JSON is universal but verbose; RON matches the Rust ecosystem and reads better for complex shapes; a `dumpdata` round-trip needs to pick one. Likely RON with JSON tolerated; settle when `dumpdata` (the inverse command) is specced.
- **Parallel test isolation.** `cargo test` runs tests in parallel by default. A per-test transaction on a shared pool serialises writes; a per-test *database* (template-cloned) keeps parallelism but pays the clone cost. Pick once a real suite is large enough to measure both.

## Cross-links

- Deep specs that constrain this: `01-app-and-settings.md` (the `test_with_pool` task-local mechanism; the ambient-pool accessor's test fall-through), `02-plugin-contract.md` (plugin-aware test setup — every plugin's `on_ready` runs inside the test `App` too), `03-orm-querysets.md` (`Manager::on(&pool)` for explicit-pool ORM tests; `Db::tx` for transaction interaction), `06-migration-engine.md` (test database schema comes from the same migrations production uses).
- Sibling outlines: `web-layer.md` (the `Client` wraps the `Router` assembled there), `auth-and-sessions.md` (the `client.login(&user)` / `as_user` helpers and the session cookie shape), `tasks.md` (testing tasks either inline-dispatched or against a transactional broker — the deep spec for tasks names the surface).
