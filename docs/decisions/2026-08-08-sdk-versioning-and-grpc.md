# SDK program, API lifecycle governance, and the gRPC decision

Status: draft for ratification (proposes answers to planning/gaps5.md #37, #41, #40; the final calls are the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #37 (tf #250), #41 (tf #254), #40 (tf #253). This records the SDK/API lifecycle/gRPC decisions; packages and tooling still need implementation.

## Why these three sit together

All three are about the *edge* of umbral: the surface a client, a partner team, or another service touches, versus the framework internals covered by the product north star (`docs/decisions/2026-08-08-product-north-star.md`) and the stability policy (`STABILITY.md`). The north star fixes Stage 1 (a declarative, plugin-first Rust framework) as the supported product and Stage 2 (a self-hosted platform posture) as the near-term differentiator. A generated SDK, an API lifecycle policy, and a protocol decision are all Stage 1-and-2 concerns: they harden the surface an app *serves*, without dragging umbral toward Stage 3 (managed cloud). None of the three requires a control plane, and each stays inside the plugin contract: the REST, OpenAPI, and GraphQL plugins already own this surface, so the work is composition, not a new product.

Everything below is anchored to what the code actually does today, so the roadmap is additive rather than aspirational.

## What already exists (the honest baseline)

**Client generation (`plugins/umbral-openapi/src/client_gen.rs`, CLI `umbral gen-client`).** This is further along than the gaps5 #37 write-up implies. `umbral gen-client --out DIR [--lang ts] [--check]` emits two files from the live model registry plus umbral-rest's per-resource config:

- `client.js`: a single-file, dependency-free ES module exporting `Umbral`, `Query`, and `UmbralError`. It works straight from a `<script type="module">` and through any bundler.
- `client.d.ts`: the full type surface: row interfaces, choice unions, per-model `Filters` / `Ordering` / `Create` / `Update`, and the paginator's `Paginated<T>` envelope.

The runtime already carries several of the pieces #37 lists as missing:

- **A typed query builder.** `new Umbral(url).from("post").filter({ status: "published" }).list()`, where the filter keys autocomplete to exactly this model's filterable (field, lookup) pairs (`__gte`, `__in`, `__contains`, `__isnull`, and so on).
- **Pagination shape awareness.** The generated `Query` exposes one method per param the configured paginator declared (page/page_size, limit/offset, cursor, or a custom paginator's params), and `Paginated<T>` is shaped to that paginator.
- **Auth wiring driven by the declared OpenAPI security schemes.** `UmbralOptions` supports `token` / `tokenPrefix`, `apiKey` / `apiKeyHeader`, an `onToken` change callback, static `headers`, `credentials` (cookie mode), a custom `fetch`, and a `getAuthHeaders()` hook explicitly documented for "a rotating JWT, a refresh flow, request signing." There is also a thin auth client (`umbral.auth.login()` / `register()` / `logout()` / `me()`) generated from the `auth_login` / `auth_logout` / `auth_me` operation ids.
- **A thin realtime client that delegates rather than reimplements.** `Umbral.on(...)` loads the realtime plugin's already-served `{realtimePath}/client.js` and calls `umbral.realtime.model(...)`, inheriting the one-shared-SSE-connection-per-origin behaviour instead of opening an `EventSource` per subscription.
- **A drift gate.** `umbral gen-client --check` writes nothing and exits non-zero when the committed client has drifted from the models. This is the CI primitive an SDK program builds on.

What is genuinely absent today, confirmed against the source:

- **No retry / backoff / 429 handling** in the `client.js` runtime. Every call is a single `fetch`.
- **No pagination auto-iterator.** There is per-page `list()`, but no `for await` / async-iterator that walks pages for you.
- **No storage client helper.** Realtime and auth are delegated / generated; storage is not.
- **No built-in token refresh.** Refresh is *possible* through the `getAuthHeaders` hook, but the client does not implement a refresh loop.
- **No packaging story at all.** No npm package, no version, no semver mapping to the app's API version, no CI publish. `gen-client` writes two loose files into a directory.
- **Only TypeScript.** `--lang` rejects everything except `ts`.

**REST versioning (`plugins/umbral-rest/src/versioning.rs`).** Opt-in and already solid at the routing layer. Two schemes ship: `VersioningScheme::UrlPath` (`/api/v1/<table>/`, one mount per allowed version, out-of-list version resolves to no route so 404) and `VersioningScheme::AcceptHeader` (version read from `Accept: application/json; version=v2` or a plain header such as `X-API-Version`, absent falls back to `default_version`, out-of-list is 406). `VersioningConfig` holds the scheme, a `default_version`, and an `allowed_versions` allow-list, and the resolved version is exposed on `RequestContext` so handlers, `transform`, and `computed` callbacks can branch on it.

What versioning does *not* have, and what #41 is about: no deprecation or Sunset response headers, no per-version changelog, no multi-version documentation rendering, and no gate that stops an SDK built against a removed version from shipping.

**GraphQL (`plugins/umbral-graphql`).** A real graph-traversal GraphQL API derived from the model registry (not an OpenAPI-to-GraphQL transliteration), deny-by-default. It exists and is a first-class plugin.

**gRPC.** Does not exist anywhere in the tree. The only reference is prior-art context in `arch.md` (section 9a), which notes that the nearest neighbour, Reinhardt, ships GraphQL *and* gRPC.

## #37: an SDK program built on the generator

The decision: **grow `gen-client` into a packaged, versioned SDK program, JS/TS first, reusing the generator that already exists rather than starting a parallel one.** The generator's fidelity (it reads the exact live surface the app serves) is the moat; what is missing is everything *around* the two emitted files that turns "generated code" into "an SDK a team pins in `package.json` and trusts."

### Stage A: close the runtime gaps (JS/TS, near-term)

These are additive changes to `client.js` / `client.d.ts` and need no new architecture:

1. **Retries with backoff.** Add opt-in retry config to `UmbralOptions` (max attempts, base delay, jitter), applied in `_request` for idempotent methods and for 429 / 503 with `Retry-After` respected. Default conservative (retry GET / HEAD only) so it cannot silently duplicate a POST.
2. **Pagination helpers.** Add an async iterator to `Query` (`for await (const row of q.rows())`) and a `q.all()` convenience that walks pages using the paginator the client already understands, so page-walking logic is not re-hand-rolled per app.
3. **A first-class refresh flow.** Promote the `getAuthHeaders` refresh pattern into a declared `refresh` option (an async callback returning a new token, invoked on 401 with a single-flight guard so concurrent requests share one refresh). Keep `getAuthHeaders` as the escape hatch for signing schemes it does not cover.
4. **A thin storage client.** Mirror the realtime delegation model: a small `umbral.storage` surface (presigned upload helpers, download URL resolution) that calls the storage plugin's endpoints rather than reimplementing S3. This depends on the storage presign/multipart work (gaps5 #59) landing first; until then it is a documented gap, not a stub.

Auth and realtime clients already exist, so "realtime/storage/auth clients" from the #37 evidence is two-thirds done; this stage finishes it.

### Stage B: packaging, semver, and CI publishing (JS/TS)

Turn the loose files into a real package:

1. **A package skeleton.** `gen-client` gains a `--package` mode that also emits `package.json`, an entry that re-exports `client.js` / `client.d.ts`, and a `README` fragment. Scoped name defaults to the app name (overridable).
2. **Semver tied to the API, not to umbral.** The SDK package version tracks the *app's* API version, not the umbral framework version. The mapping rule: a new API version (a new entry in `allowed_versions`) is a new SDK major; an additive, backward-compatible change to the current API version is an SDK minor; a fix is a patch. This is the seam that connects #37 to #41: the compatibility gate in #41 decides which SDK majors stay buildable.
3. **CI publishing.** A documented GitHub Actions (and generic CI) recipe: run `gen-client --check` as a gate on every PR (fail if the committed client drifted), and on a tagged release run `gen-client --package` and `npm publish`. The `--check` gate already exists; the publish step is new glue, not new framework code.

### Stage C: other languages (roadmap, demand-gated)

The generator's model-registry reader is language-neutral; only the emitter is TS-specific. Once the JS/TS package is real and in use, add emitters behind the same `--lang` flag, in this order, each gated on actual demand:

- **Python** (the highest-value second target: data / scripting / server-to-server consumers), emitting a typed client with `httpx` and Pydantic models.
- **Rust** (a native client for service-to-service calls, reusing the same DTO shapes umbral-rest already derives).
- **Kotlin / Swift** only if a mobile BaaS-style consumer materializes; explicitly deferred until then.

The commitment is JS/TS as the supported SDK; the rest are roadmap entries, each a separate emitter behind the existing flag, not a promise made before there is a consumer.

## #41: API lifecycle governance

REST versioning gives us the *mechanism* (multiple versions served at once, an allow-list, a resolved version on the request). #41 adds the *policy and the signals* around it. The design keeps it opt-in and plugin-owned, consistent with versioning itself.

### Response signals: Deprecation and Sunset headers

Extend `VersioningConfig` with per-version lifecycle metadata: a version can be marked `deprecated` (optionally with a date) and given a `sunset` date and an informational link. When a request resolves to a deprecated version, umbral-rest emits the standard headers:

- `Deprecation: true` (or an `@`-prefixed timestamp per the IETF draft) on every response served from a deprecated version.
- `Sunset: <HTTP-date>` when a retirement date is set, so clients and monitoring can see the deadline machine-readably.
- `Link: <changelog-url>; rel="deprecation"` pointing at the migration notes.

These are pure response decoration on the version already resolved on `RequestContext`; no routing change. A retired version simply leaves `allowed_versions` and returns to the existing 404 / 406 behaviour, so "sunset" has a concrete meaning already implemented at the routing layer.

### Per-version changelog and multi-version docs

- **Changelog.** A per-version changelog file (one section per API version) that the OpenAPI plugin can surface, so the `Link` header above resolves to a real page. This is documentation plumbing, not framework runtime.
- **Multi-version docs.** The OpenAPI document is already generated from the live surface. Extend the doc build to render one document per allowed version (the version is already on `RequestContext` and drives per-version `transform` / `computed`), so a partner can read exactly the v1 shape while v2 is live.

### SDK compatibility gates (the tie to #37 and STABILITY.md)

This is where #41 and #37 meet. Two gates:

1. **Generation gate.** `gen-client --check` already fails CI when the committed client drifted from the models. Extend it to also fail when the SDK targets an API version that is no longer in `allowed_versions` (a client pinned to a retired version), turning a silent runtime 404 into a build-time error.
2. **Deprecation-window gate.** Tie the *timing* to `STABILITY.md`. That document already defines the deprecation window for the framework's Stable tier (at least one minor pre-1.0, two post-1.0) and `#[deprecated]` marking. Mirror the same window for an *API version*: a version marked deprecated stays in `allowed_versions` for at least one minor release before it may be removed, and its removal ships with a changelog migration note, exactly as the framework's Stable-tier removals do. This makes "the API surface" a governed tier alongside the code surface, rather than a separate ad-hoc policy.

The net: STABILITY.md governs the *crate* surface; this policy governs the *served-API* surface, with the same window, the same changelog discipline, and machine-readable headers so consumers are warned in-band.

## #40: the gRPC decision

Recommendation: **gRPC / protobuf is not near-term scope. Do not build it now.**

Rationale, grounded in what umbral already serves:

1. **The wire surface is already covered by three protocols.** REST (typed, versioned, paginated, throttled) is the default; GraphQL (`plugins/umbral-graphql`) covers graph traversal and client-chosen query shapes; OpenAPI plus `gen-client` covers typed client generation. gRPC's headline wins (a strict IDL contract, streaming, cross-language stubs) are substantially met: the IDL-equivalent contract is the OpenAPI document plus the model registry, streaming is served by the realtime plugin's SSE, and cross-language stubs are exactly what the #37 SDK program delivers. Adding gRPC would be a fourth way to say the same thing, and the GraphQL plugin's own docstring makes precisely this argument against transliterating one protocol into another.
2. **It fights the ORM-is-the-only-database-interface and plugin contracts less cleanly than it looks.** A gRPC surface would need its own protobuf schema generation from the model registry (a third schema emitter after OpenAPI and GraphQL), its own auth integration (protobuf has no cookie / CSRF story, so the secure-by-default posture would need re-deriving for a new transport), and its own versioning lifecycle re-expressed in protobuf's additive-only field rules. That is a large, mostly-parallel surface for a benefit the north star does not prioritize: Stage 1 is a *web* framework and Stage 2 is a *self-hosted platform*, and neither names binary-RPC interop as a differentiator.
3. **The prior-art pressure is weak.** The only in-repo reason to consider gRPC is that Reinhardt ships it (`arch.md` section 9a). But `arch.md` frames Reinhardt as the nearest neighbour to study, not a feature-parity checklist, and the section's own thesis is that the *distinguishing* property is the runtime `Plugin` contract, not the count of wire protocols. Matching a peer's protocol list is not a strategic reason.

### What adoption would take if demand appears

So the door is not nailed shut, record the seam. gRPC becomes worth reconsidering if a concrete consumer needs binary framing, bidirectional streaming beyond SSE, or a strict cross-language contract that OpenAPI cannot satisfy (for example a polyglot internal service mesh adopting umbral for one service). If that demand is real, the additive path, built as a plugin like everything else, would be:

1. **`umbral-grpc` plugin** that reads the same `ModelMeta` registry the OpenAPI and GraphQL plugins read, and emits a `.proto` service and message set (one message per exposed model, one service per resource), reusing the deny-by-default exposure model GraphQL already established.
2. **A tonic-based server surface** mounted through the existing `Plugin::wrap_router` / route hooks, so gRPC coexists with REST on the same app rather than forking it, standing on tonic rather than reimplementing HTTP/2 framing (per the "don't reimplement primitives" principle).
3. **Auth integration** that maps the existing auth plugin's token / session identity onto gRPC metadata, and re-derives the secure-by-default posture (there is no CSRF concern for non-browser gRPC, but token validation, throttling, and object-scoping must be re-applied, not assumed).
4. **Versioning** expressed in protobuf's own additive field-number discipline, gated by the same lifecycle policy as #41.

Until a consumer with that shape exists, this stays a deferred backlog note, not committed work.

## Summary of the three decisions

- **#37:** Grow the existing `gen-client` into a packaged, versioned, CI-published SDK, JS/TS first. Near-term: add retries, pagination iterators, a first-class refresh flow, and a storage client (auth and realtime clients already exist), then package.json / semver / npm publish. Python and Rust emitters are demand-gated roadmap behind the same `--lang` flag.
- **#41:** Add an API lifecycle policy on top of the existing REST versioning: `Deprecation` / `Sunset` / `Link` response headers on deprecated versions, per-version changelogs and multi-version OpenAPI docs, and two SDK compatibility gates (drift and retired-version) whose timing mirrors the STABILITY.md deprecation window, so the served-API surface is a governed tier alongside the crate surface.
- **#40:** Do not build gRPC near-term. REST plus GraphQL plus OpenAPI/SDK already cover the wire surface, and gRPC would add a third schema emitter and a parallel auth / versioning surface for a benefit the north star does not prioritize. Record the `umbral-grpc` plugin seam (registry-driven `.proto` generation on tonic, auth-metadata mapping) so it stays additive if a concrete polyglot-mesh consumer ever appears.
