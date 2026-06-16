# read-replica

A standalone umbra example showing a custom **`DatabaseRouter`** that sends every **read** to a replica pool and every **write** to the primary — the foundation behind read/write replica split (gaps2 #69, folds in #23).

The point: the model and handlers are written exactly like a single-database app. Routing is one trait impl wired in at `App::builder().router(...)`; the ORM consults it on every terminal.

## Run it

```bash
cd examples/read-replica
cargo run
```

Then, in another shell:

```bash
curl localhost:3000/notes/add   # WRITE -> primary
curl localhost:3000/notes/add
curl localhost:3000/notes        # READ  -> replica
```

Watch the server log — each request prints the routing decision:

```
router: WRITE -> default (primary)  table=note
router: READ  -> replica            table=note
```

## How it works

```rust
struct ReplicaRouter;
impl DatabaseRouter for ReplicaRouter {
    fn db_for_read(&self,  _m: &ModelMeta, _c: &RouteContext) -> Alias { Alias::new("replica") }
    fn db_for_write(&self, _m: &ModelMeta, _c: &RouteContext) -> Alias { Alias::new("default") }
}

App::builder()
    .database("default", primary)   // writes land here
    .database("replica", replica)   // reads come from here
    .router(ReplicaRouter)
    .model::<Note>()
    // ...
```

`Note::objects().create(..)` resolves `db_for_write` → `default`; `Note::objects().fetch()` resolves `db_for_read` → `replica`. Need read-your-writes for one query? `Note::objects().on(&primary).fetch()` pins the pool and bypasses the router.

## Demo vs. production

The demo runs against **one** sqlite file (see `.env`), so the "replica" is the same database as the primary and reads see writes immediately — the router is still consulted and logged, you just can't observe replication lag. For a real split:

```bash
UMBRA_DATABASE_URL=postgres://.../primary \
UMBRA_REPLICA_URL=postgres://.../replica \
cargo run
```

The routing code does not change. A handful of caveats live in the design spec's follow-ups (`docs/superpowers/specs/2026-06-16-database-router-foundation-design.md`) — notably that `get_or_create`/`update_or_create` already read-your-writes (probe the primary), so they're safe against replica lag.

## Note on the schema

This example creates the `note` table with a raw `CREATE TABLE` at startup to stay focused on routing. A real app declares the model and runs `makemigrations` + `migrate`; the replica gets the schema through replication.
