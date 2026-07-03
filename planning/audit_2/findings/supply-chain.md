# Audit — Dependencies & Supply Chain (+ facade / testing crates)

Slug: `supply-chain`
Auditor scope: root virtual manifest, every `crates/*/Cargo.toml` + `plugins/*/Cargo.toml`, `Cargo.lock`, the `umbral` facade re-exports, and `umbral-testing`.
Date: 2026-07-02

**Tooling note (mandatory disclosure):** `cargo audit` and `cargo deny` are **NOT installed** in this environment (`cargo audit` → "no such command: audit"; `cargo-deny`, `cargo-outdated` not on PATH). No automated RUSTSEC cross-check was possible. Every claim below is grounded in `Cargo.toml`/`Cargo.lock` version facts and `cargo tree` output. Where a claim would depend on a specific RUSTSEC advisory ID, it is marked **"unverified — needs cargo audit."** No CVE IDs are invented.

Toolchain: rustc/cargo 1.96.0. Lock has 571 packages. Resolver 3, edition 2024, workspace version 0.0.4.

---

## A. Executive summary

The workspace pins its *first-party* HTTP/DB/crypto stack at current versions — axum 0.8.9, sqlx 0.8.6, tokio 1.52.3, reqwest 0.12.28 (rustls, `default-features=false`), argon2 0.5, rustls 0.23.40 — and all optional heavy backends (Redis, S3, OTLP, email-API) are correctly `dep:`-gated behind off-by-default features. The REST/OpenAPI "serializers are a plugin" boundary is enforced structurally: `umbral-core` and the `umbral` facade have **zero** dependency on `umbral-rest`/`umbral-openapi` (verified), so a REST-free app compiles no serializer code. No `git`/`branch`/`rev` deps, no `[patch]` overrides, no `=`-pinned versions.

