# starknet-explorer — networks are tenants

A standalone umbra example: **schema-per-tenant multitenancy where blockchain networks are the tenants.** One Starknet explorer app serves multiple networks (Sepolia testnet + Mainnet), with per-network data isolated per Postgres schema and cross-network data shared in `public`.

## The mental model

The data splits in two, **by plugin name**:

| Plugin | Role | Tables | Lives in |
|---|---|---|---|
| `explorer` | **TENANT app** (NOT in `shared_apps`) | `transaction`, `address`, `token` | one schema per network |
| `access` | **SHARED app** | `api_key` | `public` |
| `content` | **SHARED app** | `blog_post` | `public` |

So Sepolia's transactions are invisible when you're serving Mainnet (and vice-versa), but the blog and API keys read identically under every network. Whatever you pass to `.shared_apps([...])` goes to `public`; everything else is tenant-owned and gets a copy of its tables in each tenant schema.

A "tenant" here is a **network**: `sepolia` and `mainnet`, resolved by the `X-Network` header (or an `*.localhost` subdomain).

## Run it (Postgres-only)

```bash
cd examples/starknet-explorer
export UMBRA_DATABASE_URL=postgres://user:pass@localhost/starknet_explorer

cargo run -- makemigrations   # generate migration files for all 4 apps
cargo run -- migrate          # run_shared: SHARED apps (tenants/access/content) -> public ONLY
cargo run                     # boot: create_tenant sepolia + mainnet, then serve
```

On boot the server calls `create_tenant` for each network (insert registry row + `CREATE SCHEMA` + migrate the `explorer` tenant tables into that schema), idempotently.

```bash
# per-network, isolated:
curl -H 'X-Network: sepolia.localhost' localhost:3000/txs/seed
curl -H 'X-Network: mainnet.localhost' localhost:3000/txs/seed
curl -H 'X-Network: sepolia.localhost' localhost:3000/txs   # only Sepolia's tx
curl -H 'X-Network: mainnet.localhost' localhost:3000/txs   # only Mainnet's tx

# shared, no network needed:
curl localhost:3000/blog
curl localhost:3000/apikeys
```

## Why `run_shared`, not `migrate`/`run`

Plain `umbra::migrate::run()` migrates **every** app into `public`, including the `explorer` tenant tables — exactly what we don't want. `umbra::migrate::run_shared(&shared_set)` migrates **only** the shared apps (`tenants` / `access` / `content`) into `public`, so `transaction` / `address` / `token` never land in `public`; they exist only inside each network's schema, created per-network by `create_tenant`.

## Adding a third network later

No code change. Either call `plugin.create_tenant("Starknet Goerli", "goerli", "goerli.localhost")` on boot, or run `cargo run -- migrate_schemas` to (re)create + migrate every active network's schema, idempotently.

See `src/main.rs` for the fully-commented walkthrough, and `plugins/umbra-tenants/` + `arch.md` for the design.
