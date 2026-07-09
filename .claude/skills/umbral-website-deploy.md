---
name: umbral-website-deploy
description: Use when touching umbral_website's Dockerfile, docker-compose.yml, .prod.env, or .github/workflows/deploy-website.yml — captures the crates.io-dep constraint and three traps that build clean but fail at runtime.
---

# Deploying umbral_website

## Context

`umbral_website/` deploys to **umbralrs.dev** as a single server-rendered Rust service (postgres + one-shot migrate + web on `:9100`, Caddy proxies the domain). The deploy workflow ships **only `umbral_website/`'s files** — nothing from `crates/`, `plugins/`, `documentation/`, or `examples/`.

That constraint drives everything below.

## Approach

### The framework comes from crates.io, not path deps

`umbral_website/Cargo.toml` pins `umbral* = "0.0.6"` from the registry. A path dep (`../crates/umbral`) cannot work: a Docker build context cannot reach outside its own directory, so `COPY ../crates` is illegal.

Consequence: **the site can only use released framework APIs.** Land a framework change → cut a release → bump the versions here. To check whether the registry version has drifted from `main`, diff the *trees*, not the log (release-plz tags live off-branch, so `git log tag..HEAD` is misleadingly empty):

```bash
git diff --stat umbral-v0.0.6 HEAD -- 'crates/**/*.rs' 'plugins/**/*.rs'   # empty == no API drift
```

The website's own sub-plugins (`plugins/<app>/`) stay path deps, but must use **sibling-relative** paths (`../site_content`), never `../../../umbral_website/plugins/...` — that escapes the build context.

`Cargo.lock` is committed and the Dockerfile builds `--locked`.

### Three things that build clean and fail at runtime

1. **The binary must be compiled at `/app`, in `rust:1-bookworm`.** Every website app resolves templates with `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("templates")`. `env!` freezes the *builder's absolute path* into `.rodata` at compile time, and it survives `strip`. Build anywhere else — a `/build` stage, a GitHub runner's `/__w/...` checkout, your own `cargo build --release` — and the image builds perfectly while every plugin-rendered page 500s on the first request. `--remap-path-prefix` does NOT fix it (it rewrites debug info and `file!()`, not a cargo env var). Both the CI job and the Dockerfile grep the binary for `/app/plugins/public` and fail the build if it is absent; expect exactly 10 `/app/plugins/*` dirs. Same-distro matters too: `rust:1-bookworm` → `debian:bookworm-slim` is Debian 12 both sides (glibc 2.36, OpenSSL 3).

   **The server compiles nothing.** CI builds the binary; the Dockerfile is a single `debian:bookworm-slim` stage that copies it in. `docker compose build` on the box is ~10s of `COPY` layers, versus ~28 min cold for the 468 registry crates. Nothing is published to any registry — only the binary is scp'd — and `.prod.env` never leaves the server, so CI never handles a production secret. Local equivalent: `bash scripts/build_binary.sh`.

2. **The healthcheck needs the real `Host` header.** In `prod`, `App::build()` mounts the host guard (`crates/umbral-core/src/hosts.rs`) which 400s any request whose Host is absent from `UMBRAL_ALLOWED_HOSTS`. A plain `curl localhost:9100` sends `Host: localhost:9100`, gets a 400, and marks the container permanently unhealthy while it serves real traffic fine. Use `curl -H "Host: umbralrs.dev" http://127.0.0.1:9100/`.

3. **`umbral.toml` must not be baked into the image.** Settings merge `Toml::file("umbral.toml")` then `Env::prefixed("UMBRAL_")`, so env wins — but the file declares `environment = "Dev"`, which overrides a *release* binary's secure `Prod` default. Baking it in means one forgotten env var silently disables Host validation and re-permits the dev secret key. It is in `.dockerignore`. (The file was named `umbra.toml` for a long time — a rename leftover the framework never loaded, so dev ran entirely off `.env`.)

### Env and secrets

