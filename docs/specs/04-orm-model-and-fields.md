# 04 — ORM: Model trait and fields

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M2 (hand-written `Model` impl) and M3 (`#[derive(Model)]` generating the same shape) |
| **Companions** | `00-overview.md`, `03-orm-querysets.md`, `05-backends-and-system-check.md`, `06-migration-engine.md` |

## Purpose

What a "model" is in umbral. Defines:

- The **`Model` trait** that every model implements, by hand at M2, then by derive at M3.
- The **field types** the user reaches for (text, int, float, bool, datetime, decimal, UUID, JSON, binary; plus opt-in `FileField`, `ImageField`, `EmailField`, `URLField`, `SlugField`).
- The **field-options attribute system** (default, unique, indexed, max_length, choices, validators).
- **Relationships** (foreign key, one-to-one, many-to-many with explicit through-tables).
- **Model `Meta`** (table name, default ordering, composite uniques, indexes).
- What **`#[derive(Model)]`** expands to: the trait impl, the sibling column module (`mod post { ... }` from `03-orm-querysets.md`), and the per-model insert struct (`NewPost`).

The invariant the whole spec serves: **a nullable column always maps to `Option<T>`**. There is no other way to express "this column may be NULL." That's enforced by the derive.

## Concepts

### The `Model` trait

```rust
pub trait Model: Sized + Send + Sync + 'static {
    type PrimaryKey: PrimaryKey;

    const TABLE: &'static str;
    const FIELDS: &'static [FieldSpec];

    fn primary_key(&self) -> Self::PrimaryKey;

    fn from_row(row: &sqlx::any::AnyRow) -> Result<Self, sqlx::Error>;
}
```

`FIELDS` is the static metadata array. Each entry describes a field's column name, SQL type, nullability, default, uniqueness, index membership, supported backends, and validators. Two other subsystems consume `FIELDS`:

- **The system check** (`05-backends-and-system-check.md`) walks every registered model's `FIELDS` and rejects field-vs-backend incompatibility at boot.
- **The migration engine** (`06-migration-engine.md`) compares the current `FIELDS` snapshot against the last recorded snapshot to autodetect schema changes.

`from_row` is generated; the user never writes it. It's exposed for the rare case where a raw query needs to map back to a model.

### Field types

| Rust type | Column type | Supported backends |
|---|---|---|
| `String` | `StrCol` (TEXT or VARCHAR(n)) | All |
| `i32`, `i64` | `IntCol` | All |
| `f32`, `f64` | `FloatCol` | All |
| `bool` | `BoolCol` | All |
| `chrono::DateTime<Utc>` | `DateTimeCol` | All |
| `chrono::NaiveDate` | `DateCol` | All |
| `chrono::NaiveTime` | `TimeCol` | All |
| `rust_decimal::Decimal` | `DecimalCol` | Postgres native; SQLite via TEXT |
| `uuid::Uuid` | `UuidCol` | Postgres native; SQLite via TEXT |
| `serde_json::Value` | `JsonCol` | Postgres `jsonb`; SQLite via TEXT |
| `Vec<u8>` | `BinaryCol` | All |
| `Option<T>` | `Nullable<T>Col` | All; nullability is on the column type, not a separate attribute |
| `Vec<T>` | `ArrayCol<T>` | **Postgres only** |
| `HashMap<String, String>` | `HStoreCol` | **Postgres only** |

Higher-level field types layered on top:

| Type | Sits on | Adds |
|---|---|---|
| `FileField` | `String` | Path string referencing the default storage; full design in outline `static-and-media.md` |
| `ImageField` | `FileField` | Image metadata (width, height); full design in outline `static-and-media.md` |
| `EmailField` | `String` | Email-shape validator |
| `URLField` | `String` | URL-shape validator |
| `SlugField` | `String` | Slug-pattern validator and an automatic index |

Backend-specific fields (`ArrayCol`, `HStoreCol`) declare their `supported_backends` in `FieldSpec`. The boot system check rejects them on a non-Postgres backend with a clear error message rather than failing at query time.

### Field options (attributes)

Attributes live on the struct field. The derive parses them into the `FieldSpec`:

```rust
#[derive(Model)]
pub struct Post {
    pub id: i64,
    
    #[umbral(fk = Author)]
    pub author_id: i64,
    
    #[umbral(max_length = 200)]
    pub title: String,
    
    #[umbral(unique, indexed)]
    pub slug: String,
    
    pub body: String,
    
    pub published_at: Option<DateTime<Utc>>,
}
```

