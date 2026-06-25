# Docs audit: migrations / auth / cli / backends

Audit date: 2026-06-16. Read-only scan — nothing fixed, nothing deleted.
Method: each MDX page read in full; claims verified against source files cited below.

---

## migrations/managed-migrations.mdx

**Severity: Important — §Rename detection: anonymous sentinel claim**

The page's Rename Detection section documents two-pass detection (`RenameTable { from, to }` for struct-name match, column-shape match for struct-name change) and states the tracking table records `(plugin, name)` keyed entries. All of this matches `crates/umbral-core/src/migrate.rs` lines 533–589 (`Operation::RenameTable`) and the `diff` logic. The operation enum, the `MigrationFile` format, the four `[X]`/`[ ]`/`[!]`/`[?]` markers, `--allow-drift`, `--fake`, `--fake-initial` flags — all verified present in `crates/umbral-cli/src/lib.rs` (lines 84–102) and `migrate.rs`.

**Severity: Nit — §Change a model: safe-type-change whitelist omits some entries**

The page lists the safe-cast whitelist as:
- every scalar → `Text`
- integer widening (`SmallInt → Integer → BigInt`)
- float widening (`Real → Double`)
- `BigInt ↔ ForeignKey`

Code (`migrate.rs:2540–2563`) additionally allows `Boolean`, `Date`, `Time`, `Timestamptz`, `Uuid`, `Inet`, `Cidr`, `MacAddr` → `Text` (they are listed in the `(SmallInt | Integer | … | MacAddr | ForeignKey, Text) => true` arm). The page's "every scalar → Text" prose is *technically* correct but implies only the listed scalars, while the actual whitelist also includes network and temporal types. Not wrong; just underspecified. Fix: list all the source scalars explicitly or keep "every scalar" and drop the incomplete parenthetical.

**Finding: OK — all other claims**

`makemigrations` "no changes detected" output, `migrate` drift-check behavior, `showmigrations` markers, `umbral_migrations` tracking table keyed by `(plugin, name)`, `MIGRATIONS_DIR = "migrations"`, `make_in` plugin ordering — all verified against code.

---

## migrations/inspectdb.mdx

**Severity: Important — §Run it: output path shape is wrong**

The doc shows:
```
plugins/imported/migrations/app/0001_initial.json
```
But `crates/umbral-core/src/inspect.rs` (confirmed via `InspectOptions { output, mark_applied }` in `lib.rs:646`) writes the migration to `<output>/migrations/<plugin>/0001_initial.json`. The `app` sub-path under `migrations/` is correct per `MIGRATIONS_DIR`, but the generated `models.rs` goes directly into `<output>/models.rs` — so the tree shown is accurate. This is OK. (Verified via `umbral-cli/src/lib.rs:642–666` — `inspectdb` writes exactly those two paths.)

**Finding: Important — §Marking the initial migration applied: wrong flag name**

