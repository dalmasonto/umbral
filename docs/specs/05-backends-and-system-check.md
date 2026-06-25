# 05 — Database backends and the boot system check

| | |
|---|---|
| **Status** | Draft |
| **Maps to milestone** | M4 (backend abstraction + boot-time system check) |
| **Companions** | `01-app-and-settings.md`, `02-plugin-contract.md`, `04-orm-model-and-fields.md`, `arch.md §5` |

## Purpose

Two things, joined at the hip:

- **`DatabaseBackend`**: the trait that abstracts dialect differences (type mapping, identifier quoting, `RETURNING` support, upsert syntax) over the top of sea-query (dialects) and sqlx (drivers). The backend is the bridge between umbral's column metadata and what the database can actually do.
- **The system check**: a boot-time validation pass that walks every registered model, plugin, and route, and fails loudly with a clear message if anything is incompatible with the active backend. Examples: a model uses `ArrayCol` on SQLite; two plugins both want to mount at `/auth`; a FK targets a non-existent model.

The whole spec exists to move classes of bug from runtime to startup. The cost is some boot-time work; the win is "you cannot deploy a configuration that would fail at query time."

## Concepts

### `DatabaseBackend`

```rust
pub trait DatabaseBackend: Send + Sync + 'static {
    fn name(&self) -> &'static str;                // "postgres" | "sqlite" | "mysql"
    fn dialect(&self) -> sea_query::DbBackend;     // for sea-query rendering

    fn supports(&self, feature: BackendFeature) -> bool;

    fn quote_identifier(&self, ident: &str) -> String;
    fn map_type(&self, ty: &SqlType) -> SqlColumnType;
    fn render_upsert(&self, table: &str, conflict_cols: &[&str], update_cols: &[&str]) -> String;
}

pub enum BackendFeature {
    InsertReturning,         // RETURNING clause on INSERT
    UpsertOnConflict,        // ON CONFLICT DO ... or equivalent
    ArrayColumns,
    HStoreColumns,
    JsonbColumns,            // jsonb-shape with indexing, not just text-encoded JSON
    FullTextSearch,
    CidrInet,
    UuidNative,              // native UUID type vs TEXT-encoded
    Boolean,                 // native bool vs SMALLINT-encoded
}
```

The umbral-shipped backends:

| Backend | `name()` | sea-query dialect | What's special |
|---|---|---|---|
| `PostgresBackend` | `"postgres"` | `PostgresQueryBuilder` | Default. Supports every `BackendFeature`. |
| `SqliteBackend` | `"sqlite"` | `SqliteQueryBuilder` | Tests and small dev. `bool` is `INTEGER`; `UUID` is `TEXT`; arrays and HStore unsupported. |

MySQL is **not** shipped. PRD §14 lists it as out of scope; the trait leaves room for a future `MySqlBackend` without restructuring.

### Field-side declaration

`FieldSpec` (from `04-orm-model-and-fields.md`) carries a `supported_backends` slice:

```rust
pub struct FieldSpec {
    // ... other fields ...
    pub supported_backends: &'static [&'static str],   // empty = all backends; non-empty = only these
}
```

Most field types leave it empty (portable). Backend-specific fields declare their support set:

```rust
impl<T: Element> ArrayCol<T> {
    pub const FIELD_SPEC: FieldSpec = FieldSpec {
        // ...
        supported_backends: &["postgres"],
    };
}
```

That's the single place the constraint is recorded. Adding a new Postgres-only field type means setting `supported_backends: &["postgres"]` on the field; everything else follows automatically.

### The system check

```rust
pub struct SystemCheck {
    pub id: &'static str,                  // e.g. "field.backend"
    pub run: fn(&CheckContext) -> Vec<SystemCheckFinding>,
}

pub struct SystemCheckFinding {
    pub check_id: &'static str,
    pub severity: Severity,                // Error or Warning
    pub location: CheckLocation,           // (Plugin name, Model name, Field name, Route path, ...)
    pub message: String,
    pub hint: Option<String>,
}

pub enum Severity { Error, Warning }
```