| Attribute | Effect |
|---|---|
| `#[umbral(default = "...")]` | Database-level default value (literal or one of: `now`, `uuid_v7`). |
| `#[umbral(unique)]` | UNIQUE constraint at column level. |
| `#[umbral(indexed)]` | Single-column index (not unique). |
| `#[umbral(max_length = N)]` | For string columns: VARCHAR(N) on Postgres; CHECK on SQLite. |
| `#[umbral(choices(Draft, Published, Archived))]` | CHECK constraint accepting one of the listed values. |
| `#[umbral(validators(slug, length(min = 1, max = 80)))]` | Model-level validators that run on `.save()` / `.create()`. Validator names map to `validator` crate functions; custom validators are functions of `fn(&self) -> Result<(), ValidationError>`. |
| `#[umbral(fk = Target)]` | Foreign-key relation. The field type is the FK column type (typically `i64`); the derive generates a sibling relation reference under `post::author`. |
| `#[umbral(fk = Target, on_delete = Cascade)]` | Specify `on_delete` (default: `Restrict`). |
| `#[umbral(rename = "blog_post_title")]` | Override the column name (default: the field name as-is). |

### Model `Meta`

Model-level options sit in a struct-level attribute group:

```rust
#[derive(Model)]
#[umbral(
    table = "blog_post",                            // default: snake_case of struct name
    ordering = [post::published_at.desc()],         // default order_by
    unique_together = [(post::author_id, post::slug)],
    indexes = [(post::published_at, post::author_id)],
)]
pub struct Post { /* fields */ }
```

The `ordering`, `unique_together`, and `indexes` columns are expressed using the sibling column constants — they're real values, not strings — so a typo fails to compile.

### Relationships

**Foreign key.** A typed FK is a normal column (`author_id: i64`) plus a sibling relation reference. The derive generates `post::author` as a relation expression usable in `.with(post::author)` (a JOIN). The column type stays the underlying scalar so it can be filtered with the usual predicates (`post::author_id.eq(3)`).

```rust
#[umbral(fk = Author)]
pub author_id: i64,
```

generates a `post::author` relation reference and (under the hood) wires up the `ForeignKey<Post, Author>` relation type the QuerySet uses.

**One-to-one.** A foreign key with `#[umbral(fk = User, unique)]`. The `unique` attribute is the difference; the underlying SQL is "UNIQUE FOREIGN KEY".

**Many-to-many.** Explicit through-table model, declared as a separate `#[derive(Model)]` struct, plus a struct-level attribute on the parent that names the relation:

```rust
#[derive(Model)]
pub struct PostTag {
    #[umbral(fk = Post, on_delete = Cascade)]
    pub post_id: i64,
    #[umbral(fk = Tag, on_delete = Cascade)]
    pub tag_id: i64,
}

#[derive(Model)]
#[umbral(m2m(tags(Tag, through = PostTag)))]
pub struct Post {
    pub id: i64,
    // … no `tags` field; the M2M is a relation reference, not a column
}
```

`post::tags` is generated as a relation reference usable in `.with_many(post::tags)` (separate batched query) per `03-orm-querysets.md`. The through-table is a first-class model that can carry its own fields (a `joined_at`, a `weight`, etc.).

### Insert shape (`NewPost`)

The derive generates a sibling `NewPost` struct that omits the primary key and any column with a database-generated default:

```rust
// generated by #[derive(Model)] on Post
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct NewPost {
    pub author_id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<DateTime<Utc>>,
}

impl Post {
    pub fn new() -> NewPost { NewPost::default() }
}
```

Inserts go through `Post::objects().create(NewPost { ... })`. The `create` terminal returns the freshly-inserted `Post` (with `id` populated via `RETURNING` on Postgres, `last_insert_rowid` on SQLite). Bulk inserts use `Post::objects().bulk_create(&[NewPost { ... }, ...])`.

## API-shape sketch

The hand-written `Model` impl from M2, against `Post` from the canonical example app:

```rust
pub struct Post {
    pub id: i64,
    pub author_id: i64,
    pub title: String,
    pub slug: String,
    pub body: String,
    pub published_at: Option<DateTime<Utc>>,
}

impl Model for Post {
    type PrimaryKey = i64;
    const TABLE: &'static str = "post";
    const FIELDS: &'static [FieldSpec] = &[
        FieldSpec { name: "id",            ty: SqlType::BigInt,        primary_key: true,  .. },
        FieldSpec { name: "author_id",     ty: SqlType::BigInt,        fk: Some("author"),  .. },
        FieldSpec { name: "title",         ty: SqlType::Varchar(200),  .. },
        FieldSpec { name: "slug",          ty: SqlType::Varchar(80),   unique: true, indexed: true, .. },
        FieldSpec { name: "body",          ty: SqlType::Text,          .. },
        FieldSpec { name: "published_at",  ty: SqlType::Timestamptz,   nullable: true, .. },
    ];
    fn primary_key(&self) -> i64 { self.id }
    fn from_row(row: &sqlx::any::AnyRow) -> Result<Self, sqlx::Error> { /* sea-query / sqlx row mapping */ }
}

pub mod post {
    use super::*;
    pub const id:            IntCol<Post>             = IntCol::new("id");
    pub const author_id:     IntCol<Post>             = IntCol::new("author_id");
    pub const author:        ForeignKey<Post, Author> = ForeignKey::new("author_id");
    pub const title:         StrCol<Post>             = StrCol::new("title");
    pub const slug:          StrCol<Post>             = StrCol::new("slug");
    pub const body:          StrCol<Post>             = StrCol::new("body");
    pub const published_at:  NullableDateTimeCol<Post> = NullableDateTimeCol::new("published_at");
}
```

