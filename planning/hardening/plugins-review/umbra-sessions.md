# Holistic review — `umbral-sessions`

Read-only review, 2026-06-16. Scope: `plugins/umbral-sessions/src/lib.rs` + `plugins/umbral-sessions/tests/**`, against the expected session-store + flash-messages surface. Cross-referenced against `planning/hardening/reviews/{race-conditions,security,correctness-domain,static-analysis}.md`; already-filed items are marked **already #N**. NEW findings marked **NEW**. Severity per the code-review skill.

## Verdict

Clean, single-backend session store that nails the hard parts: hash-at-rest tokens, lazy row creation (the correct "no row until first write" behavior), session-fixation defense with data carry-over, secure cookie flags, and a working flash-messages port. The architecture is right - fully user-agnostic so the dep arrow runs auth → sessions, with the `AuthUser` hydration correctly living in `umbral-auth`. **The completeness story is "one good backend, several features deferred."** The one real defect is already filed (`set_data` lost update, #71); everything net-new here is missing breadth (no rolling expiry, no Redis/cache backend, no session-cleanup command, no cookie/cache backends) rather than broken behavior.

**Completeness one-liner:** DB-backed sessions + flash messages are complete and correct; alternative backends (cache/cookie/Redis), rolling expiry, and `clearsessions` cleanup are all deferred, and the one concurrency bug (`set_data`) is already #71.

**Worst finding:** **already #71 (Critical)** — `set_data` read-modify-writes the whole `data` JSON blob with no atomicity (`lib.rs:400-457`); concurrent same-cookie writes silently drop each other's keys (cart / flash / CSRF state). Already filed. The net-new worst is **NEW (Important)** — no rolling/sliding expiry: `expires_at` is fixed at creation and never extended on activity, so an actively-used session hard-expires mid-use (`lib.rs:224`, no refresh path).

## Completeness (vs the expected session-store + flash-messages surface)

| Capability | Status | Notes |
|---|---|---|
| DB session backend | **Complete** | `Session` model, ambient pool, hash-at-rest id. The `db` backend equivalent. |
| Lazy row creation | **Complete** | `session_layer` mints token in memory; row materializes on first write (`lib.rs:825-879`). Kills the "fresh load leaves 3 anon rows" bug. Correct. Tested. |
| Cookie flags (Secure/HttpOnly/SameSite) | **Complete** | `HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` in every env but Dev (`lib.rs:314-329`). Correct secure-by-default. |
| Session fixation defense | **Complete** | `login_user_id` destroys the old (attacker-knowable) token's row + mints a fresh UUID, carrying `data` over (`lib.rs:485-528`). Tested (`flash_messages_survive_login_token_regeneration`). |
| Flash messages | **Complete** | `Messages` extractor, 5 standard severity levels, `add`/`drain`/`peek`, session-key reserved. Anonymous no-ops gracefully. Tested incl. survive-login. |
| Lazy expiry cleanup | **Partial** | `read_session` deletes an expired row on read (`lib.rs:249-254`). No scheduled sweep — expired-but-never-read rows accumulate forever. |
| Anonymous sessions | **Complete** | `user_id: Option<String>` NULL = anonymous; first-class. |
| Polymorphic user PK | **Complete** | `user_id` stored as `Display` string (gap #59); any `UserModel` PK round-trips. |
| **Rolling / sliding expiry** | **MISSING** | a save-every-request option that refreshes `expires_at` on activity. Here `expires_at` is set once at `create_session` (`lib.rs:224`) and never extended. An active user is logged out exactly `DEFAULT_TTL_SECONDS` after *login*, regardless of activity. |
| **Cache / Redis backend** | **MISSING (deferred)** | `lib.rs:39-40` "One session backend: DB rows." Alternative backends (cache, cached-db, Redis) are common. The CLAUDE.md plugin contract makes a swappable `SessionStore` trait the natural shape; none exists. |
| **Signed-cookie backend** | **MISSING (deferred)** | `lib.rs:41-42` "Cookie-only signed-store alternative is deferred until a real low-traffic case asks." Honestly deferred. |
| **`clearsessions` command** | **MISSING (deferred)** | `lib.rs:45-47` "a future `umbral-tasks` periodic job, or a `clearsessions` management command, lands when one or the other is real." No `Plugin::commands()` impl at all (`SessionsPlugin` has none). |
| **CSRF integration** | **Out of scope (separate plugin)** | `umbral-security` owns CSRF; sessions doesn't touch it. The CSRF token rides in `data` like any other key (and is therefore subject to the #71 lost-update). Architecturally correct to keep them separate. |
| `session.cycle_key()` (non-login rotation) | **MISSING** | rotating the key without a full login (privilege change, etc.) is a useful capability. Only the login path rotates here. Minor. |

No `todo!()`/`unimplemented!()`/`// TODO`/`// FIXME` in `src/`. The deferrals at `lib.rs:36-47` are documented honestly.

## Findings (net-new)

### Critical

1. **already #71 — `set_data` lost update + corrupt-data→empty-map silent decode.** `lib.rs:400-457`. Read-modify-write of the whole `data` blob (`:446` `from_str(...).unwrap_or_default()` discards a corrupt blob to `{}` with no log; `:407-456` merge-and-overwrite races concurrent writers). Both halves already filed under #71 (lost-update + log-decode-failure). The lazy-create race at `:426-444` is correctly handled (`UniqueViolation` → re-read); the *merge* is the unfixed part. Not re-counted.

### Important

2. **NEW - no rolling/sliding expiry.** `lib.rs:224` (set once), `session_layer` (`:825`) reuses a live row but never refreshes `expires_at`, `set_data`/`update_values` paths never touch it. An active session dies `DEFAULT_TTL_SECONDS` after login regardless of use - the opposite of most users' mental model and of a save-every-request expiry-refresh option. **Fix:** an opt-in `rolling: bool` on `SessionsPlugin` that bumps `expires_at = now + ttl` on each authenticated read (or each write). → **NEW gap (file)**.

3. **NEW - no `SessionStore` trait; the DB backend is hardcoded.** All helpers (`create_session`/`read_session`/`set_data`/…) call `Session::objects()` directly. The CLAUDE.md "thin core, plugin-heavy / swap any built-in" ethos and a pluggable session-engine model both point at a `SessionStore` abstraction. As-is, adding a cache/Redis backend means forking every helper. **Fix (design):** extract a `SessionStore` trait (get/set/destroy/touch) with the current DB impl as the default, so a future `umbral-cache`-backed store drops in. Larger than a hardening fix - flag as a deferred-spec item, not a quick patch. → **NEW gap (file)**.

4. **NEW — expired rows accumulate with no sweep, and there's no `clearsessions`.** `lib.rs:249-254` only deletes an expired row *when it is read*. A session created and abandoned (the common case — a bounced visitor) is never read again and its row lives forever. `SessionsPlugin` ships no `Plugin::commands()` and no periodic task. On a busy site this is unbounded table growth on `session`. **Fix:** a `clearsessions` `PluginCommand` (`DELETE WHERE expires_at < now()`) now, a periodic `umbral-tasks` job later. → **NEW gap (file)**, or fold into the existing `lib.rs:45` deferral by promoting it to a tracked entry.

### Minor

5. **NEW — `Messages::add`/`drain` inherit the `set_data` lost-update (#71) and additionally race themselves.** `lib.rs:663-673,704-711`: `add` does `read → push → set_data`, `drain` does `read → set_data(empty)`. Two concurrent `add`s (a redirect + an XHR) lose one message; an `add` racing a `drain` can resurrect a drained message or drop a new one. Same root cause as #71 (`set_data` isn't atomic) — once #71 lands an atomic JSON-merge op, `Messages` should route through it (append-merge, not read-modify-write). Noting so the #71 fix covers the messages call sites, not just the cart/CSRF ones.

6. **NEW — `secure_attr()` silently drops `Secure` whenever settings are *unresolved*, only logging nothing.** `lib.rs:314-318`: `match get_opt() { Some(Dev) => "", _ => "Secure; " }` — the `_` arm (incl. `None`/unresolved) correctly defaults to `Secure`, which is right. But the *Dev* arm drops `Secure` with no warning, and there's no guard that a non-loopback Dev bind (the misconfig `reviews/security.md` flags for Host validation) also ships insecure cookies. Ties into the Host-validation-in-Dev item (already noted in `security.md` as NEW). Minor here; the real fix is the shared "non-loopback bind in Dev" boot warning.

7. **NEW — `read_session` lazy-delete races a concurrent refresh (benign).** `lib.rs:249-254`. Already characterized as benign in `reviews/race-conditions.md:19` ("worst case is one extra anonymous round; no corruption"). Logged here only so it isn't re-audited; no action.

### Nit

8. **NEW — `set_cookie_header`/`clear_cookie_header` build the `Set-Cookie` string by hand.** `lib.rs:323-344`. Manual `format!` of cookie attributes is fine and tested, but a `cookie` crate builder would be harder to get subtly wrong (attribute ordering, escaping). The framework already standardizes on crates elsewhere; cosmetic.

9. **NEW — `login_user_id` carry-over uses `let _ =` on the data-restore UPDATE.** `lib.rs:516-519`: if the carry-over `update_values` fails, flash/cart silently vanish across login with no log. Per the CLAUDE.md "don't swallow secondary errors" rule, this should `tracing::warn!` on failure (the login still succeeds, so it's a warn not an error — same shape as the `last_login` bump which *does* log). Minor.

## Tests

Good behavioral coverage, properly exercising the public path. `tests/integration.rs` (752 LOC) round-trips create/read/destroy, expiry-deletes-the-row, data JSON round-trip, login bumps `last_login`, logout destroys + clears, messages add/drain/peek, anonymous flash, and — importantly — `login_destroys_anonymous_session_and_issues_new_token` + `flash_messages_survive_login_token_regeneration` (the fixation + carry-over invariants). `tests/lazy_session*.rs` prove the no-row-until-write and exactly-one-row behaviors through a real router via `tower::oneshot`. `session_layer_does_not_clobber_handler_set_cookie` guards the login-cookie-rotation interaction.

**Gaps in coverage (all NEW):**
- **No concurrency test for `set_data`** — the #71 lost-update is exactly the kind of bug a two-task interleaving test would pin. When #71 is fixed, a "two concurrent `set_data` on the same token both survive" test should land with it.
- **No expiry-refresh test** (because rolling expiry doesn't exist — finding #2).
- **No test that an expired-but-never-read row is *not* cleaned up** (would document finding #4's accumulation behavior).
- **No `Secure` flag test for the non-Dev environment path** — `set_cookie_header_carries_secure_defaults` exists but the env-gated branch (`secure_attr`) isn't exercised across both Dev and non-Dev.
- **No test for `messages` concurrency** (finding #5) — the single-threaded add/drain round-trips pass, but the interleaving that loses a message is untested.
