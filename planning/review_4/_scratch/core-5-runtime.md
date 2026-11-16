# review_4 · umbral-core — runtime + contracts slice (slice 5 of 5)

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | ≈9.7k LOC — 19 src files of `crates/umbral-core/src` (lib/plugin/check/db/route_context/router/backend/settings/cli/codegen/typegen/rt/shutdown/timezone/ratelimit/cache/auth_contract/validate/storage), plus skimmed crate tests (`settings*`, `check*`, `plugin_*`, `typegen*`, `cli*`, `timezone*`, `ratelimit*`, `pool*`, `db_pool*`, `system_check*`) |
| **Reviewer** | core-5 runtime (slice file — assembler renumbers RUN-n → CORE-n) |
| **Verdict** | minor work |
| **Findings** | 0 critical · 1 high · 5 medium · 9 low · 2 info — 17 new, 0 known |

## Summary

- This slice is the framework's backbone: the Plugin trait + AppContext, the ambient `DbPool` registry with its four transaction runners, settings/env parsing with the trusted-proxy policy, the boot system-check catalogue, the codegen/typegen seams, and the small runtime utilities (rt, shutdown, timezone, ratelimit, cache, auth_contract, storage, validate).
- Overall state is healthy: no `unsafe`, no request-path panics beyond documented boot-time `expect`s, the historic security fixes hold (session-var reset gate uses `Release`/`Acquire`; `client_ip` resolves hops right-anchored so spoofed leftmost XFF is ignored; `RESET ALL` is scoped to umbral GUC names).
- The one HIGH is a silent-drift generator bug: `typegen` emits the SQL **column** name as the TypeScript property, but the serde wire name is the Rust **field** name — so every model using the documented `#[sqlx(rename)]` attribute gets a `.ts` file whose keys don't exist on the wire. The same wrong-name assumption is live in `umbral-openapi` (routing note below).
- The MEDIUMs are bounded: four hand-copied transaction runners (protocol defined 4×, rationale comment in 2 of 4), `db::close()` never closing runtime tenant pools, an unbounded warn-dedupe set in `timezone.rs` (the exact class `ratelimit.rs` already fixed), `ensure_dependency` missing the dotted `name.workspace = true` manifest spelling, and the CLI reporting a typo'd flag on a valid command as "unknown command".
- The LOWs cluster into two families: documentation drift (four stale/misattached rustdoc sites, incl. `ratelimit`'s module doc contradicting its own shipped sweep) and small drift/robustness nits (swallowed `.env` parse errors, a Debug omission, an interactive-prompt underflow, poison-policy asymmetry, an unpinned `is_postgres_only` mirror).
- Fix first: RUN-1 (wrong generated types ship to frontends), then RUN-3 (shutdown promise broken for tenant DBs) and RUN-6 (misleading first-contact error for every plugin command).

## Findings

### RUN-1 · HIGH · correctness / codegen contract · self-verified
**`typegen` writes the SQL column name as the TypeScript property, but the serde wire name is the Rust field name — every `#[sqlx(rename)]` model generates a `.ts` interface whose keys don't exist on the wire.**
- **Where**: `crates/umbral-core/src/typegen.rs:266` (emission), `crates/umbral-core/src/typegen.rs:16-19` (the claim it breaks), `crates/umbral-macros/src/lib.rs:1804-1808`, `crates/umbral-macros/src/lib.rs:2411`, `crates/umbral-core/src/orm/model.rs:565-568`, `crates/umbral-core/src/migrate.rs:1504-1507`
- **Evidence**:
  ```rust
  // typegen.rs:266 — the property name comes from `Column.name` …
  let _ = writeln!(out, "  {}: {ty};", column.name);

  // typegen.rs:16-19 — … while the module doc promises the serde wire shape:
  //! The JSON umbral-rest puts on the wire, which is the model struct serialised
  //! by serde. Concretely:
  //!
  //! - Field names stay exactly as declared. No camelCase rewrite: the wire is
  //!   snake_case, and a generated type that quietly renames fields is a lie.

  // umbral-macros/src/lib.rs:1805-1808 — `#[sqlx(rename)]` makes the two diverge:
  // remaps it (which `FromRow` honours, so the migration + column const
  // must match). `field_name_str` stays the Rust-facing identity (const
  // name, serde/relation keys); `column_name_str` is the SQL identity.
  let column_name_str = sqlx_rename(&field.attrs).unwrap_or_else(|| field_name_str.clone());

  // umbral-macros/src/lib.rs:2411 — that SQL identity is what FieldSpec carries:
  name: #column_name_str,
  ```
  The chain: `FieldSpec.name` is the column name (its own doc, `model.rs:565-567`, is stale — see out-of-scope 2) → `Column::from(&FieldSpec)` copies it verbatim (`migrate.rs:1505` `name: f.name.to_string()`) → `ModelMeta.fields` → typegen's `push_field(…, column: &Column, …)` writes `column.name`. `FieldSpec` carries **no** Rust-field-name member, and the `Model` derive emits no `#[serde(rename)]` for models (the 5 `serde(rename` sites in the macro are the Choices and Dto derives only), so serde serialises the Rust field name while typegen emits the column name.
- **Impact**: Model `Post { #[sqlx(rename = "author_id")] pub author: ForeignKey<User>, … }` — the exact shape documented in `documentation/docs/v0.0.1/orm/column-types.mdx:192-211` and emitted by `inspectdb` for keyword-colliding columns (`inspect.rs:1816`, e.g. `#[sqlx(rename = "type")] pub type_: String`). `umbral typegen` emits `author_id: number;` while the wire JSON carries `"author": 5`. The frontend then either fails to type-check on `post.author` or reads `post.author_id` and gets `undefined` at runtime — no error anywhere. `crates/umbral-core/tests/sqlx_rename_column.rs` proves the divergence at the metadata level; no typegen test (`tests/typegen.rs`, `typegen_registry.rs`, `dto_typegen.rs`) uses a renamed field, so nothing catches it.
- **Fix**: carry the wire name in metadata — add `rust_name: &'static str` (a.k.a. `wire_name`) to `FieldSpec` and `Column`, populated from `field_name_str` at `umbral-macros/src/lib.rs:2411`; switch typegen's property name (and `Resolver::pk_name`) to it, leaving `Column.name` as the SQL identity for DDL/filtering. Mirror in `umbral-openapi` (out-of-scope 1). Add a typegen test over a `#[sqlx(rename)]` model asserting the emitted key is the field name.

### RUN-2 · MEDIUM · drift · self-verified
**The commit→fire-signals transaction protocol is hand-copied in four runners; the bodies are identical except the `begin*` call, and the gaps6 #15 rationale comment already survives in only two of four.**
- **Where**: `crates/umbral-core/src/db.rs:1058-1082` (`transaction`), `1098-1121` (`transaction_on`), `1129-1147` (`transaction_sqlite`), `1157-1175` (`transaction_pg`)
- **Evidence**:
  ```rust
  // db.rs:1064-1069 (and byte-for-byte equivalents at 1104-1109, 1135-1140, 1163-1168)
  match f(&mut tx).await {
      Ok(val) => {
          let pending = tx.take_pending_signals();
          tx.commit().await.map_err(E::from)?;
          for sig in pending {
              sig().await;
          }
          Ok(val)
      }
  // db.rs:1076-1078 vs db.rs:1143-1146 — same behavior, drifting commentary:
          // Best-effort rollback — if it fails we surface the original error.
          // The buffered signals are never drained here, so they're simply
          // dropped with `tx` — a rolled-back write fires nothing (gaps6 #15).
          let _ = tx.rollback().await;          // sqlite/pg variants: bare, no comment
  ```
- **Impact**: The post-commit signal-firing contract (signals buffered during the transaction fire only after a successful commit; a rollback fires nothing) is the safety rule that keeps `create`/`delete` signals from lying. It is implemented four times; a future change applied to `transaction` only (e.g. firing a failure signal on commit error, or draining pending signals before rollback for logging) silently leaves `transaction_on`/`transaction_sqlite`/`transaction_pg` divergent — the tree's signature drift mode. The comment loss is the canary: two copies already lost their rationale.
- **Fix**: extract one generic helper (`async fn run_in_tx(tx: Transaction, f) -> …`) that owns the take-commit-fire / rollback-drop protocol; the four public fns become `begin*().await? → run_in_tx(tx, f)` one-liners. Signals fire once, in one place.

### RUN-3 · MEDIUM · resource lifecycle
**`db::close()` iterates only build-time `POOLS`; runtime tenant pools registered via `register_tenant_pool` are never closed on graceful shutdown, while the caller logs "draining DB pools".**
- **Where**: `crates/umbral-core/src/db.rs:773-782` (`close`), `db.rs:141-150` (`DYNAMIC_POOLS`, `Box::leak`'d `&'static`), consumer `crates/umbral-core/src/app.rs:214-215`
- **Evidence**:
  ```rust
  // db.rs:773-782 — closes POOLS only; DYNAMIC_POOLS is never read here
  pub async fn close() {
      if let Some(pools) = POOLS.get() {
          for db in pools.values() {
              match db {
                  DbPool::Sqlite(p) => p.close().await,
                  DbPool::Postgres(p) => p.close().await,
              }
          }
      }
  }

  // app.rs:214-215
  tracing::info!("umbral: server stopped accepting; draining DB pools");
  crate::db::close().await;
  ```
  `close`'s own doc sets the contract it breaks: "For SQLite this also lets WAL checkpoint; for Postgres it sends a clean `Terminate` so the server doesn't log the connections as unexpectedly lost." (`db.rs:760-762`). The live producer is `plugins/umbral-tenants/src/lib.rs:538` (`register_tenant_pool` from `TenantRouter`), and the pools are `Box::leak`ed (`db.rs:275`) so this is their only close opportunity.
- **Impact**: A multi-tenant app that onboarded N tenant databases stops cleanly and its log says pools were drained, but N pools of up to `db_max_connections` connections each stay open: Postgres logs every one as an unexpected disconnect, and per-tenant SQLite files never get their WAL checkpointed by the shutdown path — on a busy tenant DB that means a slower next boot and an un-checkpointed WAL left on disk. Bounded (process exit follows), but the promise in the log/doc is false for exactly the apps that need the path most.
- **Fix**: in `close()`, after closing `POOLS`, read `DYNAMIC_POOLS` and close each entry (the leaked `&'static DbPool` derefs fine for `.close().await`); take the read guard, collect the clones, drop the guard, then await closes.

### RUN-4 · MEDIUM · memory / drift
**`timezone.rs`'s wash-once warning set is unbounded and grows one `String` per distinct unknown tz name — while the public `tz_or_utc` is documented for "per-user tz overrides surfaced from the session", i.e. caller-supplied strings.**
- **Where**: `crates/umbral-core/src/timezone.rs:38-45`, doc claim at `timezone.rs:61-63`, contrast `crates/umbral-core/src/ratelimit.rs:66-70`
- **Evidence**:
  ```rust
  // timezone.rs:38-45
  fn should_warn_unknown_tz(name: &str) -> bool {
      static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
      let mut seen = WARNED
          .get_or_init(|| Mutex::new(HashSet::new()))
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
      seen.insert(name.to_string())
  }

  // timezone.rs:61-63 — the documented consumer is a per-user value:
  /// Parse an IANA name, falling back to UTC with a one-shot warning
  /// on failure. Public for callers that need the raw lookup (e.g.
  /// per-user tz overrides surfaced from the session).

  // ratelimit.rs:66-70 — the sibling primitive got the fix for this exact class:
  /// the map without bound (audit_2 core-web #4). Every `SWEEP_EVERY` checks the
  /// limiter runs [`RateLimiter::sweep_at`] over the WHOLE map, dropping every
  ```
  Nothing ever removes an entry; the set is keyed on the arbitrary `name` argument. In-tree reachability is narrow — today the only caller is `active_tz()` feeding `settings.time_zone` (one configured string) — but the function is re-exported (`crates/umbral/src/lib.rs:316`) and its doc invites the per-user use, so the trap is loaded for the documented third-party consumer: a session/profile tz field lets a user insert unbounded distinct strings (`X1/Y`, `X2/Y`, …), each persisting for the process lifetime, and every entry also permanently suppresses the warning for that name.
- **Impact**: An app taking the docstring's advice grows a process-global set monotonically with attacker-influenced input, with no eviction and no cap — the same bounded-key-map problem `ratelimit` closed with `SWEEP_EVERY`, left open here. In-tree today: memory only from the single settings value (negligible).
- **Fix**: cap the set (e.g. warn-always after 100 entries, or a fixed-size ring), or key the dedupe on a hash with a bounded `VecDeque`; at minimum, reword the docstring so the per-user pattern ships with the bound.

### RUN-5 · MEDIUM · codegen / drift · self-verified
**`ensure_dependency` misses the dotted workspace spelling `name.workspace = true`, so it appends a second key and leaves a Cargo.toml cargo refuses to parse — the exact "broken project" failure the function's own docstring exists to prevent.**
- **Where**: `crates/umbral-core/src/codegen.rs:352-397` (check at `365-366`, `383`; doc at `340-351`)
- **Evidence**:
  ```rust
  // codegen.rs:365-366
  let key = format!("{name} =");
  let table_header = format!("[dependencies.{name}]");
  ...
  // codegen.rs:383
  if in_dependencies && trimmed.starts_with(&key) {
      return Ok(false);
  }
  ```
  Against `umbral-rest.workspace = true` the `starts_with("umbral-rest =")` test fails (character 11 is `.`, not ` `), as does `trimmed == "[dependencies.umbral-rest]"`; the function then inserts `umbral-rest = "{spec}"` (`codegen.rs:396-397`). The doc at `codegen.rs:344-350` names this exact hazard class for the table form ("Form-blind: … we'd append a second `umbral-rest = "..."` key and cargo would refuse to parse the manifest at all") and closes with "Writing the file and leaving the crate unable to build it is a generator that produced a broken project." Tests exist for the plain and table forms (`codegen.rs:549`, `:720`, `:748`) — none for the dotted/space-less spellings.
- **Impact**: Run a generator (`umbral-rest`'s class generators, `startcommand`) against a plugin whose manifest uses Cargo's inherited-dependency spelling (`dep.workspace = true`, increasingly the norm in a workspace like this tree) and the next `cargo build` fails loudly with a duplicate/conflicting `umbral-rest` key. Loud, not silent — dev-time only — hence MEDIUM per the rubric's codegen calibration.
- **Fix**: match the prefix `format!("{name}.")` (and `format!("{name}=")`) inside `[dependencies]` as "already present", mirroring the table-form branch; add a test with `umbral-rest.workspace = true`. Longer term, a per-crate `[dependencies]`-section parser instead of two prefix probes.

### RUN-6 · MEDIUM · ergonomics · self-verified
**A typo'd flag on a valid plugin command is reported as "unknown command", because `CommandSet::dispatch` maps clap's `UnknownArgument` (raised under a *matched* subcommand) to `Unmatched`, discarding clap's precise diagnostic.**
- **Where**: `crates/umbral-core/src/cli.rs:296-306`; consumer `crates/umbral-cli/src/lib.rs:531-545` + `unknown_token` at `:740-745`
- **Evidence**:
  ```rust
  // cli.rs:300-306
  return match e.kind() {
      clap::error::ErrorKind::DisplayHelp
      | clap::error::ErrorKind::DisplayVersion => {
          Ok(DispatchOutcome::Help(e.render().to_string()))
      }
      clap::error::ErrorKind::InvalidSubcommand
      | clap::error::ErrorKind::UnknownArgument => Ok(DispatchOutcome::Unmatched),
      _ => Err(Box::new(e)),
  };

  // umbral-cli/src/lib.rs:740-745 — the later "unknown command" path then names the first non-flag token:
  fn unknown_token(argv: &[std::ffi::OsString]) -> Option<String> {
      argv.iter()
          .skip(1)
          .find(|a| !a.to_string_lossy().starts_with('-'))
          .map(|a| a.to_string_lossy().into_owned())
  }
  ```
  `UnknownArgument` is returned by clap when a **matching** subcommand receives an unrecognized flag; mapping it to `Unmatched` throws away the one diagnostic that says what's wrong. The argv then falls through to the built-in parser, which reports `InvalidSubcommand` for the plugin's name, and `unknown_token` returns that name — so the CLI prints `error: unknown command \`tasks-worker\`` for a command that exists and is registered.
- **Impact**: First contact with any plugin command goes wrong for a trivial typo: `umbral tasks-worker --intervall 5` answers "unknown command `tasks-worker`" plus the full catalogue, sending the user to look for a missing plugin instead of a misspelled flag. Same shape for built-in commands via the umbral-cli half (`umbral migrate --dry-rn` — `unknown_token` returns `migrate`). The unified-catalog intent (gaps2 #54) is right; this is its collateral.
- **Fix**: in `cli.rs`, before mapping `UnknownArgument`, check whether the first non-flag argv token names one of `self.entries`; if it does, return `Err(Box::new(e))` so clap's "unexpected argument" message renders (the consumer's `Err(e) => return Err(e)` already propagates). Keep `Unmatched` for `InvalidSubcommand` / top-level unknowns. Routing note 4 covers the umbral-cli twin.

### RUN-7 · LOW · docs
**`plugin.rs`'s module doc says this file is an incomplete M7 subset and its `on_ready` doc route plugins through a method that doesn't exist.**
- **Where**: `crates/umbral-core/src/plugin.rs:8-10`, `plugin.rs:540-543`, `plugin.rs:625-633`
- **Evidence**:
  ```rust
  // plugin.rs:8-10
  //! See `docs/specs/02-plugin-contract.md` for the eventual target
  //! shape; this file ships the M7 v1 subset (no middleware, no commands,
  //! no inventory auto-registration).

  // plugin.rs:541-542
  /// Wire signals, start background work, seal admin registrations. …
  /// `ctx.runtime()` when the runtime handle lands.
  ```
- **Impact**: All three "missing" pieces shipped (`fn middleware` at ~:413, `fn commands` at ~:511, `inventory` re-export + `inventory::collect!(DtoMeta)` in lib.rs) — a reader auditing "is the plugin contract complete?" gets a false negative from the file that defines it. The `ctx.runtime()` mention is flagged as future ("when the runtime handle lands") so it's honest, but `AppContext` (only `pool` + `settings`) has no such method anywhere in core; gaps6 #8 (RuntimeProfile) is the tracked home.
- **Fix**: rewrite the module paragraph to describe the shipped contract (or point at gaps6 #8 for the remaining runtime-handle work) and drop the `ctx.runtime()` reference from the sync rationale.

### RUN-8 · LOW · docs
**Milestone-of-record docs are stale in three more places, two of them factually wrong about the shipped runtime.**
- **Where**: `crates/umbral-core/src/lib.rs:5`, `crates/umbral-core/src/backend.rs:13-19` (+ `:434-439`), `crates/umbral-core/src/check.rs:9-17`
- **Evidence**:
  ```rust
  // lib.rs:5
  //! Status: M0 shipped — Settings, db pool, web re-exports, App builder.

  // backend.rs:13-18
  //! - [`PostgresBackend`] — declared and queryable for compatibility
  //!   checks, but the umbral pool is still `sqlx::SqlitePool` at M4. The
  //!   real `sqlx::PgPool` wiring lands when there's a real user need;

  // check.rs:9-11
  //! At M4 the only check that's meaningful without a model registry or
  //! Plugin walk is [`settings_required`] — it verifies that production
  ```
- **Impact**: `backend.rs` is not merely old, it's wrong: `DbPool::Postgres` is a live runtime path (`db.rs` dispatch, `transaction_pg`), and `BackendDetectError`'s Display ("M4 supports `sqlite://` and `postgres://`", `backend.rs:434-439`) misdescribes the shipped detector. `check.rs:9` undercounts a catalogue that now spans model, plugin, route and settings checks (second sentence of the same paragraph admits the deferred ones "land alongside" — they have). A reader trusting the docs will re-derive wrong assumptions about the pool layer.
- **Fix**: replace the milestone prose with the current one-liners; keep history in git, not rustdoc.

### RUN-9 · LOW · docs
**A rustdoc block collision in `check.rs` attaches the wildcard-doc to `auto_user_columns_nullable` and leaves `settings_allowed_hosts_wildcard` undocumented.**
- **Where**: `crates/umbral-core/src/check.rs:402-419` (function at `:474`)
- **Evidence**:
  ```rust
  // check.rs:402-403 — the wildcard check's doc …
  /// Warn when `allowed_hosts` contains the `"*"` wildcard in
  /// `Environment::Prod` (audit_2 core-app-config #13). A wildcard makes the
  // … no blank line, then, at :412:
  /// An `auto_user` / `auto_user_add` column must be nullable (gaps3 #55).
  fn auto_user_columns_nullable(_ctx: &CheckContext<'_>) -> Vec<SystemCheckFinding> {
  ```
- **Impact**: `cargo doc` shows the wildcard rationale as the doc of the nullable-column check (both blocks attach to `auto_user_columns_nullable`), and `settings_allowed_hosts_wildcard` (`:474`) has no doc at all — the explanation for a Prod-only security warning is filed under the wrong function.
- **Fix**: move lines 402-411 down to line 474.

### RUN-10 · LOW · docs (contradicts shipped behavior)
**`ratelimit.rs`'s module doc still promises "a periodic sweep is a future hardening" while the periodic sweep shipped, is on by default, and is untested.**
- **Where**: `crates/umbral-core/src/ratelimit.rs:39-44` vs `ratelimit.rs:66-70` and `:207-213`
- **Evidence**:
  ```rust
  // ratelimit.rs:39-44 (module doc, "Scope and limits")
  //! - **Unbounded key set.** The `HashMap` grows one entry per distinct
  //!   key and entries are pruned lazily on next `check` of that key, never
  //!   swept globally. … a periodic sweep is a
  //!   future hardening.

  // ratelimit.rs:66-70 (the shipped fix the doc denies)
  /// the map without bound (audit_2 core-web #4). Every `SWEEP_EVERY` checks the
  /// limiter runs [`RateLimiter::sweep_at`] over the WHOLE map, dropping every
  const SWEEP_EVERY: usize = 1000;
  ```
- **Impact**: The module doc is the first thing a reader sees; it asserts a live memory-bound defect that audit_2 core-web #4 already closed — inviting a duplicate finding/tracker entry and hiding the sweep's existence from anyone tuning the limiter. `tests/ratelimit.rs` has no test touching the sweep (six cases, all window semantics), so the shipped bound itself has zero regression coverage.
- **Fix**: rewrite the bullet to describe `SWEEP_EVERY` and add a unit test asserting the key map shrinks after `SWEEP_EVERY` churned keys (a `len()` probe via an existing accessor, or a test-only hook).

### RUN-11 · LOW · correctness (swallowed error)
**`merge_dotenv` silently drops malformed `.env` lines via `iter.flatten()`, so a typo'd `.env` line quietly boots the app with defaults.**
- **Where**: `crates/umbral-core/src/settings.rs:321`
- **Evidence**:
  ```rust
  for (key, value) in iter.flatten() {
      let Some(key) = dotenv_key(&key) else {
          continue;
      };
  ```
- **Impact**: `dotenvy`'s iterator yields `Err` for a malformed line (an unclosed quote, an invalid escape). `.flatten()` discards it with no message, so a developer who mistypes `UMBRAL_SECRET_KEY="abc` gets an app running on the dev default with nothing anywhere saying why — the same class CLAUDE.md calls out under "fix, don't patch" (a `.ok()`-style discard). Dev-only surface, hence LOW.
- **Fix**: match on the item and `tracing::warn!`/`eprintln!` the malformed line's number (dotenvy's error carries it); keep the last-wins/prefix semantics otherwise.

### RUN-12 · LOW · observability / drift
**The hand-written redacting `Debug` for `Settings` omits `trusted_proxy_hops` — the manual field list already drifted once.**
- **Where**: `crates/umbral-core/src/settings.rs:592-617` vs the struct field at `settings.rs:440-441`
- **Evidence**:
  ```rust
  // settings.rs:592-594 — "prints the rest verbatim so the value stays useful"
  impl std::fmt::Debug for Settings {
      fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
          f.debug_struct("Settings")
  // … fields: database_url, databases, max_form_body_bytes, …, static_root, extra
  // trusted_proxy_hops: absent.
  ```
- **Impact**: `trusted_proxy_hops` is the security-relevant setting from audit_2 H9 (mis-set hops mis-attribute client IPs and weaken throttles) and is exactly the field an operator wants in a `?settings` incident dump — it's the only struct field the impl drops. The pattern (manual field list) silently drops any field added later, so the omission is a symptom, not a typo.
- **Fix**: add `.field("trusted_proxy_hops", &self.trusted_proxy_hops)`; consider a `debug_assert`-style compile-time tripwire or a test comparing field names against a mirror list so the next addition fails loudly.

### RUN-13 · LOW · correctness (interactive-tool panic)
**`prompt::ask_target` underflows on input "0": `plugins.get(n - 2)` panics in debug builds.**
- **Where**: `crates/umbral-core/src/codegen.rs:526-534`
- **Evidence**:
  ```rust
  if let Ok(n) = answer.parse::<usize>() {
      if n == 1 {
          return Ok(Target::Root);
      }
      if let Some(p) = plugins.get(n - 2) {   // n == 0 ⇒ 0 - 2 underflow
          return Ok(Target::Plugin(p.clone()));
      }
      println!("  no such choice: {n}");
  ```
- **Impact**: A developer answering the "Where should it live?" prompt with `0` (a plausible mistake — it's the first item's neighbour) crashes the generator with "attempt to subtract with overflow" in debug; release wraps to a huge index, prints "no such choice: 0", and re-prompts — a debug/release behavior split in an interactive path. LOW: dev-time, recoverable.
- **Fix**: `answer.parse::<usize>().ok().and_then(|n| n.checked_sub(2)).and_then(|i| plugins.get(i))` (root for `1`, re-prompt for `0`).

### RUN-14 · LOW · drift
**The dynamic-pool registry's poison policy is asymmetric: writes `.expect()` (fail-closed), reads `.ok()` (fail-open) — and the fail-open read makes `pool_for_dispatched` panic with a message that blames configuration.**
- **Where**: `crates/umbral-core/src/db.rs:270-272` (write), `db.rs:244-249` (read), `db.rs:250-252` (panic text), `db.rs:284-288` (availability read)
- **Evidence**:
  ```rust
  // db.rs:270-272 — write path: panic on poison
      .write()
      .expect("umbral: dynamic pool registry poisoned");

  // db.rs:244-248 — read path: poison ⇒ silently treated as "alias absent"
  if let Some(p) = DYNAMIC_POOLS
      .get()
      .and_then(|reg| reg.read().ok().and_then(|m| m.get(alias).copied()))
  …
  // db.rs:251 — the resulting panic misattributes the cause
  panic!("umbral: no database registered under alias '{alias}'");
  ```
  `pool_alias_registered` (`db.rs:286-288`) likewise returns `false` on poison ("A router can use this to fall back to the default pool" — so a poisoned lock silently reroutes tenant traffic to the default DB if a router gated on it).
- **Impact**: Poisoning is near-unreachable (the write critical section can't panic short of OOM), so this is a consistency hazard, not a live bug — but if it occurs, three sites in one subsystem react three ways (panic, silently-absent, silently-false) and the loudest one gives a misleading reason. The reset-gate path in the same file already uses the deliberate `PoisonError::into_inner` recovery (`timezone.rs:43` chooses recovery too) — the file should pick one policy.
- **Fix**: pick recovery (`.unwrap_or_else(PoisonError::into_inner)`) for the registry reads and keep the write path panicking only if recovery is impossible; or make both fail-closed with the "poisoned" wording. Note the choice in the `DYNAMIC_POOLS` doc.

### RUN-15 · LOW · drift
**The Postgres-only SqlType set is duplicated between `check.rs::is_postgres_only` and `SqliteBackend::map_type`'s panic arms, with nothing pinning the two in agreement.**
- **Where**: `crates/umbral-core/src/check.rs:1013-1034`, `crates/umbral-core/src/backend.rs:330-379`
- **Evidence**:
  ```rust
  // check.rs:1016-1034 — the boot gate's list
      matches!(
          ty,
          SqlType::Array(_)
              | SqlType::Inet
              | SqlType::Cidr
              | SqlType::MacAddr
              // gaps2 #70: text-backed Postgres types (XML / LTREE /
              // BIT VARYING) have no SQLite equivalent; …
              | SqlType::Xml
              | SqlType::Ltree
              | SqlType::Bit
              | SqlType::FullText
              // BUG-10: … | SqlType::Decimal | SqlType::DecimalN(_)
              | SqlType::BigDecimal
              | SqlType::Geometry(_)
              | SqlType::Geography(_)
      )

  // backend.rs:330-379 (SQLite map_type) — the runtime twin, as panic arms:
              SqlType::Array(_) => panic!(
                  "umbral::backend::SqliteBackend::map_type: SqlType::Array is Postgres-only. \
                   The field.backend system check should have failed boot; …"
              ),
  ```
- **Impact**: The pair agrees today (verified variant by variant), but the boot check exists precisely to keep these panics unreachable; a new PG-only `SqlType` added to the panic arm and not to `is_postgres_only` makes boot pass and the panic reachable, and vice versa the check rejects a type the backend could have mapped. `tests/backend.rs` point-checks only `Uuid` — no exhaustive agreement test exists.
- **Fix**: a test iterating every `SqlType` variant asserting `SqliteBackend.map_type` panics iff `is_postgres_only(ty)` (or vice versa via a `catch_unwind` matrix), or derive one list from the other.

### RUN-16 · INFO · architecture
**Where**: `crates/umbral-core/src/check.rs:973-1007` — `plugin_security_missing` gates built-in behaviour by string-matching plugin names (`names.contains(&"auth")`, `"sessions"`, `"security"`); core can't otherwise know a plugin's role, so a `Plugin::provides()`-style role tag would delete the string coupling — names are reserved so this is safe, observation only (audit_2 core-web #1 owns the warning-vs-error question).

### RUN-17 · INFO · perf
**Where**: `crates/umbral-core/src/db/router.rs:202-205` + `crates/umbral-core/src/db/route_context.rs:106-110` — `schema_qualified_table` calls `route_context::current()` per table reference: an `Arc` clone per FROM/JOIN under a task-local, and a fresh `Arc::new(RouteContext::default())` when none is set (CLI, tests, background work). Cold-path micro-allocation next to SQL string building; no action required.

## Strengths

- **`block_on_ready` (`plugin.rs:79-130`) is a model bridge**: three runtime-flavor branches (multi-thread → `block_in_place`, current-thread → `std::thread::scope` + local runtime, no-runtime → fresh runtime), each documented with *why*, a scoped-thread `expect` whose panic cannot be a poisoned-mutex false positive, and a real behavioural test in `plugins/umbral-rls/tests/on_ready_runtime.rs`. This is the hardest piece of the runtime slice and it is right.
- **The boot-check catalogue argues its own severity**: every `SystemCheckFinding` carries a `check_id`, a tailored `message` explaining the failure mode in user terms, and an actionable `hint` ("make it nullable: `{}: Option<ForeignKey<AuthUser>>`") — and the dangerous ones (`settings.allowed_hosts` wildcard, SQLite-in-Prod, `plugin.security_missing`) are deliberately Warnings with the rationale in the doc block. Five dedicated test files (`system_check*.rs`) pin them.
- **The settings security policy is verifiably correct where history says it went wrong**: `client_ip` anchors XFF resolution at the configured hop count from the right (trusting nothing at `hops == 0`) and `trusted_proxy_hops` defaults to 0 — the audit_2 H9 fix holds; the `Debug` redaction masks `secret_key`, URL userinfo and `extra` (with the one omission in RUN-12).
- **The RLS/session-var reset gate holds**: `SESSION_VARS_IN_USE` is written with `Ordering::Release` (`db.rs:601`) and read with `Ordering::Acquire` (`db.rs:608`), and the reset is scoped to umbral's own GUC names (`UMBRAL_GUC_NAMES`, gaps4 #16) instead of `RESET ALL` — the two historic bugs in this exact code are both fixed, not patched.
- **The codegen seam declines instead of guessing**: `write_new_file` never overwrites, `declare_module`/`insert_before_marker` refuse when the file isn't the expected shape, `resolve_target` validates plugin names before `Path::join`, and 16 inline tests exercise each decline path — RUN-5/RUN-13 are gaps in coverage, not in the design.
- **cli.rs's precedence rules are single-sourced and logged**: reserved-name shadowing emits an `eprintln` when a plugin command is shadowed by a built-in, `CommandSet` is built once per dispatch, and `render_help` merges built-ins + plugin commands into one catalogue.

## Clean areas checked

- **`unsafe` audit**: zero `unsafe` blocks in the slice (only the keyword string in `codegen.rs:173`'s keyword list). No FFI, no raw pointers.
- **Ambient-state inventory**: every process-wide static in the slice enumerated — `SETTINGS`, `POOLS`, `DYNAMIC_POOLS`, `ATOMIC_DEFAULT`, `SESSION_VARS_IN_USE`, `UMBRAL_GUC_NAMES`, `ROUTER`/`DEFAULT`, `ACTIVE` (backend, `backend.rs:384`), `AMBIENT` (cache), `STORAGES`, `DEFAULT_AUTH`, `DRAINING`, `MEM_SEQ`, `WARNED` (timezone) — the ambient-config class is the tracked by-design decision (Known); no *new kind* of global (no hidden thread-locals, `static mut`, or cross-request caches) crept in beyond it. `rt.rs` and `shutdown.rs` hold no state of their own.
- **Request-path panics**: the reachable panics in the slice are documented boot/registration `expect`s (`pool_for_dispatched` before `App::build`, `backend::init` twice, `settings` unset) with "did you call App::build()?" messages; none are reachable from a request on a built app.
- **Transaction correctness**: pending signals fire only after a successful commit and are dropped on rollback in all four runners (gaps6 #15 semantics); `commit` failure propagates via `?` before any signal fires.
- **`ttl`/window arithmetic in `ratelimit.rs`**: window pruning, `retry_after`, `remaining`, and the `sweep` are consistent with the six behavioural tests (which, however, miss the sweep — RUN-10).
- **Route-context isolation**: `ROUTE_CONTEXT` is a task-local; `current()` never leaks a previous request's context across tasks, and `tests/route_context.rs` proves the handler-visible tenant via the public path.
- **`validate.rs`**: small, pure combinator surface; no panics, exhaustive-match style kept.
- **`storage.rs` registry**: `STORAGES` set-once with a panic message naming the plugin (`StoragePlugin`) — the register-once contract is explicit.
- **`auth_contract.rs`**: `DEFAULT_AUTH` warns and ignores a second set instead of panicking midway through the plugin list (deliberate); `parse_basic_credentials` handles the header shapes without panicking on malformed input.
- **codegen file surgery**: CRLF/trailing-newline preservation tested (`edits_preserve_crlf_and_a_missing_trailing_newline`), path-escape refusal tested (`resolve_target_refuses_a_plugin_name_that_escapes_the_project`).
- **Trusted-proxy env parsing**: `UMBRAL_TRUSTED_PROXY_HOPS` + `client_ip_with_hops` boundaries (0, n > list length, padded entries) verified against the doc table at `settings.rs:150-156`.

## Known issues already tracked elsewhere (not counted in the totals)

- `gaps6 #15` — buffered signals dropped on rollback (shipped; the semantics are correct and cited in the code comments — RUN-2 is about the copy drift, not the semantics).
- `review_3/codex #14` — process-wide `OnceLock` ambient state (by design; covers POOLS/DYNAMIC_POOLS/ATOMIC_DEFAULT/SETTINGS/ROUTER/DEFAULT/AMBIENT/STORAGES/DEFAULT_AUTH/ACTIVE/DRAINING and the rest; `WARNED` in RUN-4 is bookkeeping, not ambient config, which is why it is numbered).
- `gaps6 #8` — RuntimeProfile / the runtime handle for plugin context (open; the `ctx.runtime()` doc reference in RUN-7 points here).
- `audit_2 core-web #1` — `plugin.security_missing` Warning-vs-Error and default security headers (deferred design call; RUN-16 only flags the name-matching coupling).
- `audit_2 core-app-config #11` — redacting `Debug` for `Settings` (fixed; RUN-12 is the residual field omission inside that fix).
- `audit_2 H9` — client-IP/trusted-proxy policy (fixed; verified holding above).
- `gaps4 #16` — `RESET ALL` scoped to umbral GUC names (fixed; verified holding above).
- `gaps2 #54` — unified CLI help/unknown-command catalogue (shipped; RUN-6 is a refinement of its error mapping, not a re-report).
- `gaps3 #81/#82` — the `umbral::codegen` seam + scaffolder (shipped; RUN-5/RUN-13 are refinements).
- `gaps2 #90` — `umbral-auth::throttle` consolidated onto `ratelimit::RateLimiter` (shipped; RUN-10's doc drift postdates it).

## Out-of-scope notes

- `plugins/umbral-openapi/src/lib.rs:562` (`properties.insert(col.name.clone(), schema)`) and `plugins/umbral-openapi/src/client_gen.rs:1007` (`{}: {}{null};", f.name`) — the same SQL-name-as-JSON-key assumption as RUN-1; the openapi/rest reviewer should confirm the generated schema/client keys for `#[sqlx(rename)]` models.
- `crates/umbral-core/src/orm/model.rs:565-567` — `FieldSpec.name` doc claims "there is no field-level column-rename attribute", contradicting the shipped `#[sqlx(rename)]` path (slice 2's file; stale doc, same root as RUN-1).
- `crates/umbral-core/src/inspect.rs:405,449` — introspected struct names come from `pascal_case_from_table` with no identifier guard; a table like `2fa_tokens` yields `pub struct 2faTokens` (non-compiling). `codegen::validate_ident` exists and is wired in scaffold/rest generators but not here (slice 3's file; dev-time codegen emitting invalid output).
- `crates/umbral-cli/src/lib.rs:531-545` + `:740-745` — the built-in layer maps `UnknownArgument`/`InvalidValue` to the "unknown command" catalogue via `unknown_token`'s first-non-flag heuristic; RUN-6's twin (umbral-cli reviewer).
- `plugins/umbral-tenants/src/lib.rs:538` — the live `register_tenant_pool` caller that makes RUN-3 observable; a tenants-side shutdown hook would be the other half of the fix.
- `plugins/umbral-auth` — `crates/umbral-core/src/auth_contract.rs`'s `FnAuthentication` doc example calls `umbral_auth::authenticate(&user, &pass)` (cannot be verified from core); umbral-auth reviewer should confirm the symbol exists.
- `crates/umbral-core/src/orm/write.rs:726` — the only in-tree `active_tz()`/`tz_or_utc` consumer (one settings string); relevant context for RUN-4's fix (slice 1).