The check phase (phase 4 in `01-app-and-settings.md`'s lifecycle) collects:

1. **Framework-built-in checks** (always run).
2. **Per-plugin checks** from each `Plugin::system_checks()` (from spec 02).

It runs every check, accumulates findings, and:

- If any finding is `Severity::Error`, returns `BuildError::SystemCheckFailed { findings }`. `App::builder().build()` does not return `Ok` — the app cannot start.
- If findings are only `Severity::Warning`, it logs them via `tracing::warn!` and proceeds.

This is the whole boot gate. Past it, every type and route is known-compatible with the active backend.

## API-shape sketch

The built-in field-backend check, in spirit:

```rust
pub const FIELD_BACKEND_CHECK: SystemCheck = SystemCheck {
    id: "field.backend",
    run: |ctx| {
        let mut findings = vec![];
        let backend_name = ctx.backend.name();

        for plugin in ctx.plugins() {
            for model_meta in plugin.models() {
                for field in model_meta.fields {
                    if !field.supported_backends.is_empty()
                        && !field.supported_backends.contains(&backend_name)
                    {
                        findings.push(SystemCheckFinding {
                            check_id: "field.backend",
                            severity: Severity::Error,
                            location: CheckLocation::Field {
                                plugin: plugin.name(),
                                model: model_meta.name,
                                field: field.name,
                            },
                            message: format!(
                                "field `{}.{}` uses a type that supports {:?}, but the active backend is `{}`",
                                model_meta.name, field.name, field.supported_backends, backend_name,
                            ),
                            hint: Some(format!(
                                "switch to a portable field type, or change DATABASE_URL to a `{}` instance",
                                field.supported_backends[0],
                            )),
                        });
                    }
                }
            }
        }
        findings
    },
};
```

The actual implementation registers a handful of these constants and walks them at boot. Plugins add their own checks via `Plugin::system_checks()`.

## Mechanics and invariants

### Which checks ship as built-in

| Check id | What it verifies |
|---|---|
| `field.backend` | Every field's `supported_backends` includes the active backend. |
| `field.fk.target_exists` | Every FK names a model that's registered in some plugin. |
| `model.table.unique` | Two models don't claim the same `TABLE` name. |
| `model.pk.present` | Every model has exactly one primary key. |
| `plugin.dependency.exists` | Every name in `Plugin::dependencies()` is the `name()` of a registered plugin. |
| `plugin.dependency.acyclic` | No dependency cycles. |
| `route.collision` | Two plugins don't claim the same route shape (e.g. both register `GET /auth/login`). |
| `settings.required` | Required settings have values (e.g. `secret_key` is non-empty in `Prod`). |

These are the framework's baseline. Each is one `SystemCheck` constant with a stable id so users can grep for failures.

### Boot order rebuilt

`01-app-and-settings.md` says the system check is phase 4 of `App::builder().build()`. Re-stated here so this spec is the source of truth for what runs *inside* that phase:

1. Construct the `CheckContext` (handles to the backend, the plugin list, the assembled router shape).
2. Walk the framework-built-in checks in a fixed order (the order above).
3. Walk each plugin's `system_checks()` in plugin-dependency order.
4. Partition findings by severity.
5. If any `Error`: return `BuildError::SystemCheckFailed`. Otherwise: log warnings, proceed.

### Plugin-contributed checks

A plugin returns extra checks via `system_checks()`:

```rust
impl Plugin for AuthPlugin {
    fn system_checks(&self) -> Vec<SystemCheck> {
        vec![
            SystemCheck {
                id: "auth.user_model.has_password_field",
                run: |ctx| { /* check the active user model has a password field */ },
            },
        ]
    }
}
```

This is how the custom-user-model contract is enforced: the auth plugin's check refuses to let the app boot if the registered user model lacks the fields auth depends on.

### Backend feature gating in non-field paths

Code paths that depend on a backend feature query it via `backend.supports(feature)`:

```rust
if ctx.backend.supports(BackendFeature::InsertReturning) {
    // build INSERT ... RETURNING *
} else {
    // build INSERT then SELECT last_insert_rowid()
}
```

The QuerySet code uses this for `create()`. Plugins that want to use Postgres-specific SQL features wrap their use behind a `backend.supports(...)` check; the system check can't enforce this from the outside.

## Trade-offs and alternatives considered

**One `DatabaseBackend` trait vs separate traits per concern (`Dialect`, `Quoting`, `UpsertRenderer`).** Splitting would let alternate backends mix-and-match implementations, but in practice a backend is a unit and umbral ships exactly two. A single trait keeps the type bounds where backends are passed around simple.

**`BackendFeature` enum vs duck-typed `cfg(postgres)`.** A typed enum makes the supported set queryable at runtime, supports plugin code that needs runtime branching, and surfaces in the system check as a structured value. `cfg(postgres)` would be compile-time only and force the framework to ship two builds. Runtime branching wins on flexibility; the perf cost of one branch on a backend method is negligible.

**Boot system check as one phase vs interleaved with build.** Interleaving (fail as soon as one model fails its check) would short-circuit faster, but cumulative reporting is more useful: a user fixing five field-backend errors prefers one boot run that lists all five over five sequential boot runs that show one each.

**Errors-vs-warnings line.** Errors block boot. Warnings only log. The default is `Error`; warnings are reserved for things like "you used `null=True` but never write NULL anywhere," which are advisory but not breaking. Plugins use `Warning` sparingly so the log doesn't become noise.

**Should backends live in `umbral-core` or a separate `umbral-backends` crate.** Today: in `umbral-core`. The two backends are inherently part of "what's a query"; carving them out adds a crate boundary that pays for nothing. Revisit if/when a non-Postgres-non-SQLite backend lands.

## Open questions

- **MySQL backend.** Out of scope for the first iteration (PRD §14). The trait leaves room for a future `MySqlBackend`; whether it ever ships depends on real demand.
- **Custom user-supplied backends.** Today the registry is closed (Postgres + SQLite). A trait object slot in `App::builder()` could let users register their own. Defer until a real ask appears.
- **`BackendFeature::UuidNative` semantics.** A Postgres `UUID` column is native; a SQLite `TEXT` UUID is encoded. The QuerySet code needs to know which encoding is in play. Either the backend handles it transparently in `from_row`, or `UuidCol` consults the backend. Pick at M4.
- **Warning collection in `tracing::warn!` vs a separate diagnostics file.** Warnings as logs is fine in dev; production might want them rolled up into a one-shot report. Defer.

## Cross-links

- `FieldSpec` (the thing the field-backend check walks): `04-orm-model-and-fields.md`.
- `Plugin::system_checks()`: `02-plugin-contract.md`.
- Where the check phase fits in the boot lifecycle: `01-app-and-settings.md` §Lifecycle phases.
- `BuildError::SystemCheckFailed` surfaces here; the type lives in: `01-app-and-settings.md`.
- The MySQL-out-of-scope rationale: PRD §14 and `arch.md §5`.
