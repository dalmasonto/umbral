# Umbral Website

The official Umbral website (**umbralrs.dev**), built with Umbral itself. It is a single server-rendered Rust service: Umbral renders the HTML, serves `/static` and `/media`, and hosts the admin, the REST API, and the OpenAPI playground. There is no separate frontend, no SPA, and no Node runtime in production.

## Backend apps

The project was created with `umbral startproject --local` and each area with `umbral startapp --local`:

| App | Purpose |
|---|---|
| `site_content` | Website-specific content extension point while the reusable content plugin is promoted |
| `features` | Framework feature catalog and release status data |
| `prebuilt_plugins` | Official Umbral plugin records and plugin-owned features |
| `plugin_directory` | Community plugin listings, submissions, comments, and compatibility data |
| `reviews` | Verified developer reviews and moderation workflow |
| `showcase` | Websites and applications using Umbral |
| `security_reports` | Plugin safety reports, advisories, warnings, and auditor workflow |
| `accounts` | Deferred GitHub identity and trust-gate models |
| `community` | Social links, community resources, and Sentinmail newsletter settings |
| `sponsor` | Sponsorship tiers and sponsor records |
| `public` | The landing page, which composes data from the apps above |

Models belong in each app's `plugins/<app>/src/models.rs`. `src/main.rs` only wires the framework plugins, the website apps, templates, routes, and CLI dispatch.

## Why crates.io deps

`Cargo.toml` pins the framework from **crates.io** (`umbral = "0.0.6"`, and likewise for every `umbral-*` plugin) rather than path-depping `../crates` and `../plugins`.

That is what makes this directory self-contained. The deploy workflow ships `umbral_website/` and nothing else, and the server's Docker build resolves the framework from the registry. A path dependency could not work: a Docker build context cannot reach outside its own directory, so `COPY ../crates` is illegal, and shipping the whole workspace to build one binary is the thing we are trying to avoid.

The trade-off is real and worth stating plainly: **the website can only use framework APIs that have been released.** If you land a framework change the site needs, cut a release first, then bump the versions here. Developing the site against un-released framework code means temporarily switching these back to path deps — just don't commit that.

`Cargo.lock` is committed. This is a deployable binary, not a library, so the lockfile is what guarantees CI and the server resolve identical dependency versions. The Dockerfile builds with `--locked` and will fail without it.

## Local development

SQLite, no Docker:

```bash
cp .env.example .env            # fill in values (see Environment below)
cargo run                       # auto-migrate + seed + serve on 127.0.0.1:8000
```

Or step through it explicitly:

```bash
cargo run -- migrate
cargo run -- createsuperuser
cargo run -- serve
```

The Tailwind bundle is a build-time artefact of the repo, not of the Docker image. `build.rs` rebuilds `static/css/umbral.css` only when the local toolchain is present; when `styles/node_modules` is missing it prints a warning and reuses the committed CSS. To work on styles:

```bash
cd styles && npm install && npm run build
```

## Backend CLI

`cargo run -- <command>` in dev, or `docker compose run --rm web <command>` against the image.

| Command | What it does |
|---|---|
| `serve` | Start the HTTP server (binds `UMBRAL_BIND_ADDR`). |
| `migrate` | Apply pending migrations. |
| `makemigrations` | Generate migrations from model changes. |
| `createsuperuser` | Create an admin (no default admin is seeded). |
| `showmigrations` | List applied vs pending migrations. |
| `seed_orm_data` | Run every website app's idempotent seed. |

> A bare `cargo run` with no subcommand auto-migrates, seeds, and serves — handy for a throwaway dev database. An explicit subcommand (`serve`, `migrate`, …) does **not** auto-migrate.

## Docker / production

Three services: **postgres** (internal only), a one-shot **migrate**, and the **web** app on `:9100`. The `web` service builds `umbral-website:latest`; `migrate` reuses that same image rather than building its own, which is why `docker compose build` has to run before `up -d`.

```bash
docker compose build                          # build umbral-website:latest
docker compose up -d                          # postgres -> migrate -> web
docker compose run --rm web createsuperuser   # create the first admin
docker compose logs -f web                    # tail the site
docker compose down                           # stop everything
```

| Service | Command | Role |
|---|---|---|
| `postgres` | – | PostgreSQL 16, data in the `pgdata` volume. **Not published** — reachable only inside the compose network. |
| `migrate` | `migrate` | One-shot: applies migrations, then exits. `web` waits for it to succeed. |
| `web` | `serve` | The site on `:9100`. Env is baked from `.prod.env` at build time. |

Caddy terminates TLS and proxies the domain to the published port:

```
umbralrs.dev, www.umbralrs.dev {
    reverse_proxy localhost:9100
}
```

Two things about this stack are load-bearing and easy to break:

- **Uploads live in the `media` named volume**, never in an image layer. Plugin logos and cover images are written to `/app/media` by `StoragePlugin`. Without the volume, every `docker compose build` would silently discard everything users uploaded.
- **The Dockerfile builds and runs in the same `WORKDIR` (`/app`).** Every website app resolves its templates with `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")`, and `env!` bakes the builder's absolute path into the binary at compile time. Build in one directory and run in another and the image builds perfectly, then every plugin-rendered page returns 500 on the first request.

