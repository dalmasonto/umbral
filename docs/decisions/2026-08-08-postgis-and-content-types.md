# PostGIS/GIS field types and generic relations (content types)

Status: Draft. Design note for gaps5 #22 (tf#235, PostGIS/GIS) and gaps5 #23 (tf#236, generic relations / content types). Nothing here is built yet; this is the plan of record for how both land on the real field/backend system so implementation can start from a shared shape.

Both features share one theme: extend the ORM's existing seams (`SqlType`, `FieldSpec`, `DatabaseBackend`, the boot system check, the Plugin contract) rather than bolt on a parallel system. PostGIS is a new family of Postgres-only field types that reuses the backend-gating machinery verbatim. Content types is a new plugin (`umbral-contenttypes`) that adds a registry model plus a virtual field, and creates its table the same way every other plugin does.

## How a field declares its SQL type and backend support today (the real API)

This is the substrate both features build on, so it is worth stating exactly. Three pieces, all in `crates/umbral-core`.

**1. The SQL type: `FieldSpec.ty: SqlType`.** Every column carries a `FieldSpec` (`orm/model.rs`), constructed once as a `const` and stored in `Model::FIELDS: &'static [FieldSpec]`. Its `ty` field is a `SqlType` (`orm/model.rs:1001`), a `Copy` enum whose variants are the abstract type catalogue: `ForeignKey`, `SmallInt`, `Integer`, `BigInt`, `Real`, `Double`, `Boolean`, `Text`, `Date`, `Time`, `Timestamptz`, `Uuid`, `Json`, `Array(ArrayElement)`, `Inet`, `Cidr`, `MacAddr`, `Xml`, `Ltree`, `Bit`, `FullText`, `Bytes`, `Decimal`. `SqlType` stays `Copy` so it can live in a `const FIELDS` slice; a type that needs to carry data (like `Array`) nests a small `Copy` enum (`ArrayElement`) rather than a `Box`. The `#[derive(Model)]` macro's `classify_field_type` (in `umbral-macros`) maps a Rust field type (`i64`, `String`, `Vec<i64>`, `serde_json::Value`, ...) to the matching `SqlType` variant.

**2. Backend support: two mechanisms.**

- **Per-field declared list: `FieldSpec.supported_backends: &'static [&'static str]`.** Empty slice means "all backends"; a non-empty slice restricts the field to exactly those backend names (matched against `DatabaseBackend::name()`, e.g. `"postgres"` / `"sqlite"`). Set via `#[umbral(backend = "postgres")]`. The `field.backend` boot check (`check.rs:606`, `field_backend`) walks every registered model and emits a `Severity::Error` finding when the active backend is not in a field's non-empty `supported_backends`.
- **Framework-known Postgres-only types: the `is_postgres_only(SqlType)` match (`check.rs:991`).** The same `field.backend` check also hard-codes which `SqlType` variants are Postgres-only (`Array`, `Inet`, `Cidr`, `MacAddr`, `Xml`, `Ltree`, `Bit`, `FullText`, `Decimal`) and rejects them on any non-Postgres backend regardless of the declared list. This is belt-and-suspenders with the declared list: a user never has to write `#[umbral(backend = "postgres")]` on an `ArrayField`, the type itself is known to be Postgres-only.

Alongside both, `DatabaseBackend::supports(BackendFeature)` (`backend.rs:49`) is the capability query the migration engine and checks use instead of `if backend.name() == "postgres"`. `BackendFeature` (`backend.rs:74`) enumerates capabilities: `ArrayColumns`, `HStoreColumns`, `JsonbColumns`, `FullTextSearch`, `CidrInet`, `UuidNative`, etc. `PostgresBackend::supports` returns `true` for all of them; `SqliteBackend::supports` returns `false` for the Postgres-only ones.

**3. SQL rendering: `DatabaseBackend::map_type` / `map_column`.** `map_type(SqlType) -> sea_query::ColumnType` (`backend.rs:54`) is the per-backend translation the migration engine reads when it emits `CREATE TABLE`. `map_column(&Column)` (`backend.rs:62`) is the richer entry point that lifts per-column hints (Postgres turns `Text + max_length = N` into `VARCHAR(N)`, and `case_insensitive` text into `citext`). Types sea-query has no native `ColumnType` variant for are rendered through `ColumnType::custom("...")`: this is how `Xml`, `Ltree`, `Bit` (`"bit varying"`), and `FullText` (`"tsvector"`) already work. The SQLite `map_type` for every Postgres-only variant is a `panic!` with a pointer back to the boot check, so a bypassed check surfaces loudly rather than emitting SQL SQLite cannot parse.