The one materially bad dependency is **`rust-s3` 0.35.1** (behind `umbral-storage`'s off-by-default `s3` feature). It is effectively unmaintained and drags in an entire **end-of-life TLS/HTTP stack** — `rustls 0.21.12` (the 0.21 line is EOL and no longer receives security fixes), `hyper 0.14.32`, `hyper-rustls 0.24.2`, `tokio-rustls 0.24.1` — *plus* a second, separate `rustls 0.23.40` via `attohttpc`. At 10M-user scale, media/static served from S3/R2/MinIO is the expected production config, so this legacy TLS code sits directly on the object-storage path.

The three most urgent items: (1) the `rust-s3`/EOL-rustls-0.21 chain on the S3 storage path (HIGH); (2) no advisory scanner (`cargo audit`/`cargo deny`) is wired into the build or this audit — the supply chain is currently unmonitored (MEDIUM/process); (3) `umbral-core` compiles a large, always-on surface (both sqlx sqlite+postgres+`any` drivers, syntect, ammonia, pulldown-cmark, chrono-tz, ipnetwork, mac_address, rust_decimal, crypto_box) with **no feature flags to slim it** — you cannot build a single-backend or minimal app (MEDIUM).

**What could not be assessed:** actual deployed feature set (is `s3`/`redis` enabled in prod?), individual RUSTSEC status of all 571 lock packages (no scanner), and whether `rsa 0.9.10` — present in the lock but **not** in the compiled graph — ever compiles under a build configuration I didn't exercise.

---

## B. Findings table

| # | Severity | Area | Location | Finding | Impact | Recommended fix | Status |
|---|----------|------|----------|---------|--------|-----------------|--------|
| SC-1 | HIGH | Deps / TLS | `plugins/umbral-storage/Cargo.toml:904` (`rust-s3 = "0.35"`, `s3` feature) → `rustls 0.21.12`, `hyper 0.14.32`, `hyper-rustls 0.24.2`, `tokio-rustls 0.24.1` | Enabling the `s3` storage backend pulls an EOL TLS stack. `rustls 0.21` is past end-of-life (fixes land only on ≥0.23); `hyper 0.14` is the legacy line. `rust-s3` 0.35.1 is effectively unmaintained and also links a *second* rustls (0.23.40 via attohttpc) — two TLS impls in one dep. | Object-storage TLS (every media upload/download at scale) rides EOL crypto that will not receive future security patches. Larger, unmonitored attack surface on a network-facing path. | Migrate the S3 backend off `rust-s3` to a maintained client (`aws-sdk-s3`, or `object_store`) that builds on `hyper 1` + `rustls 0.23`. Interim: run `cargo audit` against a `--features s3` lock and treat any rustls-0.21/hyper-0.14 advisory as release-blocking. | deferred: rust-s3→aws-sdk-s3 swap (large migration) |
| SC-2 | MEDIUM | Process | Repo (no `deny.toml`, no CI advisory step observed) | No `cargo audit` / `cargo deny` is installed or wired in; this audit could run neither. The supply chain has no automated RUSTSEC gate. | Known-vulnerable transitive crates can enter the tree unnoticed between manual reviews. | Add `cargo-deny` with a `deny.toml` (advisories + bans + duplicate-version policy) and a `cargo audit` step to CI, gating merges. Commit `Cargo.lock` is already done — good; this closes the loop. | ✅ done |
| SC-3 | MEDIUM | Deps / bloat | `crates/umbral-core/Cargo.toml` (`sqlx … ["sqlite","postgres","any", …]`, plus `syntect`, `ammonia`, `pulldown-cmark`, `chrono-tz`, `ipnetwork`, `mac_address`, `rust_decimal`, `crypto_box`) | `umbral-core` has **no `[features]` table**. Every umbral app unconditionally links *both* the SQLite and Postgres sqlx drivers (+ `any`), the full markdown pipeline (pulldown-cmark + ammonia + syntect), timezone DB (chrono-tz), and exotic Postgres column types — even a Postgres-only app that never renders markdown. | Cannot build a single-backend or minimal binary. Always-on code that most apps never call is permanent attack surface and binary bloat (matters across 10M-user fleets / image size). | Introduce cabinet features on `umbral-core` (`sqlite`, `postgres`, `markdown`, `timezone`, `pg-extra-types`) with a sensible default, so unused subsystems (and their transitive deps) can be dropped. | deferred: feature-gating core touches all consumers |
| SC-4 | LOW | Deps / duplicates | `Cargo.lock` (`cargo tree -d`) | Duplicated major versions across the tree: `bitflags 1.3.2 + 2.x`, `mio 0.8.11 + 1.2.1`, `rand 0.8 + 0.9`, `rand_core 0.6 + 0.9`, `getrandom 0.2 + 0.3 + 0.4`, `nom 7 + 8`, `hashbrown 0.15 + 0.17`, `heck 0.4 + 0.5`, `phf 0.11 + 0.12`, `tungstenite 0.24 + 0.29`, `quick-xml 0.32 + 0.38`. Old copies come mainly from `notify 6` (SC-5) and `rust-s3` (SC-1). | Compile time, binary size, and duplicated code paths (each version its own audit target). | Bump `notify` (SC-5) and replace `rust-s3` (SC-1) to collapse most of these; enforce a duplicate-version ceiling via `cargo deny` bans. | deferred: resolves with SC-1/SC-5; deny.toml warns now |
| SC-5 | LOW | Deps / outdated | `plugins/umbral-livereload/Cargo.toml:331` (`notify = "6"`) | `notify 6.1.1` is behind current (7.x/8.x) and pulls old transitives: `inotify 0.9.6`, `bitflags 1.3.2`, `mio 0.8.11`. Livereload is opt-in and dev-oriented, limiting blast radius. | Stale watcher stack; source of several SC-4 duplicates. Low because it should never be enabled in production. | Bump `notify` to the current major; document that `umbral-livereload` must not be enabled in prod builds. | deferred: notify 7/8 is API-breaking; dev-only |
| SC-6 | LOW | Facade | `crates/umbral/src/lib.rs:88` (`pub use serde_json as _serde_json;`), `:97` (`pub use umbral_core::_sea_query;`), `:105` (`pub use sqlx as _sqlx;`) | The facade re-exports the **full `sqlx` and `sea_query` surface** publicly (needed by macro-generated code). The `_` prefix is a naming convention only — nothing stops user/plugin code from writing `umbral::_sqlx::query("…")` and bypassing the ORM, the exact raw-SQL anti-pattern CLAUDE.md forbids. | Contract erosion, not a direct vuln: a plugin author can hand-roll SQLite-only, non-parameterised SQL through the "internal" re-export and evade the ORM's backend/parameterisation guarantees. | Keep the re-export (macros need it) but document it as `#[doc(hidden)]` and add the `cargo deny`/grep gate CLAUDE.md already prescribes for `sqlx::query` to also flag `_sqlx::`/`_serde_json` reaches in plugin/user crates. | ✅ done |
| SC-7 | LOW | Testing crate | `crates/umbral-testing/Cargo.toml` (`publish = true`; `sqlx`, `tempfile`, `fake` as normal `[dependencies]`) | The test-helper crate is publishable with its deps in `[dependencies]` (not dev/optional) and has no `cfg` gating. Nothing structurally prevents a downstream app from listing it under `[dependencies]`, shipping `TempPool`/`TestClient`/factories into a release binary. | Bloat + reachable test scaffolding in prod if mis-wired. **Mitigated:** the helpers are inert (in-tempfile SQLite pool, an in-process axum client, fake-data factories) — there is **no auth bypass, no `ensure_tables_for_tests`, no raw SQL, no `unsafe`** reachable here (verified: `grep` for `cfg(test)`/`ensure_tables`/`sqlx::query`/`CREATE TABLE`/`unsafe` in `src/` = 0 hits). Docs correctly steer users to `[dev-dependencies]`. | Optional: mark the crate's runtime helpers behind a doc note, or accept as-is (this mirrors `axum-test` and similar). No code change required for security. | ✅ done |
| SC-8 | LOW | Features | `plugins/umbral-logs/Cargo.toml:388` (`default = ["admin"]`), `plugins/umbral-tasks/Cargo.toml:979` (`default = ["admin"]`) | Both plugins pull the **entire `umbral-admin` UI** by default. Adding `umbral-logs` or `umbral-tasks` to a non-admin app silently drags in the admin crate + its auth/session/security/permissions dep fan-out unless the user sets `default-features = false`. | Larger default dependency graph than a "just logging"/"just tasks" app expects. | Keep the convenience default but document the `default-features = false` slim path prominently in each plugin's page (the manifests already comment it — surface it in user docs). | deferred: doc-only; manifests already note the slim path |

No issues found in the following areas I checked: `git`/`branch`/`rev` deps (none), `[patch]` overrides (none), `=`-pinned versions (none), REST/OpenAPI feature gating (structurally clean — core/facade don't depend on them), optional-dep `dep:` syntax (all correct), reqwest/lettre/reqwest-api TLS (all `rustls`, `default-features=false`, no accidental OpenSSL), facade top-level globs (none — only scoped `umbral_core::web::*`).

---

## C. Detailed findings (CRITICAL/HIGH)

### SC-1 (HIGH) — `rust-s3 0.35.1` drags an EOL TLS stack onto the S3 storage path

**Evidence.** `plugins/umbral-storage/Cargo.toml`:

```toml
# S3 (and S3-compatible: MinIO, R2) Storage backend …
s3 = ["dep:rust-s3", "dep:futures-executor"]
...
rust-s3 = { version = "0.35", optional = true, default-features = false, features = ["blocking", "tokio-rustls-tls"] }
```

`cargo tree -p umbral-storage --features s3` (abridged):

```
rust-s3 v0.35.1
├── attohttpc v0.28.5
│   └── rustls v0.23.40          # TLS stack #1
├── hyper v0.14.32               # legacy hyper
├── hyper-rustls v0.24.2
│   ├── hyper v0.14.32
│   └── rustls v0.21.12          # TLS stack #2 — EOL 0.21 line
│       └── rustls-webpki v0.101.7
├── tokio-rustls v0.24.1
│   └── rustls v0.21.12
├── rustls-native-certs v0.6.3
│   └── rustls-pemfile v1.0.4
└── quick-xml v0.32.0            # + a second quick-xml 0.38 elsewhere
```

`cargo tree -i rustls@0.21.12` confirms the **only** path to rustls 0.21 in the whole workspace is `rust-s3 → hyper-rustls 0.24 / tokio-rustls 0.24`. Nothing else uses it.

**Scenario.** A 10M-user deployment stores user media on S3/R2 and builds with `--features s3`. Every presigned upload/download and every bucket operation negotiates TLS through `rustls 0.21.12`. The rustls 0.21 series is end-of-life: the maintainers ship security fixes to 0.23+ only. When the next rustls advisory lands (record-splitting, cert-verification, or DoS class — the 0.21 line has had several historically), the S3 path silently remains vulnerable because no patched 0.21 will be published, and `cargo update` can't move a transitive that `rust-s3` pins. Specific current advisories are **unverified — needs cargo audit**, but the EOL status is a maintenance fact independent of any single CVE.

**Fix.** Replace the storage S3 backend with a maintained client on the modern stack. Sketch of the manifest change (the storage-module code change is out of scope for this audit, which is read-only on Rust source):

```toml
# plugins/umbral-storage/Cargo.toml — replace rust-s3
[dependencies.aws-sdk-s3]         # builds on hyper 1 + rustls 0.23
version = "1"
optional = true
default-features = false
features = ["rustls"]
# (or `object_store` with the `aws` feature for a lighter, sync-friendly API)
```

Until the swap lands, gate releases on `cargo audit` run against a lock generated with `--features s3`, and treat any `rustls`-0.21 / `hyper`-0.14 advisory as release-blocking. Document that `s3` currently carries an EOL TLS stack so operators can weigh it against a self-hosted static path.

---

## D. Blind spots (could not verify from provided artifacts)

- **No advisory scanner available.** `cargo audit`/`cargo deny` are not installed here, so no RUSTSEC IDs were confirmed. Every "known-CVE-class" statement is EOL/version-fact based and flagged "unverified — needs cargo audit." A real scan against multiple feature combinations (`--all-features`, `--features s3`, `--features redis`, `--features s3,otel`) is required before production sign-off.
- **`rsa 0.9.10` is in `Cargo.lock` but NOT in the compiled graph.** `cargo tree -i rsa` and `cargo tree -i rsa --all-features` both print "nothing to print" — it's a phantom lock entry from an unenabled sqlx-mysql code path. RUSTSEC-2023-0071 (Marvin timing side-channel, no fixed version) *would* apply if some build config compiles it; I could not construct one, so it is **not** counted as a live finding. Flag for the cargo-audit follow-up.
- **Actual production feature set unknown.** Whether `s3`, `redis`, `otel`, `email/api`, `realtime/redis` are enabled in the real deploy determines whether SC-1 is HIGH-live or dormant. Assumed S3-on for the 10M-user media scenario.
- **Transitive integrity of all 571 lock packages** not individually reviewed; only advisory-prone families (rustls/ring/hyper/openssl/time/rsa/idna/tungstenite/prost/tonic/mio/bitflags) were spot-checked.
- **`umbral-core` crypto (`crypto_box 0.9` + `rand_core 0.6` OsRng) for `Masked<T>`** was version-checked only (current RustCrypto). The *correctness* of the sealed-box construction is the Security auditor's scope, not this pass.

---

## E. Prioritized action plan

**Quick wins (< 1 day)**
1. Install and run `cargo audit` + `cargo deny check` locally against `--all-features`, `--features s3`, `--features redis`; triage output (closes the SC-2 gap and confirms/kills the rsa blind spot). Add `deny.toml`.
2. Bump `notify` to current major in `umbral-livereload` (SC-5) — collapses several SC-4 duplicates.
3. Surface the `default-features = false` slim path for `umbral-logs`/`umbral-tasks` in their user docs (SC-8); add `#[doc(hidden)]` to the facade's `_sqlx`/`_serde_json`/`_sea_query` re-exports (SC-6).

**Short term (< 2 weeks)**
4. Wire `cargo audit` + `cargo deny` into CI as a merge gate (SC-2).
5. Add a `cargo deny` ban that fails CI on rustls < 0.23 and hyper < 1 anywhere in the graph — this makes SC-1's regression visible and unmergeable.

**Structural (needs design work)**
6. Replace `rust-s3` with `aws-sdk-s3` / `object_store` on the S3 storage backend (SC-1) — the only fix that actually removes EOL rustls 0.21 / hyper 0.14.
7. Introduce `[features]` on `umbral-core` (`sqlite`/`postgres`/`markdown`/`timezone`/`pg-extra-types`) so apps can drop unused subsystems and their transitive deps (SC-3).

---

## Docs updated

**None.** I own `documentation/docs/v0.0.1/testing/` (`test-client.mdx`, `factories.mdx`). Both were checked against `crates/umbral-testing/src/lib.rs` and are accurate:
- `test-client.mdx` describes `TempPool` as "a tempfile-backed SQLite pool, deleted when the guard drops" — matches the impl (`with_max_connections` builds a `tempfile::tempdir()`-backed `SqliteConnectOptions`, `_dir: TempDir` dropped with the guard). The "dev-dependency, never shipped in a release build" Callout is correct guidance (and the right mitigation for SC-7).
- `factories.mdx` references `umbral_testing::fake::…` — backed by `pub use fake;` at `lib.rs:354`. The `Factory` trait / `seq()` / `build()`/`create*` surface matches the code.

No contradictions found; no edits made.

---

## Clarifying questions (would change severity)

1. Is the `s3` storage feature enabled in the production deploy? If yes, SC-1 stays HIGH-live; if S3 is never used, it drops to a dormant MEDIUM.
2. Is `umbral-livereload` ever compiled into production images? If it's strictly a local-dev plugin, SC-5/its share of SC-4 are cosmetic.
3. Do you intend `umbral-testing` to be usable at runtime (e.g. seed/QA endpoints), or is it strictly `[dev-dependencies]`? That decides whether SC-7 warrants making the deps optional.
