# Outline — OpenAPI (umbral-openapi)

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at M12 entry. |
| **Maps to milestone** | M12 |
| **Companions** | `02-plugin-contract.md`, outline `rest.md`, outline `web-layer.md`, outline `auth-and-sessions.md`, outline `admin.md` |

## Purpose

`umbral-openapi` is the optional plugin that turns a running umbral REST surface into a browsable, machine-readable API description: an OpenAPI 3.x JSON/YAML document plus a Swagger UI mounted at a configurable path (default `/api/docs`). It exists so a team that has already declared models, `ModelSerializer`s, and `ViewSet`s in `umbral-rest` does not also have to hand-write schemas — the description is *derived* from the REST surface that already runs the requests. Structurally, the plugin is the cleanest possible demonstration of the dependency direction from `arch.md §1`: `umbral-openapi` depends on `umbral-rest`, `umbral-rest` depends on `umbral` (the facade), and `umbral-core` depends on neither. A REST-free app cannot need OpenAPI, so the build graph forbids it — Cargo's ban on circular crate deps doing the same enforcement work the spec asks of it. The user-experience target is the Django-shape one-liner: `cargo add umbral-openapi`, add `.plugin(OpenApiPlugin::default())` to `App::builder()`, and a working Swagger UI appears at `/api/docs` describing every registered viewset.

## Key concepts

**utoipa integration.** The underlying schema generator is `utoipa`, chosen because its `ToSchema` derive maps cleanly onto the shape `umbral-rest` already produces from `#[derive(ModelSerializer)]`. The plugin owns no schema-generation logic of its own; it owns the *bridge* from umbral-rest's registry of serializers and viewsets into utoipa's `OpenApi` builder. That keeps the surface small and lets utoipa updates flow through without touching umbral-rest.

**Schema collection from the REST surface.** At boot, inside `on_ready`, the plugin walks the registry that `umbral-rest` exposes (every `ModelSerializer` registered as a `ToSchema`, every `ViewSet` registered as a path group with verbs, parameters, and response types) and assembles a single `utoipa::openapi::OpenApi` document. The discovery mechanism — whether the registry is a compile-time `inventory` slice populated by the `#[derive(ModelSerializer)]` macro or a runtime registry walked off the plugin list — is an open question owned by `rest.md` and inherited here.

**Swagger UI mounting.** The plugin contributes routes via `Plugin::routes()`: one route serves the JSON document (`/api/docs/openapi.json`), one serves the UI (`/api/docs`). Mount path is configurable, and the UI assets ship embedded in the binary so no separate static-file deployment is needed:

```rust
App::builder()
    .plugin(OpenApiPlugin::default().mount("/api/docs"))
    .build()?;
```

**Schema customisation.** A `#[openapi(...)]` attribute on a serializer or viewset adds descriptions, tags, examples, and operation IDs without forcing every field to be annotated. Defaults come from doc-comments where possible, matching DRF's tendency to surface docstrings as descriptions:

```rust
#[derive(ModelSerializer)]
#[openapi(tag = "posts", description = "A blog post.")]
pub struct PostSerializer { /* … */ }
```

**Authentication scheme reflection.** The plugin reads the auth/permission classes attached to each viewset (via `umbral-rest`'s permission machinery, which in turn knows about `umbral-auth`'s `Auth<User>` extractor and session cookies) and emits the matching OpenAPI `securitySchemes`: `bearerAuth` for token auth, `cookieAuth` for session auth, and named custom schemes that plugins can register. The exact registration API is an open question.

**Content negotiation.** `umbral-rest`'s renderer/parser pairs declare a content type per operation; the plugin reflects them as `content` entries on responses and request bodies. A viewset that supports JSON and a future browsable HTML renderer gets both listed; the schema describes whichever ones are actually wired.

## Promote-to-deep trigger

Promote at M12 entry, once `umbral-rest` (M10) has frozen its serializer/viewset registry and the admin (M11) has surfaced any cross-cutting "what does the registry expose?" questions that an OpenAPI generator also needs answers to.

## Open questions

- **Registry discovery.** Compile-time `inventory` collection populated by `#[derive(ModelSerializer)]` and the `ViewSet` macro vs a runtime walk of the plugin list calling `rest_registrations()`. The choice ripples into `rest.md`; pick one once a second consumer (admin or openapi) needs the same registry.
- **API versioning.** When the same app exposes `/api/v1/...` and `/api/v2/...` as distinct viewset groups, the plugin can emit one merged document with `servers` entries or one document per version. Likely needs a `versions(["v1", "v2"])` builder method; defer until a real consumer asks.
- **Security-scheme registration.** Built-in reflection of `Auth<User>` and `Session` is straightforward; a third-party auth plugin that ships a custom permission class needs a way to declare its OpenAPI security scheme. Probably an extra trait method on the REST permission trait, but the shape lives with `rest.md`.
- **Alternative UIs.** Swagger UI is the default, but ReDoc and Stoplight Elements are common asks. Settle on a single bundled UI vs a `ui(OpenApiUi::Redoc)` choice with extra crates feature-gated behind it.
- **Schema export at build time.** `manage.py spectacular --file schema.yml` (DRF-style) for CI consumers. Probably a `commands()` contribution exposing `umbral-cli openapi export`; defer until a real need arises.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (the `Plugin` trait this plugin implements; `dependencies()` returns `&["rest"]`; routes and `on_ready` are the contribution points).
- Sibling outlines: `rest.md` (the entire reason this plugin exists; depends on it both at the Cargo level and at the schema-source level), `web-layer.md` (route mounting and the UI's static-asset response shape), `auth-and-sessions.md` (the auth surface whose permission classes get reflected as `securitySchemes`), `admin.md` (a likely co-consumer of the same REST registry).
- `arch.md §1` (dependency direction — the structural reason this plugin can exist), `arch.md §6.6` (the one-line scope sentence this outline expands).
