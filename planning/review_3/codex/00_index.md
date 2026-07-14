# Umbra Core And Plugin Review Index

Scope: core crates and built-in plugins in `/home/dalmas/E/projects/umbra`.

This pass focused on security, performance, correctness, simplicity, isolation, operations, and API footguns. Each concrete review item is in its own file in this folder.

## Items

- `01_oauth_missing_sessions_dependency.md` - OAuth routes require sessions but the plugin only declares `auth`.
- `02_graphql_mutations_lack_object_scope.md` - GraphQL mutations are table-level, not row/object scoped.
- `03_graphql_subscription_context_missing_auth.md` - GraphQL subscription transports do not carry the same auth/private-field context as POST queries.
- `04_graphql_child_loader_global_limit_truncates_relations.md` - Relation loaders apply one global limit across all parent IDs.
- `05_graphql_missing_query_complexity_depth_budget.md` - GraphQL has list limits but no query depth or complexity budget.
- `06_admin_m2m_write_not_atomic_with_parent.md` - Admin parent saves commit before many-to-many writes.
- `07_auth_bearer_touch_last_used_hot_path_write.md` - Bearer auth updates token usage on every authenticated request.
- `08_pg_route_context_reset_all_overhead.md` - PostgreSQL session variable setup resets all GUCs on checkout.
- `09_tenant_apps_inverse_mode_can_share_forgotten_tables.md` - Tenant inverse mode can make forgotten apps shared.
- `10_storage_media_access_public_by_default.md` - Media serving is public unless an access policy is configured.
- `11_storage_custom_backend_mounts_local_servedir.md` - Custom storage still mounts a local `ServeDir`.
- `12_realtime_redis_broker_unbounded_queue.md` - Redis realtime broker uses an unbounded internal queue.
- `13_rest_docs_contradict_hardened_defaults.md` - REST rustdoc still describes earlier unsafe defaults.
- `14_global_oncelock_state_limits_multi_app_tests.md` - Process-global registries make multi-app and test isolation brittle.
- `15_cache_redis_clear_flushdb_scope.md` - Redis cache clear uses `FLUSHDB`.
- `16_analytics_pageview_path_privacy.md` - Automatic analytics pageviews send raw paths by default.
- `17_tasks_enqueue_timeout_is_api_only.md` - Task enqueue timeout is accepted but not persisted or enforced.
- `18_cache_page_buffers_unbounded_get_responses.md` - Page cache buffers eligible responses without an object-size cap.
- `19_realtime_raw_group_publish_bypasses_policy.md` - Low-level realtime group publishing bypasses sender policy checks.
- `20_route_registry_drift_affects_audit_surfaces.md` - Plugin route metadata can drift from actual routes.
- `21_orm_dynamic_postgres_readbacks_leak_hidden_fields.md` - Dynamic ORM Postgres write/readback paths bypass read-side field policy.
- `22_orm_dynamic_in_filters_fail_open.md` - Dynamic IN filters can widen all-invalid filters to the whole queryset.
- `23_orm_postgres_specific_terminals_drift_from_generic_paths.md` - Postgres-only typed ORM terminals skip generic terminal behavior.
- `24_orm_raw_sql_escape_hatch_lacks_bind_and_write_routing.md` - Raw SQL has no bound-parameter variant and defaults to read routing.
- `25_orm_form_insert_integer_pk_assumption.md` - Dynamic form inserts return only `i64` primary keys.
- `26_orm_try_for_each_offset_pagination.md` - Chunked ORM iteration uses offset pagination for large scans.
- `27_orm_core_clean_areas.md` - Clean areas observed in the core ORM design.

## Strong Areas Observed

- The core ORM has a substantial amount of hardening already: typed predicates, SeaQuery-built SQL, fail-closed equality filters, read-side field policy, validation, cleaners, masking, soft-delete guards, and transactional dynamic writes.
- Core app build performs useful production checks, route collision checks, static file safety checks, backend mismatch checks, and model alias checks.
- Security middleware has strong defaults around CSRF, cache controls, common security headers, and boot-time warnings.
- Static file serving has path traversal and symlink defenses.
- REST now has read-only defaults, internal-table deny defaults, hard denied fields, object scopes, throttling hooks, and no-store defaults.
- RLS fails closed for PostgreSQL-only production usage and quotes identifiers defensively.
- Realtime has origin checking, max frame size, replay caps, and optional session-backed authentication.
