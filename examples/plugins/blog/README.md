# `BlogPlugin` example — the full umbra-rest plugin shape

A complete, runnable blog plugin showing every concern a real plugin author would touch:

| Concern | Where it lives |
|---------|----------------|
| Model declaration | `src/blog.rs` — `#[derive(Model)] struct Post` |
| Plugin trait | `src/blog.rs` — `impl Plugin for BlogPlugin` |
| Model auto-registration | `Plugin::models()` returns `Post`; no `.model::<Post>()` in main.rs |
| HTTP routes (HTML-ish) | `Plugin::routes()` mounts `/blog` and `/blog/{id}` |
| REST customisation | `blog::rest_resource()` returns a `ResourceConfig` with `transform` + `computed` |
| DRF-style `@action` (collection) | `ResourceConfig::action("recent", GET, Collection, ...)` |
| DRF-style `@action` (detail) | `ResourceConfig::action("publish", POST, Detail, ...)` |
| Seed-on-boot | `Plugin::on_ready()` inserts three rows on the first run |

The user's `main.rs` is a single `.plugin(BlogPlugin)` line for the model+routes side, plus `.plugin(RestPlugin::default().resource(blog::rest_resource()))` for the REST surface — nothing else.

## Running

```bash
cd examples/plugins/blog
cargo run
```

Then hit:

```bash
# HTML list / detail
curl http://127.0.0.1:3002/blog
curl http://127.0.0.1:3002/blog/1

# Auto-generated CRUD (umbra-rest)
curl http://127.0.0.1:3002/api/post/
curl http://127.0.0.1:3002/api/post/1

# Collection-scope @action
curl 'http://127.0.0.1:3002/api/post/recent/?limit=3'

# Detail-scope @action
curl -X POST http://127.0.0.1:3002/api/post/2/publish/
```

The CRUD responses are masked / enriched by the resource config:

- `password_hash`-style hide is illustrated by the `author_email` transform: outbound the value is `***@example.com`, the underlying column is intact.
- A `summary` field is added per row, computed from the first 120 chars of `body`.

## What this example does NOT show

- Templates with a real `base.html` / `{% block content %}` flow — see `examples/derive-demo` for that.
- Authentication. The resource has no `.permission(...)` set so everything is open. Real apps stack `IsAuthenticated` / `IsStaff` / a custom `Permission`.
- Multiple plugins exercising cross-plugin dependencies (`Plugin::dependencies()`). Worth a follow-on example once a real consumer exists.

## Database state

The example uses a file-backed sqlite (`blog-plugin.db`) so re-running the binary keeps the seed + any `publish` mutations. Delete the file to start fresh.
