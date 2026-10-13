# check=skip=SecretsUsedInArgOrEnv
# ^ Docker's build-time lint flags any ENV whose *name* mentions a secret, and
#   it is right to: a key baked into an image leaks to everyone who pulls it.
#   The one below is a false positive on a fixture — it is committed to the
#   repository in this very file, is documented as not-a-secret, and the
#   container is a loopback-only benchmark silo. See the ENV block for why it
#   has to be fixed rather than random.

# Runtime image for the measured application.
#
# The binary is built on the HOST (cargo build --release) and copied in. A
# conventional multi-stage Dockerfile would rebuild the entire umbral workspace
# inside the container: slow, and tens of GB of build artefacts.
#
# Base is pinned to ubuntu:22.04 to match the build host's glibc 2.35. glibc is
# backward-compatible but not forward, so a newer runtime is fine and an older
# one is not.
FROM ubuntu:22.04

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates procps \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY target/release/umbral-bench-prod /app/umbral-bench-prod
COPY templates /app/templates
COPY migrations /app/migrations

# Data lives on a bind mount so the SQLite file is on the host's NVMe and not
# on overlayfs (which copy-up's on every write).
VOLUME ["/data"]

# `migrations/` MUST be copied, and its absence is not a harmless omission.
# The migration engine reads a missing migrations directory as "every applied
# migration is missing" and fails boot with
# `DriftDetected { missing: [("bench","0001_auto"), ...] }` — a startup failure
# that looks like a schema problem and is actually a missing COPY. Task 4 hit
# exactly this drift from a different cause (no plugin registered to own the
# bench migrations) and it cost a diagnosis cycle there.

# `?mode=rwc` (read-write-create) is REQUIRED, not decoration: sqlx opens a
# file-backed SQLite URL read-only unless it is told to create, so without it
# the first boot — the one against an empty /data — dies in `connect()` with
# `SqliteError { code: 14, message: "unable to open database file" }`.
# `bench/data/` is empty by design, so this container is always the cold start
# that creates the file. Same suffix the harness's own umbral.toml carries.
#
# `UMBRAL_SECRET_KEY` is REQUIRED too, for a reason that is not obvious from
# this file. `Environment`'s default is build-dependent (crates/umbral-core/
# src/settings.rs, audit_2 H14): a DEBUG build defaults to `Dev`, a RELEASE
# build defaults to `Prod` ("secure by default" — a deploy that forgets
# `UMBRAL_ENVIRONMENT` is locked down). This image runs a release binary with
# no umbral.toml, so it is `Prod`, and in `Prod` the framework refuses to boot
# while `secret_key` is the built-in dev default:
#
#   SystemCheckFailed { findings: [ check_id: "settings.required", severity:
#   Error, "Settings.secret_key is still set to the insecure dev default in
#   Environment::Prod" ] }
#
# The value below is a FIXTURE key for a loopback-only benchmark container, not
# a deployment secret — and it is fixed on purpose: sessions and CSRF tokens
# signed under one key must stay valid when the container is recreated, so the
# rig cannot hand out a fresh random key per boot.
ENV UMBRAL_DATABASE_URL="sqlite:///data/bench-run.db?mode=rwc" \
    UMBRAL_BIND="0.0.0.0:8114" \
    UMBRAL_SECRET_KEY="bench-silo-fixture-key-not-for-deployment-0001" \
    RUST_LOG="warn"

EXPOSE 8114
ENTRYPOINT ["/app/umbral-bench-prod"]
CMD ["serve"]