Downstream, the same `SqlType` drives four consumers that both new features must extend: the admin widget picker (`plugins/umbral-admin/src/view.rs:928`, a `match col.ty` returning an input kind), the OpenAPI schema mapper (`plugins/umbral-openapi/src/lib.rs:807`, `match ty` returning a `(json_type, format)` pair), the REST filter surface (`plugins/umbral-rest/src/filtering.rs`), and `inspectdb` in both directions (`crates/umbral-core/src/inspect.rs`: `map_postgres_type` DB-to-`SqlType`, `render_field_type` `SqlType`-to-Rust).

That is the full contract. Everything below plugs into these five points.

---

# Part A: PostGIS / GIS field types (gaps5 #22)

## Goal

Give umbral first-class spatial columns on Postgres: `geometry` and `geography`, with a declared subtype and SRID, GiST indexes, SRID validation, spatial predicates in the typed QuerySet, and integration into admin, REST, OpenAPI, and inspectdb. They are Postgres-only and reuse the existing backend-gating so SQLite fails clearly at boot, exactly like `ArrayField` and `Decimal` do.

## A.1 New SqlType variants

Add two variants to `SqlType`, each carrying a small `Copy` payload, mirroring the `Array(ArrayElement)` precedent so `SqlType` stays `Copy` and usable in `const FIELDS`:

