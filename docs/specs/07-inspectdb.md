# 07 — `inspectdb`: introspect an existing database into models

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M6 (introspect existing DB → models that feed straight into the M5 migration engine) |
| **Companions** | `00-overview.md`, `04-orm-model-and-fields.md`, `06-migration-engine.md`, `arch.md §0`, `umbral-PRD.md §10` (phase 0.2 — "Porting MVP") |

## Purpose

The porting payoff. A team running Django, Rails, Node, or anything else with a Postgres database points umbral at the existing DB and gets:

1. **Rust model files** — one `#[derive(Model)] struct` per table, with the right field types, options, and relations.
2. **An initial migration file** — `0001_initial.json` that, applied to an empty database, would recreate the introspected schema.
3. **A pre-marked applied state** — the migration is recorded in the tracking table as applied without running, so the next `migrate` is a no-op until the user actually changes a model.

After that, the team is on the same declare → migrate → change → migrate loop as a greenfield project. There's no parallel porting code path; the introspected schema simply enters the M5 engine at "step 0."

What this spec owns:

- The **introspection step** (sea-schema → an intermediate `SchemaSnapshot`).
- The **DB-type → Rust-type mapping table**.
- **Name resolution** for tables, columns, relations, and plugin assignment.
- **Conflict resolution** when an introspected name collides with a registered built-in plugin's table (e.g. `auth_user`).
- The **output shape** (where files land, how the generated migration is marked applied).

**What shipped at M6 v1.** SQLite-only introspection via `PRAGMA table_info`, the type-mapping subset that matches the M5 `SqlType` catalogue (integers, floats, bool, text, date / time / timestamptz, uuid, plus their nullable variants), and a flat output: one `models.rs` with `#[derive(Model)]` structs and one `migrations/0001_initial.json`. The `--mark-applied` flag inserts a row into `umbral_migrations` so a follow-up `migrate` is a no-op. The user wires the generated `models.rs` into their binary by hand (`mod models;` plus one `.model::<T>()` per struct on the builder) until the plugin contract lands.

**Deferred to later milestones.**

- Postgres introspection (needs the M4 backend abstraction to grow an `introspect` hook) — slated for the same milestone that adds the Postgres backend body.
- Generated plugin crate (`lib.rs` with `impl Plugin`, `Cargo.toml`) — needs the M7 plugin contract.
- Conflict resolution against registered plugins' migrations — needs the M7 plugin contract.
- Foreign-key detection, M2M heuristics, `#[umbral(max_length = n)]` rendering for `VARCHAR(n)`, NUMERIC / JSON / BYTEA / array / custom-type mappings — gated on the field types existing in `umbral-core`; today's catalogue covers the scalars and date / time / uuid only.
- `--strip-prefix`, `--ignore-builtin`, `--plugin <name>` flags — deferred with the plugin-crate output they're attached to.

The "spec owns" list above is the eventual target shape. Drift between today's code and that shape is intentional and tracked here; the same way `06-migration-engine.md` calls out the M5 / M8 split for column-level ops.

What this spec **does not** own:

- The mechanics of applying a migration. That's `06-migration-engine.md`.
- The `Model` trait or field types themselves. That's `04-orm-model-and-fields.md`.

## Concepts

### What gets introspected

For each table:

- Columns (name, SQL type, nullable, default, primary key membership).
- Indexes (single and composite; unique vs non-unique).
- Foreign keys (column, target table, target column, on-delete behavior).
- Check constraints (best-effort; surfaced as comments on the field if not directly representable).

sea-schema does the heavy lifting. The result is an intermediate `IntrospectedSchema` value the rest of the pipeline walks.

### DB-type → Rust-type mapping

