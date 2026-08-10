# Support lifecycle, marketplace governance, and the functions story

Status: draft for ratification (proposes answers to gaps5 #89, #90, #91; the final call is the maintainer's)
Date: 2026-08-08
Decision coverage: planning/gaps5.md #89 (tf#302), #90 (tf#303), #91 (tf#304). This records the policy/product decisions; implementation deliverables remain on the linked tasks.

## Why these three sit in one document

All three are platform-posture questions, and they all resolve against the same anchor: the staged product north star (`docs/decisions/2026-08-08-product-north-star.md`). That decision commits umbral to Stage 1 (a declarative, plugin-first framework, the supported product and the 1.0 promise), Stage 2 (an opt-in self-hosted platform posture), and Stage 3 (an optional managed control plane, gated on demand). The recurring pattern across #89, #90, and #91 is: define the contract now on the Stage 1 surface, and record the seam that Stage 3 would attach to, without building the Stage 3 service. Support lifecycle (#89) extends the policy in `STABILITY.md`; marketplace governance (#90) extends the manifest contract in `docs/specs/plugin-manifest-and-registry.md`; the functions story (#91) decides whether a new deployable unit is warranted at all (it is not, yet).

None of the three requires new runtime code to be ratified. They are policy and contract decisions that later implementation work references.

## 89. Support lifecycle: LTS policy, supported-versions matrix, maintenance branches, enterprise tiers

### Context and the gap

`STABILITY.md` today defines API tiers (Stable, Evolving, Internal), lockstep versioning, deprecation windows, MSRV, and the 0.x to 1.0 gate. What it does NOT say is how long any given release is *supported*: whether an old minor keeps receiving security fixes, how many lines are maintained at once, and what a team paying for support actually buys. gaps5 #89 asks for a formal support lifecycle. This is the difference between "the API is stable" (which `STABILITY.md` covers) and "this specific version will get fixes until date X" (which it does not).

### The pre-1.0 reality constrains the answer

umbral is at 0.0.11, lockstep across all crates, breaking changes allowed in a MINOR bump. Promising multi-year LTS windows on a pre-1.0 framework would be dishonest: the surface still moves by design. So the lifecycle policy has two eras, matching how `STABILITY.md` already splits pre-1.0 from post-1.0.

### Policy: pre-1.0 (now)

- **Only the latest release is supported.** During 0.x, fixes (including security fixes) land on `main` and ship in the next release. There is no back-porting to an older 0.0.x. A team on 0.0.9 upgrades to the current release to get a fix. This is honest for a framework whose minors can break intentionally, and it matches the lockstep-version model: there is one supported line, the newest.
- **No LTS designation exists yet.** LTS is a post-1.0 concept; naming one now would over-promise.
- **The supported-versions matrix is therefore a single row:** the current release is Supported; everything prior is End of Life. The matrix format below is defined now so it is ready to grow at 1.0.

### Policy: post-1.0 (the shape we commit to now, effective at 1.0)

- **Supported window.** The current `1.Y` minor and the one immediately prior receive fixes. That gives a team at least one minor's worth of runway to upgrade before its line goes End of Life. This pairs with the post-1.0 deprecation window in `STABILITY.md` (at least two minor releases), so a deprecation announced in `1.Y` is still callable through `1.Y+2`, and the `1.Y` line it was announced on is itself supported through `1.Y+1`.
- **LTS releases.** Designated LTS minors receive security fixes for a longer, published horizon (proposed: 18 months from the LTS release date) even after newer minors ship. LTS cadence is proposed at one LTS line per 12 months. Non-LTS minors follow the current-plus-prior window above.
- **Maintenance branches.** Each supported line (the current minor, the prior minor, and every in-horizon LTS) has a maintenance branch `release/1.Y`. Security and correctness fixes are cherry-picked back from `main` to each in-support branch and released as a PATCH on that line. This is the mechanism that makes "supported" mean something: a branch a fix can actually land on. Pre-1.0 there is only `main`, so no maintenance branch exists yet.
- **What "supported" covers.** Security fixes always; correctness regressions on a best-effort basis; NOT new features (those land on `main` only). MSRV is never raised on a maintenance branch (a PATCH never raises MSRV, per `STABILITY.md`).

### The supported-versions matrix (format)

A table published in `STABILITY.md` and updated on each release. Columns: line, status, first released, security-supported until, notes. Illustrative shape (values are examples, not commitments beyond the policy above):

| Line | Status | Released | Security-supported until | Notes |
|---|---|---|---|---|
| 0.0.x (current) | Supported | rolling | until next release | Pre-1.0: only the latest release is supported |
| earlier 0.0.x | End of Life | -- | -- | Upgrade to current |
| 1.0.x (future) | LTS (proposed) | at 1.0 | +18 months | First LTS candidate |

### Enterprise support tiers (the commercial layer, contract only)

The support *lifecycle* above is the free, public promise attached to releases. An *enterprise support model* is a separate, optional commercial layer that does NOT change the code or the public lifecycle. It is recorded here as a seam, not built:

- **Community (free).** The public lifecycle above. Fixes via the supported lines, issues on the public tracker, best-effort. This is what every user gets.
- **Standard (paid, future).** Private security pre-notification (advance notice of a fix before public disclosure), a response-time target on filed issues, and upgrade guidance. Attaches to the same maintenance branches; no private code fork.
- **Extended / LTS-plus (paid, future).** Security support for a specific line beyond its public End-of-Life horizon, and prioritized fixes. Implemented as privately maintained back-ports on a per-customer basis off the last public maintenance branch.

Enterprise tiers are a business, not a framework feature: they belong to Stage 3 (the managed/commercial posture) in the north star and are out of scope to build now. They are named here so the lifecycle policy leaves room for them (the maintenance-branch model is exactly the substrate a paid extended-support tier would sell against) without committing umbral to run a support organization pre-1.0.

### What this adds to STABILITY.md

A new "Support lifecycle" section: the pre-1.0 "latest only" rule, the post-1.0 current-plus-prior window, the LTS definition and cadence, the maintenance-branch mechanism, the supported-versions matrix (single row today), and a one-paragraph pointer to the optional enterprise tiers as a Stage 3 commercial layer. It extends rather than replaces the existing Security section (security fixes ship as a PATCH against the current minor), by making explicit which lines "current" expands to after 1.0.

## 90. Marketplace governance: signing, verified publishers, security badges, compatibility metadata

### Context and the gap

`docs/specs/plugin-manifest-and-registry.md` (gaps5 #6) already defines the manifest, the boot-time compatibility check, and the static catalog/index format. Its section 5 explicitly reserves signing, verified publishers, and security badges as additive layers, and its section 4 already ships the compatibility metadata (`umbral_req`, drift fields, `security`). gaps5 #90 is the design for those reserved layers. The governing constraint from that spec and from the north star: the marketplace *hosting site*, the *registry service*, the *signing infrastructure* (key custody, trust roots, rotation), and any *moderation/certification workflow* are Stage 3 and OUT of scope. What is IN scope here is the data contract for governance, so the catalog format and tooling agree on the shape before any service exists.

Everything below is an ADDITIVE layer on the existing catalog record. The unit is still a plugin name with an array of published versions (spec section 4.1); governance fields attach to a version entry without a format break, gated by a bumped `catalog_version`.

### Compatibility metadata (already shipped, restated)

Delivered by gaps5 #6: `umbral_req` (one lockstep `VersionReq`), the capability list, `owns_migrations`, and the `security` posture enum are already in both the manifest and the catalog. Compatibility badges and security badges render as pure functions of fields already present. #90 adds nothing here; it consumes what #6 built. This is the "compatibility metadata" quarter of #90, done.

### Security badges

A security badge is a pure function of the catalog's existing `security` field (`Supported` / `EndOfLife` / `Advisory { rustsec }` / `Unknown`) cross-referenced with RUSTSEC:

- `Supported` renders green ("fixes shipped").
- `EndOfLife` renders red ("no longer maintained").
- `Advisory { rustsec }` renders red with the advisory id linked to the RUSTSEC entry.
- `Unknown` (the thin-default manifest, plugin has not opted in) renders neutral gray ("unverified"), which is itself the signal to a catalog reader.

No new catalog data is required; the badge is a rendering rule over section 4 fields plus a RUSTSEC lookup. Ratifying #90 fixes the rule (the color mapping and the RUSTSEC cross-reference), not new schema.

### Compatibility badge

Renders from `umbral_req` matched against a chosen umbral version: green when the plugin's declared range includes it, amber when a newer plugin version covers it, red when nothing does. Again a pure function of section 4 fields; the badge rule is the deliverable.

### Verified publishers

Publisher verification attaches as an OPTIONAL `publisher` object on each catalog version entry, under a bumped `catalog_version` (from `"1"` to `"2"`). Shape:

```json
"publisher": {
  "id": "acme",
  "display_name": "Acme, Inc.",
  "verified": true,
  "verification_method": "domain",
  "verified_domains": ["acme.com"],
  "verified_at": "2026-08-01T00:00:00Z"
}
```

- `verification_method` is one of `domain` (control of a domain proven, e.g. a DNS TXT challenge), `github_org` (ownership of a GitHub org verified), or `manual` (maintainer-attested).
- Absent object means an unverified publisher; the reader shows no publisher badge, exactly as a `Unknown` security posture shows a neutral badge.
- The verification *process* (who runs the challenge, how the record is stored, how identity is revoked) is Stage 3 registry-service work. The *data contract* (the shape a verified publisher record takes in the catalog) is what this document ratifies, so the eventual service and today's tooling agree.

### Signing (recommendation: sigstore for the hosted path, minisign as the offline floor)

gaps5 #90 offers sigstore or minisign; the right answer is a layered pick, and both attach as an OPTIONAL `signature` object on the version entry (still `catalog_version` `"2"`).

- **Recommendation: sigstore (cosign) is the primary, hosted-registry signing path.** Its keyless, OIDC-backed, transparency-log model (Rekor) fits a marketplace: a publisher signs with an ephemeral key tied to a verified identity (the same GitHub org or domain that backs the `publisher` object), and the signature is publicly auditable without the marketplace running key custody. This is the strongest trust story and it composes with the verified-publisher identity rather than duplicating it. It is Stage 3 to *operate* (it needs the transparency log and the identity binding a running registry provides).
- **minisign is the offline, self-hosted floor.** For a plugin distributed outside any hosted registry (a private plugin, an air-gapped deploy, the Stage 2 self-hosted posture), a detached minisign signature over the version entry's canonical JSON, plus the signer's public key id, is a low-dependency mechanism a team can verify with no service at all. This is the honest option for Stage 1 and Stage 2 where no registry service exists.
- **Both serialize the same way in the catalog:** a `signature` object naming the scheme, the detached signature, and the key/identity reference.

```json
"signature": {
  "scheme": "sigstore",
  "bundle": "<base64 cosign bundle>",
  "identity": "https://github.com/acme/acme-billing/.github/workflows/release.yml@refs/tags/v1.2.0"
}
```

or, for the offline floor:

```json
"signature": {
  "scheme": "minisign",
  "sig": "<base64 detached signature>",
  "public_key_id": "RWQf6LRCGA9i..."
}
```

Because a catalog version entry is already a self-contained record (spec section 4.1), signing it is additive: the signature covers the canonical JSON of the entry. What this document ratifies is the `signature` object shape and the two-scheme recommendation (sigstore primary for the hosted path, minisign floor for the offline/self-hosted path). The signing *infrastructure* (key custody, rotation, trust roots, the transparency log) stays Stage 3.

### The additive layering, summarized

| Governance concern | Catalog change | When |
|---|---|---|
| Compatibility metadata | none (shipped in #6) | now |
| Security badge | rendering rule over existing `security` field | now (rule), Stage 3 (hosted UI) |
| Compatibility badge | rendering rule over existing `umbral_req` | now (rule), Stage 3 (hosted UI) |
| Verified publisher | optional `publisher` object, `catalog_version` "2" | contract now, verification service Stage 3 |
| Signing | optional `signature` object, sigstore or minisign | contract now, signing infra Stage 3 |

Every row is additive on the section 4.1 record; nothing breaks the `"1"` format except the two optional objects, which arrive together under `"2"`. This preserves the spec's core promise: the marketplace's trust layer attaches cleanly LATER, without a format break.

## 91. The functions story: handlers plus tasks now, a deployable unit only at Stage 3

### The question

Serverless platforms (Supabase Edge Functions, Firebase Cloud Functions, Cloudflare Workers) offer a "function" as a first-class deployable unit: a piece of code with its own secrets, logs, schedule, and resource limits, deployed and scaled independently of the app. gaps5 #91 asks whether umbral needs such a unit, or whether "functions" are already covered by what umbral has.

### What umbral has today

- **Handlers.** Request-scoped code mounted on a route (a plugin's `routes()` / `routes_builder()`). This is umbral's "run code in response to an HTTP request," which is exactly what an edge/HTTP function is.
- **Tasks.** The `umbral-tasks` plugin: DB-backed background jobs with a worker, plus `beat` for scheduled/periodic execution. This is umbral's "run code on a schedule or off the request path," which is exactly what a scheduled/background function is.
- **Settings and secrets.** Config via `Settings.extra` and the manifest's `required_settings` (with a `secret` flag), env-overridable. This is umbral's "a function's secrets."
- **Logs.** Structured `tracing` across the app, including handlers and workers.

Mapping the four defining traits of a serverless function onto what exists: secrets -> `required_settings` / settings; logs -> `tracing`; schedules -> tasks plus `beat`; the request entry point -> handlers. The one trait with no in-framework equivalent is **independent resource limits and independent deployment/scaling** (a function that scales and bills separately from the app), and that is precisely a control-plane concern.

### Recommendation

**Functions are best expressed as handlers plus tasks in umbral today. Do NOT introduce a new deployable "function" runtime unit now.** Concretely:

- A synchronous, request-triggered function is a **handler** on a route.
- An asynchronous, scheduled, or event-triggered function is a **task** (with `beat` for schedules).
- Its secrets are declared `required_settings`; its logs are `tracing`; its config is settings.

This is the honest Stage 1 answer: umbral is a framework, code runs inside the one app process (plus workers), and there is no control plane to give a function independent secrets scoping, isolated resource limits, or separate scaling. Inventing a "function unit" now would either be a thin rename of handlers/tasks (no new capability, pure confusion) or would require the very control plane the north star defers to Stage 3. Either way it is wrong to build today.

### The seam, recorded for Stage 3

If a deployable function unit is ever wanted, it is a Stage 3 feature because the traits it adds over a handler/task are all control-plane traits:

- **Independent deployment and scaling.** Requires the managed runtime (the control plane, gaps5 #5) that can provision and scale a unit separately from the app process.
- **Per-function secrets scoping.** Requires the project/environment/secrets model that Stage 3's control plane owns (a function's secrets are a subset scoped to that function, not the whole app's settings).
- **Per-function resource limits and metering.** Requires per-tenant provisioning and billing/metering (gaps5 #84), Stage 3.
- **Per-function logs and quotas.** A projection over the control plane's logs and quota system.

The seam that keeps Stage 3 additive rather than a rewrite: **a "function" would be modeled as a plugin capability**, not a new core primitive. A function unit is a handler-or-task plus a deployment descriptor (secrets scope, schedule, resource limits, entry point). That descriptor is exactly the kind of metadata the plugin manifest (`docs/specs/plugin-manifest-and-registry.md`) already carries for capabilities and required settings. So the future path is: a `FunctionSpec` capability on the `Plugin` trait (entry point plus the four control-plane traits as declarative fields) that the Stage 3 control plane reads to provision an independently deployed unit. Today that same code runs in-process as a handler or task; Stage 3 would let the control plane lift it out. Nothing about writing the handler/task today blocks that lift, because the code is already isolated behind the plugin contract.

This mirrors the north-star discipline exactly: name the Stage 1 answer (handlers plus tasks), keep the plugin contract as the boundary, and design the seam (a manifest-described function capability a control plane could later manage) so Stage 3 is composition, not a fork.

### What ratifying #91 commits us to

- The docs describe "how do I write a function in umbral" as "a handler for request-triggered code, a task for scheduled/background code," with the settings/secrets and tracing/logs mapping above. No new concept is marketed.
- No `Function` type, no function runtime, no per-function deployment is built pre-Stage-3.
- The `FunctionSpec`-as-plugin-capability seam is recorded (here) so that if Stage 3 is greenlit, the function unit is an additive manifest capability, not a core rewrite.

## Open decisions for the maintainer

1. **#89 post-1.0 support window:** confirm "current minor plus one prior" as the free supported window, and the LTS horizon (proposed 18 months) and cadence (proposed one LTS line per 12 months). These are the numbers a team plans upgrades against; they should be deliberate.
2. **#89 enterprise tiers:** confirm that enterprise support stays an out-of-scope Stage 3 commercial layer, or signal that a paid support tier is a near-term goal (which would pull maintenance-branch tooling earlier).
3. **#90 signing scheme:** ratify sigstore as the primary hosted-path scheme with minisign as the offline floor, or pick a single scheme. The catalog `signature` object supports either; the question is which to build first when Stage 3 lands.
4. **#90 catalog_version bump:** confirm the verified-publisher and signature objects land together under `catalog_version` "2" (versus trickling in under separate bumps).
5. **#91:** ratify "functions are handlers plus tasks, a deployable unit is Stage 3" and the `FunctionSpec`-as-plugin-capability seam, or signal that a function unit is wanted sooner (which reprioritizes toward the Stage 3 control plane).