## Secrets (sops + age)

`.prod.env` holds the production environment and is gitignored. It is encrypted with [sops](https://github.com/getsops/sops) + [age](https://github.com/FiloSottile/age) into `secret.env`, which **is** committed, and decrypted by CI on deploy.

```bash
age-keygen -o keys.txt                              # one-time: generate a key pair
bash scripts/encrypt_envs.sh <your-age-public-key>  # .prod.env -> secret.env
```

Decrypt locally with `sops --decrypt secret.env > .prod.env`. Put the private key from `keys.txt` into the `AGE_PRIVATE_KEY` repo secret. `keys.txt` and `.prod.env` are both gitignored and both excluded from the deploy payload and the Docker image.

sops's dotenv parser rejects blank lines, so `.prod.env` uses `#` comment lines as separators. `encrypt_envs.sh` strips blank lines if any reappear.

## Deploy

`.github/workflows/deploy-website.yml` is manual (`workflow_dispatch`). It decrypts `secret.env`, stages a clean copy of `umbral_website/` — excluding `target/`, `styles/node_modules/`, `media/`, the local `.env`, and `keys.txt` — and `scp`s only that directory's files to `/home/umbral_website` on the server. Nothing from `crates/`, `plugins/`, `documentation/`, or `examples/` is transferred.

Bringing the stack up is deliberately manual:

```bash
cd /home/umbral_website && docker compose build && docker compose up -d
```

Required GitHub secrets: `AGE_PRIVATE_KEY`, `CONTABO_HOST`, `CONTABO_USER`, `SSH_PRIVATE_KEY`.

## Environment

Settings resolve from `umbral.toml` first, then get overridden by `UMBRAL_*` environment variables — figment merges the env layer last, so env always wins. `.env` is the local dev environment; `.prod.env` is the production one, baked into the image as `/app/.env`.

`umbral.toml` is **local-development only and is not shipped into the image.** It declares `environment = "Dev"`, which would override the secure `Prod` default a release binary picks up on its own; baking it in would mean one forgotten environment variable silently disables Host-header validation and re-permits the dev secret key. In production, `/app/.env` is the single source of truth.

> The file used to be named `umbra.toml`, left over from the umbra → umbral rename. The framework only ever loads the exact name `umbral.toml`, so for a long time it was read by nothing at all — local dev was in fact driven entirely by `.env`. It is now correctly named, which is why the SQLite `database_url` below finally takes effect when `.env` is absent.

| Variable | Example |
|---|---|
| `UMBRAL_SECRET_KEY` | `$(openssl rand -hex 32)`. The framework rejects the default dev key at boot when the environment is `prod`. |
| `UMBRAL_DATABASE_URL` | `sqlite://umbral_website.db?mode=rwc` locally · `postgres://USER:PASS@postgres:5432/DB` in Docker, where the host **must** be `postgres` |
| `UMBRAL_BIND_ADDR` | `127.0.0.1:8100` locally · `0.0.0.0:9100` in Docker. A container that binds loopback is unreachable even when the port is published. |
| `UMBRAL_ENVIRONMENT` | `prod` in production. Turns on Host-header validation and prod error pages. A release binary defaults to `Prod`. |
| `UMBRAL_ALLOWED_HOSTS` | `umbralrs.dev,www.umbralrs.dev`. The default is `localhost,127.0.0.1`, so behind Caddy every request would be rejected on the Host header. |
| `UMBRAL_OAUTH_REDIRECT_BASE` | `https://umbralrs.dev`. Callbacks are built as `{base}/oauth/{provider}/callback`; it defaults to `http://localhost:8100`. |
| `UMBRAL_OAUTH_{GOOGLE,GITHUB}_CLIENT_{ID,SECRET}` | Social login. A provider with no credentials is simply not registered. |
| `UMBRAL_MASK_PUBLIC_KEY` / `UMBRAL_MASK_PRIVATE_KEY` | `Masked<T>` field encryption. **Never rotate these** — a new keypair makes every already-encrypted column permanently unreadable. |
| `POSTGRES_USER` / `POSTGRES_PASSWORD` / `POSTGRES_DB` | Postgres init credentials. They must match the user, password, and database in `UMBRAL_DATABASE_URL`. |

Register these exact OAuth callback URLs in the provider consoles:

```
https://umbralrs.dev/oauth/google/callback
https://umbralrs.dev/oauth/github/callback
```

## Layout

```
src/                 main.rs (plugin wiring, routes), widgets/, seed_command.rs
plugins/<app>/       one crate per website app: models.rs, lib.rs, templates/
templates/           site-level templates (base.html, 404.html, 500.html, …)
static/              css/, img/, js/ — served at /static, includes the built umbral.css
styles/              Tailwind source; `npm run build` emits static/css/umbral.css
media/               runtime uploads (gitignored; a named volume in production)
migrations/<app>/    per-app migrations, applied by `migrate`
scripts/             encrypt_envs.sh (sops), setup_db.sh (optional host Postgres)
umbral.toml          dev-only settings, overridden by UMBRAL_* env vars; not in the image
Dockerfile           multi-stage build; builds and runs in /app
docker-compose.yml   postgres + migrate + web
```
