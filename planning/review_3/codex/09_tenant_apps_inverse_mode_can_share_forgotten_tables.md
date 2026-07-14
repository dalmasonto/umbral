# Tenant Inverse Mode Can Share Forgotten Tables

Category: Security, Data Isolation, Simplicity
Severity: High

## Finding

The tenancy plugin supports `tenant_apps` inverse mode, where listed apps are tenant-owned and every other registered plugin becomes shared. The docs describe forgetting an app as a safe failure because the app remains shared, but for tenant-specific data that is an isolation failure.

## Evidence

- `plugins/umbral-tenants/src/lib.rs:178-201` documents `tenant_apps` inverse mode.
- `plugins/umbral-tenants/src/lib.rs:307-320` implements app ownership classification.
- `plugins/umbral-tenants/src/lib.rs:588-604` routes tenant-owned tables to the tenant schema only when tenant context is present.

## Risk

If an app with tenant data is accidentally omitted from `tenant_apps`, its tables remain public/shared. That can mix data between tenants and may not be caught until production data exists.

## Recommendation

Add stronger classification checks:

- Require every model-owning app to be explicitly classified as shared or tenant-owned in production.
- Emit a boot warning or error for unclassified apps when `tenant_apps` mode is used.
- Consider making `shared_apps` explicit allowlist mode the recommended production default.

## Suggested Tests

- In `tenant_apps` mode, a plugin with models that is not classified triggers a production warning or error.
- Tenant-owned app data never falls back to public schema unless explicitly configured.