| Postgres column type | Rust field type | Notes |
|---|---|---|
| `BIGINT`, `BIGSERIAL` | `i64` | `BIGSERIAL` is treated as `i64` with `default = autoincrement`. |
| `INTEGER`, `SERIAL` | `i32` | |
| `SMALLINT` | `i16` | |
| `REAL` | `f32` | |
| `DOUBLE PRECISION` | `f64` | |
| `BOOLEAN` | `bool` | |
| `TEXT`, `VARCHAR(n)` | `String` | `VARCHAR(n)` adds `#[umbral(max_length = n)]`. |
| `CHAR(n)` | `String` | Same, with a comment noting the original was fixed-width. |
| `TIMESTAMPTZ` | `chrono::DateTime<Utc>` | |
| `TIMESTAMP` (no TZ) | `chrono::NaiveDateTime` | The mapping flags this in the generated comment because TZ-naive columns are an ambiguity source. |
| `DATE` | `chrono::NaiveDate` | |
| `TIME` | `chrono::NaiveTime` | |
| `NUMERIC(p, s)`, `DECIMAL(p, s)` | `rust_decimal::Decimal` | Precision and scale recorded in a comment. |
| `UUID` | `uuid::Uuid` | |
| `JSON`, `JSONB` | `serde_json::Value` | |
| `BYTEA` | `Vec<u8>` | |
| `TEXT[]`, `INT[]`, etc. | `Vec<T>` | Generates `#[umbral(supported_backends = ["postgres"])]` implicitly. |
| `HSTORE` | `HashMap<String, String>` | Same. |
| `CIDR`, `INET`, `MACADDR`, `TSVECTOR`, custom types | `String` plus a `// TODO: native type` comment | Mapped to string with a comment so the user can manually upgrade once umbral grows native support. |

Nullable columns wrap the Rust type in `Option<>` (the only path; see `04-orm-model-and-fields.md` §Nullable invariant).

### Name resolution

Default rule: table names become struct names by stripping a configurable plugin prefix (default: none) and converting to UpperCamelCase. Column names become field names as snake_case (typically identity).

Examples (no prefix stripping):

- `post` → `struct Post`
- `blog_post` → `struct BlogPost`
- `auth_user_groups` → `struct AuthUserGroups`

With `--strip-prefix blog_`:

- `blog_post` → `struct Post`
- `blog_post_tag` → `struct PostTag`
- `post` (no prefix) → `struct Post` (collision; see §Conflict resolution)

The `--plugin <name>` argument scopes the generated models under a plugin (which is the unit of organisation per `02-plugin-contract.md`). All introspected models go into one plugin's directory; the user can rearrange after.

### Conflict resolution

Three kinds of conflict surface:

1. **Generated struct name collides with itself.** Two tables map to the same struct name (after prefix stripping). The importer aborts with a clear error and the user re-runs without `--strip-prefix` or chooses a different stripping rule. M6 does not try to disambiguate automatically; a wrong guess silently shadows the user's intent.

2. **Introspected table collides with a registered built-in plugin's table.** Most commonly: `auth_user`, `django_session`, `auth_permission`. The importer checks every registered plugin's `Plugin::migrations()` for an introspected table name and:
   - If the column shape matches the built-in plugin's expected shape: marks the table as "owned by the built-in plugin" and does not generate a model file for it; the built-in plugin's migration is marked applied. This is the "port a Django app to umbral-auth" path.
   - If the shape doesn't match (extra columns, missing columns, different types): aborts with a diff explaining what doesn't match. The user can re-run with `--ignore-builtin auth` to put the introspected table under the imported plugin instead.

3. **Cross-table FK target not found.** A FK in `post.author_id` references `author`, but no `author` table exists in the introspection. This means the user's database has dangling references; the importer surfaces them as warnings and generates the FK column as a plain `i64` with a comment.

### Output layout

```
my-app/
└── plugins/
    └── imported/                          # the plugin the importer creates (name configurable)
        ├── src/
        │   ├── lib.rs                     # Plugin impl
        │   ├── models.rs                  # generated #[derive(Model)] structs
        │   └── meta.rs                    # any constants the importer wants the user to see
        └── migrations/
            └── 0001_initial.json          # generated migration, snapshot_after captures the imported state
```