```rust
pub enum SqlType {
    // ... existing variants ...
    /// PostGIS `geometry(<kind>, <srid>)` - planar spatial column. Postgres-only.
    Geometry(GeometrySpec),
    /// PostGIS `geography(<kind>, <srid>)` - spheroidal spatial column. Postgres-only.
    Geography(GeometrySpec),
}

/// The subtype + SRID of a spatial column. `Copy` so it nests inside `SqlType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GeometrySpec {
    pub kind: GeometryKind,
    /// Spatial Reference System Identifier. 4326 (WGS84 lon/lat) is the default.
    /// 0 means "unspecified SRID" (PostGIS `geometry` with no SRID constraint).
    pub srid: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GeometryKind {
    Geometry,          // any (the unconstrained base type)
    Point,
    LineString,
    Polygon,
    MultiPoint,
    MultiLineString,
    MultiPolygon,
    GeometryCollection,
}
```

Carrying SRID inside the `SqlType` (rather than as a side-channel on `FieldSpec` like `fk_target` / `max_length`) keeps the type and its SRID together, which is what PostGIS itself does: `geometry(Point, 4326)` is one column type. The alternative (an `srid: i32` field on `FieldSpec`) was rejected because SRID is intrinsic to the column type in PostGIS, not a display hint like `max_length`.

## A.2 Rust type and the derive mapping

sqlx has no native PostGIS support, so umbral ships a thin newtype exactly like `TsVector` does for `tsvector`. Put it in a new `crates/umbral-core/src/orm/gis.rs`:

```rust
/// A spatial value. Wraps a `geo_types::Geometry<f64>` and carries sqlx
/// `Type`/`Encode`/`Decode` impls for Postgres that read/write EWKB (the
/// binary wire form PostGIS speaks). Serde serializes as GeoJSON.
pub struct Geometry(pub geo_types::Geometry<f64>);
```

The derive's `classify_field_type` maps the Rust field type plus attributes to the variant:

- `umbral::orm::gis::Geometry` (or a `Point` alias) as the Rust type gives a `SqlType::Geometry(...)` field.
- The subtype and SRID come from `#[umbral(...)]` attributes, because a single Rust newtype cannot encode them: `#[umbral(geometry = "point", srid = 4326)]` and `#[umbral(geography = "point", srid = 4326)]`. `geography = ...` selects the `Geography` variant; `geometry = ...` selects `Geometry`. `srid` defaults to `4326` when omitted; `kind` defaults to `Geometry` (unconstrained) when the attribute value is absent.

```rust
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
pub struct Cafe {
    pub id: i64,
    pub name: String,
    #[umbral(geography = "point", srid = 4326, gist_index)]
    pub location: umbral::orm::gis::Geometry,
}
```

Dependencies stay honest with the design principles: reuse `geo-types` (the de facto Rust geometry types) and a WKB/EWKB codec (`wkb` or `geozero`) plus `geojson` for serialization. umbral does not reimplement geometry math; the value is the glue into the field system.

## A.3 Backend gating (the whole point of reusing the check)

1. **`BackendFeature::PostGis`.** Add the variant. `PostgresBackend::supports` returns `true` (the PostGIS extension being installed is a DBA concern, reported true the same way `HStoreColumns` is per `backend.rs:127`); `SqliteBackend::supports` returns `false`.
2. **`is_postgres_only`** (`check.rs:991`) gains `SqlType::Geometry(_) | SqlType::Geography(_)`. That single edit makes the existing `field.backend` check fail at boot with a clear message when a spatial field is registered against SQLite, no new check needed. The message the user sees is the generic "type X is Postgres-only, but the active backend is `sqlite`" already produced by `field_backend`.
3. **`SqliteBackend::map_type`** gains a `panic!` arm for both variants, matching the existing backstop for `Array` / `Inet` / `Decimal`. Reaching it means boot was bypassed.
4. **`PostgresBackend::map_type`** renders through `ColumnType::custom`, the same escape hatch `Xml` / `Ltree` / `FullText` use:

```rust
SqlType::Geometry(spec) => ColumnType::custom(&pg_spatial_type("geometry", spec)),
SqlType::Geography(spec) => ColumnType::custom(&pg_spatial_type("geography", spec)),
// pg_spatial_type("geography", {Point, 4326}) => "geography(Point,4326)"
// kind == Geometry and srid == 0 render the bare "geometry" / "geography".
```

## A.4 SRID validation and the CREATE EXTENSION concern

- **Extension bootstrap.** The migration engine emits `CREATE EXTENSION IF NOT EXISTS postgis;` ahead of the first `CREATE TABLE` that has a spatial column, exactly as the `citext` path already auto-creates its extension (`backend.rs:150-154` documents that precedent). This keeps a fresh database working without a manual DBA step; on a managed Postgres where the operator lacks `CREATE EXTENSION`, they pre-install it and the `IF NOT EXISTS` makes the statement a no-op.
- **SRID validation at write time.** A `geography`/`geometry` column declared `srid = 4326` rejects a value whose SRID differs, at the database level (PostGIS enforces the typmod). umbral adds an app-level guard on the write path (the `Geometry` newtype's encode) that stamps the declared SRID onto a value that carries SRID 0, and errors early with a clear message on a genuine mismatch, so the failure is a framework `WriteError` rather than a raw Postgres `22023`.
- **Boot check for SRID sanity.** A small additive `field.gis_srid` system check (optional, low priority) can warn when `srid` is set on a `geometry` column but the value type is a bare `Geometry` with no meaningful CRS. Not load-bearing for v1.

## A.5 GiST index

Spatial queries are useless without a GiST index. Extend the index story rather than inventing a parallel one:

- `#[umbral(gist_index)]` sets a new `FieldSpec.gist_index: bool` (sibling to the existing `index: bool` at `model.rs:732`).
- The migration engine, where it already emits `CREATE INDEX idx_<table>_<column>` for `index`, emits `CREATE INDEX idx_<table>_<column>_gist ON <table> USING GIST (<column>)` for `gist_index`. GiST is the access method PostGIS spatial predicates use; a plain B-tree index does not help a `ST_DWithin`/`&&` query.
- `gist_index` is Postgres-only and rides the same `field.backend` gate (a spatial column already fails on SQLite, so a GiST index on it never reaches SQLite).

## A.6 Spatial lookups in the QuerySet

Follow the `JsonCol` precedent (`orm/column.rs`, the `has_key` / json-path predicates). The derive already generates a per-column module (`cafe::LOCATION`); give spatial columns a `GeometryCol` type whose inherent methods build `Predicate<T>`. Because spatial columns are Postgres-only, these predicates render only Postgres SQL through `sea_query::Expr::cust_with_exprs` (raw function-call SQL with bound parameters); there is no SQLite variant to write (the JSON predicates needed `Predicate::new_with_sqlite` precisely because JSON is cross-backend, `mod.rs:201`; spatial is not, so the default `Predicate::new` path is correct and can never be reached on SQLite).

Predicate surface for v1 (the common spatial questions):

- `.dwithin(&other, meters)` -> `ST_DWithin(col, $1, $2)` (on `geography`, distance is in meters; on `geometry`, in SRID units). The workhorse "within N meters of" query.
- `.intersects(&other)` -> `ST_Intersects(col, $1)`.
- `.contains(&other)` -> `ST_Contains(col, $1)`.
- `.within(&other)` -> `ST_Within(col, $1)`.
- `.bbox_overlaps(&other)` -> `col && $1` (the index-accelerated bounding-box operator; the cheap pre-filter).

Ordering by distance (`ORDER BY col <-> $1`, the KNN operator) plugs into the existing `order_by` seam as a raw order expression, so "nearest N" works. Distance as a selected annotation (`ST_Distance` in the projection) rides the annotation path when a real consumer needs the number in the payload; deferred until then.

## A.7 Admin, REST, OpenAPI, inspectdb

- **Admin widget** (`plugins/umbral-admin/src/view.rs:928` `widget_for` match, and the `SqlType::...` discovery in `discovery.rs:93`). v1: `SqlType::Geometry(_) | SqlType::Geography(_)` map to a `"geo"` widget that is a WKT/GeoJSON textarea (portable, no JS map dependency) plus a read-only static map preview. A richer interactive map picker is a follow-on; it routes through umbra's standard front-end libraries rather than hand-rolled map code. List display renders a compact WKT summary or the centroid coordinates, truncated like any long value.
- **REST** serializes spatial columns as GeoJSON geometry objects (the `Geometry` newtype's serde impl produces GeoJSON), which is what web clients and Leaflet/Mapbox consume directly. The REST filter surface (`filtering.rs`) exposes a bounded spatial filter family (`location__dwithin=<lon>,<lat>,<meters>`, `location__bbox=<minx>,<miny>,<maxx>,<maxy>`) that compiles to the `GeometryCol` predicates above. Writes accept GeoJSON or WKT.
- **OpenAPI** (`plugins/umbral-openapi/src/lib.rs:807` type match) maps `Geometry`/`Geography` to `("object", None)` with a `$ref`/inline GeoJSON geometry schema (a `type`/`coordinates` object). This keeps generated clients honest about the shape.
- **inspectdb** must recover subtype and SRID, which `information_schema` alone cannot give (it reports geometry columns as `USER-DEFINED` with `udt_name = "geometry"`). The fix: `crates/umbral-core/src/inspect.rs` queries the PostGIS catalog views `geometry_columns` and `geography_columns` (which expose `type`, `srid`, `coord_dimension` per column) and builds `SqlType::Geometry(GeometrySpec { kind, srid })`. `render_field_type` then emits `umbral::orm::gis::Geometry` with the reconstructed `#[umbral(geometry = "...", srid = N)]` attribute so a re-migrate round-trips. `map_postgres_type` (`inspect.rs:449`) gains the `"geometry"` / `"geography"` udt cases feeding into that catalog lookup.

## A.8 What is deferred for PostGIS v1

Raster (`raster` type), 3D/4D coordinates (`ST_Force3D`, M/Z dimensions beyond storage), topology (`topology` extension), coordinate transforms in the ORM (`ST_Transform` as a query-time projection), and the interactive admin map picker. All are additive on the shape above.

---

# Part B: generic relations / content types (gaps5 #23)

## Goal

A `GenericForeignKey` that can point at a row of any model, backed by a content-type registry, so cross-cutting features (comments, audit logs, tags, notifications, object-level permissions) can attach to any model without a dedicated FK column per target. Ships as a new built-in plugin `umbral-contenttypes`, structurally identical to any third-party plugin: it depends only on the `umbral` facade, contributes a model, and creates its own table through its own migrations.

## B.1 The ContentType registry model

The registry is a plain umbral model owned by the plugin. It maps a stable integer id to the (plugin, model, table) triple that identifies a registered model:

```rust
#[derive(Debug, Clone, sqlx::FromRow, serde::Serialize, serde::Deserialize, Model)]
#[umbral(table = "umbral_content_type")]
pub struct ContentType {
    pub id: i64,
    /// The owning plugin's name (e.g. "blog"), from `Plugin::name()`.
    #[umbral(max_length = 100)]
    pub app_label: String,
    /// The model's `Model::NAME` (e.g. "Post").
    #[umbral(max_length = 100)]
    pub model: String,
    /// The model's `Model::TABLE` (e.g. "post"). Stored so a GFK resolve
    /// can build a `DynQuerySet` without re-deriving the table name.
    #[umbral(max_length = 100)]
    pub table_name: String,
}
// (app_label, model) is unique_together.
```

## B.2 How the table is created: via the plugin's own migrations

This is the CLAUDE.md contract in action, not a special case. `ContentTypesPlugin` implements `Plugin::models()` returning `vec![ModelMeta::for_::<ContentType>()]` (the exact pattern `umbral-tenants` uses at `plugins/umbral-tenants/src/lib.rs:726` for its `Tenant` model). `makemigrations` autodetects the model and writes a migration file under the plugin's own crate; `migrate` walks every registered plugin, orders by the dependency graph, and applies it. The `umbral_content_type` table is created exactly the way the auth, sessions, and tasks tables are. Nothing is special-cased.

## B.3 Populating the registry (through the ORM, never raw SQL)

The registry rows are derived data: one row per registered model. Populate them in `Plugin::on_ready` (the startup lifecycle hook, `plugin.rs:543`), which runs after migrations. Walk the model registry (`crate::migrate::registered_models()`, the same walk the boot checks use) and upsert one `ContentType` per model, through the ORM:

```rust
for meta in umbral::migrate::registered_models() {
    ContentType::objects()
        .get_or_create(/* app_label + model predicate */, /* defaults incl. table_name */)
        .await?;
}
```

Reads and writes go through `ContentType::objects()` (`get_or_create`, `filter`, `create`) per the "plugins use the ORM, not raw SQL" rule. No `sqlx::query` anywhere in the plugin. Stale rows (a model removed from the app) are left in place by default (an audit-log row may still reference them); a `prune` management command removes orphans on request.

A process-local cache (`OnceLock<HashMap<(String, String), i64>>` and the reverse map) makes the id-to-model and model-to-id lookups allocation-free after first use, refreshed from the table at `on_ready`.

## B.4 The GenericForeignKey field

A GFK is not one column. Like Django's, it is a pair of real columns plus a virtual accessor:

- `content_type: ForeignKey<ContentType>` - which model the row points at.
- `object_id: String` - the target row's primary key, stored as a string so a GFK can point at an `i64`-keyed, `String`-keyed, or `Uuid`-keyed model uniformly. This reuses the PK-agnostic key shape the ORM already standardized on (`orm/mod.rs:66`, `pk_key`): the codebase already lifted the `i64`-PK assumption end-to-end, so `object_id: String` is the natural home for a heterogeneous target PK.

The virtual `GenericForeignKey` is a helper type (in the plugin) that reads/writes those two columns together and resolves the target late-bound:

```rust
pub struct GenericForeignKey; // marker/accessor; the two columns are the storage

impl GenericForeignKey {
    /// Typed escape hatch: set both columns from a concrete target instance.
    pub fn set<T: Model>(row: &T) -> (i64 /* content_type_id */, String /* object_id */);

    /// Resolve to the concrete row, late-bound, as JSON (admin/REST already
    /// speak `serde_json::Value`). Uses the registry to find the target
    /// model's `ModelMeta`, then `DynQuerySet::for_meta(&meta)`.
    pub async fn resolve(content_type_id: i64, object_id: &str)
        -> Result<Option<serde_json::Value>, GfkError>;
}
```

Resolution goes through the existing late-bound path: look up the `ContentType` row by id, get its `table_name`/`model`, find the matching `ModelMeta` in the registry, and query with `DynQuerySet::for_meta(&meta).filter(pk == object_id).fetch(...)`. This is the "Late-bound model (admin)" row of the CLAUDE.md ORM table, and it is exactly what the admin already uses, so no new ORM surface is required for the read path.

**Typed escape hatches.** For code that knows the target type at compile time, a `generic_fk::<T>(&row)` free function returns the `(content_type_id, object_id)` pair, and a `resolve_typed::<T>(object_id) -> Option<T>` uses the normal typed `T::objects()` path when `T` is known, skipping the dynamic layer. The dynamic `resolve` is the fallback for genuinely heterogeneous call sites (rendering a comment thread over mixed target types).

## B.5 The reverse side: GenericRelation

The target model wants "all comments on this post". That is a query, not a stored column, so it is a helper rather than a field:

```rust
// All rows of `Comment` whose GFK points at `post`.
Comment::objects()
    .filter(comment::CONTENT_TYPE.eq(ContentType::id_for::<Post>()?))
    .filter(comment::OBJECT_ID.eq(post.id.to_string()))
```

The plugin exposes this as a `generic_relation::<Comment, Post>(&post)` convenience returning a `QuerySet<Comment>`, and a `ReverseGeneric` accessor analogous to the existing reverse-FK accessors (`orm/reverse_accessor.rs`). A composite index on `(content_type, object_id)` (via `unique_together`-style index metadata on the GFK-bearing model) keeps the reverse lookup fast; the plugin documents adding it on any model that carries a GFK.

## B.6 Admin and REST integration

- **Admin.** The GFK renders as a two-part widget: a `<select>` of content types (populated from the registry) plus an object-id input that becomes an autocomplete once a content type is chosen (the admin already has model-scoped autocomplete for FKs, `plugins/umbral-admin/src/inlines.rs` FK handling). In list display, the GFK shows the resolved target's string representation via `resolve`, falling back to `Model#id` when the target is gone. Generic inlines (editing comments inline under their target's change page) reuse the existing inline machinery keyed on the `(content_type, object_id)` pair instead of a single FK column.
- **REST.** A GFK serializes as `{ "content_type": "blog.Post", "object_id": "42" }` (the `app_label.model` label is stable and human-readable), and optionally embeds the resolved object under a `target` key when the endpoint opts into expansion (mirroring `select_related`'s resolved-FK serialization at `orm/foreign_key.rs:277`). Writes accept the same `{content_type, object_id}` shape; the plugin validates the content type against the registry and the object's existence before the write, returning a clean 400 on a dangling reference rather than a raw FK error.

## B.7 What content types unlocks (and what is deferred)

The registry plus GFK is the shared substrate for the features gaps5 #23 names: a `Comment` model with a GFK, an audit-log row that references any changed model, a `Tag` through a generic through-model, notifications targeting any object, and object-level permissions keyed on `(content_type, object_id)`. Those consumer plugins are separate follow-on work; this note scopes only the `umbral-contenttypes` foundation (registry model, GFK field, typed and dynamic resolution, admin/REST integration). Cross-database GFKs (target on another database) are out of scope: a GFK has no physical FK constraint, so the cross-database FK guard does not apply, but multi-database resolution is deferred until a real consumer needs it.

---

## Cross-cutting notes

- **Facade re-exports.** PostGIS `Geometry` / `GeometrySpec` / `GeometryKind` are core ORM types, so they go in `umbral-core` and re-export through `umbral::orm::gis` (power-user surface, not the prelude, matching how `TsVector` and the raw query builders are exposed). `ContentType` / `GenericForeignKey` live in the `umbral-contenttypes` plugin crate and depend only on the facade.
- **New `SqlType` variants are additive.** Adding `Geometry` / `Geography` touches every exhaustive `match ty` on `SqlType` (the two backends, `check.rs`, admin `view.rs`, openapi, inspectdb, typegen). That is one logical change per the commit-cadence rule, and the compiler's exhaustiveness checking is the checklist: it will not build until every consumer handles the new variants. Content types adds no `SqlType` variant at all (it reuses `ForeignKey` and `Text`), so it is purely additive plugin code.
- **Docs.** Each feature ships its user-facing page when it lands: `documentation/docs/v0.0.1/orm/gis.mdx` (a new area entry or under `orm`) and `documentation/docs/v0.0.1/plugins/content-types.mdx`, each with purpose, one example, and a link back to this note.

## See also

- `crates/umbral-core/src/orm/model.rs` - `SqlType`, `ArrayElement`, `FieldSpec` (the field metadata contract).
- `crates/umbral-core/src/backend.rs` - `DatabaseBackend`, `BackendFeature`, `map_type` / `map_column`.
- `crates/umbral-core/src/check.rs` - `field_backend` and `is_postgres_only` (the backend gate both new Postgres-only types ride).
- `crates/umbral-core/src/orm/column.rs` - the `JsonCol` predicate pattern the spatial lookups follow.
- `crates/umbral-core/src/inspect.rs` - `map_postgres_type` / `render_field_type` (inspectdb, both directions).
- `plugins/umbral-tenants/src/lib.rs` - a plugin that contributes a model via `Plugin::models()` and creates its table through its own migrations.
- `arch.md` - the plugin contract and the Postgres-first / backend-check design principles.
</content>
</invoke>