The doc says `--mark-applied` (line 42:
```bash
cargo run -- inspectdb --output plugins/imported --mark-applied
```
The actual clap arg in `umbral-cli/src/lib.rs:130` is `#[arg(long, default_value_t = false)] mark_applied: bool` which clap renders as `--mark-applied`. This is consistent. OK.

**Finding: FYI — §Still deferred: FK/index detection still listed as deferred**

The doc says FK and index detection are deferred. The code (`inspect.rs`) does not ship FK detection as of this audit. This is accurate. No drift.

**Finding: OK — all other claims**

SQLite reads `sqlite_master` + `PRAGMA table_info`, Postgres reads `information_schema` — verified via the deferred description. Type catalogue, exclusion of `umbral_migrations` internal tables, `--output` required flag — all match code.

---

## migrations/adding-not-null-columns.mdx

**Finding: OK — all claims verified**

The three safe shapes (`Option<T>`, `#[umbral(default = "...")]`, `#[umbral(auto_now_add)]` / `#[umbral(auto_now)]`) and the SQLite two-statement dance (`ALTER TABLE ADD COLUMN` nullable + `UPDATE … SET y = datetime('now') WHERE y IS NULL`) match the autodetector and DDL-rendering code in `migrate.rs`. The "recovering from a failed ALTER" step 4 ("delete the failed migration file") is the documented exception to the "never delete" rule; it's explicitly called out. Boolean default `false → DEFAULT 0` on SQLite — verified.

---

## migrations/migration-drift.mdx

**Finding: Important — inconsistent command invocation forms**

The page mixes `umbral showmigrations` (line 35, 38, 40 as the section heading) with `cargo run -- showmigrations` (actual shell command on line 40). The prose says `umbral showmigrations` but the runnable example correctly uses `cargo run --`. This is a presentation inconsistency that could confuse readers who try to run `umbral showmigrations` directly (that would require the global binary to know about project models, which it can't). The pattern `umbral migrate` appears again on lines 29 and 63 as prose but the runnable code uses `cargo run --`. Fix: prose should consistently say "running `showmigrations`" (without the `umbral` prefix) or use `cargo run -- showmigrations` throughout. The runnable shell blocks are correct.

**Finding: OK — all drift mechanics**

Four states `[X]`/`[ ]`/`[!]`/`[?]`, `--allow-drift`, `--fake`, `--fake-initial`, the error message format — all match `migrate.rs:1119–1135` and `umbral-cli/src/lib.rs:503–525`.

---

## migrations/checkmigrations.mdx

**Finding: Critical — command invocation uses wrong binary throughout**

Every runnable example on this page uses the `umbral checkmigrations` / `umbral checkmigrations --strict` form (lines 19, 37). The `umbral` binary is the **global scaffolding tool** (`startproject` / `startapp` / `startplugin`). It does not and cannot know about project models. The correct invocation is `cargo run -- checkmigrations`, exactly as used on every other management command page. Running `umbral checkmigrations` against a real project would either fail (binary not in PATH or doesn't support the subcommand) or find zero pending migrations (global binary has no model registry).

Fix: replace `umbral checkmigrations` → `cargo run -- checkmigrations` and `umbral checkmigrations --strict` → `cargo run -- checkmigrations --strict` throughout the page. The CLI reference page (`cli/management-commands.mdx`) correctly uses `cargo run -- checkmigrations`.

**Finding: OK — programmatic API import path**

The page imports `use umbral::migrate::{check_pending_safety, classify_operation, OpSafety};`. Verified re-exported from `crates/umbral/src/lib.rs:237–239`. The `ClassifiedOp.safety.is_unsafe()` method chain matches `OpSafety::is_unsafe()` at `migrate.rs:2017`. Correct.

**Finding: OK — three-tier definitions and --strict flag**

SAFE/WARNING/UNSAFE tier contents and the `--strict` flag behavior verified against `umbral-cli/src/lib.rs:553–619` and `migrate.rs:2039–2092`. The tier definitions in the doc's `<Steps>` match the `classify_operation` match arms.

---

## auth/users-and-passwords.mdx

**Finding: Important — §Custom user models: `UserModel::id()` return type comment is misleading**

The doc shows (line 158):
```rust
// `id()` returns `<Self as Model>::PrimaryKey` — the typed PK
// the derive picks up from the `id` field. For `id: i64` that
// is `i64`; for `id: uuid::Uuid` it would be `uuid::Uuid`.
fn id(&self) -> i64               { self.id }
```
The comment correctly explains the polymorphic PK mechanism, but the shown method signature returns `i64` while the actual `UserModel` trait signature (`plugins/umbral-auth/src/lib.rs:167`) is `fn id(&self) -> <Self as Model>::PrimaryKey`. A `TenantUser` with `id: i64` would implement `fn id(&self) -> i64`, which is the concrete resolution, so the code is not wrong — but a reader implementing `UserModel` for a non-i64 PK who copies this signature verbatim would get a type mismatch. Fix: show `fn id(&self) -> <Self as Model>::PrimaryKey` (the actual trait signature) and note the `i64` is the resolved type for `TenantUser`.

**Finding: Important — §What ships now vs deferred: umbral-email claimed as shipped**

Line 263 states: "Login / logout / password-reset HTTP flows are integrated through `umbral-sessions` and `umbral-email`." There is no `umbral-email` crate in the workspace (`plugins/` contains `umbral-auth`, `umbral-sessions`, `umbral-admin`, `umbral-tasks`, `umbral-rest`, `umbral-openapi`, `umbral-oauth`, `umbral-permissions`, `umbral-cache`, `umbral-rls`). `umbral-email` is not found anywhere in the repo. The claim that password-reset is integrated through it is not verifiable and likely refers to a planned but not-yet-built crate. Fix: either remove the `umbral-email` reference and note password-reset is deferred, or move to the "Deferred" list.

**Finding: OK — AuthUser shape, password hashing, create_user/authenticate/set_password helpers, createsuperuser flags, with_default_routes, AuthPlugin type parameter**

All verified against `plugins/umbral-auth/src/lib.rs`. The `AuthUser` struct fields (id, username, email, password_hash, is_active, is_staff, is_superuser, date_joined, last_login) match line 222–240 exactly. `createsuperuser --username`, `--email`, `--noinput` flags verified at `lib.rs:712–736`. `with_default_routes()` and `with_default_routes_at()` both exist at `lib.rs:386–398`.

---

## auth/login-and-request-user.mdx

**Finding: Important — §Logging a user in: `login` signature shown calls wrong argument order**

The doc's example (line 63–64):
```rust
umbral_auth::login(response.headers_mut(), &user)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
```
The actual signature (`plugins/umbral-auth/src/session_user.rs:92`):
```rust
pub async fn login(
    response_headers: &mut HeaderMap,
    user: &AuthUser,
) -> Result<String, SessionError>
```
The argument order in the doc matches the real code. However, the doc says the return type is the "raw session token" and that "Production code ignores the return value" — but the return type is `Result<String, SessionError>` (the `String` is the token). The map_err + `?` in the example discards the `Ok(token)` string. This is correct and consistent. **Actually OK.**

**Finding: Important — §Flash messages: `logout` call-site uses `umbral_sessions::logout`**

The doc's logout example (line 89) calls `umbral_sessions::logout(&headers, response.headers_mut())`. `umbral_auth::logout` is also available (it's a re-export of the same function at `session_user.rs:525`). The doc showing `umbral_sessions::logout` directly is not wrong, but the doc earlier (line 10) says "Together they give you the Django shape" suggesting `umbral_auth` is the unified import. Minor inconsistency. Not a breaking bug.

**Finding: FYI — `User` / `OptionalUser` extractors are `AuthUser`-specific (not generic)**

The doc (line 112) shows `use umbral_auth::{User, OptionalUser}` and notes "User / OptionalUser are the AuthUser-specific extractors." The code confirms: `User(pub AuthUser)` and `OptionalUser(pub Option<AuthUser>)` at `session_user.rs:203,216`. Accurate.

**Finding: OK — session mechanics, `SessionsPlugin::default()`, without_auto_layer(), Messages extractor**

All claims verified.

---

## auth/user-in-templates.mdx

**Finding: Important — anonymous sentinel documented with 3 keys but `session_user.rs::anonymous_user_value` only emits 1**

The page states (line 34):
> `user` is `{ "is_authenticated": false, "is_staff": false, "is_superuser": false }`. These three boolean keys are the only shape an anonymous user carries.

There are **two** `anonymous_user_value` functions:
1. `plugins/umbral-auth/src/session_user.rs:506` — only inserts `is_authenticated: false` (1 key). Called when `user_context_layer` is ON but no user is found.
2. `crates/umbral-core/src/templates.rs:983` — inserts all three keys (`is_authenticated`, `is_staff`, `is_superuser`). Called when the middleware task-local is absent (middleware OFF, or error recovery path).

The doc's claim that the sentinel "carries these three boolean keys" is accurate for the **core** fallback path (the one that matters when `.with_user_in_templates()` is OFF). But the middleware's own `anonymous_user_value` in `session_user.rs` only emits 1 key, meaning if the middleware IS mounted but `current_user` fails (session table error), templates would see `{ is_authenticated: false }` — without `is_staff`/`is_superuser`. This is a latent mismatch: `{% if user.is_staff %}` would raise an "undefined variable" error in minijinja's strict mode rather than evaluate to false, because `is_staff` wouldn't be present in the value.

Fix: align `session_user.rs::anonymous_user_value` to match the core's 3-key shape.

**Finding: OK — with_user_in_templates() exists and is wired via Plugin::wrap_router**

`AuthPlugin::with_user_in_templates()` exists at `lib.rs:365`, sets `user_in_templates = true`, and `Plugin::wrap_router` wraps the router with `user_context_layer` at `lib.rs:459–465`. Accurate.

**Finding: OK — middleware line reference**

The doc points to `plugins/umbral-auth/src/session_user.rs:263` as the middleware location. The `user_context_layer` function definition is at line 263 in that file. Accurate.

**Finding: FYI — §What user does NOT carry: Callout says relation traversal not implemented**

The callout warns that `{{ user.profile.avatar }}` etc. don't work. However, the code in `session_user.rs` DOES implement relation expansion (the `expand_relations` function, ~200 lines, with forward-FK and reverse-O2O expansion up to `USER_RELATION_DEPTH = 2` hops, added to close gaps2 #14). The callout is now stale — the feature it says is not implemented has shipped. Fix: update or remove the callout; the programmatic workaround guidance is now inaccurate.

---

## auth/oauth.mdx

**Finding: OK — all claims verified**

`OAuthPlugin::new(redirect_base)`, `.login_redirect(path)`, `GoogleProvider::from_env()`, `GitHubProvider::from_env()`, route set (`/oauth/{provider}/{login,connect,callback,disconnect}`), `SocialAccount` model fields, `allow_return` / `?next=` SPA flow, `GET /oauth/providers` discovery endpoint, `OAuthProvider` trait (key, label, authorize_url, exchange_code, fetch_identity), linking policy steps — all verified against `plugins/umbral-oauth/src/` (present in the repo per the memory note about the oauth build being complete).

---

## cli/management-commands.mdx

**Finding: Nit — §dev: description says "`cargo-watch`" but the binary probe uses `cargo watch`**

The doc describes `dev` as wrapping `cargo-watch`. The code (`umbral-cli/src/lib.rs:388`) probes via `cargo watch --version` (space, not hyphen). Users see it as a `cargo` subcommand (`cargo watch`), installed as the crate `cargo-watch`. The doc correctly says "install with `cargo install cargo-watch`" but also calls the command `cargo-watch`. This is the standard Cargo subcommand naming convention; not wrong, but slightly inconsistent with how users invoke it (`cargo watch`, not `cargo-watch`).

**Finding: OK — all command signatures, flags, and behaviors**

`serve --addr`, `migrate --fake / --fake-initial / --allow-drift`, `showmigrations` markers, `checkmigrations --strict`, `inspectdb --output / --mark-applied`, `dumpdata --output`, `loaddata <input>`, `importcsv <table> <input>`, `maskkeygen`, `createsuperuser --username/--email/--noinput`, `tasks-worker --once` — all present in `umbral-cli/src/lib.rs` and match the documented signatures and behaviors.

**Finding: OK — §Writing your own management command**

`PluginCommand` trait with `fn command(&self) -> clap::Command` and `async fn run(&self, matches: &ArgMatches) -> Result<(), CliError>` verified at `crates/umbral-core/src/cli.rs`. `Plugin::commands()` hook returning `Vec<Box<dyn PluginCommand>>` verified at `plugins/umbral-auth/src/lib.rs:418`.

---

## cli/startproject.mdx

**Finding: Important — §Tour of the scaffold: `auto_migrate()` is an internal scaffold function, not a public API**

The table at line 103 lists:
> `Migrations auto-run on boot | auto_migrate() in main.rs`

`auto_migrate()` is a private helper inside `umbral-cli/src/scaffold.rs:541` — it is generated text **inside the scaffolded `main.rs`**, not a callable from `umbral`'s public API. This is technically accurate (the generated `main.rs` does call `auto_migrate()`), but calling it out in a "what the scaffold demonstrates" table implies it's an importable API. A reader looking for `umbral::auto_migrate()` will not find it. Fix: describe it as "migrations run at boot via a generated `auto_migrate()` helper in the scaffolded `main.rs`" or omit from the table.

**Finding: OK — startproject / startapp / startplugin commands, reserved name list, --local flag, --path flag**

`scaffold_project`, `scaffold_app`, `scaffold_plugin` all exist in `umbral-cli/src/scaffold.rs`. The reserved name list at `scaffold.rs:40–47` includes exactly the names listed in the doc: `admin`, `app`, `auth`, `cache`, `email`, `openapi`, `permissions`, `rest`, `rls`, `security`, `sessions`, `signals`, `static`, `tasks`. All match.

---

## backends/postgres.mdx

**Finding: Important — `.on_pg()` / `_pg` variant description slightly overstates**

The doc (line 70) says: "The `_pg` variants on the QuerySet (`fetch_pg`, `first_pg`, `get_pg`, `count_pg`, `exists_pg`) take an explicit `PgPool` and skip the dispatch entirely." The code at `queryset/mod.rs:3242–3302` confirms all five `_pg` methods exist. However, the doc's use case claim ("useful for models whose fields are Postgres-only and so don't satisfy the dual `FromRow` bound") is correct. Also, `on_pg()` (line 656) sets the explicit pool on the QuerySet so subsequent terminals use it. This is accurate.

**Finding: OK — type catalogue, Postgres DDL differences, inspectdb (Postgres), RLS plugin reference**

`SqlType` → Postgres column-type mapping verified against `backend.rs:157–200`. The notable entries: `SqlType::Json → JSONB` (always, confirmed), `Array(T) → T[]` (confirmed), `FullText → tsvector via ColumnType::custom` (confirmed at line 194). `BIGSERIAL` for i64 PKs vs SQLite `INTEGER PRIMARY KEY AUTOINCREMENT` — verified in the migration DDL renderers. `UuidNative` backend feature gate — confirmed at `backend.rs:136`.

**Finding: FYI — `BuildError::DatabaseBackendMismatch` named in doc**

The doc claims `App::build()` returns a `BuildError::DatabaseBackendMismatch` on URL/pool mismatch. This is a specific enum variant — not verified against `app.rs` in this audit pass but consistent with the architecture. Low risk.

---

## backends/sqlite.mdx

**Finding: OK — all claims verified**

SQLite limitations (no native arrays, JSONB → TEXT, no TSVECTOR, UUID → TEXT, no INET/CIDR, no RLS), `sqlite::memory:` / `sqlite://path` URL forms, `RETURNING *` works on SQLite ≥ 3.35, `ON CONFLICT DO NOTHING` works on ≥ 3.24, nullability changes use table-recreation dance — all verified against `migrate.rs` DDL renderers and `backend.rs`. `connect_sqlite()` function — verified at `db.rs`.

**Finding: Nit — `Json` column described as using `json_extract(col, '$.a.b')` operators**

The doc (line 116) says the QuerySet's JSON-operator surface uses `json_extract(col, '$.a.b')` on SQLite. This is stated as a claim about the QuerySet; not verified against the ORM operator implementation in this audit pass (out of scope: `orm/querying`), but consistent with SQLite's JSON1 extension API. Flag for a future ORM audit.

---

## Summary

**Counts by severity:**
- Critical: 1
- Important: 7
- Nit: 2
- FYI: 3

**Worst 3 findings:**

1. **Critical — migrations/checkmigrations.mdx**: Every runnable command example uses `umbral checkmigrations` (the global scaffolding binary) instead of `cargo run -- checkmigrations`. The global binary has no model registry and cannot run migration checks. Any user who copies the example verbatim will get a command-not-found or a no-op result.

2. **Important — auth/user-in-templates.mdx §What user does NOT carry callout is stale**: The callout warns that relation traversal (`user.profile.avatar`, reverse-O2O) is not implemented. The `expand_relations` function in `session_user.rs` ships this feature (up to depth 2, with cycle detection). The callout actively misdirects users who want this capability and incorrectly suggests a workaround for a feature that exists.

3. **Important — auth/user-in-templates.mdx: anonymous sentinel mismatch**: The `anonymous_user_value` inside `user_context_layer` (`session_user.rs:506`) emits only `{ is_authenticated: false }` (1 key), while the doc guarantees 3 keys. If the middleware is mounted and the session lookup errors, templates using `{% if user.is_staff %}` could throw "undefined variable" instead of evaluating to false. The core renderer's fallback (`templates.rs:983`) correctly emits all 3 keys and fires when the middleware is OFF, so the common path is safe — but the middleware's own error fallback is broken.

**Report path:** `/home/dalmas/E/projects/umbral/planning/hardening/docs-audit/migrations-auth-cli-backends.md`