The plugin `lib.rs` is a minimal `Plugin` impl that wires the generated models in and returns `migrations()` from the `migrations/` directory. The user adds it to their `App::builder()` call:

```rust
App::builder()
    .plugin(ImportedPlugin::default())
    // …
    .build()?;
```

Subsequent `cargo run -p umbral-cli -- migrate` does nothing (the migration was marked applied). When the user changes a model, `makemigrations` produces `0002_xxx.json` against the imported snapshot, and the loop runs normally.

## API-shape sketch

CLI surface:

```
cargo run -p umbral-cli -- inspectdb \
    --plugin imported \
    --output plugins/imported \
    [--strip-prefix blog_] \
    [--ignore-builtin auth] \
    [--mark-applied]
```

| Flag | Effect |
|---|---|
| `--plugin <name>` | The umbral plugin name to assign generated models to. Default: `imported`. |
| `--output <path>` | Where to write the generated crate. Default: `plugins/<plugin>`. |
| `--strip-prefix <p>` | Strip `p` from table names before generating struct names. Optional. |
| `--ignore-builtin <plugin>` | Don't try to map introspected tables onto this built-in's schema; just generate models for them. Optional, repeatable. |
| `--mark-applied` | Record `0001_initial` in the tracking table as applied without running it. Default `true` in a database that already has tables; off if the target is empty. |

Internally, the pipeline:

```rust
pub async fn inspectdb(opts: InspectOptions) -> Result<InspectReport> {
    let schema = sea_schema::postgres::DiscoverState::discover(&opts.database_url).await?;
    let intermediate = build_intermediate(&schema, &opts)?;
    let conflicts = check_conflicts(&intermediate, registered_plugins())?;
    if !conflicts.is_empty() { return Err(InspectError::Conflicts(conflicts)); }

    let model_files = generate_model_files(&intermediate, &opts)?;
    let initial_migration = generate_initial_migration(&intermediate)?;
    let plugin_lib = generate_plugin_lib(&opts.plugin)?;

    write_outputs(&opts.output, model_files, initial_migration, plugin_lib).await?;

    if opts.mark_applied {
        umbral::migrations::record_applied(&opts.plugin, "0001_initial").await?;
    }

    Ok(InspectReport { /* counts of tables, columns, FKs, warnings */ })
}
```

The intermediate value (`IntermediateSchema`) is a list of model descriptors. Each descriptor has:

- A struct name (post-resolution).
- A `TABLE` (the original table name; the rendered `#[umbral(table = "…")]` may differ if the resolution stripped a prefix).
- Field descriptors (Rust type, attributes, comments).
- Relation descriptors (FK and inferred M2M relationships).

`build_intermediate` is where the mapping table lives. `generate_*` are mechanical translations from the intermediate to file contents.

## Mechanics and invariants

### Generated migration is bit-for-bit a regular migration

The output of `generate_initial_migration` is a `0001_initial.json` exactly like one written by `makemigrations`. The same `snapshot_after` shape. The same operation list (one `CreateTable` per imported table, plus the `AddIndex` and `AddForeignKey` operations as the schema requires). The migration engine doesn't know or care that it came from `inspectdb`.

This is what makes "imported schema enters the M5 loop seamlessly" true. There's no separate inspectdb-applied table or porting code path; the imported state is a regular applied migration.

### `--mark-applied` semantics and safety

Marking applied is the right default when introspecting against a non-empty database, because the tables already exist. Running the migration there would fail (tables already present).

The flag is **off** if the target connection points at an empty database. The user explicitly asks "I want the inspected migration applied" by running `migrate` against that empty target. The importer can detect "empty database" by checking that no tables other than `umbral_migrations` exist.

The drift detection from `06-migration-engine.md` protects this path: if a user marks 0001_initial as applied and later edits the file, the next `migrate` run refuses to proceed with `MigrationError::DriftDetected`.

### Many-to-many detection

A table with exactly two FK columns (and optionally trivial metadata like `joined_at`, `weight`) is heuristically classed as a through-table. The importer:

