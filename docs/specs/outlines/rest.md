# Outline — REST (umbra-rest, optional plugin)

| | |
|---|---|
| **Status** | Outline. Promotes to a deep spec at M10 entry. |
| **Maps to milestone** | M10 |
| **Companions** | `02-plugin-contract.md`, `03-orm-querysets.md`, `04-orm-model-and-fields.md`, outline `web-layer.md`, outline `auth-and-sessions.md`, outline `openapi.md`, `arch.md §6.5` |

## Purpose

`umbra-rest` is umbra's Django REST Framework analog: serializers, viewsets, routers, pagination, filtering, throttling, content negotiation, renderers, and parsers. It is an **optional plugin** — `umbra-core` does not depend on it (this is the structural proof from `arch.md §1` Pillar 3 that "serializers are a plugin"), and a REST-free app must compile with zero serializer code and pay no serializer overhead. The plugin sits on serde for the JSON layer and on the QuerySet API from `03-orm-querysets.md` for data access, so a `ModelSerializer<Post>` is a thin mapping between `Post` (the ORM model) and its wire shape, not a parallel data layer. ViewSets reuse the routing surface from outline `web-layer.md`; the plugin contributes its built-in classes (auth/permission/throttle) via the same `Plugin` trait the rest of the framework uses. Importantly, REST serializers are **structurally different from `forms.md`'s `ModelForm`**: serializers handle JSON (parsed by a `Parser`, rendered by a `Renderer`, validated against a typed wire schema), while forms handle HTML form-encoded input and produce server-rendered HTML — different content types, different validation paths, different error shapes (machine-readable JSON envelopes vs field-attached HTML error nodes). They share a validator catalog (the same `validator` crate functions used in `#[umbra(validators(...))]`) but nothing else.

## Key concepts

**Serializers / `ModelSerializer`.** A serializer is a typed struct that knows how to go in both directions: deserialize a JSON request body into a validated input value, and serialize a model instance into a JSON response body. The hand-rolled path is `impl Serializer for X`; the derive path is `#[derive(ModelSerializer)]` on a struct whose field set is the subset of the model's fields the API exposes, plus per-field options (`read_only`, `write_only`, `source = "..."`, nested serializers for FK / M2M expansions). Validation runs at deserialize time and yields a structured error response.

```rust
#[derive(ModelSerializer)]
#[umbra(model = Post, fields = [id, title, body, author, published_at])]
pub struct PostSerializer {
    #[umbra(read_only)] pub id: i64,
    #[umbra(max_length = 200)] pub title: String,
    pub body: String,
    #[umbra(nested = AuthorSerializer)] pub author: Author,
    pub published_at: Option<DateTime<Utc>>,
}
```

**ViewSets and routers.** A `ViewSet` bundles the standard CRUD operations against one queryset + one serializer; a `Router` mounts a viewset under a URL prefix and auto-generates the canonical URL set (`GET /` list, `POST /` create, `GET /:id` retrieve, `PUT/PATCH /:id` update, `DELETE /:id` destroy). The viewset returns an axum-shape `Router` so it composes with the plugin's `routes()` method from `02-plugin-contract.md`.

```rust
impl ViewSet for PostViewSet {
    type Model = Post;
    type Serializer = PostSerializer;
    fn queryset(&self) -> QuerySet<Post> { Post::objects() }
    fn permissions(&self) -> Vec<BoxedPermission> { vec![Box::new(IsAuthenticated)] }
}
```

**Authentication, permission, and throttle classes.** Pluggable trait objects attached per-viewset (or globally via settings). Authentication classes resolve `Auth<User>` from request headers / cookies (token, session, basic — backed by outline `auth-and-sessions.md`). Permission classes gate the action (`IsAuthenticated`, `IsAdminUser`, `DjangoModelPermissions`, custom). Throttle classes ride on `tower-governor` and scope by IP, user, or a custom key.

**Pagination, filtering, ordering.** Pagination styles ship as classes: `PageNumberPagination`, `LimitOffsetPagination`, `CursorPagination` (opaque cursor over an ordered field). Filtering integrates with the QuerySet API — declared filter sets translate `?status=published&author=3` into `.filter(post::status.eq(...))` calls. Ordering is a comma-separated `?ordering=-published_at,title` parsed against an allow-list per viewset.

**Content negotiation, renderers, parsers.** A `Renderer` writes a response body in some content type (JSON by default; browsable HTML deferred per the spec-set audit). A `Parser` reads a request body of a given content type (JSON, form, multipart). Content negotiation picks a renderer from the `Accept` header against the viewset's declared renderer list, falling back to JSON.

## Promote-to-deep trigger

Promote at M10 entry, when the plugin lands. The deep spec resolves the open questions below, fixes the canonical class hierarchies, and pins the wire-error envelope shape.

## Open questions

- **Serializer derive vs hand-rolled `impl`.** A `#[derive(ModelSerializer)]` covers the 90% case; the hand-rolled `impl Serializer` is the escape hatch for custom validation flows. Decide whether both are first-class or whether the derive is sugar over a documented hand-rolled shape (mirrors the M2 → M3 progression for `Model`).
- **Async vs sync hooks.** DRF's `perform_create` / `perform_update` are sync. ORM writes through the QuerySet API are async (`.create(...).await`). Most likely the viewset action method is async and the hook signatures follow; needs confirmation against the trait-object cost.
- **Relation handling: nested vs flat.** Two strategies for FK/M2M fields: nested (serialize the related object inline) and flat (emit the primary key). Both are useful; the question is which is the default for `#[derive(ModelSerializer)]` and how a single field opts into the other. Hyperlinked relations (DRF's `HyperlinkedRelatedField`) are likely deferred unless reverse-routing lands first.
- **Throttle scope.** Per-IP is the safe default; per-user requires auth to have already run; per-action and per-endpoint scopes are useful. Decide the default scope and the override surface.
- **Wire-error envelope.** Field-level validation errors need a documented JSON shape (DRF uses `{ "field_name": ["msg"], "non_field_errors": ["msg"] }`). The deep spec pins one; outline `forms.md`'s HTML error shape is deliberately different.

## Cross-links

- Deep specs that constrain this: `02-plugin-contract.md` (the `Plugin` trait this plugin implements), `03-orm-querysets.md` (the queryset surface viewsets read from), `04-orm-model-and-fields.md` (the `Model` shape `ModelSerializer` maps).
- Sibling outlines: `web-layer.md` (extractors, middleware, the `Router` viewsets mount on), `auth-and-sessions.md` (authentication classes wrap its `User` and login backends), `forms.md` (deliberately distinct: HTML forms vs JSON serializers), `openapi.md` (depends on `umbra-rest`; generates schemas from registered viewsets and serializers).
- `arch.md §6.5` — the canonical `umbra-rest` description and the "core does not depend on this crate" rule.