`.prod.env` is gitignored; only its sops+age encrypted form `secret.env` is committed. `bash scripts/encrypt_envs.sh <age-public-key>` produces it; CI decrypts with `AGE_PRIVATE_KEY` straight into the staged payload.

- sops's dotenv parser **rejects blank lines** — `.prod.env` uses `#` separators.
- `UMBRAL_BIND_ADDR` must be `0.0.0.0:9100`; a container binding loopback is unreachable even when the port is published. The workflow greps for this and fails the deploy otherwise.
- **Compose must publish to `127.0.0.1:9100:9100`, not `9100:9100`.** These two settings look contradictory but are not: the container binds `0.0.0.0` *inside* its netns, and Docker publishes that port on the host's loopback only. Publishing on `0.0.0.0` served the whole site over plaintext HTTP straight from the public internet, bypassing Caddy and TLS — the host guard 400s a raw-IP `Host`, but `curl -H 'Host: umbralrs.dev' http://<ip>:9100/` returned 200 with the full page. Caddy runs on the host, so it reaches loopback fine.
- `UMBRAL_DATABASE_URL`'s host must be `postgres` (the compose service name), and its user/password must match the `POSTGRES_*` triple in the same file.
- **Never rotate `UMBRAL_MASK_PUBLIC_KEY` / `UMBRAL_MASK_PRIVATE_KEY`** — a new keypair makes every existing `Masked<T>` column permanently unreadable.

### Caddy, SSE and WebSockets (`info/caddy.json`)

The site serves HTML, SSE (`/realtime/sse`) and WebSockets (`/realtime/ws`) on **one host** — it cannot be split onto an `sse.` subdomain the way feedpool is.

- **WebSockets need no special handler config.** Caddy v2's `reverse_proxy` performs the HTTP upgrade and transitions to a bidirectional tunnel automatically. What it *does* need is HTTP/1.1 to the upstream: `transport.versions` defaults to `["1.1", "2"]`, and the route pins `["1.1"]` so nobody can flip it to `h2c` and silently break every upgrade. The server's `protocols` must keep `"h1"` — browsers open WebSockets over HTTP/1.1.
- **Never rewrite the `Host` header** on this route. `ws.rs` has a CSWSH guard that compares the request's `Origin` against its `Host`; Caddy v2 passes the original Host through by default, so `Origin: https://umbralrs.dev` matches `Host: umbralrs.dev` and the same-origin check passes. Add a `header_up Host` rewrite and every WebSocket upgrade 403s in prod (and `UMBRAL_ALLOWED_HOSTS` rejects the request first anyway).
- `flush_interval: -1` is **redundant for SSE** — Caddy ignores it and flushes immediately whenever the response is `Content-Type: text/event-stream`. It is kept only as a version-independent guarantee; its sole real effect is unbuffered HTML.

### Uploads

`StoragePlugin::media("/media", "./media")` writes to `/app/media`. That is a **named volume** in compose. Without it, every `docker compose build` silently discards everything users uploaded.

## Why

`scp-action` has no `--exclude`, so the workflow stages a clean payload with `rsync --exclude` first. That is not cosmetic: the repo root's `target/` is >100G, and `keys.txt` is the age *private* key.

## Pitfalls

- `docker compose up -d` alone tries to **pull** `umbral-website:latest`; `migrate` reuses the image `web` builds. Always `docker compose build` first.
- `build.rs` shells out to Tailwind only when `styles/node_modules` exists. The image ships no node, so it takes the lenient branch and uses the committed `static/css/umbral.css`. CSS is a build-time artefact of the repo, not the image.
- `main`'s workspace `Cargo.toml` drifted once: it read `0.0.5` while `0.0.6` was published and tagged, because release-plz's release commit lands on the tag, not on `main`. If the website's crates.io pins ever fail to resolve, check that drift first.

## See also

- `umbral_website/README.md` — the operator-facing version of all this.
- `crates/umbral-core/src/hosts.rs`, `crates/umbral-core/src/settings.rs:635` (merge order).
