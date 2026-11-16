# review_4 · repo-hygiene

| | |
|---|---|
| **Sweep** | 2026-10-10 · commit `e0d5e18b` (+ uncommitted `planning/gaps6.md`) |
| **Size** | Repo-root surface (not a crate; LOC N/A by construction): 6 workflows · `deny.toml` · `release-plz.toml` + 28 crate `CHANGELOG.md`s · root `Cargo.toml`/`Cargo.lock` · root docs (README, CHANGELOG, STABILITY, SECURITY, LICENSE-*, CONTRIBUTING, llms.txt, FEATURES.md) · `scripts/` · stray artifacts |
| **Reviewer** | repo-hygiene reviewer (one of ~25) |
| **Verdict** | needs attention |
| **Findings** | 0 critical · 1 high · 1 medium · 9 low · 1 info — 12 new, 3 known |

## Summary

- The repo-root automation is exactly two things: a supply-chain audit (`audit.yml`) and one scaffold **type-check** (`scaffold.yml`). No workflow runs the workspace test suite, `cargo fmt --check`, `cargo clippy`, or a feature matrix — for 28 crates published in lockstep (HYGIENE-1). That is the one structural item in this file.
- `deny.toml` is commit 0.0's supply-chain gate that was never wired, and its empty `advisories.ignore` directly contradicts the six triaged ignores in `.cargo/audit.toml`; `audit_2`'s verification stamp nonetheless records SC-2 as Fixed (HYGIENE-2).
- Release plumbing itself is consistent: `release-plz.toml` covers all 28 members 1:1, all 28 `*-v0.0.13` tags exist, and `Cargo.lock` is committed at the root as CLAUDE.md requires. The 9 crate changelogs without a `0.0.13` section are release-plz's correct no-change behaviour (verified against git history), not a defect.
- The rest is documentation drift, all one-edit fixes: the root `CHANGELOG.md` never got a `[0.0.13]` section while `[Unreleased]` still holds shipped 0.0.13 items (HYGIENE-3); `STABILITY.md` says 0.0.11 and claims CI enforcement that does not exist (HYGIENE-4); `crates/README.md` still describes the pre-`ec8dda33` layout (HYGIENE-5); `README.md:46` advertises `umbral startapp`, removed in 0.0.13 (HYGIENE-11); `REAL-GAPS.md` two rows claim shipped state the tree contradicts (HYGIENE-8).
- Fix order: (1) one test workflow — `fmt`/`clippy`/`test --workspace` + `--no-default-features` checks (HYGIENE-1); (2) decide the audit/deny gate and drop the false stamp (HYGIENE-2); (3) the doc batch (HYGIENE-3…11), which is an afternoon total.

## Findings

### HYGIENE-1 · HIGH · ci/process · self-verified
**No CI job runs the workspace test suite (or `fmt`, `clippy`, or a feature matrix); the only automated checks on the 28 published crates are a dependency audit, one `#[ignore]`d scaffold test, and the website release build.**