At M3, `#[derive(Model)]` generates exactly that. Writing it by hand at M2 is the design proof: if the hand-written shape is right, the derive only has to mechanise it.

## Mechanics and invariants

### Nullable ↔ `Option<T>` is the only path

There is no `#[umbral(nullable)]` attribute. The only way to express NULL is `Option<T>`. The derive walks the field types; an `Option<X>` field produces a `Nullable<X>Col` in the sibling module and `nullable: true` in `FieldSpec`. Removing the `Option<>` makes the column NOT NULL. This is the type-system version of Django's `null=True` / `blank=True` distinction collapsed to one rule.

### Defaults

Two layers of default exist:

- **Rust-side default** via `#[derive(Default)]` on the model. Used by `NewPost::default()` when the user doesn't fill a field. Lives in Rust.
- **Database-side default** via `#[umbral(default = "...")]`. Generates `DEFAULT ...` in the schema. Lives in SQL.

These are deliberately separate. A model can have a database default of `now()` for `created_at` while the Rust struct's `Default::default()` returns `DateTime::UNIX_EPOCH`. The migration engine reads the database-side default; the QuerySet `.create()` reads the Rust-side one for fields the caller omitted.

### Validators run at `.save()` / `.create()`

The full-clean equivalent runs before any INSERT or UPDATE. Validators come from `#[umbral(validators(...))]` plus any model-level `impl Validate for Post`. A failed validator yields `Error::Validation { field, message }`. Validators are sync.

### Generated names

| Generated identifier | Source |
|---|---|
| Column module (`mod post`) | `snake_case(struct_name)` |
| `NewPost` insert struct | `New + struct_name` |
| Relation reference (`post::author`) | The `fk` target struct's name, snake-cased |
| Default table name | `snake_case(struct_name)` |

These are overridable (`table`, `rename`) but the defaults match what the canonical example expects.

## Trade-offs and alternatives considered

**Field types as column wrappers (`StrCol<Post>`) vs raw Rust types in the struct.** The struct itself stores the raw Rust types (`String`, `Option<DateTime<Utc>>`, `i64`). The `StrCol`/`IntCol`/`...Col` types are only used in the sibling column module to carry the *column metadata* (name, model) for predicate building. The user never types `StrCol` in their struct definition. This keeps the struct readable as a plain data carrier while still giving the QuerySet API enough type information.

**Attribute syntax via `#[umbral(...)]` vs separate attributes.** A single `#[umbral(...)]` group reads like Django's class-attribute block and lets one attribute parser handle everything. Separate `#[fk(...)]`, `#[unique]`, etc. would require multiple parsers and pollute the attribute namespace.

**Generated `NewPost` insert struct vs in-place struct literal with `id: 0`.** A separate insert struct catches "I forgot to set author_id" at compile time (the field is non-optional). An in-place literal would either need every field optional (defeating the type-system invariant for required columns) or a magic placeholder for `id`. The cost is one extra generated type per model; the win is correctness at the call site.

**Many-to-many with an explicit through-table, mandatory.** Django lets you define a M2M without naming the through-table; the framework generates one. umbral requires the through-table to be a real `#[derive(Model)]` struct. Reason: through-table rows are first-class — they can carry fields like `joined_at` or `weight` — and uniformly treating them as models means the migration engine, admin, and REST plugin all handle them with zero special cases. The cost is one extra struct for the M2M case; the win is "no special path for M2M."

## Open questions

- **Exact attribute parsing for `m2m`, `choices`, and `validators`.** The shape is sketched, but each one has corners — variadic `choices`, validator argument syntax, and `m2m`'s nested form. Resolve at M3 when the derive needs to be implemented.
- **Model inheritance / abstract base classes.** Django supports abstract models that contribute fields without their own table. Useful for timestamped models (`abstract = True`, `created_at`, `updated_at` mixed in). umbral can express this via Rust trait composition or a derive helper. Defer until a real use case lands (likely M9+ for plugin authors who want a timestamped base).
- **Custom user model swap.** The auth outline owns the mechanism; this spec just needs to know that the swap target satisfies `Model`. Resolve in `auth-and-sessions.md`.
- **Computed properties (Django's `@property`).** Rust idiom: just write a method. No framework support needed unless the admin or REST plugin needs to list computed properties as "fields." Revisit when that surfaces.

## Cross-links

- The QuerySet API that uses `post::title`, `post::author`, etc.: `03-orm-querysets.md`.
- `FIELDS` consumers: `05-backends-and-system-check.md` (boot check), `06-migration-engine.md` (snapshot diff).
- `FileField` and `ImageField` storage semantics: outline `static-and-media.md`.
- Validator integration (`#[umbral(validators(...))]`): outline `forms.md` (shared validator catalog) and the `validator` crate.
- Custom user model details: outline `auth-and-sessions.md`.
- Through-table migrations interacting with FK ordering: `06-migration-engine.md`.
