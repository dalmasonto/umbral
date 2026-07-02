# Audit — `umbral-macros`, `umbral-cli`, `umbral-casing`

Slug: `core-macros-cli`
Scope: `crates/umbral-macros/`, `crates/umbral-cli/`, `crates/umbral-casing/` only. SQL-emission and runtime enforcement live in `umbral-core` (out of scope) and are called out as blind spots where a finding depends on them.

## A. Executive summary

These three crates are compile-time / dev-time tooling: proc-macros that emit `Model`/`Form`/`Choices`/`#[task]` code, the `umbral` scaffolding + management CLI, and pure casing helpers. There is **no attacker-reachable runtime surface here** — the macro inputs are the developer's own source, the CLI runs on the developer/operator's machine, and casing is deterministic string work. So the risk posture is "footguns baked into generated defaults," not "exploitable-now." No CRITICAL or HIGH issues were found in the provided artifacts.

The three most urgent issues are all in the **generated scaffold** and the **`Form` derive defaults**, i.e. code the framework hands users as a starting point: (1) `umbral startproject` generates a seed step that plants a fixed-password superuser `admin`/`admin` the moment the compiled binary is launched with no subcommand and no users exist — a plausible path to a default-credential admin in a production deploy; (2) `#[derive(Form)]` is opt-out (a denylist of `noform`/`primary_key`/`masked`/…), so deriving `Form` on a model that has a privilege boolean makes that field mass-assignable by default, the classic ModelForm footgun Django's allowlist exists to prevent; (3) the generated `base.html` pulls Tailwind from `https://cdn.tailwindcss.com` with no SRI, an untrusted third-party script in every scaffolded page.

Command injection is **not** present: both `forward_to_project` and `dev` build argv via `std::process::Command::args(...)` (exec, no shell). Identifier/SQL injection through macro attributes is developer-controlled compile-time input, not attacker input. The casing crate is correct and well-tested.

What I could not assess: whether `umbral-core`'s migration engine actually escapes/quotes the `#[umbral(default = "...")]` string (the macro passes it through verbatim; the doc-comment says "emitted verbatim on CREATE TABLE"), and the runtime enforcement of `noedit`/masked semantics. Both live outside these three crates.