- **Where**: `.github/workflows/` (all six workflows). The complete set of `cargo` invocations in the tree: `scaffold.yml:64`, `deploy-website.yml:93`, `audit.yml:42,47`.
- **Evidence**: enumerating every workflow — the *only* Rust steps are
  - `scaffold.yml:64` `run: cargo test -p umbral-cli --test scaffold_compiles -- --ignored --nocapture`
  - `deploy-website.yml:93` `cargo build --release --locked` (the website's own standalone project)
  - `audit.yml:42,47` `cargo install cargo-audit --locked` / `cargo audit`

  `scaffold.yml`'s own header states the absence and why the one check it has is deliberately not a test job (lines 20-22):
  ```
  # It is its own workflow rather than part of a test job because it type-checks a whole
  # application (every umbral crate plus axum, sqlx, ...) and is minutes, not seconds.
  # The test itself is `#[ignore]`d for the same reason; this is what un-ignores it.
  ```
  The scaffold test is `#[ignore]`d precisely so the normal loop skips it — routed from `planning/review_4/umbral-cli.md:162`: "a local `cargo test` never builds generated output; flows to the CI/test-infra reviewer." There is no other CI provider config (`.github/` contains only `workflows/`). CONTRIBUTING.md:90-97's substitute is local discipline ("A change in `umbral-core` can silently break the facade's re-exports, so verify everything ... `cargo fmt` / `cargo clippy --all-targets` / `cargo build` / `cargo test`").
- **Impact**: a behaviour regression that compiles passes every gate: `audit.yml` does not trigger on `src` changes (its paths are dependency manifests, `Cargo.lock`, `.cargo/audit.toml`, itself), and `scaffold.yml` type-checks a scaffolded app against the checkout — a compile-level fixpoint, not a test run. Trace one scenario: change `to_snake_case` in `crates/umbral-casing` (docs call it "the **canonical** implementation — byte-identical to what `#[derive(Model)]` uses to compute a model's `TABLE` constant"); the change compiles, no workflow runs the casing tests that pin the ASCII boundary rules, the change reaches `main`, and the next `release` dispatch publishes 28 crates — the first signal is a user's migration diffing table names. The cost is already incurred at this commit: `umbral-tasks`' `--no-default-features` break is described by the sibling reviewer as invisible precisely because "every default-features build and test stays green, so CI cannot see it" (`planning/review_4/umbral-tasks.md:62`), and its proposed fix is a one-line CI check (`--no-default-features`); `crates/umbral-core`'s Postgres aggregate decodes wrong while its test suite is SQLite-only (`planning/review_4/umbral-core.md:47`), so nothing anywhere exercised the PG arm. The local fallback is undermined by the repo's own disk constraint: root `Cargo.toml:65-72` documents `target/` at ~80 GB without the line-tables profile — the machine that is supposed to run the suite is the one where running it is most expensive.
- **Fix**: add a `test` workflow on `push`/`pull_request` with the same path filter as `scaffold.yml` (`crates/**`, `plugins/**`, `Cargo.lock`): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`. Add a small matrix (or one job) running `cargo check -p <plugin> --no-default-features` per plugin so the routed umbral-tasks defect class dies in CI; optionally a Postgres service container for the `*_pg` suites so the PG arm is exercised somewhere. This is a defense-in-depth gap meeting the HIGH definition — the concrete scenario above is on the release path of all 28 crates.
- **Self-verification (static)**: re-read = enumerated all six workflow files end-to-end; refute = searched for any other Rust runner (none; no other CI config), and `release`'s `cargo publish` builds packages but runs no tests; trace = the casing/`TABLE` scenario above; quote = the three invocations and the scaffold header verbatim.

### HYGIENE-2 · MEDIUM · config/drift · self-verified
**`deny.toml` is a committed supply-chain gate that nothing runs, and its empty `advisories.ignore` contradicts `.cargo/audit.toml`'s six triaged ignores — the config comment claims CI wiring that does not exist, and audit_2's stamp records it as Fixed.**

- **Where**: `deny.toml:1-6,15-19,26-27`; `.cargo/audit.toml:11-45`; `SECURITY.md:33-35`; `planning/audit_2/findings/supply-chain.md:3`; `planning/gaps5.md:109`.
- **Evidence**: `deny.toml:1-6`:
  ```
  # cargo-deny configuration — the supply-chain gate (audit_2 supply-chain SC-2).
  #
  # Run locally / in CI:
  #   cargo install cargo-deny        # one-time
  #   cargo deny check                # advisories + bans + licenses + sources
  ```
  `grep -rn "deny" .github/` = 0 hits; no workflow installs or runs `cargo-deny`. And the two configs disagree on the same advisory set:
  ```
  [advisories]
  version = 2
  yanked = "deny"
  ignore = []          # deny.toml:15-19 — "Empty = every open advisory is
                       # release-blocking, which is the intent"
  ```
  versus `.cargo/audit.toml:11-45`, which deliberately tolerates six RUSTSEC IDs with per-entry reachability arguments (e.g. `"RUSTSEC-2023-0071"` — "rsa — Marvin timing side-channel... a Cargo.lock phantom"). `planning/audit_2/findings/supply-chain.md:3` stamps SC-2 "**Fixed** ... SC-2 (`deny.toml` + `audit.yml` CI)" — the `audit.yml` half is true (`audit.yml` header says "Added for audit_2 H24"); the `deny.toml` half is contradicted by the tree. Per the rubric, a shipped tracker claim contradicted by the code is a numbered finding (cross-ref in Known, not a substitute). `SECURITY.md:33-35` compounds it: the roadmap says "`cargo-audit` and `cargo-deny` run in CI", but cargo-audit has since shipped while cargo-deny genuinely has not — the line is half-stale in one direction and accurate in the other.
- **Impact**: triage believes the license/ban/duplicate-version gate is on; it is not — nothing checks licenses (e.g. a copyleft dep entering the tree), wildcard deps, or the duplicate-version debt `deny.toml:20-27` documents. Worse, whoever wires `cargo deny check` per the header gets a red first run on exactly the advisories `audit.toml` triages as unreachable (Marvin/rsa, quick-xml, rkyv), inviting a blanket `ignore = []` → "just clear it" resolution that destroys audit.toml's justification discipline. One more coverage gap on top: `audit.yml` scans only the root `Cargo.lock`, while `umbral_website/Cargo.lock` and eight `examples/*/Cargo.lock` (standalone projects that get built; the website one gets deployed) are never audited.
- **Fix**: pick one gate and make it real — either add a `cargo deny` job whose `advisories.ignore` mirrors audit.toml's six entries with the same comments, or delete `deny.toml` and say audit.toml is the gate; correct the `audit_2` stamp either way. Extend the audit job to the standalone lockfiles (`cargo audit --file umbral_website/Cargo.lock`, or document the exclusion explicitly).

### HYGIENE-3 · LOW · docs/process
**The root `CHANGELOG.md` has no `0.0.13` section: `[Unreleased]` still carries features that shipped in 0.0.13, and its compare link still points at `v0.0.12`.**

- **Where**: `CHANGELOG.md:15` (`## [Unreleased]`), `:51` (`## [0.0.12]`), `:157` (`[Unreleased]: ...compare/umbral-v0.0.12...HEAD`); `changelog/0.0.13.md`.
- **Evidence**: the `[Unreleased]` body (line 15 ff.) holds e.g. "**Bring-your-own email sender (`EmailBackend` trait, features #91).**" while `changelog/0.0.13.md:30` lists the same feature under 0.0.13 (“**Email** - a pluggable `EmailBackend` trait…”). `grep "^## \[0.0.13\]"` on the root file = no hits; the previous-releases list links `./changelog/0.0.x.md` but stops before adding 0.0.13; `e0d5e18b` ("docs(changelog): 0.0.13 release notes") added `changelog/0.0.13.md` and index without updating the root file.
- **Impact**: the repo's canonical `CHANGELOG.md` tells a reader that shipped features are unreleased, and the public release history visibly skips a version. `changelog/` (added at the sweep commit) now overlaps this file's role with no statement of which is authoritative.
- **Fix**: promote `[Unreleased]` to `## [0.0.13]`, link `changelog/0.0.13.md`, add `[0.0.13]: ...umbral-v0.0.12...umbral-v0.0.13` and repoint `[Unreleased]` at `...umbral-v0.0.13...HEAD` — or declare `changelog/` canonical in one line and stop maintaining the root body.

### HYGIENE-4 · LOW · docs
**`STABILITY.md` is stale by two releases and claims CI enforcement that no workflow provides.**

- **Where**: `STABILITY.md:3`, `:17`, `:28`, `:48`.
- **Evidence**: line 3 "Status: draft (planning/gaps5.md #2, tf #215). Current release: 0.0.11, pre-1.0."; line 17 `"umbral 0.0.11" is unambiguous across the facade, core, macros, CLI, and every plugin`; line 28 "MSRV is **Rust 1.85** (Rust edition 2024), declared as `rust-version` in the workspace and enforced in CI."; line 48 "The tiers, deprecation windows, and MSRV policy here are enforced in CI". No workflow references `rust-version`, MSRV, or a stability check.
- **Impact**: this is the file a downstream reviewer reads to judge semver/MSRV posture; both mechanical facts (current version, "enforced in CI") are wrong — and the wrong CI claim is exactly the gap HYGIENE-1 files. The version is stale twice over (0.0.12 and 0.0.13 shipped since).
- **Fix**: bump to 0.0.13; reword lines 28/48 to the actual mechanism ("declared as `rust-version` in the workspace manifest"; a CI job lands with HYGIENE-1) or delete the CI clauses until they are true.

### HYGIENE-5 · LOW · docs
**`crates/README.md` still documents the pre-`ec8dda33` workspace layout, and its crate table omits two crates.**

- **Where**: `crates/README.md:26`, `:38`, table at `:7-12`.
- **Evidence**: line 26 "All cargo commands run from inside this directory:"; line 38 "Build artefacts land at `crates/target/` and `crates/Cargo.lock` (both inside this directory, gitignored except `Cargo.lock`…)". Reality since `ec8dda33` ("refactor(workspace): move the Cargo workspace root to the repo root"): `crates/Cargo.toml` deleted, `crates/Cargo.lock` renamed to root `Cargo.lock`, artifacts under the repo-root `target/`. That commit updated only the "new crate" snippet (`crates/README.md:87-89`) and left these lines. The table lists only `umbral-core`, `umbral-macros`, `umbral`, `umbral-cli` — `umbral-casing` and `umbral-testing` are missing. Root `Cargo.toml:71` points readers here for the disk guidance ("See crates/README.md \"Build, test, run\""), so the stale lines are on a path people actually follow.
- **Impact**: a contributor follows the wrong convention and looks for artifacts (and disk usage) in directories that no longer exist, while debugging the ~80 GB target problem.
- **Fix**: three-line edit — "from the repo root", root `target/` + root `Cargo.lock`; add the two missing table rows.

### HYGIENE-6 · LOW · repo/scripts
**`scripts/create_db.sh` is gitignored yet referenced by an example README, so a fresh clone cannot run the documented helper — and the file contains no secret that would justify the ignore.**

- **Where**: `.gitignore:23` (`create_db.sh`); `scripts/create_db.sh` (present on disk, untracked); `examples/derive-demo/README.md:12`.
- **Evidence**: `git check-ignore -v scripts/create_db.sh` → `.gitignore:23:create_db.sh`; `git ls-files scripts/` shows it untracked; the example README says `bash scripts/create_db.sh`. Reading the script, it generates the password from the database name (`DB_PASSWORD="$DB_NAME"`) for local test databases via `sudo -u postgres` — there is no credential in it to protect.
- **Impact**: the documented path fails on every machine but the author's ("No such file or directory"); the ignore rule protects nothing.
- **Fix**: commit the script (drop the `.gitignore:23` entry), or delete the example README's reference. The `examples/` side is out of this sweep's scope — routed below.

### HYGIENE-7 · LOW · config/drift
**`deploy-docs.yml`'s stated rationale for yarn is false (nothing declares a package manager), and `documentation/` commits two competing lockfile families.**

- **Where**: `.github/workflows/deploy-docs.yml:6`, `:44-47`; `documentation/specra.config.json`; `documentation/pnpm-lock.yaml`, `documentation/pnpm-workspace.yaml`, `documentation/yarn.lock`.
- **Evidence**: workflow line 6: "# It builds with yarn (the package manager declared in specra.config.json) and" — `grep -rn "packageManager" documentation/specra.config.json documentation/package.json` = 0 hits. `git ls-files documentation | grep lock` shows **both** `documentation/pnpm-lock.yaml` (Jun 1) and `documentation/yarn.lock` (Jul 11) tracked, plus `pnpm-workspace.yaml`; CI installs with `yarn install --frozen-lockfile` (`deploy-docs.yml:47`).
- **Impact**: two lockfiles advertise two supported package managers; whoever runs `pnpm install` updates a lockfile no pipeline consumes, and the real (yarn) install is the only one kept green without any statement that it is canonical. The justifying comment is unverifiable from the tree.
- **Fix**: delete `pnpm-lock.yaml` + `pnpm-workspace.yaml` (or migrate CI to pnpm), and correct the comment to name what is actually true (yarn.lock is the lockfile CI uses). The `documentation/` tree is out of this sweep's scope by decision; the workflow half is this file's.

### HYGIENE-8 · LOW · docs/tracker
**`planning/REAL-GAPS.md` carries two shipped-status claims the tree contradicts: a per-test DB rollback that does not exist, and a crate that does not exist.**

- **Where**: `planning/REAL-GAPS.md:35` (row 18, Testing Utilities) and `:27` (row 10, File Uploads & Media Storage).
- **Evidence**: line 35 marks row 18 "✅ `umbral-testing` — `TestClient`/`TempPool`/`TestResponse` + `Factory` trait …" against the requirement text "Test client that boots the app in-memory, **DB rollback per test**, factory fixtures". `umbral-testing` has no rollback-per-test; its own boot documentation says rows persist for the life of the test binary (one database per process). Routed to this file by `planning/review_4/umbral.md:101`. Line 27 says "pluggable `Storage` (`FsStorage` in umbral-media)" — no `plugins/umbral-media` exists (`ls plugins | grep -i media` empty; media merged into `umbral-storage`).
- **Impact**: the tracker's ✅ hides the strongest reason to build the missing feature (test isolation), and dead crate names send readers to nothing. Rubric note: a shipped tracker claim contradicted by the code is a numbered finding, never Known-only.
- **Fix**: correct both rows — mark the rollback part unbuilt (the umbrella item can stay ✅ for the rest, with the gap named), and rename `umbral-media` to `umbral-storage` (or drop the parenthetical).

### HYGIENE-9 · LOW · docs/spec
**`arch.md` promises a `test_with_pool(pool, async { … })` helper that was never shipped (0 grep hits), routed to this file by the facade reviewer.**

- **Where**: `arch.md:101`; routed via `planning/review_4/umbral.md:99`.
- **Evidence**: "The override path is a `Manager::on(&pool)` explicit-pool escape hatch plus a `test_with_pool(pool, async { ... })` helper that scopes the override for a test future. Designed concretely in `docs/specs/01-app-and-settings.md`." `grep -rn "test_with_pool" crates/ plugins/` = 0 hits. The shipped substitute is `umbral-testing::boot` (whose one-shot semantics are reviewed in `planning/review_4/umbral-testing.md`).
- **Impact**: `arch.md` is the authoritative spec per CLAUDE.md; its "Designed concretely" pointer makes a non-existent API look shipped, and contributors write tests against it. Doc-vs-reality drift in the one file that is supposed to be canonical.
- **Fix**: strike the helper from `arch.md:101` (or implement it); record the decision in `docs/specs/01-app-and-settings.md`, which the sentence cites.

### HYGIENE-10 · LOW · ci/supply-chain
**Deploy/publish actions are pinned to floating tags — including the third-party actions that receive the deploy SSH key and the crates.io token — and no dependabot config exists.**

- **Where**: `.github/workflows/deploy-website.yml:194,219`; `.github/workflows/release-plz.yml:59`; `.github/workflows/deploy-docs.yml:56`; `.github/workflows/deploy-architecture.yml:46`; no `.github/dependabot.yml` (`.github/` holds only `workflows/`).
- **Evidence**: `appleboy/scp-action@v0.1.7` and `appleboy/ssh-action@v1.0.3` (`deploy-website.yml`) are handed `secrets.SSH_PRIVATE_KEY`, `secrets.CONTABO_HOST/USER` (`:196-198,221-223`); `release-plz/action@v0.5` runs the publish path; `peaceiris/actions-gh-pages@v4` pushes with a repo-write token (and, in `deploy-architecture.yml`, a cross-repo deploy key per its header). All are movable version tags, not commit SHAs.
- **Impact**: a moved or compromised tag executes with the repo's deploy/publish credentials; floating tags also let step behaviour change between identical commits. Standard hardening gap, not a live incident.
- **Fix**: pin third-party actions to full commit SHAs (at minimum the four credential-bearing ones) and add `.github/dependabot.yml` with the `github-actions` ecosystem so the pins get bumped.

### HYGIENE-11 · LOW · docs
**The root README advertises `umbral startapp`, a command 0.0.13 removed.**

- **Where**: `README.md:46`; `changelog/0.0.13.md`; `crates/umbral-cli/src/scaffold_cli.rs:339-345`.
- **Evidence**: README lines 43-47 — "Add a new plugin (an \"app\") to your project at any time:" followed by `umbral startapp blog`. 0.0.13 merged startapp into startplugin; the CLI's own test asserts `startapp` is **not** a scaffolder. The README is the repo front page a new user copies from.
- **Impact**: the advertised on-ramp command fails or misleads; the first command a reader runs is wrong.
- **Fix**: replace with the shipped surface (`umbral startplugin …` / `umbral plugin add`, per `changelog/0.0.13.md`). CLI-internal wording is the CLI reviewer's; this is the README claim.

### HYGIENE-12 · INFO · docs
**Where**: `FEATURES.md:26` (mtime 2026-06-26) and `llms.txt:3` (mtime 2026-07-08) — dated docs that drift silently. `FEATURES.md:26` lists a "✅ **umbral-static plugin.**" that no longer exists under that name (`ls plugins/` has no `static` or `media` crate; serving lives in `umbral-storage` / `umbral-core`'s `static_files`), and `llms.txt` predates the 0.0.7–0.0.13 surface entirely. No action required beyond a refresh or an "as of 0.0.X" stamp on both.

## Strengths

- **`audit.yml` is the best-shaped workflow in the tree and does exactly what its name says**: correct trigger set (dependency manifests + `Cargo.lock` + `.cargo/audit.toml` + itself), a weekly cron to surface newly-published advisories against unchanged pins, `contents: read`, and per-entry justified ignores in `.cargo/audit.toml` with explicit "delete this when the upstream fix resolves" instructions — the opposite of a blanket silence.
- **Release plumbing is exact**: `release-plz.toml` has a `[[package]]` block for all 28 workspace members (checked set-wise against root `Cargo.toml`, no missing/extra), and all 28 `*-v0.0.13` tags exist — the lockstep `version_group` story holds end-to-end, including for the 9 packages with no changes (their missing changelog section is release-plz's correct no-op, verified below).
- **`Cargo.lock` is committed at the root** and was carried through the workspace move (`ec8dda33`) — right for a tree with binaries, and it is what `audit.yml` scans and what makes the repo's builds reproducible.
- **`deploy-website.yml` is a careful deploy**: builds in a fixed path (`/app`) so `CARGO_MANIFEST_DIR`-frozen paths survive, decrypts sops/age secrets only at deploy time, stages with explicit excludes (`keys.txt`, `.env`, `*.db` per its header), and the release job fails the run if `migrate` errors or the healthcheck never goes healthy.
- **`.gitignore` covers the real hazards**: `*.db` (`:17`), `.env` (`:38`), `target`; no database or environment artifact is tracked (`git ls-files | grep '\.db$'` = empty; root `.env` is ignored, never committed).
- **The disk-pressure configuration is real and documented where needed**: root `Cargo.toml:73-77` sets `debug = "line-tables-only"` for `[profile.dev]`/`[profile.test]` with the ~80 GB→~20 GB rationale and a pointer to the env-var alternative — the profile is committed, not folklore.

## Clean areas checked

- **License files**: `LICENSE-MIT` and `LICENSE-APACHE` present and consistent with `license = "MIT OR Apache-2.0"`; every crate inherits via `license.workspace = true`.
- **Per-crate `CHANGELOG.md` coverage vs 0.0.13** (examined per the sweep brief): 9 of 28 crates lack a `[0.0.13]` section (umbral-casing, umbral-testing, analytics, health, livereload, logs, realtime, rls, tenants), but `git log 203fb853..25c53161 -- <those paths>` contains only the release commit's dependency-pin bumps — under `version_group` release-plz writes no section for a package with no changes. Not a defect; the scratch file's F6 asked for confirmation and this confirms it (F6 covered only the 6 `crates/*`; the same holds for 7 plugins).
- **Workflow trigger paths**: `scaffold.yml` fires on exactly the paths that can break the scaffold's compile fixpoint (`crates/**`, `plugins/**`, `Cargo.lock`, itself); `audit.yml`'s paths are the right ones for a lockfile gate; the three deploy workflows are `workflow_dispatch`-only except `deploy-architecture` (push to `main`, path-filtered) — appropriate, no trigger gap found.
- **Actions vs permissions scope**: each workflow requests minimal permissions (`contents: read` for audit/scaffold; `contents: write` only where a push happens), and `release-plz` deliberately sets `cancel-in-progress: false` to never cancel an in-flight publish.
- **Destructive commands in automation**: none — no `rm -rf` outside the website build staging area, no `git reset`, no database deletion in any workflow.
- **Root manifest vs documented layout**: members/excludes match CLAUDE.md (6 crates + 22 plugins; `examples`, `umbral_website`, `documentation` excluded), `edition 2024` / `rust-version 1.85` / resolver 3 as documented.
- **`release-plz.yml` inputs**: the `dry_run` boolean is passed through as a string per the action's interface — matches release-plz/action@v0.5's documented usage; not a finding.
- **Stray artifacts at the repo root**: only an empty `.tmp_build/` directory (untracked, invisible to git) and the git-ignored `crates/umbra-cli-demo.db` (Known below) — nothing tracked that should not be.
- **`.env` / secret files at the root**: none tracked; the `age` key handling in `deploy-website.yml` writes to `~/.config/sops/age/` on the runner, never the workspace.

## Known issues already tracked elsewhere (not counted in the totals)

- `planning/review_4/umbral-cli.md:160` — `crates/umbra-cli-demo.db` (pre-rename artifact, May 30, git-ignored by `.gitignore:17`, zero source references); the CLI reviewer owns it; provenance verified again here and left for that triage rather than renumbered. `[open]`
- `planning/review_4/umbral.md` UMBRAL-2 — `arch.md:65-66`'s escape hatches (`umbral::axum::*`, `umbral::db::query!`) do not exist; re-verified for this file (facade has no `axum` module; `umbral::db::query` re-exports `_sea_query::Value` for `raw_with`; sqlx's `macros` feature is deliberately off) and the same table is copied in `docs/decisions/2026-05-30-spec-set-design.md:45-46,190` — the fix should sweep that copy too. `[open]`
- `planning/gaps5.md:109` (gaps5 #20, tf#233) — open class-level item whose recommended fix includes "cargo-deny/cargo-audit CI". HYGIENE-2 is the *specific* defect (deny half unshipped + contradicted `audit_2` stamp + config contradiction), kept numbered per the rubric's class-tracker boundary. `[open]`

## Out-of-scope notes

- `examples/derive-demo/README.md:12` — references `scripts/create_db.sh` (HYGIENE-6); `examples/` is out of this sweep's scope, so the README half routes to whoever owns examples — the script half should simply be committed.
- `planning/review_4/umbral-cli.md:162` — the routed note that `scaffold_compiles` runs only in CI and is `#[ignore]`d locally; absorbed into HYGIENE-1's evidence (its CI-job request is part of that fix).
- `planning/review_4/umbral-tasks.md:63` — the routed "add `cargo check -p umbral-tasks --no-default-features` to CI" request; absorbed into HYGIENE-1's fix sketch.
- `docs/decisions/2026-05-30-spec-set-design.md:45-46,190` — the same false escape-hatch table as UMBRAL-2; belongs to that finding's fix, noted here so the decision-doc copy is not missed.
- `documentation/` lockfile duplication and the false `specra.config.json` comment (HYGIENE-7): the tree is out of scope by the sweep's decision, but the workflow comment is this file's; no separate route needed.
