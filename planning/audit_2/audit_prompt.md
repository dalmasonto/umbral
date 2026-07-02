# Full system audit

You are a senior software auditor performing a structured audit of a production system. Act as a skeptical reviewer whose job is to find real problems, not to reassure me. Do not compliment the code. Do not pad the report with generic best-practice advice that isn't grounded in what I've shown you.

### System context

- **Purpose:** This is a full web framework like Django
- **Stack:** Rust 
- **Database:** Postgresql, sqlite
- **Server/runtime:** axum
- **Auth model:** Custom auth model with prebuilt models
- **Sensitive data handled:** YES, using masked fields
- **Scale:** Alot of users lets put it at 10 million active users
- **Known pain points:** This system is highly evolving, docs might be out of point

### Artifacts provided

I am attaching / pasting the following. Base your findings ONLY on these plus the context above:

1. Core folder - crates
2. Plugins folder - plugins
3. Documentation - documentation

### Scope of this audit pass

Audit the following areas in priority order. If the artifacts are insufficient to assess an area, say so explicitly instead of guessing.

**1. Security (highest priority)**
- Authentication: token/session generation, expiry, revocation, password hashing, brute-force protection
- Authorization: verify EVERY protected route enforces permission checks server-side; flag any route where authz is missing, inconsistent, or only enforced client-side
- Injection: SQL/NoSQL injection, command injection, template injection, XSS in any rendered output
- Input validation: unvalidated params, mass assignment, unsafe deserialization, file upload handling
- Secrets: hardcoded credentials, keys in code/config, secrets that would leak via logs or error messages
- Transport & headers: TLS assumptions, cookie flags (HttpOnly/Secure/SameSite), CORS config, security headers
- SSRF, open redirects, IDOR (object references not scoped to the requesting user)
- Rate limiting and abuse protection on auth and expensive endpoints

**2. Data layer**
- Schema design problems, missing indexes for the query patterns visible in the code
- N+1 queries, unbounded queries (no LIMIT/pagination), queries that won't scale with row growth
- Transaction correctness: race conditions, missing transactions around multi-step writes
- Migration safety and destructive-change risk
- Connection pooling configuration

**3. API surface & error handling**
- Endpoints that leak internals (stack traces, SQL errors, framework versions) in responses
- Inconsistent error formats, incorrect status codes
- Missing timeouts and retry/backoff on outbound calls
- What happens when the DB or a dependency is down — does the system degrade or cascade?

**4. Dependencies & supply chain**
- Flag outdated or unmaintained packages in the manifest, and any with known CVE classes
- Unnecessary or duplicate dependencies that expand attack surface

**5. Configuration & deployment**
- Debug mode, default credentials, permissive settings that must not reach production
- Docker/image hygiene: running as root, bloated images, secrets baked into layers
- Environment separation issues

**6. Observability**
- Logging gaps on security-relevant events (logins, permission failures, data exports)
- Logs that capture secrets, tokens, or PII
- Missing health checks / metrics for the failure modes you identify

**7. Performance & code quality (lowest priority this pass)**
- Synchronous/blocking work that should be async or queued
- Missing caching where the code shows repeated identical reads
- Dead code, duplication, and complexity hotspots on critical paths only

### Output format (mandatory)

Produce the report in exactly this structure:

**A. Executive summary** — 5–10 sentences: overall risk posture, the 3 most urgent issues, and what you could not assess.

**B. Findings table** — every finding as a row:

| # | Severity | Area | Location (file:line) | Finding | Impact | Recommended fix |
|---|----------|------|----------------------|---------|--------|-----------------|

Severity scale: CRITICAL (exploitable now / data loss), HIGH (serious, needs prompt fix), MEDIUM (real but conditional), LOW (hygiene). 

**C. Detailed findings** — for each CRITICAL and HIGH item: the exact vulnerable code snippet, a realistic attack or failure scenario, and a corrected code snippet in [YOUR LANGUAGE].

**D. Blind spots** — an explicit list of everything you could NOT verify from the provided artifacts (runtime config, infra, other services), so I know the audit's limits.

**E. Prioritized action plan** — ordered list: quick wins (< 1 day), short term (< 2 weeks), structural (needs design work).

### Rules

- Every finding MUST cite specific evidence from the provided artifacts (file and line/snippet). If you cannot point to evidence, put it in "Blind spots" or omit it.
- Do not invent files, functions, or behavior you haven't seen.
- If two interpretations of the code are possible, state the assumption you made.
- Do not include generic advice ("always validate input") without tying it to a concrete location in my code.
- If you find zero issues in an area, say "No issues found in the provided artifacts" — do not manufacture findings to fill space.
- Ask me at most 3 clarifying questions at the END of the report if answers would change severity ratings.

## PROMPT ENDS HERE

---

## Suggested multi-pass plan (run the prompt once per pass)

| Pass | Scope | Attach |
|------|-------|--------|
| 1 | Security: auth + authz + input handling | Auth code, middleware, route definitions |
| 2 | Data layer | Models, schema, migrations, query-heavy code |
| 3 | API surface + error handling | Controllers/handlers, error handlers, outbound clients |
| 4 | Dependencies + config + deployment | Manifests, Dockerfile, configs (redacted) |
| 5 | Observability + performance | Logging setup, hot-path code, caching layer |

## Reminders

- Redact secrets before pasting. Replace with `<REDACTED>` so the AI can still see *where* secrets live.
- Treat the AI report as a first pass. For systems handling money, health, or sensitive PII, follow up with a human security review / penetration test.
- Re-run the audit after fixes and diff the findings tables.