## B. Findings table

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|
| 1 | MEDIUM | Config/secure-default | `umbral-cli/src/scaffold.rs:688` + `:510-516` | Generated `src/seed/credentials.rs` creates a superuser `admin`/`admin`; `main.rs` runs `seed::all()` on any no-subcommand launch (`!user_invoked_cli`) | A binary launched as `./app` (no args) in prod with an empty user table gets a known-password superuser = full admin compromise | Gate the seed on `settings.environment == Dev`; or generate it commented-out; or require a `--seed` flag rather than triggering on bare launch |
| 2 | MEDIUM | Input validation / mass assignment | `umbral-macros/src/lib.rs:3849-3857`, `:3470-3486` | `#[derive(Form)]` includes every field as user-submittable unless it hits the skip denylist (`noform`/`primary_key`/`auto_now*`/`id`/masked/reverse). No container-level allowlist (`fields = [...]`) exists | Deriving `Form` on a model with `is_staff`/`is_superuser`/`balance` etc. exposes those for mass assignment unless the dev remembers `#[umbral(noform)]` on each | Add an opt-in `#[form(fields = [...])]` allowlist (Django ModelForm parity) and/or document that sensitive columns MUST be `noform`; consider defaulting bool→false-only |
| 3 | LOW | Transport / supply chain | `umbral-cli/src/scaffold.rs:886` | Generated `templates/base.html` loads `https://cdn.tailwindcss.com` via `<script>`, no SRI, no local fallback | Third-party script executes in every scaffolded page; CDN compromise = XSS; breaks any CSP the `SecurityPlugin` would add | Ship a vendored/compiled CSS asset, or at minimum add SRI + a comment that it must be removed before prod (comment exists; the script does not enforce it) |
| 4 | LOW | Secrets / observability | `umbral-cli/src/lib.rs:329-339` | `maskkeygen` prints `UMBRAL_MASK_PRIVATE_KEY=...` to stdout | The X25519 private key (decrypts every masked column) lands in terminal scrollback, shell history if redirected, and CI job logs | Warn in output + doc that the private key must be captured to a secret store, not logs; the sibling `createsuperuser` already avoids flags "because flags land in shell history" — same reasoning applies |
| 5 | LOW | API/doc-vs-code | `umbral-cli/src/scaffold.rs:848-849` vs `:510-516` | Generated `README.md` says "`cargo run -- serve` … First run — applies migrations and starts the server", but the boot guard runs `auto_migrate()` only when NO subcommand is passed; `serve` is a subcommand so it skips migrate | Operator follows the README, runs `serve`, hits missing-table errors on a fresh DB | Fix the generated README to say bare `cargo run` migrates+serves, or make the guard also auto-migrate under an explicit `serve` |
| 6 | LOW | Injection (developer-controlled) | `umbral-macros/src/lib.rs:231-235`, `:1284-1287` | `#[umbral(default = "...")]` string is emitted into the `FieldSpec` verbatim (`quote!{ #s }`); doc-comment claims core emits it "verbatim on CREATE TABLE" | If core does not quote/escape, a developer who interpolates untrusted data into a default injects DDL. Not attacker-reachable (compile-time, dev source) | Verify core parameterizes/escapes the default; if it truly emits verbatim, that is a core finding — flag it there |
| 7 | LOW | Config hygiene | `umbral-cli/src/scaffold.rs:784`, `:797` | `umbral.toml` and `.env` are generated with `secret_key = "umbral-insecure-dev-key-change-me"` | Weak key ships in the repo skeleton | Mitigated: framework errors at boot when this exact key is used with `environment = "Prod"` (per the generated comment). Keep the boot guard; consider generating a random dev key per scaffold |
| 8 | LOW | Correctness | `umbral-macros/src/lib.rs:3994-4011` | `Form` derive parses FK values with a hardcoded `#raw_var.parse::<i64>()` and `ForeignKey::new(v: i64)` | `#[derive(Form)]` on a model whose FK target has a `String`/`Uuid` PK fails to compile — inconsistent with the completed PK-generalization work | Use `pk_kind_for_table` (already computed for the field builder) to drive parsing, matching the typed PK the target model declares |

## C. Detailed findings (MEDIUM)

### #1 — Generated scaffold plants a default-credential superuser on no-arg launch

`scaffold_project` writes `src/seed/credentials.rs`:

```rust
// scaffold.rs:682-704 (generated content)
pub async fn test_credentials() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if AuthUser::objects().count().await? > 0 { return Ok(()); }
    umbral_auth::create_superuser("admin", "admin@example.com", "admin").await ...
    // loud stderr banner: "NEVER ship this in production"
}
```

and wires it into `main.rs`:

```rust
// scaffold.rs:510-516 (generated content)
let argv: Vec<String> = std::env::args().collect();
let user_invoked_cli = argv.iter().skip(1).any(|a| !a.starts_with('-'));
if !user_invoked_cli {
    auto_migrate().await?;
    seed::all().await?;   // -> test_credentials() -> admin/admin
}
```

**Scenario.** An operator builds the scaffolded project and launches the release binary the most natural way — `./myapp` with no arguments (systemd `ExecStart=/opt/app/myapp`, a Docker `CMD ["./myapp"]`, etc.). `user_invoked_cli` is false, the user table is empty on a fresh prod DB, so `create_superuser("admin", …, "admin")` runs and a full-privilege `admin`/`admin` account now exists on the public admin. The `secret_key` boot guard forces the operator to change the key but does nothing about the seed, so "I set a real secret and prod booted" is not a signal that the seed is gone. The stderr banner is a mitigation only if someone reads startup logs.

**Corrected snippet** (generated `credentials.rs` / `main.rs`):

```rust
// Only seed dev credentials in the Dev environment.
if umbral_core::settings::get().environment == Environment::Dev {
    seed::all().await?;
}
// ...and in test_credentials(), belt-and-suspenders:
if umbral_core::settings::get().environment != Environment::Dev {
    return Ok(()); // never mint the default admin outside Dev
}
```

(These three crates are audit-only; the fix lands in the scaffold template strings in `scaffold.rs`.)

### #2 — `#[derive(Form)]` is a denylist, not an allowlist (mass assignment)

