# Umbral Website

Official Umbral website project, built with Umbral itself.

## Backend Apps

The project was created with `umbral startproject --local` and the backend
areas were created with `umbral startapp --local`:

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

Models belong in each app's `plugins/<app>/src/models.rs`. `src/main.rs`
only wires the framework plugins, website apps, templates, routes, and CLI
dispatch.

## Commands

```bash
cargo run -- makemigrations
cargo run -- migrate
cargo run -- createsuperuser
cargo run -- serve
```

Do not add auto-migration to `main.rs`; the website should use the explicit
`makemigrations` then `migrate` workflow.
