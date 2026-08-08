# Live-service CI and reliability/chaos testing

Status: draft for ratification (proposes the answers to gaps5 #93 and #94; the final call is the maintainer's)
Date: 2026-08-08
Closes: planning/gaps5.md #93 (tf #306), planning/gaps5.md #94 (tf #307)

## Honesty up front

Both of these are CI-infrastructure items, not code changes that can be merged and called done. Their whole value is that a hosted runner boots real Postgres, Redis, and MinIO (and, for #94, a fault-injection proxy) and runs the suite against them on every push. Nothing in this document can be verified locally beyond the one code fix it calls out (the SQLITE_BUSY flake); the workflows themselves only prove out once they run on GitHub Actions with the service containers attached. This doc is the design and the acceptance criteria, not a claim that live CI already passes. The one thing shipped here that runs without any new infra is the busy_timeout flake fix.

## Where CI is today

The repo has five GitHub Actions workflows, none of which run the test suite against a live database:

- `audit.yml` - cargo-audit against the RustSec DB on dependency changes and weekly.
- `release-plz.yml` - release PR and crate publishing.
- `deploy-docs.yml`, `deploy-website.yml` - static site deploys.
- `scaffold.yml` - scaffold smoke checks.

There is no `cargo test` job at all. `crates/README.md:63-77` documents the status quo: most tests run on in-memory SQLite and need nothing; a handful of Postgres-only tests (full-text search, native uuid relations, array/JSON/network types, PG backup) are gated `#[ignore]` and self-skip unless `UMBRAL_TEST_POSTGRES_URL` points at a server, in which case you run them with `--include-ignored`. That is a reasonable local story, but it means the Postgres-only behaviour is never exercised by any automated gate, and Redis/S3-backed paths (cache, distributed limiter seams, storage) have no live coverage either. This is exactly the hole gaps5 #93 names.

## #93 - Live-service CI matrix

### Goal

A GitHub Actions job that boots real Postgres, Redis, and MinIO as service containers, points the framework's env vars at them, and runs the full suite including the `--include-ignored` Postgres tests, across the relevant feature-flag combinations. This turns "Postgres behaviour is tested if you remember to set a URL locally" into "Postgres behaviour is a merge gate."

### Design

Use GitHub Actions `services:` containers rather than Docker Compose for the base matrix. Service containers are the native, cache-friendly path: the runner starts them, health-checks them, and exposes them on `localhost` before the job's steps run. Compose is kept as the documented local-repro path (a `docker-compose.ci.yml`) and as the substrate for the #94 chaos job, which needs the toxiproxy sidecar that `services:` cannot wire between containers as cleanly.

Services for the base job:

- Postgres (`postgres:16`), health-checked with `pg_isready`, exposed on 5432. Sets `UMBRAL_TEST_POSTGRES_URL=postgres://umbral:umbral@localhost:5432/umbral_test`.
- Redis (`redis:7`), health-checked with `redis-cli ping`, exposed on 6379. Sets `UMBRAL_TEST_REDIS_URL=redis://localhost:6379`.
- MinIO (`minio/minio`), health-checked on `/minio/health/ready`, exposed on 9000, with a fixed access/secret key and a pre-created bucket (an init step runs `mc mb`). Sets the S3-compatible env the storage plugin reads (endpoint, region `us-east-1`, path-style addressing on).

Because several plugins currently read live-service config only from `#[ignore]`d tests or from `Settings`, part of landing #93 is making sure each such suite reads its endpoint from an env var and self-skips (not fails) when the var is absent - mirroring the existing `UMBRAL_TEST_POSTGRES_URL` convention. Any suite that hard-requires a service without an env gate is a bug to fix as this lands, so the SQLite-only local run stays green.

Feature-flag matrix. The framework gates optional behaviour behind cargo features (thumbnails/media processing, plugin backends, etc.), so a single `cargo test` does not exercise the flagged paths. The job runs a small, explicit matrix rather than a combinatorial explosion:

- `default` - the default feature set, all services available.
- `all-features` - `cargo test --all-features --workspace`, the widest surface.
- `no-default-features` per crate where that is meaningful (proves the REST-free / minimal build still compiles and tests, which is the architectural promise in CLAUDE.md that "a REST-free app compiles with zero serializer code").

Each matrix leg runs `cargo test --workspace ... -- --include-ignored` so the Postgres-gated tests actually execute against the live server. Disk pressure is real here (a full build approaches ~78 GB per the release notes), so the job sets `CARGO_INCREMENTAL=0` and the line-tables-only debug profile the workspace already configures, and uses a single shared `target/` across matrix legs where the runner allows.

### Sketch (illustrative, not yet validated on a runner)

```yaml
name: live-ci
on:
  pull_request:
  push:
    branches: [main]
  workflow_dispatch:

jobs:
  live:
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        features: [default, all-features]
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_USER: umbral
          POSTGRES_PASSWORD: umbral
          POSTGRES_DB: umbral_test
        ports: ["5432:5432"]
        options: >-
          --health-cmd "pg_isready -U umbral"
          --health-interval 5s --health-timeout 5s --health-retries 10
      redis:
        image: redis:7
        ports: ["6379:6379"]
        options: >-
          --health-cmd "redis-cli ping"
          --health-interval 5s --health-timeout 5s --health-retries 10
      minio:
        image: bitnami/minio:latest
        env:
          MINIO_ROOT_USER: umbral
          MINIO_ROOT_PASSWORD: umbralsecret
          MINIO_DEFAULT_BUCKETS: umbral-test
        ports: ["9000:9000"]
        options: >-
          --health-cmd "curl -f http://localhost:9000/minio/health/ready"
          --health-interval 5s --health-timeout 5s --health-retries 20
    env:
      CARGO_INCREMENTAL: "0"
      CARGO_PROFILE_TEST_DEBUG: line-tables-only
      UMBRAL_TEST_POSTGRES_URL: postgres://umbral:umbral@localhost:5432/umbral_test
      UMBRAL_TEST_REDIS_URL: redis://localhost:6379
      UMBRAL_TEST_S3_ENDPOINT: http://localhost:9000
      UMBRAL_TEST_S3_ACCESS_KEY: umbral
      UMBRAL_TEST_S3_SECRET_KEY: umbralsecret
      UMBRAL_TEST_S3_BUCKET: umbral-test
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Test (${{ matrix.features }})
        run: |
          FLAGS=""
          [ "${{ matrix.features }}" = "all-features" ] && FLAGS="--all-features"
          cargo test --workspace $FLAGS -- --include-ignored
```

The exact image tags, env var names each plugin reads, and the feature legs are the parts to nail down against the live plugins when this is implemented; treat the sketch as the shape, not the final file.

### The SQLITE_BUSY flake fix (shipped with #93)

There is a known flake: a subset of tests hand-roll their own `SqliteConnectOptions` pools instead of going through the framework's `db::connect_sqlite` (which already sets `busy_timeout(5s)`, WAL, `synchronous=NORMAL`, and `foreign_keys=ON`, see `crates/umbral-core/src/db.rs:117-142`) or the `umbral-testing::TempPool` helper (which also applies `busy_timeout(5s)`, see `crates/umbral-testing/src/lib.rs:72-88`). A file-backed pool with more than one connection and no busy_timeout makes two concurrent writers race straight to `SQLITE_BUSY` instead of the second one waiting. Under CI's parallel test execution this surfaces as intermittent failures.

Current state (audited 2026-08-08): the production and shared-helper paths are already correct; most hand-rolled test pools have already been given a `busy_timeout(5s)`. About 30 test files repo-wide still build a raw pool without one. The memory of "~150 test pools" was the original scale; the residual is now the ~30 that were not swept.

Fix, root cause not symptom (per CLAUDE.md "Fix, don't patch"): the durable fix is not to sprinkle `.busy_timeout(...)` onto 30 more call sites and leave the 31st for next time. It is to route every test pool through one helper so the pragma is set in exactly one place:

1. Add a `umbral_testing::sqlite_pool()` (and a `sqlite_pool_with_max(n)`) that returns a configured `SqlitePool` with the same pragmas the production `connect_sqlite` uses - WAL, `synchronous=NORMAL`, `busy_timeout(5s)`, `foreign_keys=ON`. `TempPool` already encodes this; expose the bare-pool form next to it.
2. Migrate the ~30 hand-rolled call sites to the helper. After migration, a grep for `SqliteConnectOptions` in `tests/` outside the helper is the lint: a new hit is a review flag, the same way `sqlx::query` in a plugin is.
3. Optionally add a small test that asserts the helper's pool reports `busy_timeout = 5000` (there is already `connect_sqlite_sets_busy_timeout` in `crates/umbral-core/tests/sqlite_pragmas.rs` covering the production path; mirror it for the test helper).

This is the one part of #93 that is a real, locally-verifiable code change and it should land as its own commit ahead of the workflow, so the live matrix starts from a flake-free baseline instead of blaming the runner for a pool-config bug.

### Acceptance criteria for #93

- A `live-ci` workflow exists and runs on PR + push to main.
- It boots Postgres, Redis, and MinIO as healthy services.
- The `#[ignore]`d Postgres suites run via `--include-ignored` and pass against the live server.
- At least the `default` and `all-features` legs are green.
- The SQLITE_BUSY flake fix has landed (single helper, call sites migrated) and the suite is green across repeated runs.
- Local SQLite-only `cargo test` still passes with no services present (every live suite self-skips on a missing env var).

## #94 - Reliability / chaos testing

### Goal

Fault-injection tests that prove the framework degrades and recovers correctly when its dependencies misbehave: database failover and latency, Redis disconnects, S3 latency, worker crashes mid-task, broker partitions, and realtime reconnect storms. These are inherently slow and infra-heavy, so they run as an opt-in, long-running CI job (nightly schedule + `workflow_dispatch`), never on the PR critical path.

### Design

Build on Docker Compose (not `services:`) because the interesting faults are between the app and its dependency, which means a fault-injection proxy has to sit in the middle. Use toxiproxy (Shopify) as that proxy: the app connects to Postgres/Redis/S3 through toxiproxy ports, and the test drives toxiproxy's control API to add latency, cut the connection, or slice bandwidth mid-run, then asserts the framework's behaviour.

The scenarios, mapped to what the framework already provides so the tests exercise real recovery paths rather than mocks:

- Worker crash mid-task -> reclaim. This one has first-class support already: `reclaim_orphaned_tasks_with` (`plugins/umbral-tasks/src/lib.rs:927`) moves any row stuck in `status='running'` past the visibility timeout back to pending, and the worker loop calls it every iteration before claiming (`plugins/umbral-tasks/src/lib.rs:880-883`). The chaos test enqueues a task, starts a worker, `SIGKILL`s it after the row goes RUNNING but before it completes, starts a fresh worker, and asserts the task is reclaimed and eventually completes exactly once. This is the highest-value scenario because the recovery code exists and just needs an adversarial test proving it under a real kill.
- Redis disconnect. With toxiproxy between the app and Redis, cut the proxy mid-operation and assert the affected subsystem (cache, and any Redis-backed limiter/broker once those land) surfaces a clean error or falls back per its documented contract, then recovers when the proxy is restored, without deadlocking or busy-looping.
- S3 latency. Add a toxiproxy latency toxic (for example 2 to 5 s) in front of MinIO and assert storage operations respect their timeouts, that presigned-URL flows are unaffected (they do not proxy through the app), and that a slow upload does not wedge a request-handling thread.
- Database failover / latency / partition. Add latency and connection-cut toxics in front of Postgres; assert the pool surfaces errors rather than hanging forever, that a transaction interrupted mid-commit does not leave the ORM in an inconsistent in-process state, and that connections recover after the partition heals. (Full multi-node failover is a bigger fixture; start with single-node partition/latency, which already exercises the pool's error and reconnect paths.)
- Broker partition / realtime reconnect storms. Realtime is best-effort with bounded buffers by design (`plugins/umbral-realtime/src/lib.rs:75-84`); the test asserts that a reconnect storm sheds load per that contract (drops rather than unbounded growth) and that the server stays responsive, rather than asserting delivery guarantees the framework explicitly does not make.

Each scenario is a `#[ignore]`d test (self-skips locally, same convention as the Postgres suites) tagged so the nightly job runs them with `--include-ignored` while the normal suite skips them. Where a scenario needs the toxiproxy control API, gate it behind an env var (`UMBRAL_TEST_TOXIPROXY_URL`) so it only runs when the proxy is present.

### Sketch (illustrative)

```yaml
name: chaos
on:
  schedule:
    - cron: "0 3 * * *"   # nightly, off the PR path
  workflow_dispatch:
jobs:
  chaos:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Boot deps + toxiproxy
        run: docker compose -f docker-compose.chaos.yml up -d --wait
      - name: Chaos suite
        env:
          UMBRAL_TEST_POSTGRES_URL: postgres://umbral:umbral@localhost:5433/umbral_test  # via toxiproxy
          UMBRAL_TEST_REDIS_URL: redis://localhost:6380                                   # via toxiproxy
          UMBRAL_TEST_S3_ENDPOINT: http://localhost:9001                                  # via toxiproxy
          UMBRAL_TEST_TOXIPROXY_URL: http://localhost:8474
        run: cargo test --workspace --features chaos -- --include-ignored chaos_
```

`docker-compose.chaos.yml` runs Postgres/Redis/MinIO plus toxiproxy, with toxiproxy exposing the proxied ports the env vars point at. The kill-the-worker scenario is a Rust test that spawns the worker as a child process and `SIGKILL`s it; it does not need toxiproxy, only the live Postgres.

### Acceptance criteria for #94

- A `chaos` workflow exists, runs nightly + on demand, and is not a PR gate.
- The worker-crash-and-reclaim scenario passes against a real killed process and live Postgres, proving `reclaim_orphaned_tasks` under an actual crash.
- At least the Redis-disconnect and S3-latency scenarios run through toxiproxy and assert clean degrade + recovery.
- Every chaos test self-skips locally when its service/env var is absent, so `cargo test` stays green with no infra.

## Sequencing

1. Land the SQLITE_BUSY helper fix first (real code, locally verifiable, unblocks a stable baseline).
2. Land the `live-ci` workflow (#93) and make each live suite env-gated; get `default` + `all-features` green with services.
3. Land the `chaos` workflow (#94) starting with the worker-crash scenario (recovery code already exists), then add the toxiproxy scenarios incrementally.

## What this does not do

This does not add MySQL/other backends to the matrix (Postgres-first is the stance), does not stand up multi-node Postgres failover (single-node partition/latency first), and does not attempt load/soak testing (that is gaps5 #97, a separate item with its own k6/vegeta scenarios). It also does not claim any of these pass today: they are designs with acceptance criteria, gated on CI infrastructure that has to be provisioned to run.