```rust
// umbral-macros/src/lib.rs:3849-3857
let skip_for_form = model_attr.noform
    || model_attr.primary_key
    || model_attr.auto_now || model_attr.auto_now_add
    || is_implicit_pk
    || form_field_is_masked(&field.ty)
    || form_is_reverse_relation(field);
if skip_for_form { any_skipped = true; continue; }
// every OTHER field, including a bare `pub is_superuser: bool`, becomes a submittable form field
```

The container attr parser accepts only `normalize_strings` (`:3470-3486`) — there is no `#[form(fields = [...])]` / `#[form(exclude = [...])]`.

**Scenario.** A developer reuses their persisted model as a form (the documented "no parallel ContactForm" win, `:3839-3846`). The model has `pub is_staff: bool`. Because `is_staff` isn't in the skip set, the generated `validate()` reads it from the submitted `data` map and the generated `Self { … }` assigns it. An attacker POSTs `is_staff=true` and self-elevates. The framework's own `AuthUser` is safe (it's handled by `umbral-auth`, not by a user `#[derive(Form)]`), but the derive's default invites this on any user model.

**Corrected direction:** add an allowlist so the safe default is "nothing is submittable unless named":

```rust
#[derive(Form)]
#[form(fields = ["title", "body"])]   // only these reach validate()/construction
struct PostForm { id: i64, title: String, body: String, is_featured: bool /* not in fields => ignored */ }
```

Until that exists, the honest mitigation is documentation: every sensitive column on a `#[derive(Form)]` model must carry `#[umbral(noform)]`.

## D. Blind spots (could not verify from these three crates)

- Whether `umbral-core`'s migration engine quotes/escapes the `#[umbral(default = "...")]` value or truly emits it verbatim (finding #6). The macro only passes the `String` through.
- Runtime enforcement of `noedit`, masked server-set semantics, and the `db_constraint = false` cross-DB FK guard — all enforced in core/plugins, not here.
- Whether `create_superuser` (in `umbral-auth`) applies any password policy — the scaffold passes the literal `"admin"`, so any policy there would be the only backstop for #1.
- The actual CSP/headers the `SecurityPlugin` sets (relevant to #3's severity) — that plugin is out of scope.
- `importcsv` reads the entire CSV into a `Vec<Vec<String>>` (`umbral-cli/src/lib.rs:767-771`) before inserting; unbounded memory on a huge file is an operator-local DoS only, not scored.

## E. Prioritized action plan

**Quick wins (< 1 day)**
- #1: gate the generated dev-superuser seed on `environment == Dev` (change the scaffold template). Highest value.
- #4: add a stderr warning line to `maskkeygen` about not letting the private key hit logs; add the Callout to the doc (done, see below).
- #5: fix the generated README's `serve` first-run claim.
- #7: generate a random dev `secret_key` per scaffold instead of the shared literal.

**Short term (< 2 weeks)**
- #2: add `#[form(fields = [...])]` / `#[form(exclude = [...])]` to the `Form` derive; until then, document the `noform` requirement prominently.
- #3: vendor a compiled CSS asset into the scaffold (or add SRI) so generated pages carry no third-party runtime script.
- #6: confirm with the core team how `default` is emitted; file a core finding if it is unescaped.

**Structural (needs design work)**
- #8: thread the target model's PK kind through the `Form` FK parsing so forms work for `String`/`Uuid`-keyed targets, closing the last of the non-i64-PK gaps in the derive layer.

## Docs updated

- `documentation/docs/v0.0.1/cli/management-commands.mdx` — added a `<Callout type="warning">` to the **maskkeygen** section warning that the printed `UMBRAL_MASK_PRIVATE_KEY` is a decryption key that must be moved into a secret store and kept out of shell history / CI logs / committed `.env` (finding #4). Reason: the code prints the private key to stdout (`umbral-cli/src/lib.rs:337-338`) and the doc previously said only "set both lines in your environment" with no handling warning, while the neighbouring `createsuperuser` section already warns that flags "land in shell history" — the secret-key case deserves the same caution. No contradictions with code were found in the CLI docs otherwise (the `serve` section correctly does not claim auto-migration; the command catalog matches `Command` in `lib.rs`).