1. Generates the through-table model normally.
2. Emits a comment on each of the FK-target models suggesting `#[umbral(m2m(...))]`.

The user accepts or rejects the suggestion by editing the generated code. M6 does **not** auto-write the m2m attribute; the heuristic is too brittle to make a hard decision (a real model with two FKs and no other state — say, a "vote" table — would be miscategorised).

### Comments preserve information that can't survive the type system

Things the mapping table marks "+ comment":

- Original `CHAR(n)` width (Rust `String` doesn't carry it).
- Original `TIMESTAMP` without time zone (the explicit choice to use a TZ-naive type, which is usually a bug).
- Original `NUMERIC(p, s)` precision and scale.
- Custom Postgres types (CIDR, TSVECTOR, etc.) that mapped to `String`.
- Check constraints whose semantics didn't fit `#[umbral(choices(...))]`.

Comments live in the generated code as `// inspectdb: original was TIMESTAMP (no TZ); consider using TIMESTAMPTZ`. They're a TODO list for the user to walk after import.

## Trade-offs and alternatives considered

**One initial migration vs N per-table migrations.** A single `0001_initial.json` is easier to reason about and matches Django's `inspectdb` behavior. The trade-off is that a partial-import error rolls back all of `0001_initial` in one transaction; for a database with thousands of tables, that's a lot to be inside one tx. Defer to per-table fragments only if a real workload runs into the limit. Spec defaults to one file.

**Native vs fallback type mapping.** The table picks the most precise Rust type that has stable representation. `UUID` is `uuid::Uuid`, not `String`, even though `String` would work; later code (admin forms, REST serializers) gets to use the precise type without explicit conversion. The cost is the user needing the `uuid` crate as a dep; the importer notes this in a `README.md` it generates next to the plugin.

**Heuristic M2M detection as comment-only vs auto-attribute.** A wrong auto-detection silently changes the data model. A comment is a TODO the user accepts knowingly. The cost is a few seconds of manual review; the benefit is no silent miscategorisation.

**`--strip-prefix` as a hint vs full smart-name resolution.** A "smart" resolver would try to infer plugin boundaries from table-name prefixes (e.g. `blog_*` → one plugin, `auth_*` → another). That's a bigger inference than M6 wants to take on. `--strip-prefix` keeps the user in control; running the importer multiple times with different prefixes is fine.

**Generate a full crate (with `Cargo.toml`, `src/lib.rs`) vs a directory drop-in.** Full crate generation reads as a higher-confidence output: the user can `cargo build` immediately and see it compile. It's more verbose but matches the user's expectation of "I ran inspectdb, here's a working starter."

## Open questions

- **Plugin partitioning.** If the existing database has natural plugin boundaries (Django apps with table-name prefixes), the user might want one umbral plugin per Django app. M6 ships with one-plugin output; M8 or later could add a `--partition-by-prefix` mode that emits multiple plugins. Defer until users ask.
- **View introspection.** Postgres views could be modelled as read-only umbral models. Defer to a follow-up — they're rare in the porting target.
- **Custom-type extensibility.** Today the mapping table is hard-coded. A future hook (a config option or a derive registration) could let users plug in their own (Postgres custom-type → Rust type) entries. Defer.
- **Detecting auth model shape against `umbral-auth`'s default `User`.** §Conflict resolution describes the "shapes match" check; the exact heuristic (which columns are required, which are bonuses) needs to be pinned to whatever shape `auth-and-sessions.md` settles on. Resolve when the auth outline gets promoted.

## Cross-links

- The migration engine that consumes the generated `0001_initial.json`: `06-migration-engine.md`.
- The `FieldSpec` and field types the introspected columns translate to: `04-orm-model-and-fields.md`.
- The plugin shape the importer generates: `02-plugin-contract.md`.
- The auth plugin's expected `User` shape (for the built-in collision case): outline `auth-and-sessions.md`.
- The PRD's "Porting MVP" phase that this spec is the centrepiece of: `umbral-PRD.md §10`.
- The north star this spec services: `arch.md §0`.
