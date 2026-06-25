# Outline — Web layer

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at the trigger below. |
| **Maps to milestone** | M7 (entry) / M9–M11 (built-in extractors) |
| **Companions** | `arch.md §2.1`, `arch.md §2.2`, `01-app-and-settings.md`, `02-plugin-contract.md`, outlines `auth-and-sessions.md`, `forms.md`, `static-and-media.md`, `security-defaults.md` |

## Purpose

`umbral::web` is the user-facing HTTP shape: the types, extractors, and middleware contract every handler and every plugin author sees. It owns the boundary that hides axum behind umbral-named types - one ergonomic surface on top, axum and tower underneath - so a plugin's `routes()` and a user's hand-written route both read as umbral code, never as axum code. The web layer is deliberately *not* a deep spec yet: M0 ships one hand-written route as an escape hatch via `App::builder().router(...)`, and the concrete shape of `Router`, `Request`, `Response`, and the extractor set is best frozen once the ORM (M1–M3), the system check (M4), and the Plugin contract (M7) have settled what handlers actually need to receive.

Scope, as fixed in the spec-set design §6: "`umbral::web` shape (Router, Request, Response, extractors `Auth<User>` / `Session` / `Path<T>` / `Json<T>` / `Form<T>` / `Query<T>`), middleware chain, generic views, multipart / file uploads, streaming responses, cookies, the 'hide axum' rule applied, the invariant that handler signatures never carry `State<X>` for any app-wide X."

## Key concepts

### Router, Request, Response

`umbral::web::Router` is a thin newtype around `axum::Router`, re-exported through the prelude alongside route-method helpers (`get`, `post`, `put`, `patch`, `delete`). `Request` and `Response` are umbral-named so day-to-day code never imports `axum::http`; the escape hatch `umbral::axum::*` exists for the rare case (`arch.md §2.1`). A `Router` composes by nesting, mounting under a prefix, and attaching middleware, and plugins return one from `Plugin::routes()` (see `02-plugin-contract.md`).

### Extractors

The per-request context table from `arch.md §2.2` is what the extractor set encodes. Each extractor wraps an axum extractor and surfaces it under an umbral name; users see umbral types only.

```rust
async fn create_post(
    auth: Auth<User>,                // umbral-auth-provided
    Path(id): Path<i64>,
    Json(payload): Json<NewPost>,
) -> Result<Json<Post>> { /* ambient pool via Post::objects() */ }
```

`Auth<User>` and `Session` come from `umbral-auth` and `umbral-sessions` respectively but are re-exported through the prelude so handler code reads as one umbral surface. `Form<T>` parses URL-encoded bodies; `Query<T>` parses the query string; `Json<T>` and `Path<T>` are the obvious shapes. Multipart bodies use a dedicated `Multipart` extractor that streams parts so large uploads never get buffered whole.

### Middleware chain

Middleware is configured through umbral's chain but the underlying type is a tower `Service`, so any tower-http or third-party layer composes (`arch.md §2.1` mixed-visibility row). Plugins contribute middleware via `Plugin::middleware()` returning `Vec<BoxedLayer>`; the global chain is assembled in topological plugin order. Cross-plugin ordering conflicts are an open question owned by `02-plugin-contract.md`.

### Generic views

Reusable view scaffolding is expressed as trait-based composition in umbral. A `ListView<T: Model>` trait fills in the boilerplate of "page through `T::objects()`, render or serialize"; no inheritance, no view-class-to-handler conversion ceremony. The trait set is what makes the admin and REST plugin small.

### Multipart, streaming, cookies

File uploads use the `Multipart` extractor for parsing; storage is owned by `static-and-media.md`. Streaming responses are `Response::stream(impl Stream<Item = Bytes>)`. Cookies read through the request and write through the response with secure-by-default flags governed by `security-defaults.md`; the `Session` extractor is the typical reason to touch cookies directly.

### The two invariants

The web layer re-states `arch.md §2.1` and `§2.2` in its own terms:

- **Hide axum.** `umbral::web::*` is the day-to-day surface; `umbral::axum::*` is the escape hatch. Plugins import only the prelude.
- **No `State<X>` in handler signatures for any app-wide X.** Process-scoped context (DB pool, settings, task queue) is read ambiently through accessors (`Post::objects()`, `umbral::settings()`); request-scoped context is extracted. This is what keeps a handler signature small and declarative.

## Promote-to-deep trigger

Promote when M0's *second* hand-written route lands and the `router(...)` escape hatch starts feeling stretched, or when `02-plugin-contract.md` needs to name `Router` concretely (the trait already references it; the type's surface gets pinned the moment a built-in plugin returns a non-trivial `routes()`).

## Open questions

- **Generic-view shape.** Trait-based composition is the direction, but the exact trait set (`ListView`, `DetailView`, `CreateView`, …) needs the ORM's `Manager` surface frozen first. Open because picking traits before M3 risks shapes the macros can't satisfy.
- **Middleware ordering across plugins.** Carried from `02-plugin-contract.md` open question #3: a `priority` field on `BoxedLayer` vs an explicit `App::builder().middleware_order(...)` override. Open because a real conflict hasn't surfaced yet; deciding in the abstract risks the wrong shape.
- **Multipart back-pressure and limits.** Per-part size caps, total-body caps, and what happens when a stream backs up against a slow storage backend. Open because the storage half lives in `static-and-media.md` and the two specs have to agree on where the cap is enforced.
- **Generic error → response mapping.** Handlers return `Result<T, Error>`; the framework needs a default `IntoResponse` for the umbral error enum that produces the right status without each handler hand-mapping. Open because the error model isn't its own spec yet — currently cross-cutting in `arch.md`.
- **CSRF integration point.** The CSRF middleware in `security-defaults.md` needs hooks in `Form<T>` (validate the token before deserialising) and in the `Session` extractor (where the token lives). Open because the precise hook surface depends on whether CSRF is a layer, an extractor, or both.

## Cross-links

- Deep specs that constrain this: `arch.md §2.1` (hide axum), `arch.md §2.2` (ambient vs explicit context), `01-app-and-settings.md` (the ambient pool handlers read from; the `router(...)` builder escape hatch), `02-plugin-contract.md` (`Plugin::routes()`, `Plugin::middleware()`, the prelude surface).
- Sibling outlines: `static-and-media.md` (multipart storage, `FileField` / `ImageField` semantics), `auth-and-sessions.md` (the `Session` and `Auth<User>` extractors), `forms.md` (`Form<T>` extractor and server-rendered form flows), `security-defaults.md` (CSRF, clickjacking, secure-cookie defaults, the middleware that ships with the first plugin set).
- arch.md: §2 cross-cutting conventions; §4.4 core feature inventory for routing/views/middleware.
